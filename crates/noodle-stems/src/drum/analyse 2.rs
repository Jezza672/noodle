//! The separation analyser of the drum rack (#501): run a drum model over a
//! clip, then say for each hit which drum parts are sounding in it.
//!
//! A hit with two active parts is mixed (a hat over a kick). Only the windows
//! of the hits are kept from the separated audio, not whole parts, so the
//! result is small enough to cache for a clip of any length. The parts are
//! named by [`DrumPart`], so what reads this does not care which model ran
//! (see [`pads_for`](super::pads_for) and [`part_for`](super::part_for)).

use crate::drum::{DrumPart, PartSeparator};
use crate::split::{fit, to_rate};
use crate::{ModelId, SeparateError, Stereo};

/// A part is active in a hit when it is within this many decibels of the
/// loudest part of the hit. Separators leak a little of every part into the
/// others (about -30 dB), so a part under this is bleed, not a drum.
///
/// Not tuned on real kits yet: the listening test on dry, loud rock, electronic
/// and room-miked kits is owed (see `docs/ROADMAP.md`).
pub const ACTIVE_WITHIN_DB: f32 = 20.0;

/// A part quieter than this (relative to full scale) is never active, whatever
/// the others are: silence next to silence is not a drum.
const FLOOR_DB: f32 = -55.0;

/// How much of a hit, from its onset, the parts are compared over: the attack
/// and early body, before the tails of different drums run together.
const COMPARE_SECONDS: f64 = 0.08;

/// Where a hit sits in the clip's audio, in frames at the clip's rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitWindow {
    pub onset: u64,
    pub start: u64,
    pub end: u64,
}

/// What the separator found in one hit.
#[derive(Clone, Debug, PartialEq)]
pub struct HitSeparation {
    /// The level of each part in the hit's attack, in decibels (full scale
    /// is 0), in the model's part order.
    pub levels: Vec<(DrumPart, f32)>,
    /// The parts that are sounding, loudest first.
    pub active: Vec<DrumPart>,
    /// The hit's window of each part, as long as the hit, at the clip's rate.
    pub audio: Vec<(DrumPart, Stereo)>,
}

impl HitSeparation {
    /// Whether two or more parts sound in this hit.
    pub fn is_mixed(&self) -> bool {
        self.active.len() >= 2
    }

    /// The hit's window of `part`.
    pub fn part_audio(&self, part: DrumPart) -> Option<&Stereo> {
        self.audio.iter().find(|(p, _)| *p == part).map(|(_, a)| a)
    }
}

/// A separation of a clip, by hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Separation {
    pub model: ModelId,
    /// The parts the model gives.
    pub parts: Vec<DrumPart>,
    /// One for each hit given to [`separate_hits`], in the same order.
    pub hits: Vec<HitSeparation>,
}

fn level_db(audio: &Stereo, from: usize, to: usize) -> f32 {
    let to = to.min(audio.len());
    if from >= to {
        return f32::NEG_INFINITY;
    }
    let sum: f32 = (from..to)
        .map(|i| 0.5 * (audio.left[i] * audio.left[i] + audio.right[i] * audio.right[i]))
        .sum();
    10.0 * (sum / (to - from) as f32 + 1e-12).log10()
}

fn slice(audio: &Stereo, from: usize, to: usize) -> Stereo {
    let to = to.min(audio.len());
    let from = from.min(to);
    Stereo {
        left: audio.left[from..to].to_vec(),
        right: audio.right[from..to].to_vec(),
    }
}

/// Runs `separator` over `audio` (at `rate`) and describes each of `hits`.
/// `progress` is told how far along it is (0 to 1) and answers whether to go
/// on; on `false` this stops with [`SeparateError::Cancelled`].
pub fn separate_hits(
    separator: &mut dyn PartSeparator,
    audio: &Stereo,
    rate: u32,
    hits: &[HitWindow],
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<Separation, SeparateError> {
    let fail =
        |what: &str, e: &dyn std::fmt::Display| SeparateError::Failed(format!("{what}: {e}"));
    if !progress(0.0) {
        return Err(SeparateError::Cancelled);
    }
    let model_rate = separator.sample_rate();
    let mix = to_rate(audio, rate, model_rate).map_err(|e| fail("can't resample", &e))?;
    if !progress(0.05) {
        return Err(SeparateError::Cancelled);
    }
    let parts = {
        let mut inner = |f: f32| progress(0.05 + 0.85 * f);
        separator.separate(&mix, &mut inner)?
    };
    drop(mix);
    // Back to the clip's rate and length, so frames line up with the hits.
    let mut at_rate: Vec<(DrumPart, Stereo)> = Vec::with_capacity(parts.len());
    for (part, stereo) in parts {
        let back = to_rate(&stereo, model_rate, rate).map_err(|e| fail("can't resample", &e))?;
        at_rate.push((part, fit(back, audio.len())));
    }
    if !progress(0.95) {
        return Err(SeparateError::Cancelled);
    }
    let compare = ((COMPARE_SECONDS * f64::from(rate)) as usize).max(1);
    let described = hits
        .iter()
        .map(|hit| {
            let (start, end) = (hit.start as usize, hit.end as usize);
            let onset = (hit.onset as usize).clamp(start, end);
            let to = (onset + compare).min(end);
            let levels: Vec<(DrumPart, f32)> = at_rate
                .iter()
                .map(|(p, a)| (*p, level_db(a, onset, to)))
                .collect();
            let loudest = levels.iter().map(|l| l.1).fold(f32::NEG_INFINITY, f32::max);
            let mut active: Vec<(DrumPart, f32)> = levels
                .iter()
                .copied()
                .filter(|&(_, db)| db > FLOOR_DB && db >= loudest - ACTIVE_WITHIN_DB)
                .collect();
            active.sort_by(|a, b| b.1.total_cmp(&a.1));
            HitSeparation {
                levels,
                active: active.into_iter().map(|(p, _)| p).collect(),
                audio: at_rate
                    .iter()
                    .map(|(p, a)| (*p, slice(a, start, end)))
                    .collect(),
            }
        })
        .collect();
    progress(1.0);
    Ok(Separation {
        model: separator.model(),
        parts: at_rate.iter().map(|(p, _)| *p).collect(),
        hits: described,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// A separator that returns the parts it was built with, whatever the mix.
    struct Canned {
        parts: Vec<(DrumPart, Stereo)>,
        rate: u32,
    }

    impl PartSeparator for Canned {
        fn model(&self) -> ModelId {
            ModelId {
                id: "canned".into(),
                version: "1".into(),
            }
        }
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn parts(&self) -> Vec<DrumPart> {
            self.parts.iter().map(|p| p.0).collect()
        }
        fn separate(
            &mut self,
            _: &Stereo,
            progress: &mut dyn FnMut(f32) -> bool,
        ) -> Result<Vec<(DrumPart, Stereo)>, SeparateError> {
            if !progress(0.5) {
                return Err(SeparateError::Cancelled);
            }
            Ok(self.parts.clone())
        }
    }

    /// `frames` of a constant `gain` from `from` to `to`, else silence.
    fn burst(frames: usize, from: usize, to: usize, gain: f32) -> Stereo {
        let mut s = Stereo::silent(frames);
        for i in from..to {
            s.left[i] = gain;
            s.right[i] = gain;
        }
        s
    }

    fn window(onset: u64, end: u64) -> HitWindow {
        HitWindow {
            onset,
            start: onset,
            end,
        }
    }

    fn run(parts: Vec<(DrumPart, Stereo)>, hits: &[HitWindow]) -> Separation {
        let frames = parts[0].1.len();
        let mut separator = Canned { parts, rate: RATE };
        separate_hits(
            &mut separator,
            &Stereo::silent(frames),
            RATE,
            hits,
            &mut |_| true,
        )
        .unwrap()
    }

    #[test]
    fn one_part_sounding_is_one_drum_and_two_are_mixed() {
        let n = 20_000;
        // Hit 0 (0..5000): kick only. Hit 1 (10000..15000): kick and hat.
        let kick = {
            let mut k = burst(n, 0, 5000, 0.5);
            for i in 10_000..15_000 {
                k.left[i] = 0.5;
                k.right[i] = 0.5;
            }
            k
        };
        let hat = burst(n, 10_000, 15_000, 0.25);
        let quiet = Stereo::silent(n);
        let separation = run(
            vec![
                (DrumPart::Kick, kick),
                (DrumPart::HiHat, hat),
                (DrumPart::Snare, quiet),
            ],
            &[window(0, 5000), window(10_000, 15_000)],
        );
        assert_eq!(separation.hits[0].active, vec![DrumPart::Kick]);
        assert!(!separation.hits[0].is_mixed());
        assert_eq!(
            separation.hits[1].active,
            vec![DrumPart::Kick, DrumPart::HiHat]
        );
        assert!(separation.hits[1].is_mixed());
    }

    #[test]
    fn bleed_far_below_the_loudest_part_does_not_make_a_hit_mixed() {
        let n = 8000;
        // The snare leaks 30 dB below the kick.
        let separation = run(
            vec![
                (DrumPart::Kick, burst(n, 0, n, 0.5)),
                (DrumPart::Snare, burst(n, 0, n, 0.5 * 0.0316)),
            ],
            &[window(0, 6000)],
        );
        assert!(!separation.hits[0].is_mixed());
        // And a part within the margin does count.
        let separation = run(
            vec![
                (DrumPart::Kick, burst(n, 0, n, 0.5)),
                (DrumPart::Snare, burst(n, 0, n, 0.5 * 0.5)),
            ],
            &[window(0, 6000)],
        );
        assert!(separation.hits[0].is_mixed());
    }

    #[test]
    fn a_silent_window_has_no_active_part() {
        let n = 4000;
        let separation = run(
            vec![
                (DrumPart::Kick, Stereo::silent(n)),
                (DrumPart::Snare, Stereo::silent(n)),
            ],
            &[window(0, 2000)],
        );
        assert!(separation.hits[0].active.is_empty());
    }

    #[test]
    fn the_hits_window_of_each_part_is_kept_at_the_clips_rate() {
        let n = 9000;
        let separation = run(
            vec![(DrumPart::Kick, burst(n, 1000, 3000, 0.5))],
            &[window(1000, 3000)],
        );
        let audio = separation.hits[0].part_audio(DrumPart::Kick).unwrap();
        assert_eq!(audio.len(), 2000);
        assert!(audio.left.iter().all(|&s| s == 0.5));
        assert!(separation.hits[0].part_audio(DrumPart::Snare).is_none());
    }

    #[test]
    fn a_model_at_another_rate_is_resampled_there_and_back() {
        let n = 44_100;
        let mut separator = Canned {
            parts: vec![(DrumPart::Kick, burst(22_050, 0, 11_025, 0.5))],
            rate: 22_050,
        };
        let separation = separate_hits(
            &mut separator,
            &Stereo::silent(n),
            RATE,
            &[window(0, 20_000)],
            &mut |_| true,
        )
        .unwrap();
        // The part is the length of the hit window, so frames line up.
        assert_eq!(
            separation.hits[0].part_audio(DrumPart::Kick).unwrap().len(),
            20_000
        );
        assert_eq!(separation.hits[0].active, vec![DrumPart::Kick]);
    }

    #[test]
    fn cancelling_stops_the_separation() {
        let mut separator = Canned {
            parts: vec![(DrumPart::Kick, Stereo::silent(100))],
            rate: RATE,
        };
        let result = separate_hits(&mut separator, &Stereo::silent(100), RATE, &[], &mut |f| {
            f < 0.3
        });
        assert_eq!(result, Err(SeparateError::Cancelled));
    }
}
