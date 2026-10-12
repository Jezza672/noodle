//! Fundamental frequency (YIN) and how far the test's moves from the
//! reference's. Meaningful on single-voice material; on a dense mix the
//! tracker finds whichever voice dominates, so `agreement` says how much of it
//! to trust.

use crate::timemap::TimeMap;
use serde::Serialize;

const TARGET_RATE: f64 = 11_025.0;
const WINDOW: usize = 1024;
const HOP: usize = 512;
const MIN_HZ: f64 = 60.0;
const MAX_HZ: f64 = 1000.0;
const THRESHOLD: f32 = 0.15;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Pitch {
    /// Median absolute difference in cents, over frames voiced in both.
    pub median_abs_cents: f64,
    /// Median signed difference in cents (positive means the test is higher).
    /// A median, because frames that straddle a note change in one signal but
    /// not the other are far off and would drag a mean.
    pub median_cents: f64,
    /// Frames voiced in both over frames voiced in either.
    pub agreement: f64,
}

/// F0 in hertz per hop of `HOP` decimated samples, `None` where unvoiced.
/// Returns the f0s and the hop in seconds; frame `i` is centred on
/// `i * hop + WINDOW / 2` decimated samples (see [`centre`]).
pub fn track(x: &[f32], rate: u32) -> (Vec<Option<f64>>, f64) {
    // A plain average before dropping samples is a weak anti-alias filter;
    // YIN only needs the low harmonics, so it is enough.
    let factor = ((f64::from(rate) / TARGET_RATE).round() as usize).max(1);
    let d: Vec<f32> = x
        .chunks_exact(factor)
        .map(|c| c.iter().sum::<f32>() / factor as f32)
        .collect();
    let r = f64::from(rate) / factor as f64;
    let lo = (r / MAX_HZ).floor() as usize;
    let hi = ((r / MIN_HZ).ceil() as usize).min(WINDOW - 1);
    let mut out = Vec::new();
    let mut start = 0;
    while start + WINDOW + hi <= d.len() {
        out.push(yin(&d[start..start + WINDOW + hi], lo.max(2), hi).map(|lag| r / lag));
        start += HOP;
    }
    (out, HOP as f64 * factor as f64 / f64::from(rate))
}

fn yin(x: &[f32], lo: usize, hi: usize) -> Option<f64> {
    let w = x.len() - hi;
    let energy: f32 = x[..w].iter().map(|v| v * v).sum();
    if energy < 1e-6 * w as f32 {
        return None;
    }
    let mut cmnd = vec![1.0f32; hi + 1];
    let mut running = 0.0f32;
    for tau in 1..=hi {
        let d: f32 = (0..w).map(|i| (x[i] - x[i + tau]).powi(2)).sum();
        running += d;
        cmnd[tau] = if running > 0.0 {
            d * tau as f32 / running
        } else {
            1.0
        };
    }
    let mut tau = lo;
    while tau < hi {
        if cmnd[tau] < THRESHOLD {
            while tau + 1 < hi && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            let (a, b, c) = (cmnd[tau - 1], cmnd[tau], cmnd[tau + 1]);
            let denom = a - 2.0 * b + c;
            let shift = if denom.abs() > 1e-9 {
                0.5 * (a - c) / denom
            } else {
                0.0
            };
            return Some(tau as f64 + f64::from(shift));
        }
        tau += 1;
    }
    None
}

/// Seconds at which frame `i` of [`track`] is centred.
fn centre(i: f64, hop: f64, rate: u32) -> f64 {
    let factor = ((f64::from(rate) / TARGET_RATE).round()).max(1.0);
    i * hop + (WINDOW as f64 / 2.0) * factor / f64::from(rate)
}

pub fn compare(reference: &[f32], test: &[f32], rate: u32, map: &TimeMap) -> Pitch {
    let (rt, rhop) = track(reference, rate);
    let (tt, thop) = track(test, rate);
    let (mut diffs, mut either) = (Vec::new(), 0usize);
    for (i, tf) in tt.iter().enumerate() {
        let src = map.to_source(centre(i as f64, thop, rate));
        let j = ((src - centre(0.0, rhop, rate)) / rhop).round();
        let rf = if j >= 0.0 {
            rt.get(j as usize).copied().flatten()
        } else {
            None
        };
        if tf.is_some() || rf.is_some() {
            either += 1;
        }
        if let (Some(a), Some(b)) = (tf, rf) {
            diffs.push(1200.0 * (a / b).log2());
        }
    }
    if diffs.is_empty() {
        return Pitch {
            median_abs_cents: f64::NAN,
            median_cents: f64::NAN,
            agreement: 0.0,
        };
    }
    let mut signed = diffs.clone();
    signed.sort_by(f64::total_cmp);
    let mut abs: Vec<f64> = diffs.iter().map(|d| d.abs()).collect();
    abs.sort_by(f64::total_cmp);
    Pitch {
        median_abs_cents: abs[abs.len() / 2],
        median_cents: signed[signed.len() / 2],
        agreement: diffs.len() as f64 / either as f64,
    }
}
