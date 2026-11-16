//! The separation analyser of the drum rack (#501): run a drum model over a
//! clip, then say for each hit which drum parts are sounding in it.
//!
//! A hit with two active parts is mixed (a hat over a kick). Only the windows
//! of the hits are kept from the separated audio, not whole parts, so the
//! result is small enough to cache for a clip of any length. The parts are
//! named by [`DrumPart`], so what reads this does not care which model ran
//! (see [`pads_for`](super::pads_for) and [`part_for`](super::part_for)).

use noodle_io::{Hit, analyse_hits};

use crate::drum::{DrumPart, PartSeparator};
use crate::split::{fit, to_rate};
use crate::{ModelId, SeparateError, Stereo};

/// A part is active in a hit when it is within this many decibels of the
/// loudest part of the hit. Separators leak the other parts into each part:
/// on real songs drumsep's leak is often within 20 dB, so a margin that wide
/// calls nearly every hit mixed; 10 dB leaves about half of a song's hits with
/// one clearly dominant part.
///
/// Not tuned by ear on real kits yet: the listening test on dry, loud rock,
/// electronic and room-miked kits is owed (see `docs/ROADMAP.md`).
pub const ACTIVE_WITHIN_DB: f32 = 10.0;

/// An event found in several parts keeps the detections of the parts within
/// this many decibels of the loudest. Wider than [`ACTIVE_WITHIN_DB`], which
/// decides what is *mixed*: a hat 14 dB under the kick it is struck with is a
/// hit of its own (its part found an onset), but the pair is still mixed.
const KEEP_WITHIN_DB: f32 = 20.0;

/// Hits quieter than this (peak, relative to full scale) are not kept when
/// hits are found in the parts: the same cut-off the drum binning uses.
const QUIET_DB: f32 = -45.0;

/// Detections in different parts within this long of each other are one event
/// (the same sound, or drums struck together).
const SAME_EVENT_SECONDS: f64 = 0.025;

/// Which part is loudest for an event is judged over this long from its first
/// onset: short, so a ringing cymbal cannot outvote a kick or snare.
const LOUDEST_SECONDS: f64 = 0.03;

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
    /// The part the hit was found in, by [`separate_and_find_hits`]; `None`
    /// when the hits were given to [`separate_hits`].
    pub home: Option<DrumPart>,
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

/// Runs `separator` over `audio` and brings its parts back to `rate` and the
/// length of `audio`.
fn parts_at_rate(
    separator: &mut dyn PartSeparator,
    audio: &Stereo,
    rate: u32,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<Vec<(DrumPart, Stereo)>, SeparateError> {
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
        let mut inner = |f: f32| progress(0.05 + 0.75 * f);
        separator.separate(&mix, &mut inner)?
    };
    drop(mix);
    // Back to the clip's rate and length, so frames line up with the hits.
    let mut at_rate: Vec<(DrumPart, Stereo)> = Vec::with_capacity(parts.len());
    for (part, stereo) in parts {
        let back = to_rate(&stereo, model_rate, rate).map_err(|e| fail("can't resample", &e))?;
        at_rate.push((part, fit(back, audio.len())));
    }
    if !progress(0.8) {
        return Err(SeparateError::Cancelled);
    }
    Ok(at_rate)
}

/// What each of `hits` holds of each of `parts` (which are as long as the clip,
/// at `rate`). `homes` says which part each hit was found in, if it was.
fn describe(
    model: ModelId,
    parts: &[(DrumPart, Stereo)],
    rate: u32,
    hits: &[HitWindow],
    homes: &[Option<DrumPart>],
) -> Separation {
    let compare = ((COMPARE_SECONDS * f64::from(rate)) as usize).max(1);
    let described = hits
        .iter()
        .zip(homes)
        .map(|(hit, home)| {
            let (start, end) = (hit.start as usize, hit.end as usize);
            let onset = (hit.onset as usize).clamp(start, end);
            let to = (onset + compare).min(end);
            let levels: Vec<(DrumPart, f32)> = parts
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
                audio: parts
                    .iter()
                    .map(|(p, a)| (*p, slice(a, start, end)))
                    .collect(),
                home: *home,
            }
        })
        .collect();
    Separation {
        model,
        parts: parts.iter().map(|(p, _)| *p).collect(),
        hits: described,
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
    let at_rate = parts_at_rate(separator, audio, rate, &mut |f| progress(f * 0.95))?;
    let homes = vec![None; hits.len()];
    let separation = describe(separator.model(), &at_rate, rate, hits, &homes);
    progress(1.0);
    Ok(separation)
}

/// Separates `audio` (at `rate`) into drum parts, then finds the hits in each
/// part rather than in the whole mix, so a hat over a kick is two hits, each
/// cut from its own part. An event heard in several parts at once (a detection
/// in each within [`SAME_EVENT_SECONDS`]) keeps the detections of the parts
/// within [`KEEP_WITHIN_DB`] of the loudest at its start; the rest is the
/// separator's leakage, not a drum, and is dropped. The hits are
/// in onset order and the [`Separation`] describes them one for one, with
/// [`HitSeparation::home`] set.
pub fn separate_and_find_hits(
    separator: &mut dyn PartSeparator,
    audio: &Stereo,
    rate: u32,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<(Vec<Hit>, Separation), SeparateError> {
    let at_rate = parts_at_rate(separator, audio, rate, &mut |f| progress(f * 0.7))?;
    let count = at_rate.len().max(1) as f32;
    let mut found: Vec<(usize, Hit)> = Vec::new();
    for (index, (_, part)) in at_rate.iter().enumerate() {
        let mono: Vec<f32> = part
            .left
            .iter()
            .zip(&part.right)
            .map(|(l, r)| 0.5 * (l + r))
            .collect();
        let base = 0.7 + 0.3 * index as f32 / count;
        let hits = analyse_hits(&mono, rate, &mut |f| progress(base + 0.3 * f / count))
            .map_err(|_| SeparateError::Cancelled)?;
        found.extend(
            hits.into_iter()
                .filter(|h| h.level_db >= QUIET_DB)
                .map(|h| (index, h)),
        );
    }
    found.sort_by_key(|(index, h)| (h.onset, *index));
    let window = ((SAME_EVENT_SECONDS * f64::from(rate)) as u64).max(1);
    let judged = ((LOUDEST_SECONDS * f64::from(rate)) as usize).max(1);
    let mut kept: Vec<(usize, Hit)> = Vec::new();
    let mut at = 0;
    while at < found.len() {
        let first = found[at].1.onset;
        let mut end = at;
        while end < found.len() && found[end].1.onset < first + window {
            end += 1;
        }
        let levels: Vec<f32> = at_rate
            .iter()
            .map(|(_, a)| level_db(a, first as usize, first as usize + judged))
            .collect();
        let loudest = levels.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if loudest > FLOOR_DB {
            kept.extend(
                found[at..end]
                    .iter()
                    .filter(|(i, _)| levels[*i] >= loudest - KEEP_WITHIN_DB)
                    .cloned(),
            );
        }
        at = end;
    }
    kept.sort_by_key(|(index, h)| (h.onset, *index));
    let windows: Vec<HitWindow> = kept
        .iter()
        .map(|(_, h)| HitWindow {
            onset: h.onset,
            start: h.start,
            end: h.end.min(audio.len() as u64),
        })
        .collect();
    let homes: Vec<Option<DrumPart>> = kept.iter().map(|(i, _)| Some(at_rate[*i].0)).collect();
    let separation = describe(separator.model(), &at_rate, rate, &windows, &homes);
    progress(1.0);
    Ok((kept.into_iter().map(|(_, h)| h).collect(), separation))
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

    fn kick() -> Vec<f32> {
        let mut phase = 0.0f32;
        (0..RATE as usize / 4)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                phase += std::f32::consts::TAU * (45.0 + 110.0 * (-t * 40.0).exp()) / RATE as f32;
                0.8 * phase.sin() * (-t * 14.0).exp()
            })
            .collect()
    }

    fn hat() -> Vec<f32> {
        let mut state = 99u32;
        let mut low = 0.0f32;
        (0..RATE as usize / 10)
            .map(|i| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let n = (state >> 8) as f32 / (1u32 << 23) as f32 - 1.0;
                low += 0.6 * (n - low);
                let t = i as f32 / RATE as f32;
                0.5 * (n - low) * (-t * 60.0).exp()
            })
            .collect()
    }

    fn put(into: &mut Stereo, at_seconds: f32, sound: &[f32], gain: f32) {
        let at = (at_seconds * RATE as f32) as usize;
        for (i, v) in sound.iter().enumerate() {
            into.left[at + i] += gain * v;
            into.right[at + i] += gain * v;
        }
    }

    #[test]
    fn hits_are_found_in_each_part_so_a_hat_over_a_kick_is_two_hits() {
        let n = 2 * RATE as usize;
        let (mut kicks, mut hats) = (Stereo::silent(n), Stereo::silent(n));
        put(&mut kicks, 0.2, &kick(), 1.0);
        put(&mut kicks, 1.2, &kick(), 1.0);
        put(&mut hats, 0.7, &hat(), 8.0);
        put(&mut hats, 1.2, &hat(), 8.0);
        // The kick leaks into the hat part, 35 dB down: leakage, not a hit.
        put(&mut hats, 0.2, &kick(), 0.018);
        let mut separator = Canned {
            parts: vec![(DrumPart::Kick, kicks), (DrumPart::HiHat, hats)],
            rate: RATE,
        };
        let (hits, separation) =
            separate_and_find_hits(&mut separator, &Stereo::silent(n), RATE, &mut |_| true)
                .unwrap();
        assert_eq!(
            hits.len(),
            4,
            "{:?}",
            hits.iter().map(|h| h.onset).collect::<Vec<_>>()
        );
        let homes: Vec<_> = separation.hits.iter().map(|h| h.home).collect();
        assert_eq!(
            homes,
            vec![
                Some(DrumPart::Kick),
                Some(DrumPart::HiHat),
                Some(DrumPart::Kick),
                Some(DrumPart::HiHat)
            ]
        );
        assert!(hits.windows(2).all(|w| w[0].onset <= w[1].onset));
        assert!(
            !separation.hits[0].is_mixed(),
            "the leak is under the margin"
        );
        assert!(
            separation.hits[2].is_mixed(),
            "the kick and the hat sound together"
        );
        assert!(separation.hits[3].is_mixed());
        // Each hit is cut from its own part.
        assert!(separation.hits[1].part_audio(DrumPart::HiHat).is_some());
    }

    #[test]
    fn finding_hits_in_parts_can_be_cancelled() {
        let n = RATE as usize;
        let mut separator = Canned {
            parts: vec![(DrumPart::Kick, Stereo::silent(n))],
            rate: RATE,
        };
        let result =
            separate_and_find_hits(&mut separator, &Stereo::silent(n), RATE, &mut |f| f < 0.5);
        assert!(matches!(result, Err(SeparateError::Cancelled)));
    }
}
