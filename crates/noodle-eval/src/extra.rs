//! Perceptual features beyond the paper's nine, in the spirit of the PEAQ
//! measures the paper's best predictor also feeds on (Roberts and Paliwal
//! combine PEAQ's features with their own): loudness in Bark bands, a noise
//! to mask ratio, a modulation difference, plus timbre distances (MFCC,
//! centroid, rolloff, flatness, chroma) and how steady the sound is from
//! frame to frame.
//!
//! These are not the ITU model. The ear model is a plain Bark band energy
//! with a 0.23 power loudness and no spreading, and the NMR takes the
//! difference of the two magnitude spectra as the noise (phase has no
//! meaning between a stretched signal and its source). The reference is read
//! at the time the [`TimeMap`] gives for each test frame, as everywhere else.
//! They feed [`crate::fit`]; nothing gates on them.

#![allow(clippy::needless_range_loop)]

use crate::stft::Spectrogram;
use crate::timemap::TimeMap;
use serde::Serialize;

const BARK_EDGES: [f64; 25] = [
    0.0, 100.0, 200.0, 300.0, 400.0, 510.0, 630.0, 770.0, 920.0, 1080.0, 1270.0, 1480.0, 1720.0,
    2000.0, 2320.0, 2700.0, 3150.0, 3700.0, 4400.0, 5300.0, 6400.0, 7700.0, 9500.0, 12000.0,
    15500.0,
];
const MELS: usize = 40;
const MFCCS: usize = 13;
/// A band is only judged when it holds this share of its frame's energy.
const AUDIBLE: f64 = 1e-6;
const EPS: f64 = 1e-12;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Extra {
    /// Loudness (Bark bands, power 0.23) the test adds, over the reference's.
    pub loud_added: f64,
    /// Its RMS over frames, which weights the bad frames.
    pub loud_added_rms: f64,
    /// Loudness the test loses.
    pub loud_missing: f64,
    /// Mean over frames of the noise-to-mask ratio, in dB; mask is 6 dB under
    /// the reference's band energy.
    pub nmr_db: f64,
    /// Share of audible (frame, band) cells where the noise beats the mask.
    pub nmr_over: f64,
    /// Mean distance between MFCCs 1 to 12.
    pub mfcc_dist: f64,
    pub centroid_abs_ln: f64,
    pub rolloff_abs_ln: f64,
    pub flatness_abs_ln: f64,
    /// One minus the cosine of the 12-bin chroma vectors, averaged.
    pub chroma_dist: f64,
    /// How alike neighbouring frames are in the test minus in the reference:
    /// negative is a more jittery sound, positive a more static one.
    pub stationarity_delta: f64,
    /// Log ratio of how fast the Bark loudness moves, test over reference.
    pub mod_abs_ln: f64,
}

impl Extra {
    pub fn features(&self) -> Vec<(&'static str, f64)> {
        vec![
            ("loud_added", self.loud_added),
            ("loud_added_rms", self.loud_added_rms),
            ("loud_missing", self.loud_missing),
            ("nmr_db", self.nmr_db),
            ("nmr_over", self.nmr_over),
            ("mfcc_dist", self.mfcc_dist),
            ("centroid_abs_ln", self.centroid_abs_ln),
            ("rolloff_abs_ln", self.rolloff_abs_ln),
            ("flatness_abs_ln", self.flatness_abs_ln),
            ("chroma_dist", self.chroma_dist),
            ("stationarity_delta", self.stationarity_delta),
            ("mod_abs_ln", self.mod_abs_ln),
        ]
    }
}

#[derive(Default)]
struct Mean(f64, usize);

impl Mean {
    fn add(&mut self, v: f64) {
        if v.is_finite() {
            self.0 += v;
            self.1 += 1;
        }
    }
    fn get(&self) -> f64 {
        if self.1 == 0 {
            f64::NAN
        } else {
            self.0 / self.1 as f64
        }
    }
}

struct Bands {
    /// Bark band of each bin, or `usize::MAX` outside them.
    bark: Vec<usize>,
    mel: Vec<Vec<(usize, f32)>>,
    chroma: Vec<usize>,
    dct: Vec<Vec<f64>>,
    hz: Vec<f64>,
}

impl Bands {
    fn new(s: &Spectrogram) -> Self {
        let hz: Vec<f64> = (0..s.bins).map(|k| s.hz(k)).collect();
        let bark = hz
            .iter()
            .enumerate()
            .map(|(k, &f)| {
                if k == 0 {
                    return usize::MAX;
                }
                (0..BARK_EDGES.len() - 1)
                    .find(|&b| f >= BARK_EDGES[b] && f < BARK_EDGES[b + 1])
                    .unwrap_or(usize::MAX)
            })
            .collect();
        let mel = |f: f64| 2595.0 * (1.0 + f / 700.0).log10();
        let top = mel(f64::from(s.rate) / 2.0);
        let centres: Vec<f64> = (0..MELS + 2)
            .map(|i| top * i as f64 / (MELS + 1) as f64)
            .collect();
        let filters = (0..MELS)
            .map(|m| {
                let (lo, mid, hi) = (centres[m], centres[m + 1], centres[m + 2]);
                hz.iter()
                    .enumerate()
                    .filter_map(|(k, &f)| {
                        let x = mel(f);
                        let w = if x > lo && x <= mid {
                            (x - lo) / (mid - lo)
                        } else if x > mid && x < hi {
                            (hi - x) / (hi - mid)
                        } else {
                            return None;
                        };
                        Some((k, w as f32))
                    })
                    .collect()
            })
            .collect();
        let chroma = hz
            .iter()
            .map(|&f| {
                if (50.0..5000.0).contains(&f) {
                    ((12.0 * (f / 440.0).log2()).round().rem_euclid(12.0)) as usize
                } else {
                    usize::MAX
                }
            })
            .collect();
        let dct = (0..MFCCS)
            .map(|c| {
                (0..MELS)
                    .map(|m| {
                        (std::f64::consts::PI * c as f64 * (m as f64 + 0.5) / MELS as f64).cos()
                    })
                    .collect()
            })
            .collect();
        Self {
            bark,
            mel: filters,
            chroma,
            dct,
            hz,
        }
    }
}

/// One frame's descriptors.
struct Frame {
    /// Bark band energies.
    bark: [f64; 24],
    mfcc: [f64; MFCCS],
    chroma: [f64; 12],
    centroid: f64,
    rolloff: f64,
    flatness: f64,
}

fn describe(mag: &[f32], b: &Bands) -> Frame {
    let mut f = Frame {
        bark: [0.0; 24],
        mfcc: [0.0; MFCCS],
        chroma: [0.0; 12],
        centroid: 0.0,
        rolloff: 0.0,
        flatness: 0.0,
    };
    let (mut total, mut moment, mut log_sum) = (0.0f64, 0.0f64, 0.0f64);
    for k in 1..mag.len() {
        let e = f64::from(mag[k]).powi(2);
        if b.bark[k] != usize::MAX {
            f.bark[b.bark[k]] += e;
        }
        if b.chroma[k] != usize::MAX {
            f.chroma[b.chroma[k]] += e;
        }
        total += e;
        moment += e * b.hz[k];
        log_sum += (e + EPS).ln();
    }
    let n = (mag.len() - 1) as f64;
    f.centroid = moment / (total + EPS);
    f.flatness = (log_sum / n).exp() / (total / n + EPS);
    let mut acc = 0.0;
    for k in 1..mag.len() {
        acc += f64::from(mag[k]).powi(2);
        if acc >= 0.95 * total {
            f.rolloff = b.hz[k];
            break;
        }
    }
    let logmel: Vec<f64> = b
        .mel
        .iter()
        .map(|taps| {
            let e: f64 = taps
                .iter()
                .map(|&(k, w)| f64::from(w) * f64::from(mag[k]).powi(2))
                .sum();
            (e + EPS).ln()
        })
        .collect();
    for (c, row) in b.dct.iter().enumerate() {
        f.mfcc[c] = row.iter().zip(&logmel).map(|(a, l)| a * l).sum();
    }
    f
}

fn loudness(e: f64) -> f64 {
    e.powf(0.23)
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for (&x, &y) in a.iter().zip(b) {
        let (x, y) = (f64::from(x).sqrt(), f64::from(y).sqrt());
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    ab / (aa * bb).sqrt().max(EPS)
}

/// Mean cosine similarity of neighbouring frames' compressed magnitudes.
fn steadiness(s: &Spectrogram) -> f64 {
    let mut m = Mean::default();
    for u in 1..s.frames {
        let (a, b) = (s.row(u - 1), s.row(u));
        if a.iter().chain(b).any(|&x| x > 1e-6) {
            m.add(cosine(a, b));
        }
    }
    m.get()
}

/// Mean absolute frame-to-frame change of Bark loudness, over its mean.
fn motion(s: &Spectrogram, b: &Bands) -> f64 {
    let frames: Vec<[f64; 24]> = (0..s.frames)
        .map(|u| {
            let mut l = [0.0; 24];
            for (k, &m) in s.row(u).iter().enumerate().skip(1) {
                if b.bark[k] != usize::MAX {
                    l[b.bark[k]] += f64::from(m).powi(2);
                }
            }
            l.map(loudness)
        })
        .collect();
    let (mut change, mut level) = (0.0, 0.0);
    for w in frames.windows(2) {
        for i in 0..24 {
            change += (w[1][i] - w[0][i]).abs();
            level += w[1][i];
        }
    }
    change / (level + EPS)
}

pub fn compare(rs: &Spectrogram, ts: &Spectrogram, map: &TimeMap) -> Extra {
    let bands = Bands::new(ts);
    let mut row = vec![0.0f32; ts.bins];
    let (mut added, mut missing, mut nmr, mut mfcc) = (
        Mean::default(),
        Mean::default(),
        Mean::default(),
        Mean::default(),
    );
    let (mut centroid, mut rolloff, mut flatness, mut chroma) = (
        Mean::default(),
        Mean::default(),
        Mean::default(),
        Mean::default(),
    );
    let (mut added_sq, mut over, mut cells) = (0.0f64, 0usize, 0usize);
    for u in 0..ts.frames {
        if !rs.row_at(map.to_source(u as f64 * ts.hop_seconds), &mut row) {
            break;
        }
        let tmag = ts.row(u);
        let (r, t) = (describe(&row, &bands), describe(tmag, &bands));
        let r_total: f64 = r.bark.iter().sum();
        if r_total < 1e-9 {
            continue;
        }
        let (mut up, mut down, mut base) = (0.0, 0.0, 0.0);
        let mut noise = [0.0f64; 24];
        for (k, (&a, &b)) in row.iter().zip(tmag).enumerate().skip(1) {
            if bands.bark[k] != usize::MAX {
                noise[bands.bark[k]] += (f64::from(a) - f64::from(b)).powi(2);
            }
        }
        let mut ratio = 0.0;
        let mut audible = 0usize;
        for i in 0..24 {
            let (lr, lt) = (loudness(r.bark[i]), loudness(t.bark[i]));
            base += lr;
            up += (lt - lr).max(0.0);
            down += (lr - lt).max(0.0);
            if r.bark[i] > AUDIBLE * r_total {
                let n = noise[i] / (0.25 * r.bark[i]);
                ratio += n;
                audible += 1;
                cells += 1;
                if n > 1.0 {
                    over += 1;
                }
            }
        }
        added.add(up / base);
        added_sq += (up / base).powi(2);
        missing.add(down / base);
        if audible > 0 {
            nmr.add(10.0 * (ratio / audible as f64 + 1e-3).log10());
        }
        mfcc.add(
            (1..MFCCS)
                .map(|c| (r.mfcc[c] - t.mfcc[c]).powi(2))
                .sum::<f64>()
                .sqrt(),
        );
        centroid.add((r.centroid / t.centroid.max(1.0)).ln().abs());
        rolloff.add((r.rolloff.max(1.0) / t.rolloff.max(1.0)).ln().abs());
        flatness.add((r.flatness / t.flatness.max(EPS)).ln().abs());
        let dot: f64 = (0..12).map(|i| r.chroma[i] * t.chroma[i]).sum();
        let (rn, tn) = (
            r.chroma.iter().map(|x| x * x).sum::<f64>().sqrt(),
            t.chroma.iter().map(|x| x * x).sum::<f64>().sqrt(),
        );
        if rn > 0.0 && tn > 0.0 {
            chroma.add(1.0 - dot / (rn * tn));
        }
    }
    Extra {
        loud_added: added.get(),
        loud_added_rms: (added_sq / added.1.max(1) as f64).sqrt(),
        loud_missing: missing.get(),
        nmr_db: nmr.get(),
        nmr_over: if cells == 0 {
            f64::NAN
        } else {
            over as f64 / cells as f64
        },
        mfcc_dist: mfcc.get(),
        centroid_abs_ln: centroid.get(),
        rolloff_abs_ln: rolloff.get(),
        flatness_abs_ln: flatness.get(),
        chroma_dist: chroma.get(),
        stationarity_delta: steadiness(ts) - steadiness(rs),
        mod_abs_ln: (motion(ts, &bands) / motion(rs, &bands).max(EPS))
            .ln()
            .abs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, seconds: f64, noise: f32) -> Vec<f32> {
        let mut seed = 12345u32;
        (0..(f64::from(rate) * seconds) as usize)
            .map(|n| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let r = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                let t = n as f32 / rate as f32;
                0.3 * (440.0 * std::f32::consts::TAU * t).sin()
                    + 0.2 * (1320.0 * std::f32::consts::TAU * t).sin()
                    + noise * r
            })
            .collect()
    }

    #[test]
    fn a_signal_against_itself_is_clean_and_noise_is_not() {
        let rate = 44_100;
        let clean = Spectrogram::standard(&tone(rate, 2.0, 0.0), rate);
        let same = compare(&clean, &clean, &TimeMap::Identity);
        assert!(same.loud_added.abs() < 1e-9 && same.loud_missing.abs() < 1e-9);
        assert!(same.mfcc_dist < 1e-6 && same.chroma_dist.abs() < 1e-9);
        assert!(same.stationarity_delta.abs() < 1e-9 && same.mod_abs_ln.abs() < 1e-9);
        assert_eq!(same.nmr_over, 0.0);
        let dirty = Spectrogram::standard(&tone(rate, 2.0, 0.3), rate);
        let bad = compare(&clean, &dirty, &TimeMap::Identity);
        assert!(bad.nmr_db > same.nmr_db + 5.0, "{bad:?}");
        assert!(bad.nmr_over > 0.1 && bad.loud_added > 0.05);
        assert!(bad.flatness_abs_ln > 0.5 && bad.mfcc_dist > 1.0);
    }
}
