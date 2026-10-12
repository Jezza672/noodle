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

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The two K-weighting stages for `rate` (shelf, then high-pass), with the
/// coefficients derived the way libebur128 does.
fn k_weighting(rate: f64) -> [Biquad; 2] {
    let pi = std::f64::consts::PI;
    let (f0, gain, q) = (
        1_681.974_450_955_533,
        3.999_843_853_973_347,
        0.707_175_236_955_419_6,
    );
    let k = (pi * f0 / rate).tan();
    let vh = 10f64.powf(gain / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let (f0, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
    let k = (pi * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, high]
}

/// Gated integrated loudness in LUFS; channels are weighted equally (the
/// surround weights do not apply to mono and stereo). Silence is -inf.
pub fn integrated_lufs(c: Clip) -> f64 {
    let rate = f64::from(c.rate);
    let block = (0.4 * rate) as usize;
    let step = (0.1 * rate) as usize;
    let frames = c.frames();
    if frames < block {
        return f64::NEG_INFINITY;
    }
    // Mean square per channel per block, from a running sum of the filtered
    // squares.
    let mut squares = vec![vec![0.0f64; frames + 1]; c.channels];
    for (ch, sq) in squares.iter_mut().enumerate() {
        let mut filt = k_weighting(rate);
        let mut acc = 0.0;
        for (i, s) in c.channel(ch).into_iter().enumerate() {
            let shelved = filt[0].run(f64::from(s));
            let y = filt[1].run(shelved);
            acc += y * y;
            sq[i + 1] = acc;
        }
    }
    let blocks: Vec<f64> = (0..=((frames - block) / step))
        .map(|b| {
            let (lo, hi) = (b * step, b * step + block);
            squares.iter().map(|s| (s[hi] - s[lo]) / block as f64).sum()
        })
        .collect();
    let lufs = |power: f64| -0.691 + 10.0 * power.log10();
    let abs: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|&p| lufs(p) > -70.0)
        .collect();
    if abs.is_empty() {
        return f64::NEG_INFINITY;
    }
    let rel_gate = lufs(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
    let gated: Vec<f64> = abs.into_iter().filter(|&p| lufs(p) > rel_gate).collect();
    lufs(gated.iter().sum::<f64>() / gated.len() as f64)
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
    fn full_scale_mono_sine_is_minus_3_lufs() {
        let x = sine(1.0, 5.0);
        let l = integrated_lufs(Clip::new(&x, 1, 48_000));
        assert!((l + 3.01).abs() < 0.05, "{l}");
    }

    #[test]
    fn six_db_quieter_is_six_lu_down() {
        let (a, b) = (sine(1.0, 5.0), sine(0.5, 5.0));
        let d = compare(Clip::new(&a, 1, 48_000), Clip::new(&b, 1, 48_000));
        assert!((d.lufs_delta + 6.02).abs() < 0.05, "{d:?}");
        assert!((d.peak_delta_db + 6.02).abs() < 0.05, "{d:?}");
    }

    #[test]
    fn silence_and_quiet_stretches_are_gated_out() {
        let tone = sine(0.5, 3.0);
        let alone = integrated_lufs(Clip::new(&tone, 1, 48_000));
        // Silence (absolute gate) and a stretch 30 dB down (relative gate)
        // must not pull the loudness down; without gating this reads 3 or more
        // LU quieter.
        let mut padded = tone.clone();
        padded.extend(std::iter::repeat_n(0.0, 48_000 * 3));
        let with_silence = integrated_lufs(Clip::new(&padded, 1, 48_000));
        assert!(
            (with_silence - alone).abs() < 0.5,
            "{with_silence} vs {alone}"
        );
        let mut with_quiet = tone;
        with_quiet.extend(sine(0.5 * 0.0316, 3.0));
        let l = integrated_lufs(Clip::new(&with_quiet, 1, 48_000));
        assert!((l - alone).abs() < 0.5, "{l} vs {alone}");
    }

    #[test]
    fn the_high_pass_stage_discounts_deep_bass() {
        let bass: Vec<f32> = (0..48_000 * 5)
            .map(|n| (2.0 * std::f32::consts::PI * 20.0 * n as f32 / 48_000.0).sin())
            .collect();
        // Without the 38 Hz high-pass a 20 Hz tone reads about -3.7 LUFS.
        let l = integrated_lufs(Clip::new(&bass, 1, 48_000));
        assert!(l < -8.0, "{l}");
    }
}
