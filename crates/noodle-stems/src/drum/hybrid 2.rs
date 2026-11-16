//! The Hybrid Demucs runtime: drumsep.
//!
//! The network takes a waveform and a complex-as-channels spectrogram of the
//! same segment and returns, per part, a spectrogram branch and a time
//! branch; the output is the inverse STFT of the first plus the second. The
//! STFT and its inverse are here, with Demucs's padding: the segment is
//! padded by `3/2 hop` on the left and enough on the right that a centred
//! STFT has two extra frames at each end, which are dropped, leaving
//! `ceil(len / hop)` frames. Segments overlap and are blended with triangular
//! weights, on audio scaled to zero mean and unit variance.

use realfft::num_complex::Complex32;

use crate::drum::stft::{Spectrogram, Stft, reflect_pad};
use crate::drum::{DrumModelManifest, DrumPart, Layout, PartSeparator, chunk_starts, triangle};
use crate::{ModelId, SeparateError, Stereo};

/// What the network returns for one segment.
pub struct HybridOutput {
    /// `[part][4][bin][frame]`.
    pub spectrum: Vec<f32>,
    /// `[part][2][sample]`.
    pub time: Vec<f32>,
}

pub trait HybridNet: Send {
    /// `wave` is `[2][segment]`; `spectrum` is `[4][bin][frame]` (left real,
    /// left imaginary, right real, right imaginary), `bin` being the STFT's
    /// without its Nyquist bin.
    fn run(&mut self, wave: &[f32], spectrum: &[f32]) -> Result<HybridOutput, SeparateError>;
}

pub struct HybridSeparator {
    manifest: &'static DrumModelManifest,
    net: Box<dyn HybridNet>,
    stft: Stft,
}

impl HybridSeparator {
    pub fn new(
        manifest: &'static DrumModelManifest,
        net: Box<dyn HybridNet>,
    ) -> Result<Self, SeparateError> {
        if manifest.layout != Layout::HybridDemucs {
            return Err(SeparateError::Failed(format!(
                "{} is not a Hybrid Demucs model",
                manifest.name
            )));
        }
        Ok(Self {
            manifest,
            net,
            stft: Stft::new(manifest.chunking.stft_size, manifest.chunking.hop),
        })
    }

    /// The spectrogram input for one segment of one channel: `[bin][frame]`
    /// real and imaginary, each `bins * le`.
    fn encode(&self, segment: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let c = self.manifest.chunking;
        let le = c.segment_samples.div_ceil(c.hop);
        let pad = c.hop / 2 * 3;
        let padded = reflect_pad(segment, pad, pad + le * c.hop - segment.len());
        let spec = self.stft.forward(&padded);
        let bins = c.bins();
        let mut re = vec![0.0; bins * le];
        let mut im = vec![0.0; bins * le];
        for b in 0..bins {
            for f in 0..le {
                let v = spec.at(b, f + 2);
                re[b * le + f] = v.re;
                im[b * le + f] = v.im;
            }
        }
        (re, im)
    }

    /// The audio of one channel from a spectrogram branch (`re`, `im`, each
    /// `[bin][frame]`), `len` samples.
    fn decode(&self, re: &[f32], im: &[f32], len: usize) -> Vec<f32> {
        let c = self.manifest.chunking;
        let le = c.segment_samples.div_ceil(c.hop);
        let pad = c.hop / 2 * 3;
        let mut spec = Spectrogram::zeros(self.stft.bins(), le + 4);
        for b in 0..c.bins() {
            for f in 0..le {
                spec.set(b, f + 2, Complex32::new(re[b * le + f], im[b * le + f]));
            }
        }
        let audio = self.stft.inverse(&spec, le * c.hop + 2 * pad);
        audio[pad..pad + len].to_vec()
    }
}

impl PartSeparator for HybridSeparator {
    fn model(&self) -> ModelId {
        ModelId {
            id: self.manifest.id.into(),
            version: self.manifest.version.into(),
        }
    }

    fn sample_rate(&self) -> u32 {
        self.manifest.sample_rate
    }

    fn parts(&self) -> Vec<DrumPart> {
        self.manifest.parts.to_vec()
    }

    fn separate(
        &mut self,
        mix: &Stereo,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<Vec<(DrumPart, Stereo)>, SeparateError> {
        let c = self.manifest.chunking;
        let (segment, bins) = (c.segment_samples, c.bins());
        let le = segment.div_ceil(c.hop);
        let parts = self.manifest.parts;
        let frames = mix.len();

        let mono: Vec<f32> = mix
            .left
            .iter()
            .zip(&mix.right)
            .map(|(l, r)| 0.5 * (l + r))
            .collect();
        let n = frames.max(1) as f32;
        let mean = mono.iter().sum::<f32>() / n;
        let variance = mono.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
        let std = variance.sqrt().max(1e-8);

        let mut sum: Vec<Stereo> = parts.iter().map(|_| Stereo::silent(frames)).collect();
        let mut total = vec![0.0f32; frames];
        let starts = chunk_starts(frames, segment, segment / 2);
        for (n_chunk, &start) in starts.iter().enumerate() {
            if !progress(n_chunk as f32 / starts.len() as f32) {
                return Err(SeparateError::Cancelled);
            }
            let len = (frames - start).min(segment);
            let mut wave = vec![0.0f32; 2 * segment];
            for i in 0..len {
                wave[i] = (mix.left[start + i] - mean) / std;
                wave[segment + i] = (mix.right[start + i] - mean) / std;
            }
            let mut spectrum = vec![0.0f32; 4 * bins * le];
            for ch in 0..2 {
                let (re, im) = self.encode(&wave[ch * segment..(ch + 1) * segment]);
                spectrum[(ch * 2) * bins * le..(ch * 2 + 1) * bins * le].copy_from_slice(&re);
                spectrum[(ch * 2 + 1) * bins * le..(ch * 2 + 2) * bins * le].copy_from_slice(&im);
            }
            let out = self.net.run(&wave, &spectrum)?;
            if out.spectrum.len() != parts.len() * 4 * bins * le
                || out.time.len() != parts.len() * 2 * segment
            {
                return Err(SeparateError::Failed(format!(
                    "{} gave {} + {} values where {} + {} were expected",
                    self.manifest.name,
                    out.spectrum.len(),
                    out.time.len(),
                    parts.len() * 4 * bins * le,
                    parts.len() * 2 * segment,
                )));
            }
            for (p, target) in sum.iter_mut().enumerate() {
                for ch in 0..2 {
                    let base = (p * 4 + ch * 2) * bins * le;
                    let audio = self.decode(
                        &out.spectrum[base..base + bins * le],
                        &out.spectrum[base + bins * le..base + 2 * bins * le],
                        segment,
                    );
                    let time = &out.time[(p * 2 + ch) * segment..][..segment];
                    let to = if ch == 0 {
                        &mut target.left
                    } else {
                        &mut target.right
                    };
                    for i in 0..len {
                        to[start + i] += (audio[i] + time[i]) * triangle(i, segment);
                    }
                }
            }
            for i in 0..len {
                total[start + i] += triangle(i, segment);
            }
        }
        for target in &mut sum {
            for channel in [&mut target.left, &mut target.right] {
                for (i, x) in channel.iter_mut().enumerate() {
                    *x = *x / total[i].max(1e-12) * std + mean;
                }
            }
        }
        if !progress(1.0) {
            return Err(SeparateError::Cancelled);
        }
        Ok(parts.iter().copied().zip(sum).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drum::{Chunking, DRUMSEP, LARSNET};

    /// A small drumsep-shaped manifest, so a test segment is quick.
    fn small() -> &'static DrumModelManifest {
        let mut m = DRUMSEP;
        m.chunking = Chunking {
            segment_samples: 8192,
            stft_size: 512,
            hop: 128,
        };
        Box::leak(Box::new(m))
    }

    fn signal(frames: usize) -> Stereo {
        Stereo {
            left: (0..frames)
                .map(|i| (i as f32 * 0.07).sin() * 0.4 + 0.1)
                .collect(),
            right: (0..frames).map(|i| (i as f32 * 0.05).cos() * 0.3).collect(),
        }
    }

    /// Part k = (k + 1) / 8 of the input through the time branch alone.
    struct TimeOnly;

    impl HybridNet for TimeOnly {
        fn run(&mut self, wave: &[f32], spectrum: &[f32]) -> Result<HybridOutput, SeparateError> {
            let segment = wave.len() / 2;
            let parts = 4;
            let bins = spectrum.len() / 4 / (segment / 128);
            let le = segment / 128;
            let mut time = Vec::new();
            for p in 0..parts {
                for ch in 0..2 {
                    time.extend(
                        wave[ch * segment..(ch + 1) * segment]
                            .iter()
                            .map(|x| x * (p + 1) as f32 / 8.0),
                    );
                }
            }
            Ok(HybridOutput {
                spectrum: vec![0.0; parts * 4 * bins * le],
                time,
            })
        }
    }

    /// The spectrogram branch alone, first part passing the input through.
    struct SpectrumOnly;

    impl HybridNet for SpectrumOnly {
        fn run(&mut self, wave: &[f32], spectrum: &[f32]) -> Result<HybridOutput, SeparateError> {
            let segment = wave.len() / 2;
            let mut out = spectrum.to_vec();
            out.extend(vec![0.0; 3 * spectrum.len()]);
            Ok(HybridOutput {
                spectrum: out,
                time: vec![0.0; 4 * 2 * segment],
            })
        }
    }

    fn max_diff(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn the_time_branch_is_blended_across_segments() {
        // Two and a bit segments, so there are overlaps.
        let mix = signal(20_000);
        let mut separator = HybridSeparator::new(small(), Box::new(TimeOnly)).unwrap();
        let parts = separator.separate(&mix, &mut |_| true).unwrap();
        let named: Vec<_> = parts.iter().map(|(p, _)| *p).collect();
        assert_eq!(named, DRUMSEP.parts);
        // Parts are made from the mix scaled to zero mean, so the mean is the
        // one thing the gain does not touch.
        let mean = mix
            .left
            .iter()
            .zip(&mix.right)
            .map(|(l, r)| 0.5 * (l + r))
            .sum::<f32>()
            / mix.len() as f32;
        for (k, (_, audio)) in parts.iter().enumerate() {
            let want: Vec<f32> = mix
                .left
                .iter()
                .map(|x| (x - mean) * (k + 1) as f32 / 8.0 + mean)
                .collect();
            assert!(max_diff(&audio.left, &want) < 1e-4, "part {k}");
        }
    }

    #[test]
    fn the_spectrogram_branch_round_trips_through_the_padding() {
        let mix = signal(20_000);
        let mut separator = HybridSeparator::new(small(), Box::new(SpectrumOnly)).unwrap();
        let parts = separator.separate(&mix, &mut |_| true).unwrap();
        // The two frames dropped at each end of a segment (as Demucs drops
        // them) cost the first and last few hundred samples of the whole clip;
        // inside, the round trip is close (the Nyquist bin is not passed).
        let inside = 600..mix.len() - 600;
        assert!(max_diff(&parts[0].1.left[inside.clone()], &mix.left[inside]) < 0.02);
        assert!(parts[1].1.left.iter().map(|x| x.abs()).fold(0.0, f32::max) < 0.5);
    }

    #[test]
    fn a_short_clip_is_one_padded_segment() {
        let mix = signal(1_000);
        let mut separator = HybridSeparator::new(small(), Box::new(TimeOnly)).unwrap();
        let parts = separator.separate(&mix, &mut |_| true).unwrap();
        assert!(parts.iter().all(|(_, p)| p.len() == 1_000));
    }

    #[test]
    fn cancelling_stops_between_segments() {
        let mut separator = HybridSeparator::new(small(), Box::new(TimeOnly)).unwrap();
        let mut calls = 0;
        let result = separator.separate(&signal(40_000), &mut |_| {
            calls += 1;
            calls < 2
        });
        assert_eq!(result.err(), Some(SeparateError::Cancelled));
    }

    #[test]
    fn a_net_with_the_wrong_output_size_is_an_error() {
        struct Wrong;
        impl HybridNet for Wrong {
            fn run(&mut self, _: &[f32], _: &[f32]) -> Result<HybridOutput, SeparateError> {
                Ok(HybridOutput {
                    spectrum: vec![],
                    time: vec![],
                })
            }
        }
        let mut separator = HybridSeparator::new(small(), Box::new(Wrong)).unwrap();
        assert!(separator.separate(&signal(1_000), &mut |_| true).is_err());
        assert!(HybridSeparator::new(&LARSNET, Box::new(TimeOnly)).is_err());
    }
}
