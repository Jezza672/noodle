//! Loudness (ITU-R BS.1770 / EBU R128 integrated, with gating) and peak.

use crate::clip::Clip;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Level {
    pub reference_lufs: f64,
    pub test_lufs: f64,
    /// Test minus reference, in LU.
    pub lufs_delta: f64,
    /// Test minus reference sample peak, in dB.
    pub peak_delta_db: f64,
}

pub fn compare(reference: Clip, test: Clip) -> Level {
    let (r, t) = (integrated_lufs(reference), integrated_lufs(test));
    Level {
        reference_lufs: r,
        test_lufs: t,
        lufs_delta: t - r,
        peak_delta_db: peak_db(test) - peak_db(reference),
    }
}

pub fn peak_db(c: Clip) -> f64 {
    let p = c.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    20.0 * f64::from(p.max(1e-9)).log10()
}

/// Gated integrated loudness in LUFS; channels are weighted equally (the
/// surround weights do not apply to mono and stereo). Silence is -inf. The
/// measurement itself is `noodle_io::LoudnessMeter`, which export uses too.
pub fn integrated_lufs(c: Clip) -> f64 {
    noodle_io::measure_loudness(c.samples, c.channels, c.rate).lufs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, seconds: f32) -> Vec<f32> {
        (0..(48_000.0 * seconds) as usize)
            .map(|n| amp * (2.0 * std::f32::consts::PI * 997.0 * n as f32 / 48_000.0).sin())
            .collect()
    }

    #[test]
    fn six_db_quieter_is_six_lu_down() {
        let (a, b) = (sine(1.0, 5.0), sine(0.5, 5.0));
        let d = compare(Clip::new(&a, 1, 48_000), Clip::new(&b, 1, 48_000));
        assert!((d.lufs_delta + 6.02).abs() < 0.05, "{d:?}");
        assert!((d.peak_delta_db + 6.02).abs() < 0.05, "{d:?}");
    }
}
