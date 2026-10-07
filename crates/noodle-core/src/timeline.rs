//! Time as the project document counts it: ticks, and the tempo map that
//! turns them into audio samples. See "Time and transport" in
//! docs/ARCHITECTURE.md.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Ticks in a quarter note.
pub const TICKS_PER_QUARTER: i64 = 960;

/// A position on the timeline in musical time, 960 to a quarter note. Clips,
/// loops and automation points sit on ticks, so they keep their place in the
/// music when the tempo changes. The engine counts in samples; the
/// [`TempoMap`] converts.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Tick(pub i64);

impl Tick {
    pub const ZERO: Tick = Tick(0);

    /// The tick `beats` quarter notes in, rounded to the nearest tick.
    pub fn from_quarters(quarters: f64) -> Tick {
        Tick((quarters * TICKS_PER_QUARTER as f64).round() as i64)
    }

    pub fn quarters(self) -> f64 {
        self.0 as f64 / TICKS_PER_QUARTER as f64
    }
}

/// A time signature, such as 3/4 or 6/8.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u8,
    /// What counts as a beat: 4 for a quarter note, 8 for an eighth. A power
    /// of two from 1 to 32.
    pub denominator: u8,
}

impl TimeSignature {
    pub const COMMON: TimeSignature = TimeSignature {
        numerator: 4,
        denominator: 4,
    };

    fn is_valid(self) -> bool {
        (1..=32).contains(&self.numerator)
            && self.denominator.is_power_of_two()
            && self.denominator <= 32
    }

    pub fn ticks_per_beat(self) -> i64 {
        4 * TICKS_PER_QUARTER / i64::from(self.denominator)
    }

    pub fn ticks_per_bar(self) -> i64 {
        i64::from(self.numerator) * self.ticks_per_beat()
    }
}

impl fmt::Display for TimeSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}

/// The tempo from `tick` on, until the next change. Tempos step; they don't
/// ramp.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoChange {
    pub tick: Tick,
    /// Quarter notes per minute.
    pub bpm: f64,
}

/// The time signature from the start of `bar` (counting from 0) on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureChange {
    pub bar: u32,
    pub signature: TimeSignature,
}

/// Where a tick falls in the bars: all counted from 0, so the downbeat of the
/// first bar is bar 0, beat 0, tick 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MusicalPosition {
    pub bar: u32,
    pub beat: u32,
    pub tick: u32,
}

/// Why a tempo map was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TempoMapError(pub &'static str);

impl fmt::Display for TempoMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid tempo map: {}", self.0)
    }
}

impl std::error::Error for TempoMapError {}

/// The tempo and time signature along the timeline, which fixes how ticks
/// map to time. It always has a tempo and a signature at the very start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawTempoMap")]
pub struct TempoMap {
    tempos: Vec<TempoChange>,
    signatures: Vec<SignatureChange>,
}

#[derive(Deserialize)]
struct RawTempoMap {
    tempos: Vec<TempoChange>,
    signatures: Vec<SignatureChange>,
}

impl TryFrom<RawTempoMap> for TempoMap {
    type Error = TempoMapError;

    fn try_from(raw: RawTempoMap) -> Result<Self, Self::Error> {
        Self::new(raw.tempos, raw.signatures)
    }
}

impl Default for TempoMap {
    /// 120 beats per minute in 4/4.
    fn default() -> Self {
        Self::constant(120.0, TimeSignature::COMMON).expect("120 bpm is valid")
    }
}

impl TempoMap {
    pub const MIN_BPM: f64 = 10.0;
    pub const MAX_BPM: f64 = 999.0;

    /// A map from the changes in it. Each list must start at the very
    /// beginning (tick 0, bar 0) and be in strictly increasing order.
    pub fn new(
        tempos: Vec<TempoChange>,
        signatures: Vec<SignatureChange>,
    ) -> Result<Self, TempoMapError> {
        if tempos.first().is_none_or(|first| first.tick != Tick::ZERO) {
            return Err(TempoMapError("the first tempo must be at tick 0"));
        }
        if tempos.windows(2).any(|pair| pair[0].tick >= pair[1].tick) {
            return Err(TempoMapError("tempo changes must be in increasing order"));
        }
        if tempos
            .iter()
            .any(|t| !(Self::MIN_BPM..=Self::MAX_BPM).contains(&t.bpm))
        {
            return Err(TempoMapError("a tempo is out of range"));
        }
        if signatures.first().is_none_or(|first| first.bar != 0) {
            return Err(TempoMapError("the first time signature must be at bar 0"));
        }
        if signatures.windows(2).any(|pair| pair[0].bar >= pair[1].bar) {
            return Err(TempoMapError(
                "time signature changes must be in increasing order",
            ));
        }
        if signatures.iter().any(|s| !s.signature.is_valid()) {
            return Err(TempoMapError("a time signature is not valid"));
        }
        Ok(Self { tempos, signatures })
    }

    /// One tempo and one time signature throughout.
    pub fn constant(bpm: f64, signature: TimeSignature) -> Result<Self, TempoMapError> {
        Self::new(
            vec![TempoChange {
                tick: Tick::ZERO,
                bpm,
            }],
            vec![SignatureChange { bar: 0, signature }],
        )
    }

    pub fn tempos(&self) -> &[TempoChange] {
        &self.tempos
    }

    pub fn signatures(&self) -> &[SignatureChange] {
        &self.signatures
    }

    /// The tempo at `tick`. Before the start it's the first tempo.
    pub fn bpm_at(&self, tick: Tick) -> f64 {
        let i = self.tempos.partition_point(|t| t.tick <= tick);
        self.tempos[i.saturating_sub(1)].bpm
    }

    /// Seconds from the start to `tick`. Ticks before the start are 0.
    pub fn seconds_at(&self, tick: Tick) -> f64 {
        let mut seconds = 0.0;
        for (i, change) in self.tempos.iter().enumerate() {
            if change.tick >= tick {
                break;
            }
            let end = self
                .tempos
                .get(i + 1)
                .map_or(tick, |next| next.tick.min(tick));
            seconds +=
                (end.0 - change.tick.0) as f64 / TICKS_PER_QUARTER as f64 * 60.0 / change.bpm;
        }
        seconds
    }

    /// The sample `tick` falls on at `sample_rate`, rounded to the nearest.
    pub fn sample_at(&self, tick: Tick, sample_rate: f64) -> u64 {
        (self.seconds_at(tick) * sample_rate).round() as u64
    }

    /// The tick, fraction included, that `sample` falls on.
    pub fn tick_at_sample(&self, sample: u64, sample_rate: f64) -> f64 {
        let mut remaining = sample as f64 / sample_rate;
        for (i, change) in self.tempos.iter().enumerate() {
            let seconds_per_tick = 60.0 / change.bpm / TICKS_PER_QUARTER as f64;
            if let Some(next) = self.tempos.get(i + 1) {
                let segment = (next.tick.0 - change.tick.0) as f64 * seconds_per_tick;
                if remaining >= segment {
                    remaining -= segment;
                    continue;
                }
            }
            return change.tick.0 as f64 + remaining / seconds_per_tick;
        }
        unreachable!("a tempo map has a tempo")
    }

    /// Where `tick` falls in the bars. Ticks before the start are at the start.
    pub fn position(&self, tick: Tick) -> MusicalPosition {
        let mut remaining = tick.0.max(0);
        let mut bar = 0u32;
        for (i, change) in self.signatures.iter().enumerate() {
            let per_bar = change.signature.ticks_per_bar();
            let bars_here = match self.signatures.get(i + 1) {
                Some(next) => i64::from(next.bar - change.bar),
                None => i64::MAX,
            };
            let whole_bars = (remaining / per_bar).min(bars_here);
            if whole_bars < bars_here {
                remaining -= whole_bars * per_bar;
                let per_beat = change.signature.ticks_per_beat();
                return MusicalPosition {
                    bar: bar + whole_bars as u32,
                    beat: (remaining / per_beat) as u32,
                    tick: (remaining % per_beat) as u32,
                };
            }
            remaining -= bars_here * per_bar;
            bar += bars_here as u32;
        }
        unreachable!("the last signature runs forever")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn tempo(tick: i64, bpm: f64) -> TempoChange {
        TempoChange {
            tick: Tick(tick),
            bpm,
        }
    }

    fn signature(bar: u32, numerator: u8, denominator: u8) -> SignatureChange {
        SignatureChange {
            bar,
            signature: TimeSignature {
                numerator,
                denominator,
            },
        }
    }

    /// 120 bpm for four quarter notes, then 60.
    fn slowing_down() -> TempoMap {
        TempoMap::new(
            vec![tempo(0, 120.0), tempo(4 * 960, 60.0)],
            vec![signature(0, 4, 4)],
        )
        .unwrap()
    }

    #[test]
    fn a_quarter_note_at_120_is_half_a_second() {
        let map = TempoMap::default();
        assert_eq!(map.seconds_at(Tick(960)), 0.5);
        assert_eq!(map.sample_at(Tick(960), RATE), 24_000);
        assert_eq!(map.sample_at(Tick::ZERO, RATE), 0);
    }

    #[test]
    fn a_tempo_change_slows_what_follows_it() {
        let map = slowing_down();
        // Four quarters at 120 take 2 s, and one more at 60 takes 1 s.
        assert_eq!(map.seconds_at(Tick(4 * 960)), 2.0);
        assert_eq!(map.seconds_at(Tick(5 * 960)), 3.0);
        assert_eq!(map.sample_at(Tick(5 * 960), RATE), 144_000);
        assert_eq!(map.bpm_at(Tick(4 * 960 - 1)), 120.0);
        assert_eq!(map.bpm_at(Tick(4 * 960)), 60.0);
    }

    #[test]
    fn samples_convert_back_to_ticks() {
        let map = slowing_down();
        for tick in [0, 1, 959, 960, 3839, 3840, 3841, 4800, 100_000] {
            let sample = map.sample_at(Tick(tick), RATE);
            let back = map.tick_at_sample(sample, RATE);
            // A sample is far shorter than a tick at these tempos, but the
            // rounding to a sample can still shift the tick a little.
            assert!(
                (back - tick as f64).abs() < 1.0,
                "{tick} came back as {back}"
            );
        }
        // Exactly 3 s in is five quarters in.
        assert_eq!(map.tick_at_sample(144_000, RATE), 5.0 * 960.0);
    }

    #[test]
    fn ticks_before_the_start_are_the_start() {
        let map = TempoMap::default();
        assert_eq!(map.seconds_at(Tick(-100)), 0.0);
        assert_eq!(map.bpm_at(Tick(-100)), 120.0);
        assert_eq!(
            map.position(Tick(-100)),
            MusicalPosition {
                bar: 0,
                beat: 0,
                tick: 0
            }
        );
    }

    #[test]
    fn positions_follow_the_time_signature() {
        // Two bars of 4/4, then 6/8.
        let map = TempoMap::new(
            vec![tempo(0, 100.0)],
            vec![signature(0, 4, 4), signature(2, 6, 8)],
        )
        .unwrap();
        let at = |tick| map.position(Tick(tick));
        assert_eq!(
            at(960 + 5),
            MusicalPosition {
                bar: 0,
                beat: 1,
                tick: 5
            }
        );
        assert_eq!(at(3840).bar, 1);
        // Bar 2 starts after 2 * 3840 ticks, and an eighth note is 480.
        assert_eq!(
            at(7680),
            MusicalPosition {
                bar: 2,
                beat: 0,
                tick: 0
            }
        );
        assert_eq!(
            at(7680 + 5 * 480 + 7),
            MusicalPosition {
                bar: 2,
                beat: 5,
                tick: 7
            }
        );
        // A 6/8 bar is 2880 ticks.
        assert_eq!(at(7680 + 2880).bar, 3);
    }

    #[test]
    fn refuses_maps_that_make_no_sense() {
        let ok_signatures = || vec![signature(0, 4, 4)];
        let tempos = |t: Vec<TempoChange>| TempoMap::new(t, ok_signatures());
        assert!(tempos(vec![]).is_err(), "no tempo");
        assert!(tempos(vec![tempo(10, 120.0)]).is_err(), "not at the start");
        assert!(tempos(vec![tempo(0, 120.0), tempo(0, 90.0)]).is_err());
        assert!(tempos(vec![tempo(0, 120.0), tempo(100, 90.0), tempo(50, 80.0)]).is_err());
        assert!(tempos(vec![tempo(0, f64::NAN)]).is_err());
        assert!(tempos(vec![tempo(0, 0.0)]).is_err());
        assert!(tempos(vec![tempo(0, 5000.0)]).is_err());

        let signatures = |s: Vec<SignatureChange>| TempoMap::new(vec![tempo(0, 120.0)], s);
        assert!(signatures(vec![]).is_err());
        assert!(signatures(vec![signature(1, 4, 4)]).is_err());
        assert!(signatures(vec![signature(0, 4, 4), signature(0, 3, 4)]).is_err());
        assert!(
            signatures(vec![signature(0, 4, 3)]).is_err(),
            "3 isn't a power of 2"
        );
        assert!(signatures(vec![signature(0, 0, 4)]).is_err());
        assert!(signatures(vec![signature(0, 7, 8)]).is_ok());
    }

    #[test]
    fn a_saved_map_is_checked_on_load() {
        let map = slowing_down();
        let text = ron::to_string(&map).unwrap();
        assert_eq!(ron::from_str::<TempoMap>(&text).unwrap(), map);

        let broken = "(tempos: [(tick: 10, bpm: 120.0)], signatures: [(bar: 0, signature: (numerator: 4, denominator: 4))])";
        assert!(ron::from_str::<TempoMap>(broken).is_err());
    }
}
