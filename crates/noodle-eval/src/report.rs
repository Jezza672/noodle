//! All the metrics for one reference and test pair.

use crate::clip::Clip;
use crate::extra::{self, Extra};
use crate::level::{self, Level};
use crate::paper::{self, PaperFeatures};
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
    /// The Roberts and Paliwal features, as the paper defines them.
    pub paper: PaperFeatures,
    /// Noodle's own diagnostics follow.
    pub spectral: Spectral,
    /// The test's vertical phase incoherence minus the reference's.
    pub vertical_phasiness_delta: f64,
    pub transients: Transients,
    pub timing: Timing,
    pub pitch: Pitch,
    pub level: Level,
    pub stereo: Option<Stereo>,
    /// Perceptual features for fitting a score; see [`crate::extra`].
    pub extra: Extra,
}

/// Scores `test` against `reference`, which must share a sample rate. `map`
/// says which reference time plays at each test time.
///
/// Level is reported by [`Report::level`] alone: before every other measure
/// the test is gain-matched to the reference by integrated loudness, so a
/// pure gain change moves nothing else.
pub fn evaluate(reference: Clip, test: Clip, map: &TimeMap) -> Report {
    assert_eq!(
        reference.rate, test.rate,
        "the clips must share a sample rate"
    );
    let (rm, mut tm) = (reference.mono(), test.mono());
    let level = level::compare(reference, test);
    if level.lufs_delta.is_finite() {
        let gain = 10f32.powf(-(level.lufs_delta as f32) / 20.0);
        tm.iter_mut().for_each(|s| *s *= gain);
    }
    let (rs, ts) = (
        Spectrogram::standard(&rm, reference.rate),
        Spectrogram::standard(&tm, test.rate),
    );
    let (rf, tf) = (
        Spectrogram::for_onsets(&rm, reference.rate),
        Spectrogram::for_onsets(&tm, test.rate),
    );
    Report {
        paper: paper::compute(&rm, &tm, reference.rate, map),
        spectral: spectral::compare(&rs, &ts, map),
        vertical_phasiness_delta: phasiness(&tm, test.rate) - phasiness(&rm, reference.rate),
        transients: transient::compare(&rs, &ts, &rf, &tf, map),
        timing: timing::compare(&rf, &tf, map),
        pitch: pitch::compare(&rm, &tm, reference.rate, map),
        level,
        stereo: stereo::compare(reference, test),
        extra: extra::compare(&rs, &ts, map),
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
        let p = &self.paper;
        vec![
            ("MPhMW", p.mph_mw),
            ("SPhMW", p.sph_mw),
            ("MPhNW", p.mph_nw),
            ("SPhNW", p.sph_nw),
            ("SSMAD", p.ssmad),
            ("SSMD_abs", p.ssmd.abs()),
            ("DeltaP_abs", p.delta_p.abs()),
            ("TrRat_abs_ln", p.tr_rat.ln().abs()),
            ("HPSTrRat_abs_ln", p.hps_tr_rat.ln().abs()),
            ("log_distance_db", self.spectral.log_distance_db),
            (
                "smoothed_band_distance_db",
                self.spectral.smoothed_band_distance_db,
            ),
            ("convergence", self.spectral.convergence),
            // Signed: positive is more phasey.
            ("vertical_phasiness_delta", self.vertical_phasiness_delta),
            // Signed: negative is blunter attacks.
            ("flux_peak_change", self.transients.flux_peak_change),
            (
                "flux_peakiness_abs_ln",
                self.transients.flux_peakiness_ratio.ln().abs(),
            ),
            (
                "percussive_share_abs_ln",
                self.transients.percussive_share_ratio.ln().abs(),
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
