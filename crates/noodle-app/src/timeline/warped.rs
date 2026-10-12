//! Laying out, trimming, fading, splitting and slipping a warped section.
//!
//! An unwarped section is a window of frames at the file's own rate, so
//! dragging an edge to a tick is a sum of frames. A warp stretches the audio,
//! so the same drag has to go through the warp's curve: where on the curve a
//! tick falls, and which moment of the file plays there.
//!
//! The curve measures where the clip plays in beats of the timeline (when it
//! follows the tempo) or in seconds from the section's start (when it keeps
//! its natural speed); [`Section::span`] and [`Section::tick_after`] move
//! between that axis and ticks.

use noodle_core::{
    AudioClip, Clip, ClipContent, ClipWarp, TICKS_PER_QUARTER, TempoMap, Tick, WarpAxis, WarpCurve,
};

/// One run through the audio of a section that repeats a loop: its stretch
/// of the timeline and the frames of the file it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pass {
    pub from: Tick,
    pub to: Tick,
    /// The file's frames the run starts and ends on. A run cut off by the
    /// section's end shows the part of the loop that fits.
    pub frames: (u64, u64),
}

/// A warped section's warp, with the tempo map and the file's rate it is
/// laid out against.
pub(super) struct Section<'a> {
    map: &'a TempoMap,
    warp: &'a ClipWarp,
    curve: WarpCurve,
    rate: f64,
}

impl<'a> Section<'a> {
    pub fn new(map: &'a TempoMap, warp: &'a ClipWarp, rate: u32) -> Self {
        Self {
            map,
            warp,
            curve: warp.curve(),
            rate: f64::from(rate),
        }
    }

    /// How far the curve's axis goes between two ticks.
    fn span(&self, from: Tick, to: Tick) -> f64 {
        match self.curve.axis {
            WarpAxis::Beats => (to.0 - from.0) as f64 / TICKS_PER_QUARTER as f64,
            WarpAxis::Seconds => {
                let (a, b) = (
                    self.map.sample_at(from.max(Tick::ZERO), self.rate),
                    self.map.sample_at(to.max(Tick::ZERO), self.rate),
                );
                (b as f64 - a as f64) / self.rate
            }
        }
    }

    /// The tick `span` of the axis after `from`.
    fn tick_after(&self, from: Tick, span: f64) -> Tick {
        match self.curve.axis {
            WarpAxis::Beats => Tick(from.0 + (span * TICKS_PER_QUARTER as f64).round() as i64),
            WarpAxis::Seconds => {
                let at = self.map.sample_at(from.max(Tick::ZERO), self.rate) as f64;
                let sample = (at + span * self.rate).max(0.0).round() as u64;
                Tick(self.map.tick_at_sample(sample, self.rate).round() as i64)
            }
        }
    }

    /// The moment of the file (in seconds) that plays at `tick`, for a
    /// section starting at `start` that shows the file from `origin`. Straight
    /// on from the section's ends.
    pub fn source_at(&self, start: Tick, origin: f64, tick: Tick) -> f64 {
        self.curve
            .source_at(self.curve.at(origin) + self.span(start, tick))
    }

    /// The tick at which the moment `source` of the file plays, for a section
    /// starting at `start` that shows the file from `origin`.
    pub fn tick_of(&self, start: Tick, origin: f64, source: f64) -> Tick {
        self.tick_after(start, self.curve.at(source) - self.curve.at(origin))
    }

    fn origin(&self, audio: &AudioClip) -> f64 {
        audio.offset as f64 / self.rate
    }

    /// The tick the fade in ends at.
    pub fn fade_in_tick(&self, start: Tick, audio: &AudioClip) -> Tick {
        let origin = self.origin(audio);
        self.tick_of(start, origin, origin + audio.fade_in as f64 / self.rate)
    }

    /// The tick the fade out starts at: the section's end less as much time
    /// as the fade's frames take at the end of the window.
    pub fn fade_out_tick(&self, start: Tick, audio: &AudioClip) -> Tick {
        let origin = self.origin(audio);
        let window_end = (audio.offset + audio.length) as f64 / self.rate;
        let faded = self.tick_of(
            start,
            origin,
            window_end - audio.fade_out as f64 / self.rate,
        );
        let width = self.natural_end(start, audio).0 - faded.0;
        Tick(self.end(start, audio).0 - width)
    }

    /// Where the section would end if it didn't repeat.
    pub fn natural_end(&self, start: Tick, audio: &AudioClip) -> Tick {
        let end = (audio.offset + audio.length) as f64 / self.rate;
        self.tick_of(start, self.origin(audio), end)
    }

    /// The loop, in seconds of the file, if this section can repeat: its
    /// clip loops, and the section reaches into the loop's start.
    pub fn loop_of(&self, audio: &AudioClip) -> Option<(f64, f64)> {
        self.warp
            .loop_seconds()
            .filter(|&(_, end)| self.origin(audio) < end - 1e-6)
    }

    /// Where the section ends: dragged out past its window, a looping one
    /// lasts as long as it was asked to.
    pub fn end(&self, start: Tick, audio: &AudioClip) -> Tick {
        self.repeating_end(start, audio)
            .unwrap_or_else(|| self.natural_end(start, audio))
    }

    /// The end of a section that repeats its loop: dragged out past its
    /// window. Such a section plays its window only up to the loop's end.
    fn repeating_end(&self, start: Tick, audio: &AudioClip) -> Option<Tick> {
        let extent = audio.extent?;
        self.loop_of(audio)?;
        let end = Tick(start.0 + extent.0);
        (end > self.natural_end(start, audio)).then_some(end)
    }

    /// Where a section that repeats its loop starts to: the tick at which the
    /// loop first wraps.
    fn first_wrap(&self, start: Tick, audio: &AudioClip) -> Option<Tick> {
        self.repeating_end(start, audio)?;
        let (_, to) = self.loop_of(audio)?;
        Some(self.tick_of(start, self.origin(audio), to))
    }

    /// The runs a section that repeats its loop is made of: the audio up to
    /// the loop's end, then the loop over and over until the section ends.
    /// `None` for any other section.
    pub fn passes(&self, start: Tick, audio: &AudioClip) -> Option<Vec<Pass>> {
        let end = self.repeating_end(start, audio)?;
        let (from, to) = self.loop_of(audio)?;
        let mut origin = self.origin(audio);
        let mut at = start;
        let mut passes = Vec::new();
        while at < end && passes.len() < 10_000 {
            let wrap = self.tick_of(at, origin, to);
            if wrap <= at {
                break;
            }
            let stop = wrap.min(end);
            let shown_to = if stop == wrap {
                to
            } else {
                // Cut off part way: as much of the loop as fits.
                self.source_at(at, origin, stop)
            };
            passes.push(Pass {
                from: at,
                to: stop,
                frames: (
                    self.frames(origin).max(0) as u64,
                    self.frames(shown_to).max(0) as u64,
                ),
            });
            at = stop;
            origin = from;
        }
        Some(passes)
    }

    /// The moment of the file that plays at `tick`, going round the loop
    /// where the section repeats it.
    pub fn source_wrapped(&self, start: Tick, audio: &AudioClip, tick: Tick) -> f64 {
        let origin = self.origin(audio);
        let out = self.span(start, tick);
        let base = self.curve.at(origin);
        if let Some((from, to)) = self.loop_of(audio) {
            let first = self.curve.at(to) - base;
            let round = self.curve.at(to) - self.curve.at(from);
            if out > first && round > 0.0 {
                return self
                    .curve
                    .source_at(self.curve.at(from) + (out - first).rem_euclid(round));
            }
        }
        self.curve.source_at(base + out)
    }

    fn frames(&self, seconds: f64) -> i64 {
        (seconds * self.rate).round() as i64
    }

    /// The section with its left edge dragged to `edge`. The audio keeps
    /// playing where it did: the start moves to where the new first moment
    /// used to play.
    pub fn trim_start(&self, clip: &Clip, audio: &AudioClip, edge: Tick) -> Option<Clip> {
        let origin = self.origin(audio);
        let end = (audio.offset + audio.length) as i64;
        // A looping section can't start past the loop's end: nothing would
        // be left to repeat.
        let limit = match self.loop_of(audio) {
            Some((_, to)) if audio.extent.is_some() => end.min(self.frames(to) - 1),
            _ => end - 1,
        };
        let mut offset = self
            .frames(self.source_at(clip.start, origin, edge.max(Tick::ZERO)))
            .clamp(0, limit.max(0));
        let mut start = self.tick_of(clip.start, origin, offset as f64 / self.rate);
        if start < Tick::ZERO {
            // Can't reach before the beginning of the timeline: start there
            // with the first moment that plays after it.
            let first = self.source_at(clip.start, origin, Tick::ZERO) * self.rate;
            offset = (first.ceil() as i64).clamp(0, limit.max(0));
            start = self
                .tick_of(clip.start, origin, offset as f64 / self.rate)
                .max(Tick::ZERO);
        }
        if offset == audio.offset as i64 {
            return Some(clip.clone());
        }
        let mut audio = audio.clone();
        audio.length = (end - offset) as u64;
        audio.offset = offset as u64;
        if let Some(extent) = audio.extent {
            let extent = clip.start.0 + extent.0 - start.0;
            audio.extent = (extent > 0).then_some(Tick(extent));
        }
        super::clips::fit_fades(&mut audio);
        Some(Clip {
            start,
            content: ClipContent::Audio(audio),
            ..clip.clone()
        })
    }

    /// The section with its right edge dragged to `edge`: a looping section
    /// dragged past its audio repeats the loop; any other is cut short or
    /// shows more of the file, to the file's end if that is known.
    pub fn trim_end(
        &self,
        clip: &Clip,
        audio: &AudioClip,
        edge: Tick,
        file_frames: Option<u64>,
    ) -> Option<Clip> {
        let mut audio = audio.clone();
        let natural = self.natural_end(clip.start, &audio);
        if self.loop_of(&audio).is_some() && edge > natural {
            audio.extent = Some(Tick(edge.0 - clip.start.0));
            return Some(Clip {
                content: ClipContent::Audio(audio),
                ..clip.clone()
            });
        }
        let origin = self.origin(&audio);
        let wanted = self.frames(self.source_at(clip.start, origin, edge)) - audio.offset as i64;
        let mut length = wanted.max(1) as u64;
        if let Some(frames) = file_frames {
            length = length.min(frames.saturating_sub(audio.offset).max(1));
        }
        audio.length = length;
        audio.extent = None;
        super::clips::fit_fades(&mut audio);
        Some(Clip {
            content: ClipContent::Audio(audio),
            ..clip.clone()
        })
    }

    /// The section with its fade in ending at `at`, as long as it can be
    /// without running into the fade out.
    pub fn set_fade_in(&self, clip: &Clip, audio: &AudioClip, at: Tick) -> Clip {
        let origin = self.origin(audio);
        let wanted = if at <= clip.start {
            0
        } else {
            self.frames(self.source_at(clip.start, origin, at) - origin)
                .max(0) as u64
        };
        let mut audio = audio.clone();
        audio.fade_in = wanted.min(audio.length - audio.fade_out.min(audio.length));
        Clip {
            content: ClipContent::Audio(audio),
            ..clip.clone()
        }
    }

    /// The section with its fade out starting at `at`.
    pub fn set_fade_out(&self, clip: &Clip, audio: &AudioClip, at: Tick) -> Clip {
        let end = self.end(clip.start, audio);
        let wanted = if at >= end {
            0
        } else {
            let at_s = self.source_wrapped(clip.start, audio, at);
            let mut end_s = self.source_wrapped(clip.start, audio, end);
            if end_s <= at_s {
                // The end is a whole number of rounds: the fade runs to the
                // loop's end.
                end_s = self.loop_of(audio).map_or(end_s, |(_, to)| to);
            }
            self.frames(end_s - at_s).max(0) as u64
        };
        let mut audio = audio.clone();
        audio.fade_out = wanted.min(audio.length - audio.fade_in.min(audio.length));
        Clip {
            content: ClipContent::Audio(audio),
            ..clip.clone()
        }
    }

    /// The section cut in two at `at`, each half playing what was there.
    /// Not inside the repeats of a loop.
    pub fn split(&self, clip: &Clip, audio: &AudioClip, at: Tick) -> Option<(Clip, Clip)> {
        if at <= clip.start || at >= self.end(clip.start, audio) {
            return None;
        }
        if self
            .first_wrap(clip.start, audio)
            .is_some_and(|wrap| at >= wrap)
        {
            return None;
        }
        let origin = self.origin(audio);
        let cut = self.frames(self.source_at(clip.start, origin, at));
        let (offset, end) = (audio.offset as i64, (audio.offset + audio.length) as i64);
        if cut <= offset || cut >= end {
            return None;
        }
        let second_start = self.tick_of(clip.start, origin, cut as f64 / self.rate);
        if second_start <= clip.start {
            return None;
        }
        let mut first = audio.clone();
        first.length = (cut - offset) as u64;
        first.fade_out = 0;
        first.extent = None;
        super::clips::fit_fades(&mut first);
        let mut second = audio.clone();
        second.offset = cut as u64;
        second.length = (end - cut) as u64;
        second.fade_in = 0;
        second.extent = audio
            .extent
            .map(|e| Tick(clip.start.0 + e.0 - second_start.0))
            .filter(|e| e.0 > 0);
        super::clips::fit_fades(&mut second);
        Some((
            Clip {
                content: ClipContent::Audio(first),
                ..clip.clone()
            },
            Clip {
                start: second_start,
                content: ClipContent::Audio(second),
                ..clip.clone()
            },
        ))
    }

    /// The section with its contents slid `delta` ticks later: it keeps its
    /// place and its end and plays other parts of the file, to the file's
    /// ends.
    pub fn slip(
        &self,
        clip: &Clip,
        audio: &AudioClip,
        delta: i64,
        file_frames: u64,
    ) -> Option<Clip> {
        let origin = self.origin(audio);
        let natural = self.natural_end(clip.start, audio);
        let earlier = Tick(clip.start.0 - delta);
        let offset = self
            .frames(self.source_at(clip.start, origin, earlier))
            .clamp(0, file_frames.saturating_sub(1) as i64);
        // The same stretch of the timeline, from the new first moment.
        let span = self.span(clip.start, natural);
        let from = offset as f64 / self.rate;
        let to = self.curve.source_at(self.curve.at(from) + span);
        let end = self.frames(to).clamp(offset + 1, file_frames as i64);
        let mut audio = audio.clone();
        audio.offset = offset as u64;
        audio.length = (end - offset) as u64;
        super::clips::fit_fades(&mut audio);
        Some(Clip {
            content: ClipContent::Audio(audio),
            ..clip.clone()
        })
    }
}
