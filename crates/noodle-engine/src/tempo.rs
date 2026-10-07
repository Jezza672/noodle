//! The tempo map as the audio thread reads it: a table of segments that
//! converts between samples and ticks without allocating.
//!
//! The project's [`TempoMap`] is for editing; this is its compiled copy for
//! one sample rate. The controller builds it and sends it over like a plan.

use noodle_core::{TICKS_PER_QUARTER, TempoMap, Tick, TimeSignature};

/// A stretch of constant tempo.
#[derive(Clone, Copy, Debug)]
struct Segment {
    start_tick: i64,
    /// The sample the segment starts on, fractions included.
    start_sample: f64,
    samples_per_tick: f64,
    bpm: f64,
}

/// A stretch of constant time signature.
#[derive(Clone, Copy, Debug)]
struct Meter {
    start_tick: i64,
    signature: TimeSignature,
}

#[derive(Clone, Debug)]
pub struct TempoTable {
    segments: Box<[Segment]>,
    meters: Box<[Meter]>,
}

impl TempoTable {
    pub fn new(map: &TempoMap, sample_rate: f32) -> Self {
        let rate = f64::from(sample_rate);
        let mut segments = Vec::with_capacity(map.tempos().len());
        let mut start_sample = 0.0;
        for (i, change) in map.tempos().iter().enumerate() {
            let samples_per_tick = rate * 60.0 / change.bpm / TICKS_PER_QUARTER as f64;
            segments.push(Segment {
                start_tick: change.tick.0,
                start_sample,
                samples_per_tick,
                bpm: change.bpm,
            });
            if let Some(next) = map.tempos().get(i + 1) {
                start_sample += (next.tick.0 - change.tick.0) as f64 * samples_per_tick;
            }
        }

        let mut meters = Vec::with_capacity(map.signatures().len());
        let mut start_tick = 0i64;
        for (i, change) in map.signatures().iter().enumerate() {
            meters.push(Meter {
                start_tick,
                signature: change.signature,
            });
            if let Some(next) = map.signatures().get(i + 1) {
                start_tick += i64::from(next.bar - change.bar) * change.signature.ticks_per_bar();
            }
        }
        Self {
            segments: segments.into(),
            meters: meters.into(),
        }
    }

    /// The tick, fractions included, that `sample` falls on.
    pub fn tick_at(&self, sample: u64) -> f64 {
        let sample = sample as f64;
        let i = self
            .segments
            .partition_point(|s| s.start_sample <= sample)
            .saturating_sub(1);
        let segment = &self.segments[i];
        segment.start_tick as f64 + (sample - segment.start_sample) / segment.samples_per_tick
    }

    /// The sample `tick` falls on, rounded to the nearest. Ticks before the
    /// start are sample 0.
    pub fn sample_at(&self, tick: f64) -> u64 {
        if tick <= 0.0 {
            return 0;
        }
        let i = self
            .segments
            .partition_point(|s| s.start_tick as f64 <= tick)
            .saturating_sub(1);
        let segment = &self.segments[i];
        let sample =
            segment.start_sample + (tick - segment.start_tick as f64) * segment.samples_per_tick;
        sample.round() as u64
    }

    /// The tempo at `tick`, in quarter notes per minute.
    pub fn bpm_at(&self, tick: f64) -> f64 {
        let i = self
            .segments
            .partition_point(|s| s.start_tick as f64 <= tick)
            .saturating_sub(1);
        self.segments[i].bpm
    }

    pub fn signature_at(&self, tick: f64) -> TimeSignature {
        let i = self
            .meters
            .partition_point(|m| m.start_tick as f64 <= tick)
            .saturating_sub(1);
        self.meters[i].signature
    }

    /// The sample position of a project tick.
    pub fn sample_at_tick(&self, tick: Tick) -> u64 {
        self.sample_at(tick.0 as f64)
    }
}

impl Default for TempoTable {
    /// 120 beats per minute in 4/4, at 48 kHz.
    fn default() -> Self {
        Self::new(&TempoMap::default(), 48_000.0)
    }
}

#[cfg(test)]
mod tests {
    use noodle_core::{SignatureChange, TempoChange};

    use super::*;

    const RATE: f32 = 48_000.0;

    fn slowing_down() -> TempoMap {
        TempoMap::new(
            vec![
                TempoChange {
                    tick: Tick(0),
                    bpm: 120.0,
                },
                TempoChange {
                    tick: Tick(4 * 960),
                    bpm: 60.0,
                },
            ],
            vec![
                SignatureChange {
                    bar: 0,
                    signature: TimeSignature::COMMON,
                },
                SignatureChange {
                    bar: 2,
                    signature: TimeSignature {
                        numerator: 6,
                        denominator: 8,
                    },
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn agrees_with_the_tempo_map() {
        let map = slowing_down();
        let table = TempoTable::new(&map, RATE);
        for tick in [0, 1, 959, 960, 3839, 3840, 3841, 4800, 100_000] {
            assert_eq!(
                table.sample_at_tick(Tick(tick)),
                map.sample_at(Tick(tick), f64::from(RATE)),
                "tick {tick}"
            );
        }
        for sample in [0u64, 1, 23_999, 24_000, 96_000, 144_000, 1_000_000] {
            let want = map.tick_at_sample(sample, f64::from(RATE));
            assert!(
                (table.tick_at(sample) - want).abs() < 1e-6,
                "sample {sample}"
            );
        }
    }

    #[test]
    fn tempo_and_signature_follow_the_tick() {
        let table = TempoTable::new(&slowing_down(), RATE);
        assert_eq!(table.bpm_at(0.0), 120.0);
        assert_eq!(table.bpm_at(3839.9), 120.0);
        assert_eq!(table.bpm_at(3840.0), 60.0);
        // Two bars of 4/4 are 7680 ticks.
        assert_eq!(table.signature_at(7679.0), TimeSignature::COMMON);
        assert_eq!(table.signature_at(7680.0).numerator, 6);
    }

    #[test]
    fn ticks_before_the_start_are_sample_zero() {
        let table = TempoTable::default();
        assert_eq!(table.sample_at(-5.0), 0);
        assert_eq!(table.bpm_at(-5.0), 120.0);
    }
}
