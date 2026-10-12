//! All the metrics for one reference and test pair.

use crate::clip::Clip;
use crate::level::{self, Level};
use crate::phasiness::phasiness;
use crate::pitch::{self, Pitch};
use crate::spectral::{self, Spectral};
use crate::stereo::{self, Stereo};
use crate::stft::Spectrogram;
use crate::timemap::TimeMap;
use crate::timing::{self, Timing};
use crate::transient::{self, Transients};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Report {
    pub spectral: Spectral,
    /// The test's phasiness minus the reference's.
    pub phasiness_delta: f64,
    pub transients: Transients,
    pub timing: Timing,
    pub pitch: Pitch,
    pub level: Level,
    pub stereo: Option<Stereo>,
}

/// Scores `test` against `reference`, which must share a sample rate. `map`
/// says which reference time plays at each test time.
pub fn evaluate(reference: Clip, test: Clip, map: &TimeMap) -> Report {
    assert_eq!(
        reference.rate, test.rate,
        "the clips must share a sample rate"
    );
    let (rm, tm) = (reference.mono(), test.mono());
    let (rs, ts) = (
        Spectrogram::standard(&rm, reference.rate),
        Spectrogram::standard(&tm, test.rate),
    );
    let (rf, tf) = (
        Spectrogram::for_onsets(&rm, reference.rate),
        Spectrogram::for_onsets(&tm, test.rate),
    );
    Report {
        spectral: spectral::compare(&rs, &ts, map),
        phasiness_delta: phasiness(&tm, test.rate) - phasiness(&rm, reference.rate),
        transients: transient::compare(&rs, &ts, &rf, &tf, map),
        timing: timing::compare(&rf, &tf, map),
        pitch: pitch::compare(&rm, &tm, reference.rate, map),
        level: level::compare(reference, test),
        stereo: stereo::compare(reference, test),
    }
}

impl Report {
    /// Every metric as one number, oriented where it helps: deviations from
    /// "unchanged" are absolute values or log ratios, so 0 is a perfect score
    /// and larger is worse for all of them except the two signed ones noted.
    /// Values that could not be measured are `NaN`.
    pub fn features(&self) -> Vec<(&'static str, f64)> {
        let miss = if self.timing.expected > 0 {
            1.0 - self.timing.matched as f64 / self.timing.expected as f64
        } else {
            f64::NAN
        };
        vec![
            ("log_distance_db", self.spectral.log_distance_db),
            (
                "smoothed_band_distance_db",
                self.spectral.smoothed_band_distance_db,
            ),
            ("convergence", self.spectral.convergence),
            // Signed: positive is more phasey.
            ("phasiness_delta", self.phasiness_delta),
            // Signed: negative is blunter attacks.
            ("peak_delta", self.transients.peak_delta),
            (
                "transient_ratio_abs_ln",
                self.transients.transient_ratio.ln().abs(),
            ),
            (
                "hp_transient_ratio_abs_ln",
                self.transients.hp_transient_ratio.ln().abs(),
            ),
            ("median_abs_offset_ms", self.timing.median_abs_offset_ms),
            ("missed_onsets", miss),
            ("drift_ms", self.timing.drift_ms),
            ("pitch_median_abs_cents", self.pitch.median_abs_cents),
            ("lufs_abs_delta", self.level.lufs_delta.abs()),
            ("peak_abs_delta_db", self.level.peak_delta_db.abs()),
            (
                "stereo_correlation_abs_delta",
                self.stereo.map_or(f64::NAN, |s| s.correlation_delta.abs()),
            ),
        ]
    }
}
