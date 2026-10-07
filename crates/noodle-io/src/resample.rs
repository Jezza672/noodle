//! Converting audio to another sample rate, for importing a file recorded at
//! a different rate from the engine's.

use std::fmt;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use crate::Audio;

#[derive(Debug)]
pub struct ResampleError(String);

impl fmt::Display for ResampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "can't change the sample rate: {}", self.0)
    }
}

impl std::error::Error for ResampleError {}

/// Returns `audio` at `rate`, using band-limited sinc interpolation. The
/// output is trimmed so that it starts when the input does, and is as long as
/// the input is at the new rate (to the nearest frame).
pub fn resample(audio: &Audio, rate: u32) -> Result<Audio, ResampleError> {
    if audio.sample_rate == rate || audio.samples.is_empty() || audio.channels == 0 {
        return Ok(Audio {
            samples: audio.samples.clone(),
            channels: audio.channels,
            sample_rate: rate,
        });
    }
    let err = |e: &dyn fmt::Display| ResampleError(e.to_string());
    let channels = audio.channels;
    let frames = audio.samples.len() / channels;
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: Some(0.95),
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let mut resampler = Async::<f32>::new_sinc(
        f64::from(rate) / f64::from(audio.sample_rate),
        1.1,
        &params,
        1024,
        channels,
        FixedAsync::Input,
    )
    .map_err(|e| err(&e))?;
    let input = InterleavedSlice::new(&audio.samples, channels, frames).map_err(|e| err(&e))?;
    let out = resampler
        .process_all(&input, frames, None)
        .map_err(|e| err(&e))?;
    let mut samples = out.take_data();
    let wanted = (frames as f64 * f64::from(rate) / f64::from(audio.sample_rate)).round() as usize;
    samples.resize(wanted * channels, 0.0);
    Ok(Audio {
        samples,
        channels,
        sample_rate: rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, frames: usize) -> Audio {
        let samples = (0..frames)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / rate as f32).sin() * 0.8)
            .collect();
        Audio {
            samples,
            channels: 1,
            sample_rate: rate,
        }
    }

    /// Frequency by counting rising zero crossings over the middle of `audio`.
    fn measured_freq(audio: &Audio) -> f32 {
        let s = &audio.samples;
        let (from, to) = (s.len() / 4, s.len() * 3 / 4);
        let crossings = (from + 1..to)
            .filter(|&i| s[i - 1] < 0.0 && s[i] >= 0.0)
            .count();
        crossings as f32 * audio.sample_rate as f32 / (to - from) as f32
    }

    #[test]
    fn pitch_and_length_survive_a_rate_change() {
        for (from, to) in [(44_100, 48_000), (48_000, 44_100), (22_050, 48_000)] {
            let out = resample(&sine(1000.0, from, from as usize), to).unwrap();
            assert_eq!(out.sample_rate, to);
            assert_eq!(out.samples.len(), to as usize, "{from} -> {to}");
            let f = measured_freq(&out);
            assert!((f - 1000.0).abs() < 5.0, "{from} -> {to}: {f} Hz");
        }
    }

    #[test]
    fn level_and_alignment_are_kept() {
        let out = resample(&sine(440.0, 44_100, 44_100), 48_000).unwrap();
        let peak = out.samples[4000..40_000]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.8).abs() < 0.02, "peak {peak}");
        // The sine starts at zero going up, so the resampled one must too.
        // Early frames follow the input's phase rather than being delayed.
        let expected = (std::f32::consts::TAU * 440.0 * 100.0 / 48_000.0).sin() * 0.8;
        assert!(
            (out.samples[100] - expected).abs() < 0.02,
            "{}",
            out.samples[100]
        );
    }

    fn rms(audio: &Audio) -> f32 {
        let s = &audio.samples[audio.samples.len() / 4..audio.samples.len() * 3 / 4];
        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    #[test]
    fn high_tones_pass_and_ones_above_the_new_nyquist_are_filtered() {
        // 10 kHz is well inside the band of both rates.
        let kept = resample(&sine(10_000.0, 44_100, 44_100), 48_000).unwrap();
        assert!(rms(&kept) > 0.5, "{}", rms(&kept));
        // 15 kHz can't exist at 22.05 kHz; it must be removed, not aliased.
        let gone = resample(&sine(15_000.0, 48_000, 48_000), 22_050).unwrap();
        assert!(rms(&gone) < 0.01, "{}", rms(&gone));
    }

    #[test]
    fn channels_stay_apart() {
        let left = sine(500.0, 44_100, 4410);
        let samples = left.samples.iter().flat_map(|&s| [s, 0.0]).collect();
        let out = resample(
            &Audio {
                samples,
                channels: 2,
                sample_rate: 44_100,
            },
            48_000,
        )
        .unwrap();
        assert_eq!(out.channels, 2);
        assert!(out.samples.chunks(2).all(|f| f[1] == 0.0));
        assert!(out.samples.chunks(2).any(|f| f[0].abs() > 0.5));
    }

    #[test]
    fn the_same_rate_or_no_audio_is_a_copy() {
        let a = sine(100.0, 48_000, 500);
        assert_eq!(resample(&a, 48_000).unwrap(), a);
        let empty = Audio {
            samples: vec![],
            channels: 2,
            sample_rate: 44_100,
        };
        assert!(resample(&empty, 48_000).unwrap().samples.is_empty());
    }
}
