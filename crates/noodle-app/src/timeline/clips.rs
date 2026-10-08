//! Moving and trimming clips: the arithmetic between ticks on the screen and
//! frames in the file.
//!
//! A clip starts at a tick but is as long as its audio, so its end depends on
//! the tempo map. Trims turn the tick the user dragged to back into frames.

use noodle_core::{AudioClip, Clip, TempoMap, Tick};

/// What a clip needs to know about its file to be laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source {
    pub sample_rate: u32,
    /// How many frames the file has.
    pub frames: u64,
}

/// The tick a clip of `length` frames starting at `start` ends on.
pub fn end_tick(map: &TempoMap, start: Tick, length: u64, rate: u32) -> Tick {
    let rate = f64::from(rate);
    let end = map.sample_at(start, rate) + length;
    Tick(map.tick_at_sample(end, rate).round() as i64)
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
        let ClipContent::Audio(audio) = &mut clip.content;
        audio.offset = offset;
        clip
    }

    fn audio(clip: &Clip) -> &AudioClip {
        clip.as_audio().unwrap()
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
        let ClipContent::Audio(a) = &mut c.content;
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
}
