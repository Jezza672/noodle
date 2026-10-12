//! Where the onsets land and whether the timing drifts.

use crate::onsets::{self, Onset};
use crate::stft::Spectrogram;
use crate::timemap::TimeMap;
use serde::Serialize;

/// An onset is the same hit if the test's is this close to where the map puts
/// the reference's.
pub const MATCH_WINDOW: f64 = 0.1;
/// Length of the stretches whose lag is measured for drift.
const DRIFT_SEGMENT: f64 = 2.0;
/// The largest lag looked for.
const MAX_LAG: f64 = 0.15;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Timing {
    /// Reference onsets that fall inside the test.
    pub expected: usize,
    pub matched: usize,
    /// Test onsets with no reference onset: hits the method invented.
    pub extra: usize,
    /// Median distance of matched onsets from where they should be, in ms.
    pub median_abs_offset_ms: f64,
    /// Mean signed offset in ms; positive means late.
    pub mean_offset_ms: f64,
    pub max_abs_offset_ms: f64,
    /// How much the lag changes over the piece, in ms: the spread (largest
    /// minus smallest) of the lags of its 2 s stretches, each found by
    /// cross-correlating the test's onset envelope with the reference's
    /// mapped onto it. A constant delay is 0 here (see `mean_offset_ms`); slow
    /// drift that never leaves the match window shows. `NaN` when fewer than
    /// two stretches have anything to correlate.
    pub drift_ms: f64,
}

/// Pairs each reference onset with the nearest unclaimed test onset within
/// [`MATCH_WINDOW`] of where `map` puts it. Returns (reference index, test
/// index, signed offset in seconds).
pub fn pair(reference: &[Onset], test: &[Onset], map: &TimeMap) -> Vec<(usize, usize, f64)> {
    let mut taken = vec![false; test.len()];
    let mut out = Vec::new();
    for (i, r) in reference.iter().enumerate() {
        let expect = map.to_output(r.time);
        let best = test
            .iter()
            .enumerate()
            .filter(|(j, t)| !taken[*j] && (t.time - expect).abs() <= MATCH_WINDOW)
            .min_by(|a, b| {
                (a.1.time - expect)
                    .abs()
                    .total_cmp(&(b.1.time - expect).abs())
            });
        if let Some((j, t)) = best {
            taken[j] = true;
            out.push((i, j, t.time - expect));
        }
    }
    out
}

pub fn compare(reference: &Spectrogram, test: &Spectrogram, map: &TimeMap) -> Timing {
    let test_len = test.frames as f64 * test.hop_seconds;
    let ref_onsets = onsets::detect(reference);
    let test_onsets = onsets::detect(test);
    let inside: Vec<Onset> = ref_onsets
        .into_iter()
        .filter(|o| map.to_output(o.time) < test_len - MATCH_WINDOW)
        .collect();
    // The reference filter above leaves out the last window of the test, so
    // the test's onsets there are not "extra" either.
    let test_onsets: Vec<Onset> = test_onsets
        .into_iter()
        .filter(|o| o.time < test_len - MATCH_WINDOW)
        .collect();
    let pairs = pair(&inside, &test_onsets, map);
    let mut abs: Vec<f64> = pairs.iter().map(|p| p.2.abs() * 1000.0).collect();
    abs.sort_by(f64::total_cmp);
    let n = abs.len();
    let signed = if n > 0 {
        pairs.iter().map(|p| p.2).sum::<f64>() * 1000.0 / n as f64
    } else {
        f64::NAN
    };
    Timing {
        expected: inside.len(),
        matched: n,
        extra: test_onsets.len().saturating_sub(n),
        median_abs_offset_ms: if n > 0 { abs[n / 2] } else { f64::NAN },
        mean_offset_ms: signed,
        max_abs_offset_ms: abs.last().copied().unwrap_or(f64::NAN),
        drift_ms: drift(reference, test, map),
    }
}

fn drift(reference: &Spectrogram, test: &Spectrogram, map: &TimeMap) -> f64 {
    let hop = test.hop_seconds;
    let ref_flux = onsets::flux(reference);
    let test_flux = onsets::flux(test);
    // The reference envelope on the test's timeline.
    let warped: Vec<f32> = (0..test.frames)
        .map(|f| {
            let pos = map.to_source(f as f64 * hop) / reference.hop_seconds;
            if pos < 0.0 || pos >= (ref_flux.len() - 1) as f64 {
                return 0.0;
            }
            let (i, t) = (pos as usize, (pos - pos.floor()) as f32);
            ref_flux[i] + (ref_flux[i + 1] - ref_flux[i]) * t
        })
        .collect();
    let seg = (DRIFT_SEGMENT / hop) as usize;
    let max_lag = (MAX_LAG / hop) as usize;
    let mut lags: Vec<f64> = Vec::new();
    let mut start = 0;
    while start + seg <= test.frames {
        let a = &warped[start..start + seg];
        let b = &test_flux[start..start + seg];
        let energy = |x: &[f32]| x.iter().map(|v| v * v).sum::<f32>();
        if energy(a) > 0.0 && energy(b) > 0.0 {
            let mut best = (f32::MIN, 0isize);
            for lag in -(max_lag as isize)..=max_lag as isize {
                let s: f32 = (0..seg as isize)
                    .filter_map(|i| {
                        let j = i + lag;
                        (j >= 0 && j < seg as isize).then(|| a[i as usize] * b[j as usize])
                    })
                    .sum();
                if s > best.0 {
                    best = (s, lag);
                }
            }
            lags.push(best.1 as f64 * hop * 1000.0);
        }
        start += seg;
    }
    if lags.len() < 2 {
        return f64::NAN;
    }
    let (lo, hi) = lags
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), &x| (l.min(x), h.max(x)));
    hi - lo
}
