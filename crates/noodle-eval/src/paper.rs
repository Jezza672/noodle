//! The objective features of Roberts and Paliwal, "An Objective Measure of
//! Quality for Time-Scale Modification of Audio" (JASA 2021,
//! arXiv:2006.06153, section II), implemented as the paper writes them and
//! under its names.
//!
//! The paper aligns by interpolating one signal to the other's length. Here
//! the caller's [`TimeMap`] says which reference time plays at each test
//! time, which is exact and also handles tempo maps. Where the paper leaves a
//! detail open this module picks one and says so; none of the choices has
//! been checked against the authors' code (the dataset run in
//! [`crate::validate`] is what tells whether they matter).
//!
//! Choices the paper does not fix:
//! * STFT: Hann window, 2048 points, hop 512, for every feature.
//! * Phasiness: the phase of frame `u` is that of the frame's centre. The
//!   comparison runs on the test's frames and the reference's phase is read
//!   at the mapped time by linear interpolation of its unwrapped phase. `β`
//!   is the local source-over-output time ratio. Each bin's phase is taken
//!   relative to the first compared frame, so the initial phase (which `β`
//!   scaling would otherwise turn into a constant offset) drops out. When the
//!   test is shorter the difference is divided by `β`, which is the paper's
//!   second case written with the reciprocal. `W(k)` is normalised by the
//!   frame's own maximum. `SPh` is the standard deviation over frames of the
//!   per-frame mean.
//! * Spectral similarity: the magnitude spectra are normalised by their
//!   maximum, the cubic is a least-squares fit over a linear frequency axis
//!   scaled to `[-1, 1]`, and the difference is reference minus test.
//! * Peak delta and transient ratio: the mean and deviation for the
//!   threshold are taken over the signal's own `ΔE`.
//! * HPSTrRat: median filters of 17 bins (8 each side), binary masks with a
//!   separation factor of 2.

use crate::stft::{Frames, Spectrogram};
use crate::timemap::TimeMap;
use crate::transient;
use serde::Serialize;
use std::f64::consts::TAU;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct PaperFeatures {
    /// Mean weighted phase difference (eqs. 4 to 7); 0 is ideal.
    pub mph_mw: f64,
    /// Its spread over time.
    pub sph_mw: f64,
    /// The same without the magnitude weight.
    pub mph_nw: f64,
    pub sph_nw: f64,
    /// Mean absolute difference of the cubic-fit spectral shape; 0 is ideal.
    pub ssmad: f64,
    /// Mean (signed) difference of the same; 0 is ideal.
    pub ssmd: f64,
    /// Onsets per second, test minus reference (eq. 11); 0 is ideal.
    pub delta_p: f64,
    /// Mean onset height of the reference over the test's (eq. 13); 1 is ideal.
    pub tr_rat: f64,
    /// Transient-signal RMS of the reference over the test's; 1 is ideal.
    pub hps_tr_rat: f64,
}

pub fn compute(reference: &[f32], test: &[f32], rate: u32, map: &TimeMap) -> PaperFeatures {
    let rf = Frames::standard(reference, rate);
    let tf = Frames::standard(test, rate);
    let (rs, ts) = (rf.magnitudes(), tf.magnitudes());
    let ph = phasiness(&rf, &rs, &tf, &ts, map);
    let (ssmad, ssmd) = spectral_similarity(&rs, &ts, map);
    let (nr, nt) = (peaks_of(&rs), peaks_of(&ts));
    let ref_seconds = reference.len() as f64 / f64::from(rate);
    PaperFeatures {
        mph_mw: ph.0,
        sph_mw: ph.1,
        mph_nw: ph.2,
        sph_nw: ph.3,
        ssmad,
        ssmd,
        delta_p: (nt.peaks.len() as f64 - nr.peaks.len() as f64) / ref_seconds,
        tr_rat: nr.selected_height() / nt.selected_height(),
        hps_tr_rat: transient::transient_rms(&rs) / transient::transient_rms(&ts),
    }
}

/// Each bin's phase mapped to `[0, 2π)` and then raised by whole turns until
/// it passes the previous frame's (eqs. 4 and 5).
fn unwrap(f: &Frames) -> Vec<f64> {
    let mut out = vec![0.0f64; f.data.len()];
    for u in 0..f.frames {
        for k in 0..f.bins {
            let mut p = f64::from(f.row(u)[k].arg());
            if p <= 0.0 {
                p += TAU;
            }
            if u > 0 {
                let prev = out[(u - 1) * f.bins + k];
                if p <= prev {
                    p += TAU * ((prev - p) / TAU).floor() + TAU;
                }
            }
            out[u * f.bins + k] = p;
        }
    }
    out
}

/// (MPhMW, SPhMW, MPhNW, SPhNW).
fn phasiness(
    rf: &Frames,
    rs: &Spectrogram,
    tf: &Frames,
    ts: &Spectrogram,
    map: &TimeMap,
) -> (f64, f64, f64, f64) {
    let (ur, ut) = (unwrap(rf), unwrap(tf));
    let (bins, h) = (rf.bins, tf.hop_seconds());
    let ref_last = (rf.frames - 1) as f64;
    // Frames of the test that have a counterpart in the reference.
    let mapped: Vec<(usize, f64)> = (0..tf.frames)
        .map(|u| (u, map.to_source(u as f64 * h) / rf.hop_seconds()))
        .take_while(|&(_, pos)| pos >= 0.0 && pos <= ref_last)
        .collect();
    let nan = (f64::NAN, f64::NAN, f64::NAN, f64::NAN);
    if mapped.len() < 3 {
        return nan;
    }
    let at = |pos: f64, k: usize| {
        let i = (pos.floor() as usize).min(rf.frames - 1);
        let j = (i + 1).min(rf.frames - 1);
        let t = pos - i as f64;
        ur[i * bins + k] + (ur[j * bins + k] - ur[i * bins + k]) * t
    };
    let (first_u, first_pos) = mapped[0];
    let base: Vec<f64> = (0..bins).map(|k| at(first_pos, k)).collect();
    let mean_beta = (mapped.last().unwrap().1 - first_pos) * rf.hop_seconds()
        / ((mapped.last().unwrap().0 - first_u) as f64 * h);
    let test_longer = mean_beta <= 1.0;

    let mut scaled = vec![0.0f64; bins];
    let (mut weighted, mut plain) = (Vec::new(), Vec::new());
    let mut row = vec![0.0f32; bins];
    for w in mapped.windows(2) {
        let ((u0, p0), (u1, p1)) = (w[0], w[1]);
        let beta = (p1 - p0) * rf.hop_seconds() / h;
        for k in 0..bins {
            scaled[k] += beta * (ut[u1 * bins + k] - ut[u0 * bins + k]);
        }
        // The magnitudes that weight the difference: the reference's when the
        // test is the longer, otherwise the test's.
        let ok = if test_longer {
            rs.row_at(p1 * rs.hop_seconds, &mut row)
        } else {
            row.copy_from_slice(ts.row(u1));
            true
        };
        let top = row.iter().copied().fold(0.0f32, f32::max);
        if !ok || top <= 0.0 {
            continue;
        }
        let (mut sw, mut sp) = (0.0, 0.0);
        for k in 1..bins {
            let mut d = (at(p1, k) - base[k]) - scaled[k];
            if !test_longer {
                d /= mean_beta;
            }
            sp += d.abs();
            sw += d.abs() * f64::from(row[k] / top);
        }
        weighted.push(sw / (bins - 1) as f64);
        plain.push(sp / (bins - 1) as f64);
    }
    let stats = |v: &[f64]| {
        let m = v.iter().sum::<f64>() / v.len() as f64;
        let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64;
        (m, var.sqrt())
    };
    if weighted.is_empty() {
        return nan;
    }
    let (mw, sw) = stats(&weighted);
    let (mn, sn) = stats(&plain);
    (mw, sw, mn, sn)
}

/// The coefficients (c1, c2, c3) of the cubic fitted to `y` over `x` in
/// `[-1, 1]`; the intercept is fitted and then dropped, which removes level.
struct CubicFit {
    xs: Vec<f64>,
    inverse: [[f64; 4]; 4],
}

impl CubicFit {
    fn new(n: usize) -> Self {
        let xs: Vec<f64> = (0..n)
            .map(|i| -1.0 + 2.0 * i as f64 / (n - 1) as f64)
            .collect();
        let mut gram = [[0.0f64; 4]; 4];
        for &x in &xs {
            for (a, row) in gram.iter_mut().enumerate() {
                for (b, cell) in row.iter_mut().enumerate() {
                    *cell += x.powi((a + b) as i32);
                }
            }
        }
        Self {
            xs,
            inverse: invert(gram),
        }
    }

    /// The fitted curve without its constant term.
    fn shape(&self, y: &[f32], out: &mut [f64]) {
        let mut b = [0.0f64; 4];
        for (&x, &v) in self.xs.iter().zip(y) {
            for (j, bj) in b.iter_mut().enumerate() {
                *bj += f64::from(v) * x.powi(j as i32);
            }
        }
        let c: Vec<f64> = (0..4)
            .map(|i| (0..4).map(|j| self.inverse[i][j] * b[j]).sum())
            .collect();
        for (o, &x) in out.iter_mut().zip(&self.xs) {
            *o = c[1] * x + c[2] * x * x + c[3] * x * x * x;
        }
    }
}

fn invert(mut m: [[f64; 4]; 4]) -> [[f64; 4]; 4] {
    let mut inv = [[0.0f64; 4]; 4];
    for (i, row) in inv.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for col in 0..4 {
        let pivot = (col..4)
            .max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))
            .unwrap();
        m.swap(col, pivot);
        inv.swap(col, pivot);
        let d = m[col][col];
        for j in 0..4 {
            m[col][j] /= d;
            inv[col][j] /= d;
        }
        for r in (0..4).filter(|&r| r != col) {
            let f = m[r][col];
            for j in 0..4 {
                m[r][j] -= f * m[col][j];
                inv[r][j] -= f * inv[col][j];
            }
        }
    }
    inv
}

/// (SSMAD, SSMD).
fn spectral_similarity(rs: &Spectrogram, ts: &Spectrogram, map: &TimeMap) -> (f64, f64) {
    let fit = CubicFit::new(ts.bins);
    let (mut row, mut rn, mut tn) = (
        vec![0.0f32; ts.bins],
        vec![0.0f32; ts.bins],
        vec![0.0f32; ts.bins],
    );
    let (mut pr, mut pt) = (vec![0.0f64; ts.bins], vec![0.0f64; ts.bins]);
    let (mut abs, mut signed, mut n) = (0.0f64, 0.0f64, 0usize);
    for u in 0..ts.frames {
        if !rs.row_at(map.to_source(u as f64 * ts.hop_seconds), &mut row) {
            break;
        }
        let normalise = |src: &[f32], dst: &mut [f32]| {
            let top = src.iter().copied().fold(0.0f32, f32::max);
            if top <= 1e-9 {
                return false;
            }
            for (d, s) in dst.iter_mut().zip(src) {
                *d = s / top;
            }
            true
        };
        if !normalise(&row, &mut rn) || !normalise(ts.row(u), &mut tn) {
            continue;
        }
        fit.shape(&rn, &mut pr);
        fit.shape(&tn, &mut pt);
        let diff = |f: &dyn Fn(f64) -> f64| {
            pr.iter().zip(&pt).map(|(a, b)| f(a - b)).sum::<f64>() / ts.bins as f64
        };
        abs += diff(&f64::abs);
        signed += diff(&|d| d);
        n += 1;
    }
    if n == 0 {
        (f64::NAN, f64::NAN)
    } else {
        (abs / n as f64, signed / n as f64)
    }
}

struct Peaks {
    peaks: Vec<usize>,
    delta: Vec<f64>,
}

impl Peaks {
    /// Mean height of the peaks more than a deviation above the mean `ΔE`
    /// (eq. 12), `NaN` if none are.
    fn selected_height(&self) -> f64 {
        let n = self.delta.len() as f64;
        let mean = self.delta.iter().sum::<f64>() / n;
        let sd = (self.delta.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n).sqrt();
        let chosen: Vec<f64> = self
            .peaks
            .iter()
            .map(|&p| self.delta[p])
            .filter(|&d| d > mean + sd)
            .collect();
        if chosen.is_empty() {
            f64::NAN
        } else {
            chosen.iter().sum::<f64>() / chosen.len() as f64
        }
    }
}

/// Onset peaks of the high-frequency-content log energy difference (eqs. 8
/// to 10): a frame is a peak when its difference beats the two frames on
/// either side.
fn peaks_of(s: &Spectrogram) -> Peaks {
    let energy: Vec<f64> = (0..s.frames)
        .map(|u| {
            s.row(u)
                .iter()
                .enumerate()
                .map(|(k, &m)| k as f64 * f64::from(m).powi(2))
                .sum()
        })
        .collect();
    let eps = 1e-8 * energy.iter().copied().fold(0.0, f64::max).max(1e-30);
    let mut delta = vec![0.0f64; s.frames];
    for u in 1..s.frames {
        delta[u] = (energy[u] + eps).log10() - (energy[u - 1] + eps).log10();
    }
    let peaks = (2..s.frames.saturating_sub(2))
        .filter(|&u| {
            [u - 2, u - 1, u + 1, u + 2]
                .iter()
                .all(|&v| delta[u] > delta[v])
        })
        .collect();
    Peaks { peaks, delta }
}
