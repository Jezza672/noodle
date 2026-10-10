//! Moving and trimming clips: the arithmetic between ticks on the screen and
//! frames in the file.
//!
//! A clip starts at a tick but is as long as its audio, so its end depends on
//! the tempo map. Trims turn the tick the user dragged to back into frames.

use noodle_core::{AudioClip, Clip, ClipContent, MidiClip, TempoMap, Tick};

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
/// is not known any better. MIDI clips are as long as they say.
pub fn clip_end(map: &TempoMap, clip: &Clip, rate: u32) -> Tick {
    match &clip.content {
        ClipContent::Audio(audio) => end_tick(map, clip.start, audio.length, rate),
        ClipContent::Midi(midi) => Tick(clip.start.0 + midi.length.0),
    }
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
pub fn trim_start(map: &TempoMap, clip: &Clip, edge: Tick, rate: u32) -> Option<Clip> {
    if let Some(midi) = clip.as_midi() {
        return Some(trim_midi_start(clip, midi, edge));
    }
    let audio = clip.as_audio()?;
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
pub fn set_fade_in(map: &TempoMap, clip: &Clip, at: Tick, rate: u32) -> Option<Clip> {
    let audio = clip.as_audio()?;
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
pub fn set_fade_out(map: &TempoMap, clip: &Clip, at: Tick, rate: u32) -> Option<Clip> {
    let audio = clip.as_audio()?;
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

/// Shortens fades that no longer fit inside the clip.
fn fit_fades(audio: &mut AudioClip) {
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
        c = set_fade_in(&map(), &c, Tick(960), RATE).unwrap();
        assert_eq!(audio(&c).fade_in, 24_000);
        // The fade out can take the rest of the clip, no more.
        c = set_fade_out(&map(), &c, Tick(0), RATE).unwrap();
        assert_eq!(audio(&c).fade_out, 24_000);
        assert_eq!(audio(&c).fade_in, 24_000);
        // Before the start, or past the end, there is no fade.
        let c = set_fade_in(&map(), &c, Tick(-500), RATE).unwrap();
        assert_eq!(audio(&c).fade_in, 0);
        let c = set_fade_out(&map(), &c, Tick(9_999), RATE).unwrap();
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
        let t = trim_start(&map(), &c, Tick(960), RATE).unwrap();
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
        let t = trim_start(&map(), &c, Tick(0), RATE).unwrap();
        assert_eq!(audio(&t).offset, 0);
        assert_eq!(audio(&t).length, 36_000);
        // 12000 frames is half a quarter note: 480 ticks.
        assert_eq!(t.start, Tick(1920 - 480));
    }

    #[test]
    fn the_start_cant_cross_the_end() {
        let c = clip(0, 0, 24_000);
        let t = trim_start(&map(), &c, Tick(9_000), RATE).unwrap();
        assert_eq!(audio(&t).length, 1);
        assert_eq!(audio(&t).offset, 23_999);
    }

    #[test]
    fn trimming_the_end_sets_the_length() {
        let c = clip(960, 0, 48_000);
        let t = trim_end(&map(), &c, Tick(960 + 960), RATE, Some(48_000)).unwrap();
        assert_eq!(audio(&t).length, 24_000);
        assert_eq!(t.start, Tick(960));
    }

    #[test]
    fn the_end_cant_go_past_the_file() {
        let c = clip(0, 10_000, 20_000);
        let t = trim_end(&map(), &c, Tick(9_600), RATE, Some(48_000)).unwrap();
        assert_eq!(audio(&t).length, 38_000);
        let t = trim_end(&map(), &c, Tick(0), RATE, Some(48_000)).unwrap();
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
        let t = trim_end(&map(), &c, Tick(960), RATE, None).unwrap();
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
        assert_eq!(clip_end(&map(), &clip, RATE), Tick(4800));
    }

    #[test]
    fn trimming_the_end_of_a_midi_clip_sets_its_length() {
        let clip = midi_clip(960, 3840, &[(0, 480, 60)]);
        let out = trim_end(&map(), &clip, Tick(2880), RATE, None).unwrap();
        assert_eq!(out.as_midi().unwrap().length, Tick(1920));
        assert_eq!(out.as_midi().unwrap().notes.len(), 1);
        let tiny = trim_end(&map(), &clip, Tick(0), RATE, None).unwrap();
        assert_eq!(tiny.as_midi().unwrap().length, Tick(MIN_MIDI_LENGTH));
    }

    #[test]
    fn trimming_the_start_of_a_midi_clip_keeps_notes_where_they_sound() {
        // Notes at clip ticks 0..480, 480..960 and 1440..1680.
        let clip = midi_clip(960, 3840, &[(0, 480, 60), (480, 480, 62), (1440, 240, 64)]);
        // Cutting 600 ticks off: the first note is gone, the second is
        // shortened to its last 360, the third moves up.
        let out = trim_start(&map(), &clip, Tick(1560), RATE).unwrap();
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
        let out = trim_start(&map(), &clip, Tick(-500), RATE).unwrap();
        assert_eq!(out.start, Tick(0));
        assert_eq!(out.as_midi().unwrap().length, Tick(4800));
        assert_eq!(out.as_midi().unwrap().notes[0].start, Tick(960));
        // It can't be cut down to nothing.
        let out = trim_start(&map(), &clip, Tick(99_999), RATE).unwrap();
        assert_eq!(out.as_midi().unwrap().length, Tick(MIN_MIDI_LENGTH));
    }

    #[test]
    fn a_midi_clip_shorter_than_the_minimum_can_still_be_trimmed() {
        let clip = midi_clip(0, 60, &[(0, 30, 60)]);
        let out = trim_start(&map(), &clip, Tick(40), RATE).unwrap();
        assert_eq!(out.start, Tick(0));
        assert_eq!(out.as_midi().unwrap().length, Tick(60));
    }

    #[test]
    fn trimming_the_end_cuts_off_notes_that_start_past_it() {
        let clip = midi_clip(0, 3840, &[(0, 240, 60), (2000, 240, 62), (2880, 240, 64)]);
        let out = trim_end(&map(), &clip, Tick(2400), RATE, None).unwrap();
        let keys: Vec<u8> = out.as_midi().unwrap().notes.iter().map(|n| n.key).collect();
        assert_eq!(keys, [60, 62]);
    }

    #[test]
    fn midi_clips_have_no_fades() {
        let clip = midi_clip(0, 3840, &[]);
        assert!(set_fade_in(&map(), &clip, Tick(480), RATE).is_none());
        assert!(set_fade_out(&map(), &clip, Tick(480), RATE).is_none());
    }
}
