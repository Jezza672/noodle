//! Take folders on the arrangement: flattening a comp into plain clips.

use noodle_core::{
    Clip, ClipContent, ClipGroup, ClipId, Command, Crossfade, DEFAULT_COMP_FADE_SECONDS, Project,
    TempoMap, Tick,
};

use super::clips;

/// The crossfade a comp join gets unless one was set: ten milliseconds across
/// the cut, half of it either side, at the tempo where the cut is.
fn default_join(map: &TempoMap, at: Tick) -> Crossfade {
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

    /// Three takes of one ramp at different levels, in a file.
    fn dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("noodle-comp-flatten");
        std::fs::create_dir_all(&dir).unwrap();
        let samples: Vec<f32> = (0..3 * N)
            .flat_map(|i| [ramp((i % N) as f64) * (1 + i / N) as f32, 0.0])
            .collect();
        noodle_io::write_wav(&dir.join("takes.wav"), &samples, 2, RATE).unwrap();
        dir
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
