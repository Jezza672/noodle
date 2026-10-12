//! Perceptually shaped distances, borrowed from the wider audio-quality
//! literature for the fitted score (see [`fit`](crate::fit)).
//!
//! * `mel_nsim`: ViSQOL's idea (Hines et al.; v3 for audio): compare
//!   spectrogram *patches* with the neurogram similarity measure (SSIM
//!   without the contrast term). Here the patches are about a third of a
//!   second of a 40-band log-mel spectrogram, one band at a time, hopping by
//!   half a patch. 0 is identical.
//! * `mfcc`, `mfcc_delta`: distance between the MFCCs (coefficients 1 to 20
//!   of a 40-band log-mel spectrum) and between their deltas. The
//!   single-ended TSM quality work of Roberts and Paliwal (2020) found MFCCs
//!   with deltas the best input to learn from.
//! * `chroma`: one minus the cosine similarity of the 12-class pitch profile,
//!   which sees a pitch that moved or a harmony that smeared.
//!
//! All take the reference at the time the [`TimeMap`] says each test frame
//! plays at, as the other spectral metrics do. They are cheap diagnostics for
//! a fit to listeners to weigh, not claims of their own: none has been
//! checked against the original implementations.

use crate::spectral::align;
use crate::stft::Spectrogram;
use crate::timemap::TimeMap;
use serde::Serialize;

const MEL_BANDS: usize = 40;
const MFCCS: usize = 20;
const LOW_HZ: f64 = 50.0;
const HIGH_HZ: f64 = 16_000.0;
/// Log-mel values are in decibels from this far below the loudest.
const RANGE_DB: f32 = 80.0;
/// Frames in a patch (about 0.34 s at a hop of 512 and 48 kHz).
const PATCH: usize = 32;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Perceptual {
    /// One minus the mean NSIM of log-mel patches; 0 is identical.
    pub mel_nsim: f64,
    /// Mean Euclidean distance between MFCC vectors.
    pub mfcc: f64,
    /// The same between MFCC deltas.
    pub mfcc_delta: f64,
    /// Mean one minus cosine similarity of the chroma profiles.
    pub chroma: f64,
}

const NONE: Perceptual = Perceptual {
    mel_nsim: f64::NAN,
    mfcc: f64::NAN,
    mfcc_delta: f64::NAN,
    chroma: f64::NAN,
};

fn hz_to_mel(hz: f64) -> f64 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f64) -> f64 {
    700.0 * (10f64.powf(mel / 2595.0) - 1.0)
}

/// Triangular filters over the spectrogram's bins: `(first bin, weights)`.
/// Bands narrower than a bin (at the bottom of a coarse spectrogram) would be
/// empty, and an empty band reads as a perfect match, so they are dropped.
fn mel_filters(s: &Spectrogram) -> Vec<(usize, Vec<f32>)> {
    let top = HIGH_HZ.min(s.hz(s.bins - 1));
    let (lo, hi) = (hz_to_mel(LOW_HZ), hz_to_mel(top));
    let edges: Vec<f64> = (0..MEL_BANDS + 2)
        .map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (MEL_BANDS + 1) as f64))
        .collect();
    (0..MEL_BANDS)
        .map(|b| {
            let (l, c, r) = (edges[b], edges[b + 1], edges[b + 2]);
            let first = (l / s.hz(1)).floor() as usize;
            let last = ((r / s.hz(1)).ceil() as usize).min(s.bins - 1);
            let weights = (first..=last)
                .map(|k| {
                    let f = s.hz(k);
                    if f <= l || f >= r {
                        0.0
                    } else if f < c {
                        ((f - l) / (c - l)) as f32
                    } else {
                        ((r - f) / (r - c)) as f32
                    }
                })
                .collect();
            (first, weights)
        })
        .filter(|(_, w): &(usize, Vec<f32>)| w.iter().any(|&v| v > 0.0))
        .collect()
}

fn mel_energies(row: &[f32], filters: &[(usize, Vec<f32>)], out: &mut [f32]) {
    for (o, (first, w)) in out.iter_mut().zip(filters) {
        *o = w
            .iter()
            .zip(&row[*first..])
            .map(|(w, m)| w * m * m)
            .sum::<f32>();
    }
}

/// Log-mel in decibels above `-RANGE_DB` relative to `peak` (an energy).
fn log_mel(e: &[f32], peak: f32) -> Vec<f32> {
    e.iter()
        .map(|&v| (10.0 * (v.max(peak * 1e-12) / peak).log10() + RANGE_DB).max(0.0))
        .collect()
}

fn mfcc(log_mel: &[f32]) -> Vec<f64> {
    (1..=MFCCS)
        .map(|k| {
            log_mel
                .iter()
                .enumerate()
                .map(|(n, &v)| {
                    f64::from(v)
                        * (std::f64::consts::PI * k as f64 * (n as f64 + 0.5)
                            / log_mel.len() as f64)
                            .cos()
                })
                .sum()
        })
        .collect()
}

/// Regression slope over the two frames either side.
fn deltas(c: &[Vec<f64>]) -> Vec<Vec<f64>> {
    (0..c.len())
        .map(|t| {
            (0..MFCCS)
                .map(|k| {
                    let at =
                        |i: isize| c[(t as isize + i).clamp(0, c.len() as isize - 1) as usize][k];
                    (at(1) - at(-1) + 2.0 * (at(2) - at(-2))) / 10.0
                })
                .collect()
        })
        .collect()
}

fn euclid(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// Energy in each of 12 pitch classes from 40 Hz to 5 kHz.
fn chroma(row: &[f32], s: &Spectrogram) -> [f64; 12] {
    let mut out = [0.0f64; 12];
    for (k, &m) in row.iter().enumerate().skip(1) {
        let hz = s.hz(k);
        if !(40.0..=5000.0).contains(&hz) {
            continue;
        }
        let class = (12.0 * (hz / 440.0).log2()).round().rem_euclid(12.0) as usize % 12;
        out[class] += f64::from(m) * f64::from(m);
    }
    out
}

/// Chroma energies this far below the loudest frame (power) count as
/// silence. Two silent frames match; a silent one against a sounding one does
/// not.
const CHROMA_SILENCE_DB: f64 = 60.0;

/// Neurogram similarity of two equal-length vectors: intensity times
/// structure, the SSIM terms without contrast.
fn nsim(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (
        a.iter().map(|&v| f64::from(v)).sum::<f64>() / n,
        b.iter().map(|&v| f64::from(v)).sum::<f64>() / n,
    );
    let (mut va, mut vb, mut cov) = (0.0, 0.0, 0.0);
    for (&x, &y) in a.iter().zip(b) {
        let (dx, dy) = (f64::from(x) - ma, f64::from(y) - mb);
        va += dx * dx;
        vb += dy * dy;
        cov += dx * dy;
    }
    let (va, vb, cov) = (va / n, vb / n, cov / n);
    let range = f64::from(RANGE_DB);
    let (c1, c2) = ((0.01 * range).powi(2), (0.03 * range).powi(2));
    let intensity = (2.0 * ma * mb + c1) / (ma * ma + mb * mb + c1);
    let structure = (cov + c2 / 2.0) / ((va * vb).sqrt() + c2 / 2.0);
    intensity * structure
}

pub fn compare(reference: &Spectrogram, test: &Spectrogram, map: &TimeMap) -> Perceptual {
    let (rows, valid) = align(reference, test, map);
    let frames: Vec<usize> = (0..test.frames).filter(|&f| valid[f]).collect();
    if frames.len() < 3 {
        return NONE;
    }
    let bins = test.bins;
    let filters = mel_filters(test);
    let mel = |row: &[f32]| {
        let mut e = vec![0.0f32; filters.len()];
        mel_energies(row, &filters, &mut e);
        e
    };
    let (mut rm, mut tm) = (Vec::new(), Vec::new());
    for &f in &frames {
        rm.push(mel(&rows[f * bins..(f + 1) * bins]));
        tm.push(mel(test.row(f)));
    }
    let peak = rm
        .iter()
        .chain(&tm)
        .flatten()
        .copied()
        .fold(0.0, f32::max)
        .max(1e-30);
    let (rl, tl): (Vec<_>, Vec<_>) = (
        rm.iter().map(|e| log_mel(e, peak)).collect(),
        tm.iter().map(|e| log_mel(e, peak)).collect(),
    );
    let (rc, tc): (Vec<_>, Vec<_>) = (
        rl.iter().map(|l| mfcc(l)).collect(),
        tl.iter().map(|l| mfcc(l)).collect(),
    );
    let mean = |v: Vec<f64>| v.iter().sum::<f64>() / v.len() as f64;
    let mfcc_distance = mean(rc.iter().zip(&tc).map(|(a, b)| euclid(a, b)).collect());
    let (rd, td) = (deltas(&rc), deltas(&tc));
    let mfcc_delta = mean(rd.iter().zip(&td).map(|(a, b)| euclid(a, b)).collect());

    // Patches run over the valid frames, which are one stretch of consecutive
    // test frames for the maps in use (gaps only at the ends).
    let patch = PATCH.min(frames.len());
    let (mut total, mut count) = (0.0, 0usize);
    let mut start = 0;
    loop {
        for band in 0..filters.len() {
            let a: Vec<f32> = (start..start + patch).map(|t| rl[t][band]).collect();
            let b: Vec<f32> = (start..start + patch).map(|t| tl[t][band]).collect();
            total += nsim(&a, &b);
            count += 1;
        }
        if start + patch >= frames.len() {
            break;
        }
        start = (start + patch / 2).min(frames.len() - patch);
    }

    let raw: Vec<([f64; 12], [f64; 12])> = frames
        .iter()
        .map(|&f| {
            (
                chroma(&rows[f * bins..(f + 1) * bins], test),
                chroma(test.row(f), test),
            )
        })
        .collect();
    let power = |c: &[f64; 12]| c.iter().sum::<f64>();
    let loudest = raw
        .iter()
        .flat_map(|(a, b)| [power(a), power(b)])
        .fold(0.0, f64::max);
    let floor = loudest * 10f64.powf(-CHROMA_SILENCE_DB / 10.0);
    let chroma_distances: Vec<f64> = raw
        .iter()
        .filter_map(|(a, b)| {
            let (ea, eb) = (power(a), power(b));
            match (ea > floor, eb > floor) {
                (false, false) => None,
                (true, true) => {
                    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
                    let norms = a.iter().map(|v| v * v).sum::<f64>().sqrt()
                        * b.iter().map(|v| v * v).sum::<f64>().sqrt();
                    Some(1.0 - dot / norms)
                }
                _ => Some(1.0),
            }
        })
        .collect();
    let chroma_distance = if chroma_distances.is_empty() {
        f64::NAN
    } else {
        mean(chroma_distances)
    };
    Perceptual {
        mel_nsim: 1.0 - total / count as f64,
        mfcc: mfcc_distance,
        mfcc_delta,
        chroma: chroma_distance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, seconds: f32, hiss: f32) -> Vec<f32> {
        let mut seed = 7u32;
        (0..(48_000.0 * seconds) as usize)
            .map(|n| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0;
                ((n as f64 * f64::from(hz) * std::f64::consts::TAU / 48_000.0).sin() as f32) * 0.4
                    + noise * hiss
            })
            .collect()
    }

    fn score(reference: &[f32], test: &[f32]) -> Perceptual {
        compare(
            &Spectrogram::standard(reference, 48_000),
            &Spectrogram::standard(test, 48_000),
            &TimeMap::Identity,
        )
    }

    #[test]
    fn identical_signals_score_zero_and_damage_scores_more() {
        let clean = tone(440.0, 2.0, 0.0);
        let same = score(&clean, &clean);
        for v in [same.mel_nsim, same.mfcc, same.mfcc_delta, same.chroma] {
            assert!(v.abs() < 1e-6, "{same:?}");
        }
        let noisy = score(&clean, &tone(440.0, 2.0, 0.05));
        assert!(noisy.mel_nsim > 0.01 && noisy.mfcc > 1.0, "{noisy:?}");
        let louder_noise = score(&clean, &tone(440.0, 2.0, 0.2));
        assert!(louder_noise.mfcc > noisy.mfcc, "{louder_noise:?} {noisy:?}");
    }

    #[test]
    fn a_clip_with_silence_matches_itself() {
        let mut clip = tone(440.0, 2.0, 0.0);
        clip.extend(std::iter::repeat_n(0.0, 96_000));
        let same = score(&clip, &clip);
        for v in [same.mel_nsim, same.mfcc, same.mfcc_delta, same.chroma] {
            assert!(v.abs() < 1e-6, "{same:?}");
        }
    }

    #[test]
    fn chroma_sees_a_moved_pitch_that_level_metrics_do_not() {
        let a = tone(440.0, 2.0, 0.0);
        let up_a_fourth = tone(440.0 * 2f32.powf(5.0 / 12.0), 2.0, 0.0);
        let octave = tone(880.0, 2.0, 0.0);
        let moved = score(&a, &up_a_fourth);
        let same_class = score(&a, &octave);
        assert!(moved.chroma > 0.3, "{moved:?}");
        // An octave leaks into the neighbouring classes differently (the
        // peak is wider in semitones lower down), so it is not exactly zero.
        assert!(same_class.chroma < 0.15, "{same_class:?}");
        assert!(
            moved.chroma > 5.0 * same_class.chroma,
            "{moved:?} {same_class:?}"
        );
    }
}
