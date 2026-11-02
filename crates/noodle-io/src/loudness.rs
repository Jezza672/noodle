//! Loudness (ITU-R BS.1770 / EBU R128 integrated, with gating) and true peak,
//! measured a chunk at a time so an export can read it as it writes.

use std::f64::consts::PI;

/// What a [`LoudnessMeter`] found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// Gated integrated loudness in LUFS; `-inf` for silence, or for audio
    /// shorter than one 400 ms block.
    pub lufs: f64,
    /// The highest level of the signal between its samples, in dBTP
    /// (BS.1770 Annex 2, 4× oversampled). `-inf` for silence.
    pub true_peak_db: f64,
    /// The highest sample, in dBFS. `-inf` for silence.
    pub sample_peak_db: f64,
}

fn to_db(amplitude: f64) -> f64 {
    if amplitude > 0.0 {
        20.0 * amplitude.log10()
    } else {
        f64::NEG_INFINITY
    }
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
    let (f0, gain, q) = (
        1_681.974_450_955_533,
        3.999_843_853_973_347,
        0.707_175_236_955_419_6,
    );
    let k = (PI * f0 / rate).tan();
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
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, high]
}

/// Taps either side of the point being interpolated.
const SIDE: usize = 6;
const TAPS: usize = 2 * SIDE;
/// The oversampling factor, so three points are interpolated between samples.
const OVERSAMPLING: usize = 4;

/// The interpolation filters: windowed sinc at 1/4, 2/4 and 3/4 of the way
/// to the next sample, each scaled to pass DC at exactly 1.
fn interpolators() -> [[f64; TAPS]; OVERSAMPLING - 1] {
    let mut filters = [[0.0; TAPS]; OVERSAMPLING - 1];
    for (p, filter) in filters.iter_mut().enumerate() {
        let f = (p + 1) as f64 / OVERSAMPLING as f64;
        let mut sum = 0.0;
        for (k, tap) in filter.iter_mut().enumerate() {
            // Tap k reads the sample k + 1 - SIDE after the one before the point.
            let t = (k as f64 + 1.0 - SIDE as f64) - f;
            let sinc = if t == 0.0 {
                1.0
            } else {
                (PI * t).sin() / (PI * t)
            };
            let window = 0.5 * (1.0 + (PI * t / SIDE as f64).cos());
            *tap = sinc * window;
            sum += *tap;
        }
        filter.iter_mut().for_each(|tap| *tap /= sum);
    }
    filters
}

/// Measures integrated loudness and peaks over audio pushed in chunks.
///
/// Channels weigh equally, which is right for mono and stereo; the surround
/// weights are not applied.
pub struct LoudnessMeter {
    channels: usize,
    filters: Vec<[Biquad; 2]>,
    interpolators: [[f64; TAPS]; OVERSAMPLING - 1],
    /// The last `TAPS` samples of each channel, oldest first.
    history: Vec<[f64; TAPS]>,
    /// Frames in a 100 ms step, and the energy of the steps so far (the
    /// squares of the filtered samples, summed over channels).
    step: usize,
    steps: Vec<f64>,
    in_step: usize,
    step_energy: f64,
    peak: f64,
    true_peak: f64,
    frames: u64,
}

impl LoudnessMeter {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let rate = f64::from(sample_rate);
        Self {
            channels,
            filters: (0..channels).map(|_| k_weighting(rate)).collect(),
            interpolators: interpolators(),
            history: vec![[0.0; TAPS]; channels],
            step: ((0.1 * rate) as usize).max(1),
            steps: Vec::new(),
            in_step: 0,
            step_energy: 0.0,
            peak: 0.0,
            true_peak: 0.0,
            frames: 0,
        }
    }

    /// Takes interleaved samples. A partial frame at the end is ignored.
    pub fn push(&mut self, samples: &[f32]) {
        for frame in samples.chunks_exact(self.channels) {
            for (ch, &sample) in frame.iter().enumerate() {
                let x = if sample.is_finite() {
                    f64::from(sample)
                } else {
                    0.0
                };
                let shelved = self.filters[ch][0].run(x);
                let y = self.filters[ch][1].run(shelved);
                self.step_energy += y * y;
                self.take(ch, x);
            }
            self.in_step += 1;
            self.frames += 1;
            if self.in_step == self.step {
                self.steps.push(self.step_energy);
                self.step_energy = 0.0;
                self.in_step = 0;
            }
        }
    }

    /// Slides `x` into a channel's history and looks at the points between
    /// the samples that now sit in the middle of it.
    fn take(&mut self, ch: usize, x: f64) {
        self.peak = self.peak.max(x.abs());
        self.true_peak = self.true_peak.max(x.abs());
        let history = &mut self.history[ch];
        history.rotate_left(1);
        history[TAPS - 1] = x;
        for filter in &self.interpolators {
            let y: f64 = filter.iter().zip(history.iter()).map(|(h, s)| h * s).sum();
            self.true_peak = self.true_peak.max(y.abs());
        }
    }

    /// The result for everything pushed.
    pub fn finish(mut self) -> Loudness {
        // Run the interpolation out over the last samples.
        for ch in 0..self.channels {
            for _ in 0..SIDE {
                self.take(ch, 0.0);
            }
        }
        Loudness {
            lufs: self.integrated(),
            true_peak_db: to_db(self.true_peak),
            sample_peak_db: to_db(self.peak),
        }
    }

    /// Frames pushed so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    fn integrated(&self) -> f64 {
        // A block is four steps, a new one every step.
        let block = (4 * self.step) as f64;
        let lufs = |power: f64| -0.691 + 10.0 * power.log10();
        let blocks: Vec<f64> = self
            .steps
            .windows(4)
            .map(|w| w.iter().sum::<f64>() / block)
            .collect();
        let absolute: Vec<f64> = blocks
            .iter()
            .copied()
            .filter(|&p| lufs(p) > -70.0)
            .collect();
        if absolute.is_empty() {
            return f64::NEG_INFINITY;
        }
        let relative = lufs(absolute.iter().sum::<f64>() / absolute.len() as f64) - 10.0;
        let gated: Vec<f64> = absolute
            .into_iter()
            .filter(|&p| lufs(p) > relative)
            .collect();
        lufs(gated.iter().sum::<f64>() / gated.len() as f64)
    }
}

/// [`LoudnessMeter`] over audio in memory.
pub fn measure(samples: &[f32], channels: usize, sample_rate: u32) -> Loudness {
    let mut meter = LoudnessMeter::new(sample_rate, channels);
    meter.push(samples);
    meter.finish()
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
        let l = measure(&sine(1.0, 5.0), 1, 48_000).lufs;
        assert!((l + 3.01).abs() < 0.05, "{l}");
    }

    #[test]
    fn chunks_of_any_size_measure_the_same_as_the_whole() {
        let x = sine(0.4, 3.0);
        let whole = measure(&x, 1, 48_000);
        let mut meter = LoudnessMeter::new(48_000, 1);
        for chunk in x.chunks(777) {
            meter.push(chunk);
        }
        assert_eq!(meter.finish(), whole);
    }

    #[test]
    fn silence_and_quiet_stretches_are_gated_out() {
        let tone = sine(0.5, 3.0);
        let alone = measure(&tone, 1, 48_000).lufs;
        // Silence (absolute gate) and a stretch 30 dB down (relative gate)
        // must not pull the loudness down; without gating this reads 3 or more
        // LU quieter.
        let mut padded = tone.clone();
        padded.extend(std::iter::repeat_n(0.0, 48_000 * 3));
        let with_silence = measure(&padded, 1, 48_000).lufs;
        assert!(
            (with_silence - alone).abs() < 0.5,
            "{with_silence} vs {alone}"
        );
        let mut with_quiet = tone;
        with_quiet.extend(sine(0.5 * 0.0316, 3.0));
        let l = measure(&with_quiet, 1, 48_000).lufs;
        assert!((l - alone).abs() < 0.5, "{l} vs {alone}");
    }

    #[test]
    fn the_high_pass_stage_discounts_deep_bass() {
        let bass: Vec<f32> = (0..48_000 * 5)
            .map(|n| (2.0 * std::f32::consts::PI * 20.0 * n as f32 / 48_000.0).sin())
            .collect();
        // Without the 38 Hz high-pass a 20 Hz tone reads about -3.7 LUFS.
        let l = measure(&bass, 1, 48_000).lufs;
        assert!(l < -8.0, "{l}");
    }

    #[test]
    fn silence_and_short_audio_have_no_loudness() {
        assert_eq!(measure(&[0.0; 48_000], 1, 48_000).lufs, f64::NEG_INFINITY);
        assert_eq!(measure(&sine(1.0, 0.2), 1, 48_000).lufs, f64::NEG_INFINITY);
        let silent = measure(&[0.0; 4_800], 1, 48_000);
        assert_eq!(silent.true_peak_db, f64::NEG_INFINITY);
    }

    #[test]
    fn true_peak_finds_what_lies_between_samples() {
        // A sine at a quarter of the sample rate with its samples off the
        // crests: every sample is at 0.707, the waveform reaches 1.0.
        let x: Vec<f32> = (0..48_000)
            .map(|n| {
                (std::f64::consts::FRAC_PI_2 * f64::from(n) + std::f64::consts::FRAC_PI_4).sin()
                    as f32
            })
            .collect();
        let l = measure(&x, 1, 48_000);
        assert!((l.sample_peak_db + 3.01).abs() < 0.05, "{l:?}");
        assert!(l.true_peak_db > -0.3 && l.true_peak_db < 0.3, "{l:?}");
        // On a sine whose samples are not the crests, true peak never reads
        // below the sample peak.
        let y = measure(&sine(0.5, 1.0), 1, 48_000);
        assert!(y.true_peak_db >= y.sample_peak_db - 1e-9, "{y:?}");
    }
}
