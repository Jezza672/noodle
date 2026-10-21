//! Moving and trimming clips: the arithmetic between ticks on the screen and
//! frames in the file.
//!
//! A clip starts at a tick but is as long as its audio, so its end depends on
//! the tempo map. Trims turn the tick the user dragged to back into frames.

use noodle_core::{AudioClip, Clip, ClipContent, ClipWarp, MidiClip, TempoMap, Tick};

pub use super::warped::Pass;
use super::warped::Section;

/// What a clip needs to know about its file to be laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source {
    pub sample_rate: u32,
    /// How many frames the file has, when its header says.
    pub frames: Option<u64>,
}

/// The tick a clip of `length` frames starting at `start` ends on.
pub fn end_tick(map: &TempoMap, start: Tick, length: u64, rate: u32) -> Tick {
    let rate = f64::from(rate);
    let end = map.sample_at(start, rate) + length;
    Tick(map.tick_at_sample(end, rate).round() as i64)
}

/// The shortest a MIDI clip can be trimmed to: an eighth of a beat.
pub const MIN_MIDI_LENGTH: i64 = 120;

/// The tick a clip ends on, laying audio out at `rate` when the clip's file
/// is not known any better. MIDI clips are as long as they say. `warp` is the
/// audio clip's warp, if it has one: a warped section lasts as long as its
/// audio takes to play once warped.
pub fn clip_end(map: &TempoMap, clip: &Clip, rate: u32, warp: Option<&ClipWarp>) -> Tick {
    match &clip.content {
        ClipContent::Audio(audio) => match warp.filter(|warp| !warp.is_identity()) {
            Some(warp) => Section::new(map, warp, rate).end(clip.start, audio),
            None => end_tick(map, clip.start, audio.length, rate),
        },
        ClipContent::Midi(midi) => Tick(clip.start.0 + midi.length.0),
    }
}

/// How an audio section is laid out against the file: frame for frame, or
/// through its warp. `None` if it names a warp that isn't there.
enum Layout<'a> {
    Frames,
    Warped(Section<'a>),
}

fn layout<'a>(
    map: &'a TempoMap,
    audio: &AudioClip,
    warp: Option<&'a ClipWarp>,
    rate: u32,
) -> Option<Layout<'a>> {
    match (audio.warp, warp) {
        (None, _) => Some(Layout::Frames),
        (Some(_), None) => None,
        (Some(_), Some(warp)) if warp.is_identity() => Some(Layout::Frames),
        (Some(_), Some(warp)) => Some(Layout::Warped(Section::new(map, warp, rate))),
    }
}

/// The runs through the loop of an audio section that repeats one, for
/// drawing it.
pub fn passes(
    map: &TempoMap,
    clip: &Clip,
    rate: u32,
    warp: Option<&ClipWarp>,
) -> Option<Vec<Pass>> {
    let audio = clip.as_audio()?;
    match layout(map, audio, warp, rate)? {
        Layout::Frames => None,
        Layout::Warped(section) => section.passes(clip.start, audio),
    }
}

/// The ticks at which an audio section's fade in ends and its fade out starts.
pub fn fade_ticks(
    map: &TempoMap,
    clip: &Clip,
    rate: u32,
    warp: Option<&ClipWarp>,
) -> Option<(Tick, Tick)> {
    let audio = clip.as_audio()?;
    Some(match layout(map, audio, warp, rate)? {
        Layout::Frames => {
            let rate = f64::from(rate);
            let start = map.sample_at(clip.start, rate);
            let tick = |sample: u64| Tick(map.tick_at_sample(sample, rate).round() as i64);
            (
                tick(start + audio.fade_in),
                tick(start + audio.length - audio.fade_out),
            )
        }
        Layout::Warped(section) => (
            section.fade_in_tick(clip.start, audio),
            section.fade_out_tick(clip.start, audio),
        ),
    })
}

/// The clip with its start at `start`.
pub fn moved(clip: &Clip, start: Tick) -> Clip {
    Clip {
        start,
        ..clip.clone()
    }
}

/// The clip with its left edge dragged to `edge`: the start moves, and the
/// audio before it is cut off (or brought back, down to the file's start).
/// `None` if that would leave nothing.
pub fn trim_start(
    map: &TempoMap,
    clip: &Clip,
    edge: Tick,
    rate: u32,
    warp: Option<&ClipWarp>,
) -> Option<Clip> {
    if let Some(midi) = clip.as_midi() {
        return Some(trim_midi_start(clip, midi, edge));
    }
    let audio = clip.as_audio()?;
    // Trims count in the file's frames, which a warp stretches.
    if let Layout::Warped(section) = layout(map, audio, warp, rate)? {
        return section.trim_start(clip, audio, edge);
    }
    let rate_f = f64::from(rate);
    let start_sample = map.sample_at(clip.start, rate_f) as i64;
    let edge_sample = map.sample_at(edge.max(Tick::ZERO), rate_f) as i64;
    let wanted = edge_sample - start_sample;
    let delta = wanted.clamp(-(audio.offset as i64), audio.length as i64 - 1);
    if delta == 0 {
        return Some(clip.clone());
    }
    let start = if delta == wanted {
        edge.max(Tick::ZERO)
    } else {
        Tick(
            map.tick_at_sample((start_sample + delta) as u64, rate_f)
                .round() as i64,
        )
    };
    let mut audio = audio.clone();
    audio.offset = (audio.offset as i64 + delta) as u64;
    audio.length = (audio.length as i64 - delta) as u64;
    fit_fades(&mut audio);
    Some(Clip {
        start,
        content: noodle_core::ClipContent::Audio(audio),
        ..clip.clone()
    })
}

/// The clip with its right edge dragged to `edge`. It can't be longer than
/// what's left of the file after its offset, if the file is known, and keeps
/// at least a frame.
pub fn trim_end(
    map: &TempoMap,
    clip: &Clip,
    edge: Tick,
    rate: u32,
    file_frames: Option<u64>,
    warp: Option<&ClipWarp>,
) -> Option<Clip> {
    if let Some(midi) = clip.as_midi() {
        let length = (edge.0 - clip.start.0).max(MIN_MIDI_LENGTH);
        // Notes that start past the new end are cut off with it.
        let notes = midi
            .notes
            .iter()
            .filter(|note| note.start.0 < length)
            .copied()
            .collect();
        return Some(Clip {
            content: ClipContent::Midi(MidiClip {
                length: Tick(length),
                notes,
            }),
            ..clip.clone()
        });
    }
    let audio = clip.as_audio()?;
    if let Layout::Warped(section) = layout(map, audio, warp, rate)? {
        return section.trim_end(clip, audio, edge, file_frames);
    }
    let rate_f = f64::from(rate);
    let start_sample = map.sample_at(clip.start, rate_f);
    let edge_sample = map.sample_at(edge.max(Tick::ZERO), rate_f);
    let mut length = edge_sample.saturating_sub(start_sample).max(1);
    if let Some(frames) = file_frames {
        length = length.min(frames.saturating_sub(audio.offset).max(1));
    }
    let mut audio = audio.clone();
    audio.length = length;
    fit_fades(&mut audio);
    Some(Clip {
        content: noodle_core::ClipContent::Audio(audio),
        ..clip.clone()
    })
}

/// How far, in ticks, the audio before an audio clip's start reaches back:
/// what a crossfade into the clip can use before the cut.
pub fn head_room(map: &TempoMap, clip: &Clip, rate: u32, warp: Option<&ClipWarp>) -> Tick {
    let Some(audio) = clip.as_audio() else {
        return Tick::ZERO;
    };
    let Some(layout) = layout(map, audio, warp, rate) else {
        return Tick::ZERO;
    };
    let back = match layout {
        Layout::Frames => {
            let rate = f64::from(rate);
            let start = map.sample_at(clip.start, rate);
            Tick(
                map.tick_at_sample(start.saturating_sub(audio.offset), rate)
                    .round() as i64,
            )
        }
        Layout::Warped(section) => {
            section.tick_of(clip.start, audio.offset as f64 / f64::from(rate), 0.0)
        }
    };
    Tick((clip.start.0 - back.0).max(0))
}

/// How far, in ticks, the file goes on after an audio clip's end: what a
/// crossfade out of the clip can play past the cut. Nothing if the file's
/// length isn't known.
pub fn tail_room(
    map: &TempoMap,
    clip: &Clip,
    rate: u32,
    file_frames: Option<u64>,
    warp: Option<&ClipWarp>,
) -> Tick {
    let (Some(audio), Some(frames)) = (clip.as_audio(), file_frames) else {
        return Tick::ZERO;
    };
    let Some(layout) = layout(map, audio, warp, rate) else {
        return Tick::ZERO;
    };
    let end = clip_end(map, clip, rate, warp);
    let forward = match layout {
        Layout::Frames => {
            let rate = f64::from(rate);
            let start = map.sample_at(clip.start, rate);
            let last = start + frames.saturating_sub(audio.offset);
            Tick(map.tick_at_sample(last, rate).round() as i64)
        }
        Layout::Warped(section) => section.tick_of(
            clip.start,
            audio.offset as f64 / f64::from(rate),
            frames as f64 / f64::from(rate),
        ),
    };
    Tick((forward.0 - end.0).max(0))
}

/// A MIDI clip with its left edge dragged to `edge`. The notes keep their
/// place on the timeline: the ones before the new start are cut off or, when
/// the clip is extended to the left, start later in the clip.
fn trim_midi_start(clip: &Clip, midi: &MidiClip, edge: Tick) -> Clip {
    let end = clip.start.0 + midi.length.0;
    let start = edge.0.clamp(0, (end - MIN_MIDI_LENGTH).max(0));
    let delta = start - clip.start.0;
    let notes = midi
        .notes
        .iter()
        .filter_map(|note| {
            let from = note.start.0 - delta;
            let to = from + note.length.0;
            (to > 0).then(|| noodle_core::MidiNote {
                start: Tick(from.max(0)),
                length: Tick(to - from.max(0)),
                ..*note
            })
        })
        .collect();
    Clip {
        start: Tick(start),
        content: ClipContent::Midi(MidiClip {
            length: Tick(end - start),
            notes,
        }),
        ..clip.clone()
    }
}

/// The clip with its fade in ending at `at`, the longest it can be without
/// running into the fade out. Dragging left of the start removes the fade.
pub fn set_fade_in(
    map: &TempoMap,
    clip: &Clip,
    at: Tick,
    rate: u32,
    warp: Option<&ClipWarp>,
) -> Option<Clip> {
    let audio = clip.as_audio()?;
    if let Layout::Warped(section) = layout(map, audio, warp, rate)? {
        return Some(section.set_fade_in(clip, audio, at));
    }
    let rate = f64::from(rate);
    let wanted = map
        .sample_at(at.max(Tick::ZERO), rate)
        .saturating_sub(map.sample_at(clip.start, rate));
    let mut audio = audio.clone();
    audio.fade_in = wanted.min(audio.length - audio.fade_out.min(audio.length));
    Some(Clip {
        content: noodle_core::ClipContent::Audio(audio),
        ..clip.clone()
    })
}

/// The clip with its fade out starting at `at`, the longest it can be
/// without running into the fade in.
pub fn set_fade_out(
    map: &TempoMap,
    clip: &Clip,
    at: Tick,
    rate: u32,
    warp: Option<&ClipWarp>,
) -> Option<Clip> {
    let audio = clip.as_audio()?;
    if let Layout::Warped(section) = layout(map, audio, warp, rate)? {
        return Some(section.set_fade_out(clip, audio, at));
    }
    let rate = f64::from(rate);
    let end = map.sample_at(clip.start, rate) + audio.length;
    let wanted = end.saturating_sub(map.sample_at(at.max(Tick::ZERO), rate));
    let mut audio = audio.clone();
    audio.fade_out = wanted.min(audio.length - audio.fade_in.min(audio.length));
    Some(Clip {
        content: noodle_core::ClipContent::Audio(audio),
        ..clip.clone()
    })
}

/// The clip cut in two at `at`: the part before it and the part from it on,
/// each a clip of its own that plays what was there, sample for sample.
/// `None` if `at` isn't inside the clip. A fade out stays with the second
/// half and a fade in with the first. A MIDI note that spans the cut is
/// shortened in the first half and sounds again at the start of the second.
pub fn split(
    map: &TempoMap,
    clip: &Clip,
    at: Tick,
    rate: u32,
    warp: Option<&ClipWarp>,
) -> Option<(Clip, Clip)> {
    if at <= clip.start {
        return None;
    }
    match &clip.content {
        ClipContent::Audio(audio) => {
            // The cut counts in the file's frames, which a warp stretches.
            if let Layout::Warped(section) = layout(map, audio, warp, rate)? {
                return section.split(clip, audio, at);
            }
            let rate = f64::from(rate);
            // From the (rounded) cut tick, so both halves agree on the frame.
            let cut = map
                .sample_at(at, rate)
                .checked_sub(map.sample_at(clip.start, rate))?;
            if cut == 0 || cut >= audio.length {
                return None;
            }
            let mut first = audio.clone();
            first.length = cut;
            first.fade_out = 0;
            fit_fades(&mut first);
            let mut second = audio.clone();
            second.offset += cut;
            second.length -= cut;
            second.fade_in = 0;
            fit_fades(&mut second);
            Some((
                Clip {
                    content: ClipContent::Audio(first),
                    ..clip.clone()
                },
                Clip {
                    start: at,
                    content: ClipContent::Audio(second),
                    ..clip.clone()
                },
            ))
        }
        ClipContent::Midi(midi) => {
            let cut = at.0 - clip.start.0;
            if cut <= 0 || cut >= midi.length.0 {
                return None;
            }
            let mut before = Vec::new();
            let mut after = Vec::new();
            for note in &midi.notes {
                let end = note.start.0 + note.length.0;
                if note.start.0 < cut {
                    before.push(noodle_core::MidiNote {
                        length: Tick(note.length.0.min(cut - note.start.0)),
                        ..*note
                    });
                }
                if end > cut {
                    let from = (note.start.0 - cut).max(0);
                    after.push(noodle_core::MidiNote {
                        start: Tick(from),
                        length: Tick(end - cut - from),
                        ..*note
                    });
                }
            }
            Some((
                Clip {
                    content: ClipContent::Midi(MidiClip {
                        length: Tick(cut),
                        notes: before,
                    }),
                    ..clip.clone()
                },
                Clip {
                    start: at,
                    content: ClipContent::Midi(MidiClip {
                        length: Tick(midi.length.0 - cut),
                        notes: after,
                    }),
                    ..clip.clone()
                },
            ))
        }
    }
}

/// The audio clip with its contents slid `delta` ticks later (earlier if
/// negative) under the same window: the clip keeps its place and length and
/// plays other parts of the file. It stops at the file's start and end.
/// `None` for a MIDI clip, or while the file's length isn't known.
pub fn slip(
    map: &TempoMap,
    clip: &Clip,
    delta: i64,
    rate: u32,
    file_frames: Option<u64>,
    warp: Option<&ClipWarp>,
) -> Option<Clip> {
    let audio = clip.as_audio()?;
    // Slipping counts in the file's frames too.
    if let Layout::Warped(section) = layout(map, audio, warp, rate)? {
        return section.slip(clip, audio, delta, file_frames?);
    }
    let rate = f64::from(rate);
    let here = map.sample_at(clip.start, rate) as i64;
    // The distance in frames at this place in the tempo map, either way.
    let far = map.sample_at(Tick(clip.start.0 + delta.abs()), rate) as i64;
    let moved = (far - here) * delta.signum();
    // Contents move later, so the window looks at earlier parts of the file.
    let wanted = audio.offset as i64 - moved;
    // Without the file's length there is no telling where it ends.
    let latest = file_frames?.saturating_sub(audio.length) as i64;
    let mut audio = audio.clone();
    audio.offset = wanted.clamp(0, latest.max(0)) as u64;
    Some(Clip {
        content: ClipContent::Audio(audio),
        ..clip.clone()
    })
}

/// Shortens fades that no longer fit inside the clip.
pub(super) fn fit_fades(audio: &mut AudioClip) {
    audio.fade_in = audio.fade_in.min(audio.length);
    audio.fade_out = audio.fade_out.min(audio.length - audio.fade_in);
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{ClipContent, NodeId};

    const RATE: u32 = 48_000;

    /// 120 bpm: a quarter note is half a second, 24000 frames, 960 ticks.
    fn map() -> TempoMap {
        TempoMap::default()
    }

    fn clip(start: i64, offset: u64, length: u64) -> Clip {
        let mut clip = Clip::audio(NodeId(1), Tick(start), "a.wav", length);
        let ClipContent::Audio(audio) = &mut clip.content else {
            unreachable!("not an audio clip")
        };
        audio.offset = offset;
        clip
    }

    fn audio(clip: &Clip) -> &AudioClip {
        clip.as_audio().unwrap()
    }

    #[test]
    fn fades_follow_the_pointer_and_stop_at_each_other() {
        let mut c = clip(0, 0, 48_000);
        c = set_fade_in(&map(), &c, Tick(960), RATE, None).unwrap();
        assert_eq!(audio(&c).fade_in, 24_000);
        // The fade out can take the rest of the clip, no more.
        c = set_fade_out(&map(), &c, Tick(0), RATE, None).unwrap();
        assert_eq!(audio(&c).fade_out, 24_000);
        assert_eq!(audio(&c).fade_in, 24_000);
        // Before the start, or past the end, there is no fade.
        let c = set_fade_in(&map(), &c, Tick(-500), RATE, None).unwrap();
        assert_eq!(audio(&c).fade_in, 0);
        let c = set_fade_out(&map(), &c, Tick(9_999), RATE, None).unwrap();
        assert_eq!(audio(&c).fade_out, 0);
    }

    #[test]
    fn a_clip_ends_where_its_audio_does() {
        // 48000 frames is a second: two quarter notes.
        assert_eq!(end_tick(&map(), Tick(960), 48_000, RATE), Tick(960 + 1920));
    }

    #[test]
    fn trimming_the_start_cuts_the_audio_before_it() {
        let c = clip(0, 0, 48_000);
        let t = trim_start(&map(), &c, Tick(960), RATE, None).unwrap();
        assert_eq!(t.start, Tick(960));
        assert_eq!(audio(&t).offset, 24_000);
        assert_eq!(audio(&t).length, 24_000);
        // The end hasn't moved.
        assert_eq!(
            end_tick(&map(), t.start, audio(&t).length, RATE),
            end_tick(&map(), c.start, 48_000, RATE)
        );
    }

    #[test]
    fn trimming_the_start_back_stops_at_the_file_start() {
        let c = clip(1920, 12_000, 24_000);
        let t = trim_start(&map(), &c, Tick(0), RATE, None).unwrap();
        assert_eq!(audio(&t).offset, 0);
        assert_eq!(audio(&t).length, 36_000);
        // 12000 frames is half a quarter note: 480 ticks.
        assert_eq!(t.start, Tick(1920 - 480));
    }

    #[test]
    fn the_start_cant_cross_the_end() {
        let c = clip(0, 0, 24_000);
        let t = trim_start(&map(), &c, Tick(9_000), RATE, None).unwrap();
        assert_eq!(audio(&t).length, 1);
        assert_eq!(audio(&t).offset, 23_999);
    }

    #[test]
    fn trimming_the_end_sets_the_length() {
        let c = clip(960, 0, 48_000);
        let t = trim_end(&map(), &c, Tick(960 + 960), RATE, Some(48_000), None).unwrap();
        assert_eq!(audio(&t).length, 24_000);
        assert_eq!(t.start, Tick(960));
    }

    #[test]
    fn the_end_cant_go_past_the_file() {
        let c = clip(0, 10_000, 20_000);
        let t = trim_end(&map(), &c, Tick(9_600), RATE, Some(48_000), None).unwrap();
        assert_eq!(audio(&t).length, 38_000);
        let t = trim_end(&map(), &c, Tick(0), RATE, Some(48_000), None).unwrap();
        assert_eq!(audio(&t).length, 1);
    }

    #[test]
    fn fades_shrink_with_the_clip() {
        let mut c = clip(0, 0, 48_000);
        let ClipContent::Audio(a) = &mut c.content else {
            unreachable!("not an audio clip")
        };
        a.fade_in = 10_000;
        a.fade_out = 30_000;
        let t = trim_end(&map(), &c, Tick(960), RATE, None, None).unwrap();
        assert_eq!(audio(&t).length, 24_000);
        assert_eq!(audio(&t).fade_in, 10_000);
        assert_eq!(audio(&t).fade_out, 14_000);
        assert!(t.as_audio().unwrap().fade_in + audio(&t).fade_out <= audio(&t).length);
    }

    #[test]
    fn moving_changes_only_the_start() {
        let c = clip(0, 5, 100);
        let m = moved(&c, Tick(7));
        assert_eq!(m.start, Tick(7));
        assert_eq!(m.content, c.content);
    }

    fn midi_clip(start: i64, length: i64, notes: &[(i64, i64, u8)]) -> Clip {
        let mut clip = Clip::midi(NodeId(1), Tick(start), Tick(length));
        let ClipContent::Midi(midi) = &mut clip.content else {
            unreachable!()
        };
        midi.notes = notes
            .iter()
            .map(|&(s, l, k)| noodle_core::MidiNote::new(Tick(s), Tick(l), k))
            .collect();
        clip
    }

    #[test]
    fn a_midi_clip_ends_where_its_length_says_whatever_the_tempo() {
        let clip = midi_clip(960, 3840, &[]);
        assert_eq!(clip_end(&map(), &clip, RATE, None), Tick(4800));
    }

    #[test]
    fn trimming_the_end_of_a_midi_clip_sets_its_length() {
        let clip = midi_clip(960, 3840, &[(0, 480, 60)]);
        let out = trim_end(&map(), &clip, Tick(2880), RATE, None, None).unwrap();
        assert_eq!(out.as_midi().unwrap().length, Tick(1920));
        assert_eq!(out.as_midi().unwrap().notes.len(), 1);
        let tiny = trim_end(&map(), &clip, Tick(0), RATE, None, None).unwrap();
        assert_eq!(tiny.as_midi().unwrap().length, Tick(MIN_MIDI_LENGTH));
    }

    #[test]
    fn trimming_the_start_of_a_midi_clip_keeps_notes_where_they_sound() {
        // Notes at clip ticks 0..480, 480..960 and 1440..1680.
        let clip = midi_clip(960, 3840, &[(0, 480, 60), (480, 480, 62), (1440, 240, 64)]);
        // Cutting 600 ticks off: the first note is gone, the second is
        // shortened to its last 360, the third moves up.
        let out = trim_start(&map(), &clip, Tick(1560), RATE, None).unwrap();
        let midi = out.as_midi().unwrap();
        assert_eq!(out.start, Tick(1560));
        assert_eq!(midi.length, Tick(3240));
        let notes: Vec<_> = midi
            .notes
            .iter()
            .map(|n| (n.start.0, n.length.0, n.key))
            .collect();
        assert_eq!(notes, [(0, 360, 62), (840, 240, 64)]);
        // Extending to the left shifts the notes right, and stops at the
        // beginning of the timeline.
        let out = trim_start(&map(), &clip, Tick(-500), RATE, None).unwrap();
        assert_eq!(out.start, Tick(0));
        assert_eq!(out.as_midi().unwrap().length, Tick(4800));
        assert_eq!(out.as_midi().unwrap().notes[0].start, Tick(960));
        // It can't be cut down to nothing.
        let out = trim_start(&map(), &clip, Tick(99_999), RATE, None).unwrap();
        assert_eq!(out.as_midi().unwrap().length, Tick(MIN_MIDI_LENGTH));
    }

    #[test]
    fn a_midi_clip_shorter_than_the_minimum_can_still_be_trimmed() {
        let clip = midi_clip(0, 60, &[(0, 30, 60)]);
        let out = trim_start(&map(), &clip, Tick(40), RATE, None).unwrap();
        assert_eq!(out.start, Tick(0));
        assert_eq!(out.as_midi().unwrap().length, Tick(60));
    }

    #[test]
    fn trimming_the_end_cuts_off_notes_that_start_past_it() {
        let clip = midi_clip(0, 3840, &[(0, 240, 60), (2000, 240, 62), (2880, 240, 64)]);
        let out = trim_end(&map(), &clip, Tick(2400), RATE, None, None).unwrap();
        let keys: Vec<u8> = out.as_midi().unwrap().notes.iter().map(|n| n.key).collect();
        assert_eq!(keys, [60, 62]);
    }

    #[test]
    fn midi_clips_have_no_fades() {
        let clip = midi_clip(0, 3840, &[]);
        assert!(set_fade_in(&map(), &clip, Tick(480), RATE, None).is_none());
        assert!(set_fade_out(&map(), &clip, Tick(480), RATE, None).is_none());
    }

    fn warped(mut clip: Clip, warp: u64) -> Clip {
        let ClipContent::Audio(audio) = &mut clip.content else {
            unreachable!("not an audio clip")
        };
        audio.warp = Some(noodle_core::WarpId(warp));
        clip
    }

    #[test]
    fn a_warped_section_lasts_as_long_as_its_warped_audio() {
        let c = warped(clip(960, 0, 96_000), 1);
        // Two seconds of audio recorded at 60 bpm is two beats of it, which
        // the timeline plays as two beats at any tempo.
        let slow = ClipWarp::at_tempo(60.0);
        assert_eq!(clip_end(&map(), &c, RATE, Some(&slow)), Tick(960 + 1920));
        // Recorded at 120 it is four beats.
        let same = ClipWarp::at_tempo(120.0);
        assert_eq!(clip_end(&map(), &c, RATE, Some(&same)), Tick(960 + 3840));
        // Without a warp, or with one that leaves it alone, it lasts two seconds.
        let mut natural = ClipWarp::at_tempo(60.0);
        natural.tempo_conversion = false;
        assert_eq!(clip_end(&map(), &c, RATE, None), Tick(960 + 3840));
        assert_eq!(clip_end(&map(), &c, RATE, Some(&natural)), Tick(960 + 3840));
        // Markers lengthen or shorten it: the clip's second beat is moved to
        // the fourth, so the whole is two beats longer.
        let mut marked = ClipWarp::at_tempo(120.0);
        marked.set_marker(noodle_core::WarpMarker {
            seconds: 0.0,
            beat: 0.0,
        });
        marked.set_marker(noodle_core::WarpMarker {
            seconds: 1.0,
            beat: 4.0,
        });
        assert_eq!(
            clip_end(&map(), &c, RATE, Some(&marked)),
            Tick(960 + 3840 + 2 * 960)
        );
        // A section showing part of the file counts only that part.
        let part = warped(clip(0, 48_000, 48_000), 1);
        assert_eq!(clip_end(&map(), &part, RATE, Some(&same)), Tick(1920));
    }

    /// A warp at 60 bpm on the 120 bpm timeline: a second of the file is a
    /// beat (960 ticks), so a tick is 50 frames.
    fn double_speed() -> ClipWarp {
        ClipWarp::at_tempo(60.0)
    }

    fn looping(warp: ClipWarp, from: f64, to: f64) -> ClipWarp {
        ClipWarp {
            loop_region: Some(noodle_core::LoopRegion {
                start: from,
                end: to,
            }),
            ..warp
        }
    }

    #[test]
    fn a_warped_section_is_trimmed_through_its_warp() {
        let warp = double_speed();
        let c = warped(clip(0, 0, 96_000), 1);
        assert_eq!(clip_end(&map(), &c, RATE, Some(&warp)), Tick(1920));
        // The end: 960 ticks in is a second of the file.
        let t = trim_end(&map(), &c, Tick(960), RATE, Some(96_000), Some(&warp)).unwrap();
        assert_eq!(audio(&t).length, 48_000);
        // Past the file is as far as it goes.
        let t = trim_end(&map(), &c, Tick(9_999), RATE, Some(96_000), Some(&warp)).unwrap();
        assert_eq!(audio(&t).length, 96_000);
        // The start: the audio before the edge is cut, and what's left plays
        // where it did.
        let t = trim_start(&map(), &c, Tick(960), RATE, Some(&warp)).unwrap();
        assert_eq!(
            (t.start, audio(&t).offset, audio(&t).length),
            (Tick(960), 48_000, 48_000)
        );
        assert_eq!(clip_end(&map(), &t, RATE, Some(&warp)), Tick(1920));
        // Brought back, it stops at the start of the file.
        let t = trim_start(&map(), &t, Tick(0), RATE, Some(&warp)).unwrap();
        assert_eq!((t.start, audio(&t).offset), (Tick(0), 0));
        // It can't be cut to nothing.
        let t = trim_start(&map(), &c, Tick(99_999), RATE, Some(&warp)).unwrap();
        assert_eq!(audio(&t).length, 1);
    }

    #[test]
    fn a_trim_follows_the_markers() {
        // The first second of the file is stretched over four beats.
        let mut warp = ClipWarp::at_tempo(120.0);
        warp.set_marker(noodle_core::WarpMarker {
            seconds: 0.0,
            beat: 0.0,
        });
        warp.set_marker(noodle_core::WarpMarker {
            seconds: 1.0,
            beat: 4.0,
        });
        let c = warped(clip(0, 0, 96_000), 1);
        // Two beats in, half a second of the file has played.
        let t = trim_end(&map(), &c, Tick(1920), RATE, Some(96_000), Some(&warp)).unwrap();
        assert_eq!(audio(&t).length, 24_000);
    }

    #[test]
    fn a_warped_section_fades_over_the_time_its_fade_takes() {
        let warp = double_speed();
        let c = warped(clip(0, 0, 96_000), 1);
        // 480 ticks is half a second of the file.
        let c = set_fade_in(&map(), &c, Tick(480), RATE, Some(&warp)).unwrap();
        assert_eq!(audio(&c).fade_in, 24_000);
        let c = set_fade_out(&map(), &c, Tick(1920 - 480), RATE, Some(&warp)).unwrap();
        assert_eq!(audio(&c).fade_out, 24_000);
        // The handles sit where the fades end.
        assert_eq!(
            fade_ticks(&map(), &c, RATE, Some(&warp)),
            Some((Tick(480), Tick(1440)))
        );
        // Past the ends there are none.
        let c = set_fade_out(&map(), &c, Tick(9_999), RATE, Some(&warp)).unwrap();
        assert_eq!(audio(&c).fade_out, 0);
    }

    #[test]
    fn a_warped_section_splits_into_two_that_play_what_was_there() {
        let warp = double_speed();
        let c = warped(clip(960, 0, 96_000), 1);
        let (a, b) = split(&map(), &c, Tick(960 + 960), RATE, Some(&warp)).unwrap();
        assert_eq!((a.start, b.start), (Tick(960), Tick(1920)));
        assert_eq!((audio(&a).offset, audio(&a).length), (0, 48_000));
        assert_eq!((audio(&b).offset, audio(&b).length), (48_000, 48_000));
        // They share the warp, so the cut is not heard.
        assert_eq!(audio(&a).warp, audio(&b).warp);
        assert_eq!(clip_end(&map(), &a, RATE, Some(&warp)), b.start);
        assert_eq!(clip_end(&map(), &b, RATE, Some(&warp)), Tick(960 + 1920));
        assert!(split(&map(), &c, Tick(960), RATE, Some(&warp)).is_none());
        assert!(split(&map(), &c, Tick(960 + 1920), RATE, Some(&warp)).is_none());
    }

    #[test]
    fn a_warped_section_slips_under_its_window() {
        let warp = double_speed();
        let c = warped(clip(960, 48_000, 48_000), 1);
        // Half a beat later: the window looks 480 ticks (24000 frames) back.
        let s = slip(&map(), &c, 480, RATE, Some(144_000), Some(&warp)).unwrap();
        assert_eq!((audio(&s).offset, audio(&s).length), (24_000, 48_000));
        assert_eq!(s.start, c.start);
        // Back to the file's start, and no further.
        let s = slip(&map(), &c, 9_999, RATE, Some(144_000), Some(&warp)).unwrap();
        assert_eq!(audio(&s).offset, 0);
        // Without the file's length there is no telling where it ends.
        assert!(slip(&map(), &c, 480, RATE, None, Some(&warp)).is_none());
    }

    #[test]
    fn a_section_naming_a_warp_that_is_not_there_is_left_alone() {
        let c = warped(clip(0, 0, 48_000), 1);
        assert!(trim_start(&map(), &c, Tick(480), RATE, None).is_none());
        assert!(trim_end(&map(), &c, Tick(480), RATE, None, None).is_none());
        assert!(set_fade_in(&map(), &c, Tick(480), RATE, None).is_none());
        assert!(set_fade_out(&map(), &c, Tick(480), RATE, None).is_none());
        assert!(split(&map(), &c, Tick(480), RATE, None).is_none());
        assert!(slip(&map(), &c, 480, RATE, Some(100_000), None).is_none());
        // Moving is fine.
        assert_eq!(moved(&c, Tick(960)).start, Tick(960));
    }

    #[test]
    fn a_warp_that_changes_nothing_edits_like_no_warp() {
        let mut natural = ClipWarp::at_tempo(60.0);
        natural.tempo_conversion = false;
        let c = warped(clip(0, 0, 48_000), 1);
        let t = trim_end(&map(), &c, Tick(960), RATE, Some(48_000), Some(&natural)).unwrap();
        assert_eq!(audio(&t).length, 24_000);
        assert!(split(&map(), &c, Tick(480), RATE, Some(&natural)).is_some());
    }

    #[test]
    fn dragging_a_looping_section_past_its_audio_repeats_the_loop() {
        // Four seconds (eight beats) of a clip at 120 bpm, looping its first
        // two beats.
        let warp = looping(ClipWarp::at_tempo(120.0), 0.0, 2.0);
        let c = warped(clip(0, 0, 192_000), 1);
        assert_eq!(clip_end(&map(), &c, RATE, Some(&warp)), Tick(7680));
        let out = trim_end(&map(), &c, Tick(11_520), RATE, Some(192_000), Some(&warp)).unwrap();
        assert_eq!(audio(&out).extent, Some(Tick(11_520)));
        assert_eq!(audio(&out).length, 192_000);
        assert_eq!(clip_end(&map(), &out, RATE, Some(&warp)), Tick(11_520));
        // Dragging it back inside the audio cuts the audio, not the loop.
        let back = trim_end(&map(), &out, Tick(1920), RATE, Some(192_000), Some(&warp)).unwrap();
        assert_eq!(audio(&back).extent, None);
        assert_eq!(audio(&back).length, 48_000);
        // The tail sounds like the loop: a fade out near the end is as long
        // as the stretch of the loop it spans.
        let faded = set_fade_out(&map(), &out, Tick(11_520 - 480), RATE, Some(&warp)).unwrap();
        assert_eq!(audio(&faded).fade_out, 12_000);
        // A section that doesn't loop isn't extended.
        let plain = ClipWarp::at_tempo(120.0);
        let out = trim_end(&map(), &c, Tick(11_520), RATE, Some(192_000), Some(&plain)).unwrap();
        assert_eq!(audio(&out).extent, None);
        assert_eq!(audio(&out).length, 192_000);
    }

    #[test]
    fn dragging_the_edge_past_the_window_does_not_change_the_audio_before_it() {
        // The window is eight beats; the loop is its first two. It repeats
        // up to the window's end whether or not the edge was dragged.
        let warp = looping(ClipWarp::at_tempo(120.0), 0.0, 2.0);
        let c = warped(clip(0, 0, 192_000), 1);
        let before = passes(&map(), &c, RATE, Some(&warp)).unwrap();
        let out = trim_end(&map(), &c, Tick(7_681), RATE, Some(192_000), Some(&warp)).unwrap();
        let after = passes(&map(), &out, RATE, Some(&warp)).unwrap();
        assert_eq!(&after[..before.len()], &before[..]);
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(after.last().unwrap().to, Tick(7_681));
    }

    #[test]
    fn a_fade_out_is_held_to_the_last_pass_of_a_loop() {
        let warp = looping(ClipWarp::at_tempo(120.0), 0.0, 2.0);
        let mut c = warped(clip(0, 0, 192_000), 1);
        let ClipContent::Audio(a) = &mut c.content else {
            unreachable!()
        };
        // 6 whole rounds of the 2-beat loop (1920 ticks) after the first.
        a.extent = Some(Tick(10_000));
        a.fade_out = 96_000;
        // The last pass starts at 9600 and is cut off at 10000: the fade
        // starts there, not a pass earlier, and a drag cannot put it before.
        assert_eq!(
            fade_ticks(&map(), &c, RATE, Some(&warp)).unwrap().1,
            Tick(9_600)
        );
        let held = set_fade_out(&map(), &c, Tick(2_000), RATE, Some(&warp)).unwrap();
        let frames = audio(&held).fade_out;
        assert!((9_500..=10_500).contains(&frames), "{frames}");
        let inside = set_fade_out(&map(), &c, Tick(9_800), RATE, Some(&warp)).unwrap();
        assert!((4_500..=5_500).contains(&audio(&inside).fade_out));
    }

    #[test]
    fn a_loop_too_short_to_play_does_not_repeat() {
        // 480 bpm not following the timeline: a beat lasts 0.125 s.
        let mut warp = looping(ClipWarp::at_tempo(480.0), 0.0, 1.0);
        warp.tempo_conversion = false;
        let mut c = warped(clip(0, 0, 192_000), 1);
        let ClipContent::Audio(a) = &mut c.content else {
            unreachable!()
        };
        a.extent = Some(Tick(11_520));
        assert!(passes(&map(), &c, RATE, Some(&warp)).is_none());
        let out = trim_end(&map(), &c, Tick(15_000), RATE, Some(192_000), Some(&warp)).unwrap();
        assert_eq!(audio(&out).extent, None);
    }

    #[test]
    fn the_repeats_of_a_loop_cant_be_split_but_the_audio_before_can() {
        let warp = looping(ClipWarp::at_tempo(120.0), 0.0, 2.0);
        let mut c = warped(clip(0, 0, 192_000), 1);
        let ClipContent::Audio(a) = &mut c.content else {
            unreachable!()
        };
        a.extent = Some(Tick(11_520));
        // The loop (two beats) wraps at tick 960: after that it only repeats.
        assert!(split(&map(), &c, Tick(9_600), RATE, Some(&warp)).is_none());
        assert!(split(&map(), &c, Tick(1920), RATE, Some(&warp)).is_none());
        let (first, second) = split(&map(), &c, Tick(480), RATE, Some(&warp)).unwrap();
        assert_eq!(audio(&first).extent, None);
        assert_eq!(audio(&second).extent, Some(Tick(11_520 - 480)));
        assert_eq!(clip_end(&map(), &second, RATE, Some(&warp)), Tick(11_520));
    }

    #[test]
    fn trimming_the_start_of_a_looping_section_keeps_its_end() {
        let warp = looping(ClipWarp::at_tempo(120.0), 0.0, 2.0);
        let mut c = warped(clip(0, 0, 192_000), 1);
        let ClipContent::Audio(a) = &mut c.content else {
            unreachable!()
        };
        a.extent = Some(Tick(11_520));
        let t = trim_start(&map(), &c, Tick(480), RATE, Some(&warp)).unwrap();
        assert_eq!((t.start, audio(&t).offset), (Tick(480), 12_000));
        assert_eq!(clip_end(&map(), &t, RATE, Some(&warp)), Tick(11_520));
        // Not into the loop's end: there would be nothing to repeat.
        let t = trim_start(&map(), &c, Tick(5_000), RATE, Some(&warp)).unwrap();
        assert!(audio(&t).offset < 48_000);
    }

    #[test]
    fn splitting_cuts_the_audio_in_two_without_a_gap() {
        let mut original = clip(960, 1_000, 48_000);
        original.content = ClipContent::Audio(AudioClip {
            fade_in: 100,
            fade_out: 200,
            ..audio(&original).clone()
        });
        // One beat (24000 frames at 120 bpm) after the start.
        let (a, b) = split(&map(), &original, Tick(960 + 960), RATE, None).unwrap();
        assert_eq!((a.start, b.start), (Tick(960), Tick(1_920)));
        assert_eq!((audio(&a).offset, audio(&a).length), (1_000, 24_000));
        assert_eq!((audio(&b).offset, audio(&b).length), (25_000, 24_000));
        assert_eq!((audio(&a).fade_in, audio(&a).fade_out), (100, 0));
        assert_eq!((audio(&b).fade_in, audio(&b).fade_out), (0, 200));
    }

    #[test]
    fn splitting_outside_the_clip_does_nothing() {
        let original = clip(960, 0, 24_000);
        assert!(split(&map(), &original, Tick(960), RATE, None).is_none());
        assert!(split(&map(), &original, Tick(100), RATE, None).is_none());
        // The clip is exactly one beat long.
        assert!(split(&map(), &original, Tick(1_920), RATE, None).is_none());
        assert!(split(&map(), &original, Tick(5_000), RATE, None).is_none());
    }

    #[test]
    fn splitting_a_midi_clip_splits_a_note_that_spans_the_cut() {
        let original = midi_clip(0, 1_920, &[(0, 480, 60), (720, 480, 62), (1_500, 100, 64)]);
        let (a, b) = split(&map(), &original, Tick(960), RATE, None).unwrap();
        let (a, b) = (a.as_midi().unwrap(), b.as_midi().unwrap());
        assert_eq!((a.length, b.length), (Tick(960), Tick(960)));
        let span = |m: &MidiClip| {
            m.notes
                .iter()
                .map(|n| (n.start.0, n.length.0, n.key))
                .collect::<Vec<_>>()
        };
        assert_eq!(span(a), vec![(0, 480, 60), (720, 240, 62)]);
        assert_eq!(span(b), vec![(0, 240, 62), (540, 100, 64)]);
    }

    #[test]
    fn slipping_moves_the_window_over_the_file_and_stops_at_its_ends() {
        let original = clip(960, 10_000, 24_000);
        // A beat later: the window looks 24000 frames earlier, but only
        // 10000 are there.
        let slipped = slip(&map(), &original, 960, RATE, Some(100_000), None).unwrap();
        assert_eq!(audio(&slipped).offset, 0);
        assert_eq!(slipped.start, original.start);
        let slipped = slip(&map(), &original, -480, RATE, Some(100_000), None).unwrap();
        assert_eq!(audio(&slipped).offset, 22_000);
        // At most to where the clip still fits in the file.
        let slipped = slip(&map(), &original, -4_800, RATE, Some(50_000), None).unwrap();
        assert_eq!(audio(&slipped).offset, 26_000);
        assert!(slip(&map(), &midi_clip(0, 960, &[]), 10, RATE, None, None).is_none());
    }
}
