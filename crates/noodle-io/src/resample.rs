//! Converting audio to another sample rate: whole files, for importing one
//! recorded at a different rate from the engine's, and a continuous stream,
//! for a device that runs at a different rate from the engine.

use std::fmt;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

const SINC_PARAMETERS: SincInterpolationParameters = SincInterpolationParameters {
    sinc_len: 128,
    f_cutoff: Some(0.95),
    interpolation: SincInterpolationType::Cubic,
    oversampling_factor: 128,
    window: WindowFunction::BlackmanHarris2,
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
    let mut resampler = Async::<f32>::new_sinc(
        f64::from(rate) / f64::from(audio.sample_rate),
        1.1,
        &SINC_PARAMETERS,
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

/// Converts audio that arrives as a continuous stream, in interleaved pieces
/// of any size, from one rate to another. It buffers input until it has a
/// chunk to convert, so the output comes in bursts and lags by a chunk and the
/// filter's delay (see [`delay`](Self::delay)).
///
/// Creating one allocates, and so can processing (the buffers grow to the
/// largest piece they are given), so use it off the audio thread.
pub struct StreamResampler {
    inner: Async<f32>,
    channels: usize,
    /// Input frames per conversion.
    chunk: usize,
    pending: Vec<f32>,
    out: Vec<f32>,
}

impl StreamResampler {
    /// A converter from `from` Hz to `to` Hz that works `chunk` input frames
    /// at a time.
    pub fn new(from: u32, to: u32, channels: usize, chunk: usize) -> Result<Self, ResampleError> {
        let err = |e: &dyn fmt::Display| ResampleError(e.to_string());
        if from == 0 || to == 0 || channels == 0 || chunk == 0 {
            return Err(ResampleError("nothing to convert".into()));
        }
        let inner = Async::<f32>::new_sinc(
            f64::from(to) / f64::from(from),
            1.1,
            &SINC_PARAMETERS,
            chunk,
            channels,
            FixedAsync::Input,
        )
        .map_err(|e| err(&e))?;
        let out = vec![0.0; inner.output_frames_max() * channels];
        Ok(Self {
            inner,
            channels,
            chunk,
            pending: Vec::with_capacity(chunk * channels * 2),
            out,
        })
    }

    /// How many output frames the first sound is late by, beyond the chunk
    /// being filled.
    pub fn delay(&self) -> usize {
        self.inner.output_delay()
    }

    /// Takes `input` (whole interleaved frames) and passes each block that
    /// can be converted to `emit`, as interleaved frames at the new rate.
    pub fn process(&mut self, input: &[f32], mut emit: impl FnMut(&[f32])) {
        let channels = self.channels;
        self.pending
            .extend_from_slice(&input[..input.len() / channels * channels]);
        let mut used = 0;
        let size = self.chunk * channels;
        while self.pending.len() - used >= size {
            let frames = self.out.len() / channels;
            let converted = {
                let source =
                    InterleavedSlice::new(&self.pending[used..used + size], channels, self.chunk);
                let target = InterleavedSlice::new_mut(&mut self.out, channels, frames);
                match (source, target) {
                    (Ok(source), Ok(mut target)) => self
                        .inner
                        .process_into_buffer(&source, &mut target, None)
                        .map(|(_, written)| written)
                        .ok(),
                    _ => None,
                }
            };
            used += size;
            if let Some(written) = converted {
                emit(&self.out[..written * channels]);
            }
        }
        self.pending.drain(..used);
    }
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

    #[test]
    fn a_stream_in_odd_pieces_keeps_its_pitch_and_length() {
        for (from, to) in [(48_000u32, 44_100u32), (44_100, 48_000), (48_000, 96_000)] {
            let audio = sine(1000.0, from, from as usize);
            let mut stream = StreamResampler::new(from, to, 1, 128).unwrap();
            let mut out = Vec::new();
            let mut at = 0;
            let mut size = 1;
            while at < audio.samples.len() {
                let end = (at + size).min(audio.samples.len());
                stream.process(&audio.samples[at..end], |block| {
                    out.extend_from_slice(block)
                });
                at = end;
                size = size % 700 + 37;
            }
            // Nearly all of a second comes out, less what is still buffered.
            let expected = to as usize;
            assert!(
                out.len() <= expected && out.len() + 128 * 2 + stream.delay() >= expected - 4,
                "{from} -> {to}: {} of {expected}",
                out.len()
            );
            let f = measured_freq(&Audio {
                samples: out,
                channels: 1,
                sample_rate: to,
            });
            assert!((f - 1000.0).abs() < 5.0, "{from} -> {to}: {f} Hz");
        }
    }

    #[test]
    fn a_stream_keeps_channels_apart() {
        let mut stream = StreamResampler::new(48_000, 44_100, 2, 128).unwrap();
        let input: Vec<f32> = (0..4800).flat_map(|i| [0.5, -(i as f32 * 0.0)]).collect();
        let mut out = Vec::new();
        stream.process(&input, |block| out.extend_from_slice(block));
        // After the filter has settled the left holds a steady 0.5.
        let settled = &out[out.len() / 4 * 2..];
        for frame in settled.chunks(2) {
            assert!(
                (frame[0] - 0.5).abs() < 0.01 && frame[1].abs() < 0.01,
                "{frame:?}"
            );
        }
    }
}
