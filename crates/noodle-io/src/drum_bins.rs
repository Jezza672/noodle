//! Finding the drum hits in a drum track, grouping the ones that sound alike
//! into bins, and guessing what each bin is. The analysis behind "Create drum
//! rack from clip" (docs/design/drum-rack-from-clip.md).
//!
//! Everything here is a pure function over `&[f32]`, offline, never on the
//! audio thread, so it is testable without a session, like `onsets.rs` and
//! `pitch.rs`.
//!
//! * [`analyse_hits`] finds the hits, cuts each one and describes it by a
//!   [`HitFeatures`]. It is the slow part (and the cancellable one); its result
//!   is kept by [`HitCache`].
//! * [`bin_hits`] groups the hits by a threshold (the dialog's *Merge more /
//!   Split more* slider). It works on the kept features, so moving the slider
//!   re-bins in milliseconds with no re-analysis.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use realfft::RealFftPlanner;

use crate::onsets::{detect_bass_onsets_with, detect_onsets_with, detect_treble_onsets_with};
use crate::pitch::PitchTracker;

/// Bumped when the hits found or the features change, so a cached analysis made
/// by an older detector is not replayed.
pub const DETECTOR_VERSION: u32 = 1;

/// The onset sensitivity: higher than the warp engine's, so a soft ghost note
/// still counts as a hit.
const SENSITIVITY: f32 = 0.7;
/// Onsets of the full band, the bass and the treble closer than this are one
/// hit (the bass detector's filter makes it report a few milliseconds late).
const PIN_MERGE: f64 = 0.010;
/// A hit is cut this long before its onset, so the attack is not clipped.
const PRE_ROLL: f64 = 0.002;
/// The longest a hit is cut to.
const MAX_CUT: f64 = 1.5;
/// A hit ends when its level has fallen this far below its peak.
const FALL_DB: f32 = 50.0;
/// The length of the audio the features describe.
const WINDOW: f64 = 0.08;
/// The audio the pitch is looked for in.
const PITCH_WINDOW: f64 = 0.15;
/// The envelope's block, in seconds.
const BLOCK: f64 = 0.002;
/// A hit with another onset this soon after it is mixed with it.
const NEXT_MIXES: f64 = 0.03;
/// A hit is mixed with what came before it when, at its start, that was still
/// this close (in decibels) to the hit's own peak.
const RING_DB: f32 = 25.0;
/// A hit cut off by the next one, with the sound still this close to its peak,
/// is truncated.
const TRUNCATED_DB: f32 = 30.0;
/// A hit quieter than this (dB relative to full scale, peak of a 2 ms block) is
/// too quiet to be a good sample.
const QUIET_DB: f32 = -45.0;
const BANDS: usize = 24;

/// The analysis was cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// What a hit is described by. All of it is independent of how loud the hit is.
#[derive(Clone, Debug, PartialEq)]
pub struct HitFeatures {
    /// The log-mel spectrum of the first 80 ms, in decibels below its loudest
    /// band (so 0 is the loudest, negative below).
    pub bands: [f32; BANDS],
    /// The spectral centroid, in Hz.
    pub centroid: f32,
    /// How noise-like the spectrum is: 0 for a tone, 1 for flat noise.
    pub flatness: f32,
    /// The share of the energy below 150 Hz, from 150 Hz to 5 kHz, and above
    /// 5 kHz. They add up to 1.
    pub split: [f32; 3],
    /// Seconds from the peak until the level is 30 dB down (the length of the
    /// cut if it never gets there).
    pub decay: f32,
    /// 0 to 1: how much of the start of the hit has a clear pitch.
    pub pitched: f32,
    /// The key number of that pitch (fractional; 69 is A 440).
    pub pitch_key: Option<f32>,
    /// How many separate bursts the first 40 ms has (a clap has several).
    pub attack_peaks: u8,
}

/// One drum hit found in a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The frame of the onset, in frames of the analysed audio.
    pub onset: u64,
    /// The frames to cut, `start` (a little before the onset) to `end`.
    pub start: u64,
    pub end: u64,
    /// The cut ends where the next hit begins, with this one still sounding:
    /// a ringing crash cut by the next hat is a poor sample.
    pub truncated: bool,
    /// Another drum is sounding with this one: another onset so close after it,
    /// or the sound of the one before still strong at its start.
    pub mixed: bool,
    /// The peak level, in decibels relative to full scale.
    pub level_db: f32,
    pub features: HitFeatures,
}

impl Hit {
    fn too_quiet(&self) -> bool {
        self.level_db < QUIET_DB
    }
}

/// The frames at which hits start in `mono`: the full-band, bass and treble
/// onsets merged, so a hat over a kick is its own onset.
fn find_onsets(mono: &[f32], rate: u32) -> Vec<u64> {
    let mut all = detect_onsets_with(mono, rate, SENSITIVITY);
    all.extend(detect_bass_onsets_with(mono, rate, SENSITIVITY));
    all.extend(detect_treble_onsets_with(mono, rate, SENSITIVITY));
    all.sort_unstable();
    let gap = (PIN_MERGE * f64::from(rate)) as u64;
    let mut merged: Vec<u64> = Vec::with_capacity(all.len());
    for frame in all {
        if merged.last().is_none_or(|&last| frame >= last + gap) {
            merged.push(frame);
        }
    }
    merged
}

fn db(power: f32) -> f32 {
    10.0 * (power + 1e-12).log10()
}

/// The level of each block of `x`, in decibels (mean power).
fn envelope(x: &[f32], block: usize) -> Vec<f32> {
    x.chunks(block)
        .map(|c| db(c.iter().map(|s| s * s).sum::<f32>() / block as f32))
        .collect()
}

/// How many separate bursts there are in an envelope: peaks within 15 dB of
/// the loudest, at least 3 dB above the dip between them.
fn count_peaks(env: &[f32]) -> u8 {
    let Some(top) = env.iter().copied().reduce(f32::max) else {
        return 0;
    };
    let mut count = 0u8;
    let mut peak = f32::MIN;
    let mut valley = f32::MAX;
    let mut counted = false;
    for &level in env {
        if level > peak {
            peak = level;
            valley = level;
            continue;
        }
        valley = valley.min(level);
        if !counted && peak - valley >= 3.0 && peak >= top - 15.0 {
            count += 1;
            counted = true;
        }
        if counted && level - valley >= 3.0 {
            // Rising again: a new burst.
            peak = level;
            valley = level;
            counted = false;
        }
    }
    if !counted && peak >= top - 15.0 {
        count += 1;
    }
    count
}

struct Spectrum {
    planner: RealFftPlanner<f32>,
    rate: u32,
    size: usize,
    window: Vec<f32>,
    /// For each band, `(first bin, weights)`.
    filters: Vec<(usize, Vec<f32>)>,
}

impl Spectrum {
    fn new(rate: u32) -> Self {
        let size = ((WINDOW * f64::from(rate)) as usize).next_power_of_two();
        let window = (0..size)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos())
            .collect();
        let hz_per_bin = rate as f32 / size as f32;
        let mel = |hz: f32| 2595.0 * (1.0 + hz / 700.0).log10();
        let unmel = |m: f32| 700.0 * (10f32.powf(m / 2595.0) - 1.0);
        let (lo, hi) = (40.0f32, (0.45 * rate as f32).min(16_000.0));
        let edges: Vec<f32> = (0..BANDS + 2)
            .map(|i| unmel(mel(lo) + (mel(hi) - mel(lo)) * i as f32 / (BANDS + 1) as f32))
            .collect();
        let filters = (0..BANDS)
            .map(|b| {
                let (a, c, d) = (edges[b], edges[b + 1], edges[b + 2]);
                let first = (a / hz_per_bin).floor() as usize;
                let last = ((d / hz_per_bin).ceil() as usize).min(size / 2);
                let weights = (first..=last)
                    .map(|k| {
                        let f = k as f32 * hz_per_bin;
                        if f <= a || f >= d {
                            0.0
                        } else if f < c {
                            (f - a) / (c - a)
                        } else {
                            (d - f) / (d - c)
                        }
                    })
                    .collect();
                (first, weights)
            })
            .collect();
        Self {
            planner: RealFftPlanner::new(),
            rate,
            size,
            window,
            filters,
        }
    }

    /// The bands, centroid, flatness and split of `x` (zero-padded or cut to
    /// the window).
    fn of(&mut self, x: &[f32]) -> ([f32; BANDS], f32, f32, [f32; 3]) {
        let fft = self.planner.plan_fft_forward(self.size);
        let mut input = vec![0.0f32; self.size];
        for (i, v) in input.iter_mut().enumerate() {
            *v = x.get(i).copied().unwrap_or(0.0) * self.window[i];
        }
        let mut out = fft.make_output_vec();
        fft.process(&mut input, &mut out).ok();
        let power: Vec<f32> = out.iter().map(|c| c.norm_sqr()).collect();
        let hz = self.rate as f32 / self.size as f32;
        let total: f32 = power.iter().sum::<f32>().max(1e-20);
        let centroid = power
            .iter()
            .enumerate()
            .map(|(k, p)| k as f32 * hz * p)
            .sum::<f32>()
            / total;
        let mut split = [0.0f32; 3];
        for (k, p) in power.iter().enumerate() {
            let f = k as f32 * hz;
            split[if f < 150.0 {
                0
            } else if f < 5_000.0 {
                1
            } else {
                2
            }] += p / total;
        }
        let band_power: Vec<f32> = self
            .filters
            .iter()
            .map(|(first, w)| {
                w.iter()
                    .enumerate()
                    .map(|(i, w)| w * power.get(first + i).copied().unwrap_or(0.0))
                    .sum()
            })
            .collect();
        let mean = band_power.iter().sum::<f32>() / BANDS as f32;
        let log_mean = band_power.iter().map(|p| (p + 1e-20).ln()).sum::<f32>() / BANDS as f32;
        let flatness = if mean > 1e-18 {
            (log_mean.exp() / mean).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mut bands = [0.0f32; BANDS];
        for (b, p) in band_power.iter().enumerate() {
            bands[b] = db(*p);
        }
        let top = bands.iter().copied().fold(f32::MIN, f32::max);
        for b in &mut bands {
            *b = (*b - top).max(-60.0);
        }
        (bands, centroid, flatness, split)
    }
}

/// The pitch of the start of a hit: how much of it is pitched, and the median
/// key of the pitched part.
fn pitch_of(x: &[f32], rate: u32) -> (f32, Option<f32>) {
    let mut tracker = PitchTracker::new(rate, 0.3);
    let n = (PITCH_WINDOW * f64::from(rate)) as usize;
    let mut frames = 0usize;
    let mut keys: Vec<f32> = Vec::new();
    for &s in x.iter().take(n) {
        if let Some(frame) = tracker.push(s) {
            frames += 1;
            if let Some(key) = frame.key.filter(|_| frame.confidence > 0.7) {
                keys.push(key);
            }
        }
    }
    if frames == 0 || keys.is_empty() {
        return (0.0, None);
    }
    keys.sort_by(|a, b| a.total_cmp(b));
    (
        keys.len() as f32 / frames as f32,
        Some(keys[keys.len() / 2]),
    )
}

/// Finds the hits in `mono` (at `rate`), cuts each and describes it.
///
/// `progress` is called with how far along it is (0 to 1) every so often and
/// returns `false` to cancel, which gives `Err(Cancelled)`.
pub fn analyse_hits(
    mono: &[f32],
    rate: u32,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<Vec<Hit>, Cancelled> {
    if !progress(0.0) {
        return Err(Cancelled);
    }
    let onsets = find_onsets(mono, rate);
    if !progress(0.2) {
        return Err(Cancelled);
    }
    let rate_f = f64::from(rate);
    let block = ((BLOCK * rate_f) as usize).max(1);
    let mut spectrum = Spectrum::new(rate);
    let mut hits = Vec::with_capacity(onsets.len());
    for (i, &onset) in onsets.iter().enumerate() {
        if !progress(0.2 + 0.8 * i as f32 / onsets.len().max(1) as f32) {
            return Err(Cancelled);
        }
        let next = onsets.get(i + 1).copied();
        let on = onset as usize;
        let pre = (PRE_ROLL * rate_f) as usize;
        let start = on.saturating_sub(pre);
        let cap = on + (MAX_CUT * rate_f) as usize;
        let next_start = next.map(|n| (n as usize).saturating_sub(pre));
        let limit = cap.min(mono.len()).min(next_start.unwrap_or(usize::MAX));
        if limit <= on {
            continue;
        }
        let env = envelope(&mono[on..limit], block);
        let peak_blocks = env.len().min((0.05 / BLOCK) as usize);
        let peak_at = (0..peak_blocks)
            .max_by(|&a, &b| env[a].total_cmp(&env[b]))
            .unwrap_or(0);
        let peak = env[peak_at];
        let fell = (peak_at + 1..env.len()).find(|&b| env[b] < peak - FALL_DB);
        let (end, by_next) = match fell {
            Some(b) => (on + (b + 1) * block, false),
            None => (limit, next_start == Some(limit) && limit < cap),
        };
        let end = end.min(limit);
        let level_at_end = env[((end - on).saturating_sub(1) / block).min(env.len() - 1)];
        let truncated = by_next && level_at_end > peak - TRUNCATED_DB;
        let before = &mono[on.saturating_sub(3 * block)..on.saturating_sub(block)];
        let ring = if before.is_empty() {
            f32::MIN
        } else {
            db(before.iter().map(|s| s * s).sum::<f32>() / before.len() as f32)
        };
        let mixed = ring > peak - RING_DB
            || next.is_some_and(|n| (n - onset) as f64 <= NEXT_MIXES * rate_f);
        let (bands, centroid, flatness, split) = spectrum.of(&mono[on..end]);
        let decay_blocks = (peak_at + 1..env.len())
            .find(|&b| env[b] < peak - 30.0)
            .unwrap_or(env.len());
        let (pitched, pitch_key) = pitch_of(&mono[on..end], rate);
        let attack = &env[..env.len().min((0.04 / BLOCK) as usize)];
        hits.push(Hit {
            onset,
            start: start as u64,
            end: end as u64,
            truncated,
            mixed,
            level_db: peak,
            features: HitFeatures {
                bands,
                centroid,
                flatness,
                split,
                decay: (decay_blocks - peak_at) as f32 * BLOCK as f32,
                pitched,
                pitch_key,
                attack_peaks: count_peaks(attack),
            },
        });
    }
    progress(1.0);
    Ok(hits)
}

/// How far apart two hits sound, about 1 for two different drums and under
/// 0.4 for two hits of one. Level does not count.
fn distance(a: &HitFeatures, b: &HitFeatures) -> f32 {
    let bands = a
        .bands
        .iter()
        .zip(&b.bands)
        .map(|(x, y)| ((x - y) / 12.0).powi(2))
        .sum::<f32>()
        / BANDS as f32;
    let centroid = ((a.centroid.max(20.0) / b.centroid.max(20.0)).log2() / 1.0).powi(2);
    let split = a
        .split
        .iter()
        .zip(&b.split)
        .map(|(x, y)| ((x - y) / 0.3).powi(2))
        .sum::<f32>()
        / 3.0;
    let decay = ((a.decay.max(0.002) / b.decay.max(0.002)).log2() / 1.5).powi(2);
    let tone = ((a.pitched - b.pitched) / 0.7).powi(2) + ((a.flatness - b.flatness) / 0.3).powi(2);
    // Scaled so that two hits of one drum are well under 0.5 apart and two different drums over 1.
    0.35 * (bands + centroid + split + decay + tone / 2.0).sqrt()
}

/// The default of the merge slider (0 to 1, more merging towards 1).
pub const DEFAULT_MERGE: f32 = 0.5;

/// The distance under which two hits are of one bin, at a merge amount.
fn threshold(merge: f32) -> f32 {
    0.2 * 10f32.powf(merge.clamp(0.0, 1.0))
}

/// What a bin of hits is guessed to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DrumKind {
    Kick,
    Snare,
    ClosedHat,
    OpenHat,
    Clap,
    LowTom,
    HighTom,
    Crash,
    Unknown,
}

impl DrumKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Kick => "Kick",
            Self::Snare => "Snare",
            Self::ClosedHat => "Closed hat",
            Self::OpenHat => "Open hat",
            Self::Clap => "Clap",
            Self::LowTom => "Low tom",
            Self::HighTom => "High tom",
            Self::Crash => "Crash",
            Self::Unknown => "Unknown",
        }
    }
}

/// 0 at or below `lo`, 1 at or above `hi`.
fn up(x: f32, lo: f32, hi: f32) -> f32 {
    ((x - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// 1 at or below `lo`, 0 at or above `hi`.
fn down(x: f32, lo: f32, hi: f32) -> f32 {
    1.0 - up(x, lo, hi)
}

/// The best guess at what a hit is, and how sure (0 to 1). Under
/// [`CONFIDENT`] the guess is [`DrumKind::Unknown`].
fn guess(f: &HitFeatures) -> (DrumKind, f32) {
    let [low, mid, high] = f.split;
    let tonal = f.pitched > 0.5;
    let pitch = f.pitch_key.unwrap_or(0.0);
    let scores = [
        (
            DrumKind::Kick,
            up(low, 0.3, 0.6)
                * down(f.centroid, 500.0, 1_200.0)
                * down(f.decay, 0.4, 0.9)
                * if tonal { down(pitch, 40.0, 46.0) } else { 1.0 },
        ),
        (
            DrumKind::Snare,
            up(mid, 0.3, 0.6)
                * down(high, 0.4, 0.7)
                * up(f.flatness, 0.02, 0.15)
                * up(f.centroid, 800.0, 2_000.0)
                * down(f.centroid, 6_000.0, 9_000.0)
                * down(f.decay, 0.3, 0.6)
                * if f.attack_peaks >= 3 { 0.4 } else { 1.0 },
        ),
        (
            DrumKind::Clap,
            up(mid, 0.3, 0.6)
                * down(high, 0.4, 0.7)
                * up(f32::from(f.attack_peaks), 1.0, 3.0)
                * up(f.flatness, 0.02, 0.15)
                * up(f.centroid, 800.0, 2_000.0),
        ),
        (
            DrumKind::ClosedHat,
            up(high, 0.3, 0.6) * up(f.centroid, 3_000.0, 6_000.0) * down(f.decay, 0.15, 0.3),
        ),
        (
            DrumKind::OpenHat,
            up(high, 0.3, 0.6)
                * up(f.centroid, 3_000.0, 6_000.0)
                * up(f.decay, 0.15, 0.3)
                * down(f.decay, 0.6, 1.0),
        ),
        (
            DrumKind::Crash,
            up(high, 0.3, 0.6) * up(f.centroid, 2_500.0, 5_000.0) * up(f.decay, 0.6, 1.0),
        ),
        (
            DrumKind::LowTom,
            if tonal {
                up(f.pitched, 0.5, 0.8)
                    * up(pitch, 36.0, 42.0)
                    * down(pitch, 49.0, 55.0)
                    * down(high, 0.1, 0.4)
            } else {
                0.0
            },
        ),
        (
            DrumKind::HighTom,
            if tonal {
                up(f.pitched, 0.5, 0.8)
                    * up(pitch, 49.0, 55.0)
                    * down(pitch, 62.0, 70.0)
                    * down(high, 0.1, 0.4)
            } else {
                0.0
            },
        ),
    ];
    let mut best = (DrumKind::Unknown, 0.0f32);
    let mut second = 0.0f32;
    for (kind, score) in scores {
        if score > best.1 {
            second = best.1;
            best = (kind, score);
        } else if score > second {
            second = score;
        }
    }
    // A guess that two kinds nearly tie on is not confident.
    let confidence = (best.1 - 0.5 * second).clamp(0.0, 1.0);
    if confidence < CONFIDENT {
        (DrumKind::Unknown, confidence)
    } else {
        (best.0, confidence)
    }
}

/// The confidence a guess needs, or the bin is labelled Unknown.
pub const CONFIDENT: f32 = 0.4;

/// A group of hits that sound alike.
#[derive(Clone, Debug, PartialEq)]
pub struct DrumBin {
    /// Indices into the hits, best default first: not truncated, not mixed, not
    /// too quiet, then closest to the medoid.
    pub members: Vec<usize>,
    /// The member nearest the rest.
    pub medoid: usize,
    pub kind: DrumKind,
    /// 0 to 1.
    pub confidence: f32,
}

/// The result of binning.
#[derive(Clone, Debug, PartialEq)]
pub struct Binning {
    /// The bins, in the order their first hit sounds.
    pub bins: Vec<DrumBin>,
    /// Hits that fit no bin well enough to be one: a quiet or truncated hit
    /// alone, or a mixed one nothing explains. Shown collapsed as "Other".
    pub other: Vec<usize>,
    /// For each hit, whether another drum sounds with it: flagged when found
    /// ([`Hit::mixed`]) or when the sum of two bins' medoids explains it.
    pub mixed: Vec<bool>,
}

fn medoid_of(hits: &[Hit], members: &[usize]) -> usize {
    *members
        .iter()
        .min_by(|&&a, &&b| {
            let cost = |i: usize| -> f32 {
                members
                    .iter()
                    .map(|&j| distance(&hits[i].features, &hits[j].features))
                    .sum()
            };
            cost(a).total_cmp(&cost(b))
        })
        .expect("a bin has members")
}

/// A power spectrum from decibels below the loudest band.
fn power_of(bands: &[f32; BANDS]) -> [f32; BANDS] {
    bands.map(|b| 10f32.powf(b / 10.0))
}

/// True when the sum of two other bins' medoids (the second at 0, -6 or -12 dB)
/// explains `f` better than any one of them: a hat over a kick.
fn explained_by_two(f: &HitFeatures, medoids: &[&HitFeatures], limit: f32) -> bool {
    let band_distance = |p: &[f32; BANDS]| -> f32 {
        let top = p.iter().copied().fold(0.0f32, f32::max).max(1e-20);
        let mut sum = 0.0;
        for (b, &pb) in p.iter().enumerate() {
            sum += ((db(pb / top) - f.bands[b]).max(-60.0) / 12.0).powi(2);
        }
        (sum / BANDS as f32).sqrt()
    };
    let one = medoids
        .iter()
        .map(|m| distance(f, m))
        .fold(f32::MAX, f32::min);
    if one < limit * 0.5 {
        return false;
    }
    let mut best_two = f32::MAX;
    for (i, a) in medoids.iter().enumerate() {
        for b in medoids.iter().skip(i + 1) {
            let (pa, pb) = (power_of(&a.bands), power_of(&b.bands));
            for gain in [1.0f32, 0.25, 0.06] {
                for flip in [false, true] {
                    let mut sum = [0.0f32; BANDS];
                    for k in 0..BANDS {
                        sum[k] = if flip {
                            pa[k] * gain + pb[k]
                        } else {
                            pa[k] + pb[k] * gain
                        };
                    }
                    best_two = best_two.min(band_distance(&sum));
                }
            }
        }
    }
    // Compare band shape alone against the single medoids' band shape.
    let one_bands = medoids
        .iter()
        .map(|m| band_distance(&power_of(&m.bands)))
        .fold(f32::MAX, f32::min);
    best_two < 0.7 * one_bands && best_two < limit
}

/// Makes a bin of `members`: finds its medoid, ranks the members best default
/// first (not mixed, not truncated, not too quiet, then closest to the medoid)
/// and guesses what it is. `mixed` is [`Binning::mixed`]. `members` must not be
/// empty.
pub fn make_bin(hits: &[Hit], mixed: &[bool], mut members: Vec<usize>) -> DrumBin {
    let medoid = medoid_of(hits, &members);
    members.sort_by(|&x, &y| {
        let key = |i: usize| (mixed[i], hits[i].truncated, hits[i].too_quiet());
        key(x).cmp(&key(y)).then(
            distance(&hits[x].features, &hits[medoid].features)
                .total_cmp(&distance(&hits[y].features, &hits[medoid].features)),
        )
    });
    let (kind, confidence) = guess(&hits[medoid].features);
    DrumBin {
        members,
        medoid,
        kind,
        confidence,
    }
}

/// Splits the bin of `members` in two or more by tightening the threshold from
/// `merge` until it falls apart. `None` when the hits are too alike to
/// separate (even at the tightest threshold they are one group).
pub fn split_bin(
    hits: &[Hit],
    mixed: &[bool],
    members: &[usize],
    merge: f32,
) -> Option<Vec<DrumBin>> {
    let subset: Vec<Hit> = members.iter().map(|&m| hits[m].clone()).collect();
    let mut at = merge;
    while at > 0.0 {
        at = (at - 0.05).max(0.0);
        let parts = bin_hits(&subset, at);
        let total: usize = parts.bins.len() + usize::from(!parts.other.is_empty());
        if total >= 2 {
            let lift =
                |local: &[usize]| -> Vec<usize> { local.iter().map(|&i| members[i]).collect() };
            let mut bins: Vec<DrumBin> = parts
                .bins
                .iter()
                .map(|b| make_bin(hits, mixed, lift(&b.members)))
                .collect();
            if !parts.other.is_empty() {
                bins.push(make_bin(hits, mixed, lift(&parts.other)));
            }
            return Some(bins);
        }
    }
    None
}

/// Groups hits into bins. `merge` (0 to 1, [`DEFAULT_MERGE`]) is how readily
/// different sounds are put together.
///
/// Greedy leader clustering of the clean hits in order, then one pass
/// moving every hit to the nearest bin's medoid; mixed hits join a bin they
/// fit but never start one.
pub fn bin_hits(hits: &[Hit], merge: f32) -> Binning {
    let limit = threshold(merge);
    let near = |f: &HitFeatures, leaders: &[usize]| -> Option<(usize, f32)> {
        leaders
            .iter()
            .enumerate()
            .map(|(b, &h)| (b, distance(f, &hits[h].features)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    };
    // 1. Leaders, from the hits that are not mixed.
    let mut leaders: Vec<usize> = Vec::new();
    for (i, hit) in hits.iter().enumerate() {
        if hit.mixed {
            continue;
        }
        if !near(&hit.features, &leaders).is_some_and(|(_, d)| d <= limit) {
            leaders.push(i);
        }
    }
    // 2. Assign everything to its nearest leader within the threshold, take
    // each bin's medoid, and move every hit to its nearest medoid.
    let assign = |leaders: &[usize]| -> Vec<Option<usize>> {
        hits.iter()
            .map(|h| {
                near(&h.features, leaders)
                    .filter(|&(_, d)| d <= limit)
                    .map(|(b, _)| b)
            })
            .collect()
    };
    let mut membership = assign(&leaders);
    let medoids: Vec<usize> = (0..leaders.len())
        .map(|b| {
            let members: Vec<usize> = (0..hits.len())
                .filter(|&i| membership[i] == Some(b))
                .collect();
            medoid_of(hits, &members)
        })
        .collect();
    membership = assign(&medoids);
    // Medoids may now have changed; keep the ones that still have members.
    let medoids: Vec<usize> = (0..medoids.len())
        .filter(|&b| membership.contains(&Some(b)))
        .map(|b| {
            let members: Vec<usize> = (0..hits.len())
                .filter(|&i| membership[i] == Some(b))
                .collect();
            medoid_of(hits, &members)
        })
        .collect();
    let membership = assign(&medoids);
    // 3. A hit a sum of two other bins' medoids explains is mixed.
    let mut mixed: Vec<bool> = hits.iter().map(|h| h.mixed).collect();
    for (i, hit) in hits.iter().enumerate() {
        if mixed[i] {
            continue;
        }
        let others: Vec<&HitFeatures> = medoids
            .iter()
            .filter(|&&m| m != i)
            .map(|&m| &hits[m].features)
            .collect();
        if others.len() >= 2 && explained_by_two(&hit.features, &others, limit) {
            mixed[i] = true;
        }
    }
    // 4. Build the bins; Other takes the quiet or truncated singletons and the
    // mixed hits that fit nothing.
    let mut bins: Vec<DrumBin> = Vec::new();
    let mut other: Vec<usize> = Vec::new();
    for b in 0..medoids.len() {
        let members: Vec<usize> = (0..hits.len())
            .filter(|&i| membership[i] == Some(b))
            .collect();
        // Identical hits can tie, leaving a bin empty or without its medoid.
        let Some(&first) = members.first() else {
            continue;
        };
        if members.len() == 1 && (hits[first].too_quiet() || hits[first].truncated || mixed[first])
        {
            other.push(first);
            continue;
        }
        bins.push(make_bin(hits, &mixed, members));
    }
    other.extend((0..hits.len()).filter(|&i| membership[i].is_none()));
    other.sort_unstable();
    bins.sort_by_key(|b| b.members.iter().min().copied());
    Binning { bins, other, mixed }
}

/// What a cached analysis is of: the source file (its hash), the window of it
/// (in frames) and the detector's version.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HitKey {
    pub file: String,
    pub start: u64,
    pub end: u64,
    pub version: u32,
}

impl HitKey {
    pub fn new(file: impl Into<String>, start: u64, end: u64) -> Self {
        Self {
            file: file.into(),
            start,
            end,
            version: DETECTOR_VERSION,
        }
    }
}

/// Analyses kept by what they are of, so reopening the dialog is instant.
#[derive(Default)]
pub struct HitCache {
    found: Mutex<HashMap<HitKey, Arc<[Hit]>>>,
}

impl HitCache {
    pub fn get(&self, key: &HitKey) -> Option<Arc<[Hit]>> {
        self.found.lock().unwrap().get(key).cloned()
    }

    pub fn insert(&self, key: HitKey, hits: Vec<Hit>) -> Arc<[Hit]> {
        let hits: Arc<[Hit]> = hits.into();
        self.found.lock().unwrap().insert(key, hits.clone());
        hits
    }

    /// The kept analysis for `key`, or `analyse` run and kept. A cancelled
    /// analysis keeps nothing.
    pub fn get_or_analyse(
        &self,
        key: HitKey,
        analyse: impl FnOnce() -> Result<Vec<Hit>, Cancelled>,
    ) -> Result<Arc<[Hit]>, Cancelled> {
        if let Some(found) = self.get(&key) {
            return Ok(found);
        }
        Ok(self.insert(key, analyse()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: u32 = 48_000;

    /// Deterministic white noise, -1 to 1.
    struct Noise(u32);
    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        }
    }

    fn seconds(s: f32) -> usize {
        (s * RATE as f32) as usize
    }

    fn highpassed(x: &mut [f32], hz: f32) {
        let a = 1.0 - (-TAU * hz / RATE as f32).exp();
        let mut low = 0.0;
        for v in x.iter_mut() {
            low += a * (*v - low);
            *v -= low;
        }
    }

    fn lowpassed(x: &mut [f32], hz: f32) {
        let a = 1.0 - (-TAU * hz / RATE as f32).exp();
        let mut low = 0.0;
        for v in x.iter_mut() {
            low += a * (*v - low);
            *v = low;
        }
    }

    fn kick() -> Vec<f32> {
        let mut phase = 0.0f32;
        (0..seconds(0.25))
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let hz = 45.0 + 110.0 * (-t * 40.0).exp();
                phase += TAU * hz / RATE as f32;
                0.8 * phase.sin() * (-t * 14.0).exp()
            })
            .collect()
    }

    fn snare() -> Vec<f32> {
        let mut noise = Noise(7);
        let mut body: Vec<f32> = (0..seconds(0.2)).map(|_| noise.next()).collect();
        highpassed(&mut body, 1_200.0);
        lowpassed(&mut body, 4_000.0);
        lowpassed(&mut body, 4_000.0);
        body.iter()
            .enumerate()
            .map(|(i, n)| {
                let t = i as f32 / RATE as f32;
                1.6 * n * (-t * 22.0).exp() + 0.35 * (TAU * 190.0 * t).sin() * (-t * 30.0).exp()
            })
            .collect()
    }

    fn hat(decay: f32, length: f32) -> Vec<f32> {
        let mut noise = Noise(99);
        let mut x: Vec<f32> = (0..seconds(length)).map(|_| noise.next()).collect();
        highpassed(&mut x, 6_000.0);
        highpassed(&mut x, 6_000.0);
        x.iter()
            .enumerate()
            .map(|(i, n)| 0.5 * n * (-(i as f32 / RATE as f32) * decay).exp())
            .collect()
    }

    fn tom(hz: f32) -> Vec<f32> {
        (0..seconds(0.4))
            .map(|i| {
                let t = i as f32 / RATE as f32;
                0.7 * (TAU * hz * t).sin() * (-t * 9.0).exp()
            })
            .collect()
    }

    fn clap() -> Vec<f32> {
        let mut noise = Noise(3);
        let mut x = vec![0.0f32; seconds(0.25)];
        for burst in 0..3 {
            let at = seconds(0.011) * burst;
            for i in 0..seconds(0.01) {
                x[at + i] += 0.6 * noise.next() * (1.0 - i as f32 / seconds(0.01) as f32);
            }
        }
        let mut tail: Vec<f32> = (0..seconds(0.25) - seconds(0.033))
            .map(|_| noise.next())
            .collect();
        for (i, v) in tail.iter_mut().enumerate() {
            *v *= 0.5 * (-(i as f32 / RATE as f32) * 25.0).exp();
        }
        for (i, v) in tail.iter().enumerate() {
            x[seconds(0.033) + i] += v;
        }
        // Noise with a band around 1 to 2 kHz.
        highpassed(&mut x, 900.0);
        let a = 1.0 - (-TAU * 3_500.0 / RATE as f32).exp();
        let mut low = 0.0;
        for v in x.iter_mut() {
            low += a * (*v - low);
            *v = low * 2.0;
        }
        x
    }

    /// Lays `hits` (source, start in seconds, gain) into a clip.
    fn track(length: f32, hits: &[(&[f32], f32, f32)]) -> Vec<f32> {
        let mut out = vec![0.0f32; seconds(length)];
        for (sound, at, gain) in hits {
            let a = seconds(*at);
            for (i, s) in sound.iter().enumerate() {
                if let Some(o) = out.get_mut(a + i) {
                    *o += s * gain;
                }
            }
        }
        out
    }

    fn run(x: &[f32]) -> Vec<Hit> {
        analyse_hits(x, RATE, &mut |_| true).unwrap()
    }

    /// A kit played as kick, hat, snare, hat, with level variation.
    fn pattern() -> (Vec<f32>, Vec<&'static str>) {
        let (k, s, h) = (kick(), snare(), hat(60.0, 0.1));
        let mut hits: Vec<(&[f32], f32, f32)> = Vec::new();
        let mut names = Vec::new();
        for bar in 0..4 {
            let t = 0.25 + bar as f32 * 2.0;
            let gain = [1.0, 0.6, 0.85, 0.7][bar];
            hits.push((&k, t, gain));
            names.push("kick");
            hits.push((&h, t + 0.5, gain));
            names.push("hat");
            hits.push((&s, t + 1.0, gain));
            names.push("snare");
            hits.push((&h, t + 1.5, 1.0 - 0.3 * gain));
            names.push("hat");
        }
        // The vectors borrow locals; leak them for the 'static sounds.
        let out = track(8.0, &hits);
        (out, names)
    }

    #[test]
    fn a_kit_comes_out_as_one_bin_per_drum_with_every_hit_in_the_right_one() {
        let (x, names) = pattern();
        let hits = run(&x);
        assert_eq!(
            hits.len(),
            names.len(),
            "hits found: {:?}",
            hits.iter()
                .map(|h| h.onset as f32 / 48000.0)
                .collect::<Vec<_>>()
        );
        let binning = bin_hits(&hits, DEFAULT_MERGE);
        assert_eq!(binning.bins.len(), 3, "{binning:#?}");
        assert!(binning.other.is_empty());
        let mut sizes: Vec<usize> = binning.bins.iter().map(|b| b.members.len()).collect();
        sizes.sort_unstable();
        assert_eq!(sizes, [4, 4, 8]);
        for bin in &binning.bins {
            let first = names[bin.members[0]];
            assert!(
                bin.members.iter().all(|&m| names[m] == first),
                "{names:?} {bin:?}"
            );
        }
    }

    #[test]
    fn the_slider_splits_and_merges_without_new_analysis() {
        let (x, _) = pattern();
        let hits = run(&x);
        let split = bin_hits(&hits, 0.0).bins.len();
        let merged = bin_hits(&hits, 1.0).bins.len();
        assert!(split >= 3, "{split}");
        assert!(merged < 3, "{merged}");
    }

    #[test]
    fn each_drum_of_the_kit_gets_its_label() {
        let (k, s, h, o) = (kick(), snare(), hat(60.0, 0.1), hat(7.0, 0.8));
        let (lo, hi, c) = (tom(110.0), tom(220.0), clap());
        let kit: [(&[f32], DrumKind); 7] = [
            (&k, DrumKind::Kick),
            (&s, DrumKind::Snare),
            (&h, DrumKind::ClosedHat),
            (&o, DrumKind::OpenHat),
            (&c, DrumKind::Clap),
            (&lo, DrumKind::LowTom),
            (&hi, DrumKind::HighTom),
        ];
        for (sound, want) in kit {
            let x = track(2.0, &[(sound, 0.3, 1.0), (sound, 1.2, 0.8)]);
            let hits = run(&x);
            assert_eq!(hits.len(), 2, "{want:?}");
            let bin = &bin_hits(&hits, DEFAULT_MERGE).bins[0];
            assert_eq!(
                bin.kind, want,
                "confidence {} {:?}",
                bin.confidence, hits[0].features
            );
        }
    }

    #[test]
    fn an_odd_sound_is_unknown() {
        let buzz: Vec<f32> = (0..seconds(0.3))
            .map(|i| {
                let t = i as f32 / RATE as f32;
                0.5 * (TAU * 2_500.0 * t).sin() * (-t * 8.0).exp()
            })
            .collect();
        let x = track(2.0, &[(&buzz, 0.3, 1.0), (&buzz, 1.2, 1.0)]);
        let bin = &bin_hits(&run(&x), DEFAULT_MERGE).bins[0];
        assert_eq!(bin.kind, DrumKind::Unknown, "{}", bin.confidence);
    }

    #[test]
    fn a_hat_over_a_kick_is_mixed_and_never_the_first_pick() {
        let (k, h, s) = (kick(), hat(60.0, 0.1), snare());
        let x = track(
            6.0,
            &[
                (&k, 0.5, 1.0),
                (&h, 1.0, 0.8),
                (&k, 1.5, 1.0),
                (&h, 1.5, 6.0), // over the kick, loud enough to hear in the spectrum
                (&k, 2.5, 1.0),
                (&h, 3.0, 0.8),
                (&s, 3.5, 1.0),
                (&h, 4.0, 0.8),
                (&k, 4.5, 1.0),
                (&s, 5.0, 1.0),
                (&h, 5.5, 0.8),
                (&k, 5.0, 1.0),
            ],
        );
        let hits = run(&x);
        let binning = bin_hits(&hits, DEFAULT_MERGE);
        let doubled = hits
            .iter()
            .position(|h| h.onset.abs_diff(seconds(1.5) as u64) < seconds(0.02) as u64)
            .unwrap();
        assert!(binning.mixed[doubled], "{:?}", hits[doubled].features);
        let kick_bin = binning
            .bins
            .iter()
            .find(|b| b.kind == DrumKind::Kick)
            .expect("a kick bin");
        assert!(!binning.mixed[kick_bin.members[0]]);
        assert_ne!(kick_bin.members[0], doubled);
        // Only the doubled hit and the kick under the snare are mixed.
        assert!(binning.mixed.iter().filter(|&&m| m).count() <= 3);
    }

    #[test]
    fn a_hit_cut_by_the_next_one_is_truncated() {
        let o = hat(7.0, 0.8);
        let h = hat(60.0, 0.1);
        // An open hat 0.1 s before a closed one, and another that rings out.
        let x = track(3.0, &[(&o, 0.5, 1.0), (&h, 0.6, 1.0), (&o, 1.5, 1.0)]);
        let hits = run(&x);
        assert_eq!(hits.len(), 3);
        assert!(hits[0].truncated);
        assert!(!hits[2].truncated);
        assert!(hits[0].end <= hits[1].onset);
        // The closed hat starts over the open one's ring; the others are clean.
        assert!(hits[1].mixed);
        assert!(!hits[0].mixed && !hits[2].mixed);
    }

    #[test]
    fn a_silent_clip_has_no_hits_and_one_hit_makes_one_bin() {
        assert!(run(&vec![0.0; seconds(3.0)]).is_empty());
        assert_eq!(bin_hits(&[], DEFAULT_MERGE).bins.len(), 0);
        let k = kick();
        let x = track(1.0, &[(&k, 0.3, 1.0)]);
        let binning = bin_hits(&run(&x), DEFAULT_MERGE);
        assert_eq!(binning.bins.len(), 1);
        assert_eq!(binning.bins[0].members.len(), 1);
    }

    #[test]
    fn cancelling_stops_a_long_analysis() {
        let k = kick();
        let hits: Vec<(&[f32], f32, f32)> = (0..200)
            .map(|i| (&k[..], 0.5 * i as f32 + 0.1, 1.0))
            .collect();
        let x = track(101.0, &hits);
        let mut calls = 0;
        let result = analyse_hits(&x, RATE, &mut |_| {
            calls += 1;
            calls < 20
        });
        assert_eq!(result, Err(Cancelled));
        assert_eq!(calls, 20);
    }

    #[test]
    fn a_bin_of_two_drums_splits_and_one_of_one_does_not() {
        let (x, names) = pattern();
        let hits = run(&x);
        let mixed: Vec<bool> = hits.iter().map(|h| h.mixed).collect();
        // The most-merged binning is one bin of everything.
        let one = bin_hits(&hits, 1.0);
        let all: Vec<usize> = (0..hits.len()).collect();
        let loose = one.bins.iter().find(|b| b.members.len() > 4);
        let members = loose.map_or(all, |b| b.members.clone());
        let parts = split_bin(&hits, &mixed, &members, 1.0).expect("it falls apart");
        assert!(parts.len() >= 2);
        let mut seen: Vec<usize> = parts.iter().flat_map(|p| p.members.clone()).collect();
        seen.sort_unstable();
        let mut want = members.clone();
        want.sort_unstable();
        assert_eq!(seen, want, "no hit lost or doubled");
        for part in &parts {
            let first = names[part.members[0]];
            assert!(part.members.iter().all(|&m| names[m] == first));
        }
        // The kicks alone are too alike to split.
        let kicks: Vec<usize> = (0..hits.len()).filter(|&i| names[i] == "kick").collect();
        assert!(kicks.len() >= 2);
        assert!(split_bin(&hits, &mixed, &kicks, DEFAULT_MERGE).is_none());
    }

    #[test]
    fn make_bin_ranks_the_clean_hit_first() {
        let (x, _) = pattern();
        let hits = run(&x);
        let mut mixed: Vec<bool> = hits.iter().map(|h| h.mixed).collect();
        let bin = &bin_hits(&hits, DEFAULT_MERGE).bins[0];
        mixed[bin.members[0]] = true;
        let again = make_bin(&hits, &mixed, bin.members.clone());
        assert_eq!(*again.members.last().unwrap(), bin.members[0]);
        assert_ne!(again.members[0], bin.members[0]);
    }

    #[test]
    fn the_cache_keeps_an_analysis_until_the_window_or_version_changes() {
        let cache = HitCache::default();
        let key = HitKey::new("abc", 0, 1000);
        let mut runs = 0;
        for _ in 0..3 {
            cache
                .get_or_analyse(key.clone(), || {
                    runs += 1;
                    Ok(Vec::new())
                })
                .unwrap();
        }
        assert_eq!(runs, 1);
        assert!(cache.get(&HitKey::new("abc", 0, 2000)).is_none());
        assert!(
            cache
                .get(&HitKey {
                    version: DETECTOR_VERSION + 1,
                    ..key.clone()
                })
                .is_none()
        );
        let cancelled = cache.get_or_analyse(HitKey::new("x", 0, 1), || Err(Cancelled));
        assert_eq!(cancelled.err(), Some(Cancelled));
        assert!(cache.get(&HitKey::new("x", 0, 1)).is_none());
    }
}
