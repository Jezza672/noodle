//! Noodle's own transient diagnostics (the paper's are in [`crate::paper`]).
//!
//! Transient preservation: whether attacks stay as sharp and as numerous as
//! in the reference. Each measure is a ratio or change relative to the
//! reference, so 1 (or 0 for the delta) is a perfect match; smeared attacks
//! and pre-echo push them down, and extra hits push them up.

use crate::onsets;
use crate::stft::Spectrogram;
use crate::timemap::TimeMap;
use crate::timing;
use serde::Serialize;

/// Half-width of the median filters that split harmonic from percussive.
const HPSS_HALF: usize = 8;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Transients {
    /// Mean relative change in the flux peak of matched onsets: -0.5 means
    /// attacks are half as sharp as in the reference.
    pub flux_peak_change: f64,
    /// How concentrated the test's spectral growth is (mean square of the
    /// flux over its mean) over the reference's: below 1 means attacks are
    /// spread out.
    pub flux_peakiness_ratio: f64,
    /// The test's percussive share of energy, after harmonic-percussive
    /// separation by median filtering, over the reference's.
    pub percussive_share_ratio: f64,
}

/// `*_fine` are the onset spectrograms ([`Spectrogram::for_onsets`]), which
/// the attack measures use; `reference` and `test` are the standard ones, for
/// the harmonic-percussive split.
pub fn compare(
    reference: &Spectrogram,
    test: &Spectrogram,
    reference_fine: &Spectrogram,
    test_fine: &Spectrogram,
    map: &TimeMap,
) -> Transients {
    let (rf, tf) = (onsets::flux(reference_fine), onsets::flux(test_fine));
    let ro = onsets::pick(&rf, reference_fine.hop_seconds);
    let to = onsets::pick(&tf, test_fine.hop_seconds);
    let pairs = timing::pair(&ro, &to, map);
    let flux_peak_change = if pairs.is_empty() {
        f64::NAN
    } else {
        pairs
            .iter()
            .map(|&(i, j, _)| f64::from(to[j].strength / ro[i].strength) - 1.0)
            .sum::<f64>()
            / pairs.len() as f64
    };
    Transients {
        flux_peak_change,
        flux_peakiness_ratio: peakiness(&tf) / peakiness(&rf),
        percussive_share_ratio: percussive_share(test) / percussive_share(reference),
    }
}

/// How concentrated the flux is: its mean square over its mean. Sharp attacks
/// pile the same total growth into few frames, smearing spreads it thin.
fn peakiness(flux: &[f32]) -> f64 {
    let sum: f64 = flux.iter().map(|&f| f64::from(f)).sum();
    let sq: f64 = flux.iter().map(|&f| f64::from(f) * f64::from(f)).sum();
    sq / sum.max(1e-30)
}

fn median(buf: &mut [f32]) -> f32 {
    let mid = buf.len() / 2;
    *buf.select_nth_unstable_by(mid, f32::total_cmp).1
}

/// The harmonic-smoothed and percussive-smoothed magnitudes (median filters
/// along time and along frequency).
pub(crate) fn medians(s: &Spectrogram) -> (Vec<f32>, Vec<f32>) {
    let (frames, bins) = (s.frames, s.bins);
    let mut buf = Vec::with_capacity(2 * HPSS_HALF + 1);
    let mut harmonic = vec![0.0f32; s.mag.len()];
    for k in 0..bins {
        for f in 0..frames {
            buf.clear();
            let (lo, hi) = (f.saturating_sub(HPSS_HALF), (f + HPSS_HALF).min(frames - 1));
            buf.extend((lo..=hi).map(|g| s.mag[g * bins + k]));
            harmonic[f * bins + k] = median(&mut buf);
        }
    }
    let mut percussive = vec![0.0f32; s.mag.len()];
    for f in 0..frames {
        let row = s.row(f);
        for k in 0..bins {
            buf.clear();
            let (lo, hi) = (k.saturating_sub(HPSS_HALF), (k + HPSS_HALF).min(bins - 1));
            buf.extend_from_slice(&row[lo..=hi]);
            percussive[f * bins + k] = median(&mut buf);
        }
    }
    (harmonic, percussive)
}

/// Fraction of the energy that a soft percussive mask keeps (Fitzgerald's
/// median-filter separation).
pub fn percussive_share(s: &Spectrogram) -> f64 {
    let (harmonic, percussive) = medians(s);
    let (mut kept, mut total) = (0.0f64, 0.0f64);
    for (i, &m) in s.mag.iter().enumerate() {
        let (p, h) = (
            f64::from(percussive[i]).powi(2),
            f64::from(harmonic[i]).powi(2),
        );
        let e = f64::from(m).powi(2);
        total += e;
        if p + h > 0.0 {
            kept += e * p / (p + h);
        }
    }
    kept / total.max(1e-30)
}

/// The separation factor of the binary masks: a cell is transient when its
/// percussive median beats the harmonic one by this factor.
const SEPARATION: f32 = 2.0;

/// Root-mean-square level of the transient component after Driedger's
/// median-filter separation with binary masks: the square root of the mean,
/// over frames, of the energy in cells the percussive mask keeps. (By
/// Parseval this is proportional to the RMS of the resynthesised signal, and
/// the constant cancels in the ratio the paper uses.)
pub fn transient_rms(s: &Spectrogram) -> f64 {
    let (harmonic, percussive) = medians(s);
    let kept: f64 = s
        .mag
        .iter()
        .enumerate()
        .filter(|&(i, _)| percussive[i] > SEPARATION * harmonic[i])
        .map(|(_, &m)| f64::from(m).powi(2))
        .sum();
    (kept / s.frames as f64).sqrt()
}
