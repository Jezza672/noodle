//! Take folders on the arrangement: flattening a comp into plain clips.

use noodle_core::{
    AudioClip, Clip, ClipContent, ClipGroup, ClipId, Command, Crossfade, DEFAULT_COMP_FADE_SECONDS,
    Project, TempoMap, Tick,
};

use super::clips;

/// The crossfade a comp join gets unless one was set: ten milliseconds across
/// the cut, half of it either side, at the tempo where the cut is.
pub(super) fn default_join(map: &TempoMap, at: Tick) -> Crossfade {
    let half = DEFAULT_COMP_FADE_SECONDS / 2.0;
    let seconds = map.seconds_at(at);
    let before = at.0 - map.tick_at_seconds((seconds - half).max(0.0)).round() as i64;
    let after = map.tick_at_seconds(seconds + half).round() as i64 - at.0;
    Crossfade {
        before: Tick(before.max(0)),
        after: Tick(after.max(0)),
    }
}

/// One take folder as plain clips: it is cut at every join of its comp, and
/// each piece plays its span's take from the file, butted against the next
/// with the join's crossfade. The unused takes are dropped (their audio stays
/// in the file). The first piece keeps the folder's clip ID. `None` if the
/// clip is not a take folder.
fn flattened(
    project: &Project,
    map: &TempoMap,
    id: ClipId,
    rate: u32,
    first_new: &mut u64,
) -> Option<Vec<(ClipId, Clip)>> {
    let clip = project.clip(id)?;
    let audio = clip.as_audio()?;
    let folder = audio.takes.as_ref()?;
    let warp = clip.as_audio()?.warp.and_then(|w| project.warp(w));
    let (from, to) = (audio.offset, audio.offset + audio.length);
    let mut cuts: Vec<(usize, Tick)> = Vec::new();
    for (index, span) in folder.comp.iter().enumerate().skip(1) {
        if span.at <= from || span.at >= to {
            continue;
        }
        let Some(tick) = clips::tick_of_frame(map, clip, span.at, rate, warp) else {
            continue;
        };
        cuts.push((index, tick));
    }
    // The spans in play: the one the window starts in, then each cut.
    let first_span = folder.span_at(from);
    let mut spans = vec![first_span];
    let mut pieces: Vec<Clip> = Vec::new();
    let mut rest = clip.clone();
    for (index, tick) in cuts {
        if let Some((before, after)) = clips::split(map, &rest, tick, rate, warp) {
            pieces.push(before);
            rest = after;
            spans.push(index);
        }
    }
    pieces.push(rest);
    let mut out = Vec::new();
    for (k, (mut piece, span)) in pieces.into_iter().zip(spans).enumerate() {
        let start = piece.start;
        let ClipContent::Audio(plain) = &mut piece.content else {
            return None;
        };
        let join = folder.comp[span].crossfade;
        let take = folder.takes.get(folder.comp[span].take)?;
        plain.takes = None;
        plain.offset += take.delta;
        plain.crossfade = (k > 0).then(|| join.unwrap_or_else(|| default_join(map, start)));
        let id = if k == 0 {
            id
        } else {
            let id = ClipId(*first_new);
            *first_new += 1;
            id
        };
        out.push((id, piece));
    }
    Some(out)
}

/// The comp edit that makes take `take` play from `from` to `to` on the take
/// folder `id` and on every take folder linked to it: the same stretch of
/// time on each, however each is placed in its file, so a comp across
/// drum mics stays aligned. One command, so one undo step. `rate_of` gives the
/// sample rate of an audio clip's file. `None` if it changes nothing (no
/// take folder, no such take, an empty range).
pub fn swipe(
    project: &Project,
    map: &TempoMap,
    id: ClipId,
    (from, to): (Tick, Tick),
    take: usize,
    rate_of: &mut dyn FnMut(&AudioClip) -> u32,
) -> Option<Command> {
    if from >= to {
        return None;
    }
    let mut commands = Vec::new();
    for member in project.linked_with(id) {
        let Some(clip) = project.clip(member) else {
            continue;
        };
        let mut changed = clip.clone();
        match &mut changed.content {
            ClipContent::Audio(audio) => {
                if audio.takes.is_none() {
                    continue;
                }
                let rate = rate_of(audio);
                let warp = audio.warp.and_then(|w| project.warp(w));
                let (window_from, window_to) = (audio.offset, audio.offset + audio.length);
                let end_tick = clips::clip_end(map, clip, rate, warp);
                // A swipe past the clip's ends stops at them.
                let frame = |tick: Tick, outside: u64| {
                    if tick <= clip.start {
                        window_from
                    } else {
                        clips::frame_of_tick(map, clip, tick, rate, warp).unwrap_or(outside)
                    }
                    .clamp(window_from, window_to)
                };
                // A swipe to the end of the clip runs on to the end of the file.
                let a = frame(from, window_from);
                if a >= window_to {
                    continue;
                }
                let b = if to >= end_tick {
                    u64::MAX
                } else {
                    frame(to, window_to)
                };
                if let Some(folder) = audio.takes.as_mut() {
                    folder.swipe(a, b, take);
                }
            }
            ClipContent::Midi(midi) => {
                let Some(takes) = midi.takes.as_mut() else {
                    continue;
                };
                let length = midi.length.0;
                let (a, b) = (
                    (from.0 - clip.start.0).clamp(0, length),
                    (to.0 - clip.start.0).clamp(0, length),
                );
                takes.swipe(Tick(a), Tick(b), take);
                midi.play_comp();
            }
        }
        if changed != *clip {
            commands.push(Command::SetClip {
                id: member,
                clip: changed,
            });
        }
    }
    match commands.len() {
        0 => None,
        1 => commands.pop(),
        _ => Some(Command::Batch(commands)),
    }
}

/// The edit that sets the crossfade of the join into span `span` of the take
/// folder `id` (audio) to `crossfade`, on the folders linked to it too (at
/// the same span, if their comps agree).
pub fn set_join(
    project: &Project,
    id: ClipId,
    span: usize,
    crossfade: Option<Crossfade>,
) -> Option<Command> {
    let spans = project.clip(id)?.as_audio()?.takes.as_ref()?.comp.len();
    let mut commands = Vec::new();
    for member in project.linked_with(id) {
        let Some(clip) = project.clip(member) else {
            continue;
        };
        let mut changed = clip.clone();
        if let ClipContent::Audio(audio) = &mut changed.content
            && let Some(folder) = audio.takes.as_mut()
            && folder.comp.len() == spans
        {
            folder.set_join(span, crossfade);
        }
        if changed != *clip {
            commands.push(Command::SetClip {
                id: member,
                clip: changed,
            });
        }
    }
    match commands.len() {
        0 => None,
        1 => commands.pop(),
        _ => Some(Command::Batch(commands)),
    }
}

/// The edit that turns the MIDI take folder `id` and the folders linked to it
/// into plain clips of what their comps play. One undo step.
pub fn flatten_midi(project: &Project, id: ClipId) -> Option<Command> {
    let mut commands = Vec::new();
    for member in project.linked_with(id) {
        let Some(clip) = project.clip(member) else {
            continue;
        };
        let mut changed = clip.clone();
        if let ClipContent::Midi(midi) = &mut changed.content {
            midi.takes = None;
        }
        if changed != *clip {
            commands.push(Command::SetClip {
                id: member,
                clip: changed,
            });
        }
    }
    (!commands.is_empty()).then_some(Command::Batch(commands))
}

/// The edit that flattens the take folder `id` and the take folders linked to
/// it into plain clips, as one undo step. The pieces of linked folders are
/// linked piece by piece, as a cut leaves them. `None` if none is a take
/// folder.
pub fn flatten(project: &Project, map: &TempoMap, id: ClipId, rate: u32) -> Option<Command> {
    let mut next = project.next_clip_id().0;
    let mut commands = Vec::new();
    // Per piece number, the clips that made it.
    let mut rows: Vec<Vec<ClipId>> = Vec::new();
    for member in project.linked_with(id) {
        let Some(pieces) = flattened(project, map, member, rate, &mut next) else {
            continue;
        };
        for (k, (piece_id, piece)) in pieces.into_iter().enumerate() {
            if rows.len() <= k {
                rows.push(Vec::new());
            }
            rows[k].push(piece_id);
            commands.push(if k == 0 {
                Command::SetClip {
                    id: piece_id,
                    clip: piece,
                }
            } else {
                Command::AddClip {
                    id: piece_id,
                    clip: piece,
                }
            });
        }
    }
    if commands.is_empty() {
        return None;
    }
    let mut group = project.next_clip_group_id();
    for row in rows.iter().skip(1).filter(|row| row.len() >= 2) {
        commands.push(Command::SetClipGroup {
            id: group,
            group: Some(ClipGroup {
                members: row.iter().copied().collect(),
            }),
        });
        group.0 += 1;
    }
    Some(Command::Batch(commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{AudioClip, Connection, Endpoint, History, Node, NodeId, Take, TakeFolder};
    use noodle_engine::{OUTPUT_ID, Registry, Settings};
    use noodle_nodes::{TRACK_INPUT_ID, render_project_with_clips};

    const RATE: u32 = 48_000;
    const N: u64 = 24_000;
    const SETTINGS: Settings = Settings {
        sample_rate: RATE as f32,
        max_frames: 512,
        channels: 2,
    };

    fn ramp(frame: f64) -> f32 {
        (frame / 10_000.0) as f32
    }

    /// Three takes of one ramp at different levels, in a file. The file is
    /// written once per test run, in a folder of this process's own: tests
    /// run in parallel, and one rewriting the file while another renders from
    /// it made the render read a half-written file (#378).
    fn dir() -> std::path::PathBuf {
        static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        DIR.get_or_init(|| {
            let dir =
                std::env::temp_dir().join(format!("noodle-comp-flatten-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let samples: Vec<f32> = (0..3 * N)
                .flat_map(|i| [ramp((i % N) as f64) * (1 + i / N) as f32, 0.0])
                .collect();
            noodle_io::write_wav(&dir.join("takes.wav"), &samples, 2, RATE).unwrap();
            dir
        })
        .clone()
    }

    /// Two tracks, each with a take folder comped take 0, take 1 from frame
    /// 8000, take 2 from 16000, linked.
    fn project() -> (Project, [ClipId; 2]) {
        let mut project = Project::new();
        let mut history = History::new();
        let mut commands = vec![
            Command::AddNode {
                id: NodeId(9),
                node: Node::new(OUTPUT_ID),
            },
            Command::AddNode {
                id: NodeId(8),
                node: Node::new("noodle.util.mix").with_config(
                    noodle_core::Config::new().with("inputs", noodle_core::Value::Int(2)),
                ),
            },
            Command::Connect(Connection {
                from: Endpoint::new(NodeId(8), "out"),
                to: Endpoint::new(NodeId(9), "in"),
            }),
        ];
        let mut ids = [ClipId(0); 2];
        for (n, slot) in ids.iter_mut().enumerate() {
            let track = NodeId(n as u64 + 1);
            commands.push(Command::AddNode {
                id: track,
                node: Node::new(TRACK_INPUT_ID),
            });
            commands.push(Command::Connect(Connection {
                from: Endpoint::new(track, "audio"),
                to: Endpoint::new(NodeId(8), format!("in{}", n + 1)),
            }));
            let mut audio = AudioClip::new("takes.wav", N);
            let mut folder = TakeFolder::new((0..3).map(|k| Take { delta: k * N }).collect(), 0);
            folder.swipe(8_000, 16_000, 1);
            folder.swipe(16_000, N, 2);
            audio.takes = Some(folder);
            let id = project.new_clip_id();
            *slot = id;
            commands.push(Command::AddClip {
                id,
                clip: Clip {
                    node: track,
                    start: Tick(0),
                    content: ClipContent::Audio(audio),
                },
            });
        }
        history
            .apply(&mut project, Command::Batch(commands))
            .unwrap();
        let link = project.link_command(&ids).unwrap();
        history.apply(&mut project, link).unwrap();
        (project, ids)
    }

    fn render(project: &Project) -> Vec<f32> {
        let mut registry = Registry::with_builtins();
        let rendered =
            render_project_with_clips(project, &mut registry, &dir(), SETTINGS, 30_000).unwrap();
        assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
        rendered.render.samples.iter().step_by(2).copied().collect()
    }

    #[test]
    fn flattening_cuts_the_comp_at_its_joins_into_plain_clips_that_sound_the_same() {
        let (mut project, ids) = project();
        let before = render(&project);
        let map = project.tempo_map().clone();
        let command = flatten(&project, &map, ids[0], RATE).unwrap();
        let mut history = History::new();
        history.apply(&mut project, command).unwrap();

        // Each folder became three clips, none with takes; the pieces after
        // the first carry the join's crossfade.
        let clips: Vec<_> = project.clips().collect();
        assert_eq!(clips.len(), 6);
        for (_, clip) in &clips {
            assert!(clip.as_audio().unwrap().takes.is_none());
        }
        let on_first: Vec<_> = project
            .clips_on(NodeId(1))
            .map(|(_, c)| c.as_audio().unwrap())
            .collect();
        let mut offsets: Vec<u64> = on_first.iter().map(|a| a.offset).collect();
        offsets.sort();
        // Take 0 from the start, take 1 from frame 8000 and take 2 from 16000
        // (a tick of rounding either way), each in its own part of the file.
        assert_eq!(offsets[0], 0);
        assert!(offsets[1].abs_diff(N + 8_000) <= 25, "{offsets:?}");
        assert!(offsets[2].abs_diff(2 * N + 16_000) <= 25, "{offsets:?}");
        assert_eq!(on_first.iter().filter(|a| a.crossfade.is_some()).count(), 2);
        // The pieces of the two tracks are linked piece by piece, and the
        // first pieces kept their link.
        assert_eq!(project.group_of(ids[0]).map(|(_, m)| m.len()), Some(2));
        let linked_rows = project
            .clips()
            .filter(|(id, _)| project.group_of(*id).is_some())
            .count();
        assert_eq!(linked_rows, 6);

        // It sounds as the comp did, sample for sample outside the joins.
        let after = render(&project);
        for f in 300..(N as usize - 100) {
            let near_join = [8_000usize, 16_000].iter().any(|&j| f.abs_diff(j) < 300);
            let tolerance = if near_join { 0.3 } else { 1e-3 };
            assert!(
                (after[f] - before[f]).abs() < tolerance,
                "frame {f}: {} vs {}",
                after[f],
                before[f]
            );
        }

        // One undo brings the folders back.
        history.undo(&mut project).unwrap();
        assert_eq!(project.clips().count(), 2);
        assert!(
            project
                .clip(ids[0])
                .unwrap()
                .as_audio()
                .unwrap()
                .takes
                .is_some()
        );
    }

    fn no_files(_: &AudioClip) -> u32 {
        RATE
    }

    #[test]
    fn a_swipe_on_one_linked_folder_comps_them_all_the_same_way_in_one_undo_step() {
        let (mut project, ids) = project();
        let map = project.tempo_map().clone();
        // 120 bpm: a tick is 1/1920 s, so 160 ticks is frame 4000.
        let command = swipe(
            &project,
            &map,
            ids[1],
            (Tick(160), Tick(400)),
            2,
            &mut no_files,
        )
        .unwrap();
        let mut history = History::new();
        history.apply(&mut project, command).unwrap();
        for id in ids {
            let folder = project
                .clip(id)
                .unwrap()
                .as_audio()
                .unwrap()
                .takes
                .clone()
                .unwrap();
            let comp: Vec<(u64, usize)> = folder.comp.iter().map(|s| (s.at, s.take)).collect();
            assert_eq!(
                comp,
                [(0, 0), (4_000, 2), (10_000, 1), (16_000, 2), (24_000, 0)],
                "{id:?}"
            );
        }
        // It plays take 2 (three times the ramp) over the swiped stretch.
        let samples = render(&project);
        // Two tracks of the same comp, summed.
        let at = |f: usize| samples[f] / 2.0;
        assert!((at(6_000) - 3.0 * ramp(6_000.0)).abs() < 1e-3);
        assert!((at(2_000) - ramp(2_000.0)).abs() < 1e-3);
        assert!((at(13_000) - 2.0 * ramp(13_000.0)).abs() < 1e-3);
        history.undo(&mut project).unwrap();
        for id in ids {
            let folder = project
                .clip(id)
                .unwrap()
                .as_audio()
                .unwrap()
                .takes
                .clone()
                .unwrap();
            assert_eq!(folder.comp.len(), 4);
        }
    }

    #[test]
    fn a_swipe_follows_each_linked_folder_to_its_own_place_in_its_file() {
        let (mut project, ids) = project();
        let map = project.tempo_map().clone();
        // The second folder starts a beat later and shows the file from
        // frame 24000 on, so the same time is a different frame in it.
        let mut moved = project.clip(ids[1]).unwrap().clone();
        moved.start = Tick(960);
        if let ClipContent::Audio(audio) = &mut moved.content {
            audio.offset = 0;
        }
        let mut history = History::new();
        history
            .apply(
                &mut project,
                Command::SetClip {
                    id: ids[1],
                    clip: moved,
                },
            )
            .unwrap();
        // Ticks 1120..1360 are 160..400 into the second folder, 4000..10000.
        let command = swipe(
            &project,
            &map,
            ids[0],
            (Tick(1_120), Tick(1_360)),
            1,
            &mut no_files,
        )
        .unwrap();
        history.apply(&mut project, command).unwrap();
        let comp = |id| {
            let folder = project
                .clip(id)
                .unwrap()
                .as_audio()
                .unwrap()
                .takes
                .clone()
                .unwrap();
            folder
                .comp
                .iter()
                .map(|s| (s.at, s.take))
                .collect::<Vec<_>>()
        };
        // Joined with the take 1 already playing from 8000.
        assert_eq!(comp(ids[1]), [(0, 0), (4_000, 1), (16_000, 2), (24_000, 0)]);
        // The first folder ends before that time: nothing to change there.
        assert_eq!(comp(ids[0]), [(0, 0), (8_000, 1), (16_000, 2), (24_000, 0)]);
    }

    #[test]
    fn the_join_crossfade_is_set_on_every_linked_folder() {
        let (mut project, ids) = project();
        let fade = Crossfade {
            before: Tick(40),
            after: Tick(40),
        };
        let command = set_join(&project, ids[0], 1, Some(fade)).unwrap();
        History::new().apply(&mut project, command).unwrap();
        for id in ids {
            let folder = project
                .clip(id)
                .unwrap()
                .as_audio()
                .unwrap()
                .takes
                .clone()
                .unwrap();
            assert_eq!(folder.comp[1].crossfade, Some(fade));
            assert_eq!(folder.comp[2].crossfade, None);
        }
    }

    fn midi_folder(project: &mut Project, history: &mut History) -> ClipId {
        let track = NodeId(1);
        let note = |start: i64, key: u8| noodle_core::MidiNote::new(Tick(start), Tick(100), key);
        let takes = noodle_core::MidiTakes::new(
            vec![
                noodle_core::TakeContent {
                    notes: vec![note(0, 60), note(960, 61)],
                    controls: Vec::new(),
                },
                noodle_core::TakeContent {
                    notes: vec![note(0, 70), note(960, 71)],
                    controls: Vec::new(),
                },
            ],
            0,
        );
        let mut clip = Clip::midi(track, Tick(0), Tick(1_920));
        if let ClipContent::Midi(midi) = &mut clip.content {
            midi.takes = Some(takes);
            midi.play_comp();
        }
        let id = project.new_clip_id();
        history
            .apply(
                project,
                Command::Batch(vec![
                    Command::AddNode {
                        id: track,
                        node: Node::new(TRACK_INPUT_ID),
                    },
                    Command::AddClip { id, clip },
                ]),
            )
            .unwrap();
        id
    }

    fn keys(project: &Project, id: ClipId) -> Vec<u8> {
        let mut keys: Vec<u8> = project
            .clip(id)
            .unwrap()
            .as_midi()
            .unwrap()
            .notes
            .iter()
            .map(|n| n.key)
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn swiping_a_midi_take_folder_plays_that_takes_notes_there() {
        let mut project = Project::new();
        let mut history = History::new();
        let id = midi_folder(&mut project, &mut history);
        assert_eq!(keys(&project, id), [60, 61]);
        let map = project.tempo_map().clone();
        // The second beat from take 1.
        let command = swipe(
            &project,
            &map,
            id,
            (Tick(480), Tick(1_920)),
            1,
            &mut no_files,
        )
        .unwrap();
        history.apply(&mut project, command).unwrap();
        assert_eq!(keys(&project, id), [60, 71]);
        history.undo(&mut project).unwrap();
        assert_eq!(keys(&project, id), [60, 61]);
    }

    #[test]
    fn editing_a_midi_take_folder_as_a_plain_clip_drops_its_takes() {
        let mut project = Project::new();
        let mut history = History::new();
        let id = midi_folder(&mut project, &mut history);
        let mut edited = project.clip(id).unwrap().clone();
        if let ClipContent::Midi(midi) = &mut edited.content {
            midi.notes[0].key = 64;
        }
        history
            .apply(&mut project, Command::SetClip { id, clip: edited })
            .unwrap();
        assert!(project.clip(id).unwrap().as_midi().unwrap().takes.is_none());
        assert_eq!(keys(&project, id), [61, 64]);
        // Flattening keeps the notes the comp plays.
        let mut project = Project::new();
        let id = midi_folder(&mut project, &mut History::new());
        let command = flatten_midi(&project, id).unwrap();
        History::new().apply(&mut project, command).unwrap();
        assert!(project.clip(id).unwrap().as_midi().unwrap().takes.is_none());
        assert_eq!(keys(&project, id), [60, 61]);
    }

    #[test]
    fn a_plain_clip_has_nothing_to_flatten() {
        let mut project = Project::new();
        let id = project.new_clip_id();
        let mut history = History::new();
        history
            .apply(
                &mut project,
                Command::Batch(vec![
                    Command::AddNode {
                        id: NodeId(1),
                        node: Node::new(TRACK_INPUT_ID),
                    },
                    Command::AddClip {
                        id,
                        clip: Clip::audio(NodeId(1), Tick(0), "a.wav", 100),
                    },
                ]),
            )
            .unwrap();
        let map = project.tempo_map().clone();
        assert!(flatten(&project, &map, id, RATE).is_none());
    }
}
