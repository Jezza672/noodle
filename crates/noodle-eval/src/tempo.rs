//! The target tempo maps of the probe grid.
//!
//! A warp under test converts a clip recorded at one steady tempo (the source)
//! to a timeline tempo (the target). The grid is constant ratios, plus three
//! changing maps whose only randomness is a fixed seed.

use noodle_core::{SignatureChange, TempoChange, TempoMap, Tick, TimeSignature};

/// How the target tempo relates to the source's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TempoCase {
    /// Target tempo over source tempo, constant: 2 is twice as fast.
    Ratio(f64),
    /// From 0.75 to 1.5 times the source tempo, climbing evenly.
    Ramp,
    /// 0.8 times the source tempo, then 1.25 times it from the middle.
    Step,
    /// Around the source tempo, by up to a fifth either way, from a seeded
    /// pair of slow oscillations.
    Wobble,
}

/// The ratios of the constant cases.
pub const RATIOS: [f64; 6] = [0.5, 0.75, 0.9, 1.1, 1.5, 2.0];

/// The seed of the wobble.
const WOBBLE_SEED: u32 = 0x5eed_0008;

/// The changing maps are cut into steps this many beats long.
const STEP_BEATS: f64 = 0.25;

impl TempoCase {
    /// The whole grid.
    pub fn all() -> Vec<Self> {
        RATIOS
            .iter()
            .map(|r| Self::Ratio(*r))
            .chain([Self::Ramp, Self::Step, Self::Wobble])
            .collect()
    }

    pub fn name(&self) -> String {
        match self {
            Self::Ratio(r) => format!("x{r}"),
            Self::Ramp => "ramp".into(),
            Self::Step => "step".into(),
            Self::Wobble => "wobble".into(),
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::all().into_iter().find(|c| c.name() == name)
    }

    /// The constant ratio, if this is a constant case.
    pub fn ratio(&self) -> Option<f64> {
        match self {
            Self::Ratio(r) => Some(*r),
            _ => None,
        }
    }

    /// The target tempo map for a source of `source_bpm` and a piece of
    /// `beats` quarter notes.
    pub fn map(&self, source_bpm: f64, beats: f64) -> TempoMap {
        let ratio_at = |beat: f64| match self {
            Self::Ratio(r) => *r,
            Self::Ramp => 0.75 + 0.75 * beat / beats,
            Self::Step => {
                if beat < beats / 2.0 {
                    0.8
                } else {
                    1.25
                }
            }
            Self::Wobble => wobble(beat),
        };
        let mut changes = Vec::new();
        match self {
            Self::Ratio(_) => changes.push(change(0.0, source_bpm * ratio_at(0.0))),
            Self::Step => {
                changes.push(change(0.0, source_bpm * 0.8));
                changes.push(change(beats / 2.0, source_bpm * 1.25));
            }
            Self::Ramp | Self::Wobble => {
                let steps = (beats / STEP_BEATS).ceil() as usize;
                for i in 0..steps {
                    // The tempo in the middle of the step stands for all of it.
                    let at = i as f64 * STEP_BEATS;
                    changes.push(change(at, source_bpm * ratio_at(at + STEP_BEATS / 2.0)));
                }
            }
        }
        TempoMap::new(
            changes,
            vec![SignatureChange {
                bar: 0,
                signature: TimeSignature::COMMON,
            }],
        )
        .expect("the probe tempo maps are valid")
    }
}

fn change(beat: f64, bpm: f64) -> TempoChange {
    TempoChange {
        tick: Tick::from_quarters(beat),
        bpm,
    }
}

/// The wobble's ratio at `beat`: two oscillations of a few beats each, with
/// seeded rates and phases.
fn wobble(beat: f64) -> f64 {
    let mut seed = WOBBLE_SEED;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(seed >> 8) / f64::from(1u32 << 24)
    };
    let (rate_a, phase_a) = (0.35 + 0.2 * next(), std::f64::consts::TAU * next());
    let (rate_b, phase_b) = (0.8 + 0.4 * next(), std::f64::consts::TAU * next());
    1.0 + 0.2 * 0.5 * ((rate_a * beat + phase_a).sin() + (rate_b * beat + phase_b).sin())
}

/// The source tempo map: one steady tempo.
pub fn steady(bpm: f64) -> TempoMap {
    TempoMap::constant(bpm, TimeSignature::COMMON).expect("a steady tempo is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_is_what_the_design_names() {
        let names: Vec<String> = TempoCase::all().iter().map(TempoCase::name).collect();
        assert_eq!(
            names,
            [
                "x0.5", "x0.75", "x0.9", "x1.1", "x1.5", "x2", "ramp", "step", "wobble"
            ]
        );
    }

    #[test]
    fn a_ratio_scales_the_source_tempo_and_a_ramp_climbs() {
        let map = TempoCase::Ratio(1.5).map(120.0, 16.0);
        assert_eq!(map.bpm_at(Tick::ZERO), 180.0);
        let ramp = TempoCase::Ramp.map(120.0, 16.0);
        assert!(ramp.bpm_at(Tick::from_quarters(0.0)) < 95.0);
        assert!(ramp.bpm_at(Tick::from_quarters(15.9)) > 175.0);
        let step = TempoCase::Step.map(120.0, 16.0);
        assert_eq!(step.bpm_at(Tick::from_quarters(7.9)), 96.0);
        assert_eq!(step.bpm_at(Tick::from_quarters(8.0)), 150.0);
    }

    #[test]
    fn the_wobble_is_the_same_every_time_and_stays_within_a_fifth() {
        let a = TempoCase::Wobble.map(120.0, 16.0);
        assert_eq!(a, TempoCase::Wobble.map(120.0, 16.0));
        assert!(a.tempos().iter().any(|t| (t.bpm - 120.0).abs() > 4.0));
        assert!(a.tempos().iter().all(|t| (96.0..=144.0).contains(&t.bpm)));
    }
}
