//! The spectrogram-mask runtime: LarsNet and Timbrica.
//!
//! One network per part. The mix is turned into a spectrogram once, cut into
//! overlapping chunks of the number of frames the network takes, each chunk
//! goes through every network, and the results are blended back (triangular
//! weights) into one gain per part, which is applied to the mix's complex
//! spectrogram, so the part keeps the mix's phase. The inverse STFT then gives
//! the part's audio. A [`DrumPart::Rest`] output is the mix minus the other
//! parts.

use crate::drum::stft::{Spectrogram, Stft};
use crate::drum::{
    DrumModelManifest, DrumPart, Layout, MaskOutput, PartSeparator, chunk_starts, triangle,
};
use crate::{ModelId, SeparateError, Stereo};

/// One part's network.
pub trait MaskNet: Send {
    /// A magnitude spectrogram `[channel (2)][bin][frame]` in, the same shape
    /// out. The network is asked for exactly the manifest's
    /// `bins` x `frames`.
    fn run(&mut self, magnitude: &[f32]) -> Result<Vec<f32>, SeparateError>;
}

pub struct MaskSeparator {
    manifest: &'static DrumModelManifest,
    /// One per file of the manifest, in order.
    nets: Vec<Box<dyn MaskNet>>,
    stft: Stft,
    output: MaskOutput,
}

impl MaskSeparator {
    /// `nets` are one per file of `manifest`, in the file order.
    pub fn new(
        manifest: &'static DrumModelManifest,
        nets: Vec<Box<dyn MaskNet>>,
    ) -> Result<Self, SeparateError> {
        let Layout::PerPartSpectrogram(output) = manifest.layout else {
            return Err(SeparateError::Failed(format!(
                "{} is not a per-part spectrogram model",
                manifest.name
            )));
        };
        if nets.len() != manifest.files.len() {
            return Err(SeparateError::Failed(format!(
                "{} needs {} networks, got {}",
                manifest.name,
                manifest.files.len(),
                nets.len()
            )));
        }
        Ok(Self {
            manifest,
            nets,
            stft: Stft::new(manifest.chunking.stft_size, manifest.chunking.hop),
            output,
        })
    }

    /// The part each network makes, in order.
    fn net_parts(&self) -> Vec<DrumPart> {
        self.manifest.files.iter().filter_map(|f| f.part).collect()
    }
}

impl PartSeparator for MaskSeparator {
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
        let chunking = self.manifest.chunking;
        let (bins, model_frames) = (chunking.bins(), chunking.frames());
        let channels = [&mix.left, &mix.right];
        let specs: Vec<Spectrogram> = channels.iter().map(|c| self.stft.forward(c)).collect();
        let frames = specs[0].frames;
        let starts = chunk_starts(frames, model_frames, model_frames / 2);
        let steps = (starts.len() * self.nets.len()).max(1);
        let mut done = 0;

        let mut results = Vec::new();
        for (net_index, part) in self.net_parts().into_iter().enumerate() {
            // Gain per channel, bin, frame, and the weight that built it.
            let mut gain = vec![0.0f32; 2 * bins * frames];
            let mut weight = vec![0.0f32; frames];
            for &start in &starts {
                if !progress(done as f32 / steps as f32) {
                    return Err(SeparateError::Cancelled);
                }
                done += 1;
                let len = (frames - start).min(model_frames);
                let mut input = vec![0.0f32; 2 * bins * model_frames];
                for (c, spec) in specs.iter().enumerate() {
                    for b in 0..bins {
                        for f in 0..len {
                            input[(c * bins + b) * model_frames + f] = spec.at(b, start + f).norm();
                        }
                    }
                }
                let out = self.nets[net_index].run(&input)?;
                if out.len() != input.len() {
                    return Err(SeparateError::Failed(format!(
                        "{} gave {} values where {} were expected",
                        self.manifest.name,
                        out.len(),
                        input.len()
                    )));
                }
                for c in 0..2 {
                    for b in 0..bins {
                        for f in 0..len {
                            let i = (c * bins + b) * model_frames + f;
                            let g = match self.output {
                                MaskOutput::Mask => out[i],
                                MaskOutput::Magnitude => out[i] / input[i].max(1e-6),
                            };
                            gain[(c * bins + b) * frames + start + f] +=
                                g.clamp(0.0, 1.5) * triangle(f, model_frames);
                        }
                    }
                }
                for f in 0..len {
                    weight[start + f] += triangle(f, model_frames);
                }
            }
            let mut out = Stereo::silent(mix.len());
            for (c, spec) in specs.iter().enumerate() {
                let mut part_spec = Spectrogram::zeros(spec.bins, frames);
                for f in 0..frames {
                    for b in 0..spec.bins {
                        // The network never sees the Nyquist bin: it takes the
                        // gain of the bin below.
                        let source = b.min(bins - 1);
                        let g = gain[(c * bins + source) * frames + f] / weight[f].max(1e-12);
                        part_spec.set(b, f, spec.at(b, f) * g);
                    }
                }
                let audio = self.stft.inverse(&part_spec, mix.len());
                if c == 0 {
                    out.left = audio;
                } else {
                    out.right = audio;
                }
            }
            results.push((part, out));
        }

        if self.manifest.parts.contains(&DrumPart::Rest) {
            let mut rest = mix.clone();
            for (_, part) in &results {
                for i in 0..mix.len() {
                    rest.left[i] -= part.left[i];
                    rest.right[i] -= part.right[i];
                }
            }
            results.push((DrumPart::Rest, rest));
        }
        // Each result in the manifest's order.
        results.sort_by_key(|(p, _)| {
            self.manifest
                .parts
                .iter()
                .position(|q| q == p)
                .unwrap_or(usize::MAX)
        });
        if !progress(1.0) {
            return Err(SeparateError::Cancelled);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drum::{LARSNET, TIMBRICA};

    /// A network that returns its input scaled: a mask of `k` as a magnitude
    /// estimate of `k` times the mix.
    struct Scale(f32);

    impl MaskNet for Scale {
        fn run(&mut self, magnitude: &[f32]) -> Result<Vec<f32>, SeparateError> {
            Ok(magnitude.iter().map(|m| m * self.0).collect())
        }
    }

    /// A network that ignores its input: a mask of `k` everywhere.
    struct Constant(f32);

    impl MaskNet for Constant {
        fn run(&mut self, magnitude: &[f32]) -> Result<Vec<f32>, SeparateError> {
            Ok(vec![self.0; magnitude.len()])
        }
    }

    fn signal(frames: usize) -> Stereo {
        Stereo {
            left: (0..frames)
                .map(|i| (i as f32 * 0.07).sin() * 0.4 + (i as f32 * 0.9).sin() * 0.1)
                .collect(),
            right: (0..frames).map(|i| (i as f32 * 0.05).cos() * 0.3).collect(),
        }
    }

    fn nets(scales: &[f32]) -> Vec<Box<dyn MaskNet>> {
        scales
            .iter()
            .map(|&s| Box::new(Constant(s)) as Box<dyn MaskNet>)
            .collect()
    }

    fn max_diff(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn each_net_makes_its_part_in_manifest_order() {
        // Longer than one chunk (512 frames of 1024 = 524288 samples), so the
        // chunks are blended and the gain is the same across the seam.
        let mix = signal(700_000);
        let mut separator = MaskSeparator::new(&LARSNET, nets(&[0.1, 0.2, 0.3, 0.4, 0.5])).unwrap();
        let parts = separator.separate(&mix, &mut |_| true).unwrap();
        let named: Vec<_> = parts.iter().map(|(p, _)| *p).collect();
        assert_eq!(named, LARSNET.parts);
        for ((_, audio), scale) in parts.iter().zip([0.1, 0.2, 0.3, 0.4, 0.5]) {
            assert_eq!(audio.len(), mix.len());
            let want: Vec<f32> = mix.left.iter().map(|x| x * scale).collect();
            assert!(max_diff(&audio.left, &want) < 2e-3, "scale {scale}");
        }
    }

    #[test]
    fn a_magnitude_output_is_an_estimate_of_the_part() {
        let mut manifest = LARSNET;
        manifest.layout = Layout::PerPartSpectrogram(MaskOutput::Magnitude);
        let manifest: &'static DrumModelManifest = Box::leak(Box::new(manifest));
        let mix = signal(30_000);
        let nets: Vec<Box<dyn MaskNet>> = [0.1, 0.2, 0.3, 0.4, 0.5]
            .into_iter()
            .map(|k| Box::new(Scale(k)) as _)
            .collect();
        let mut separator = MaskSeparator::new(manifest, nets).unwrap();
        let parts = separator.separate(&mix, &mut |_| true).unwrap();
        for ((_, audio), k) in parts.iter().zip([0.1f32, 0.2, 0.3, 0.4, 0.5]) {
            let want: Vec<f32> = mix.left.iter().map(|x| x * k).collect();
            assert!(max_diff(&audio.left, &want) < 2e-3, "scale {k}");
        }
    }

    #[test]
    fn a_mask_output_is_a_gain_on_the_mix() {
        let manifest = &LARSNET;
        let mix = signal(30_000);
        let nets: Vec<Box<dyn MaskNet>> = [1.0, 0.5, 0.0, 0.25, 0.75]
            .into_iter()
            .map(|k| Box::new(Constant(k)) as _)
            .collect();
        let mut separator = MaskSeparator::new(manifest, nets).unwrap();
        // A mask of 1.0 is the mix itself, whatever the magnitudes were.
        let parts = separator.separate(&mix, &mut |_| true).unwrap();
        assert!(max_diff(&parts[0].1.left, &mix.left) < 1e-3);
        assert!(parts[2].1.left.iter().all(|x| x.abs() < 1e-6));
    }

    #[test]
    fn the_remainder_is_the_mix_minus_the_other_parts() {
        let mix = signal(20_000);
        let mut separator = MaskSeparator::new(&TIMBRICA, nets(&[0.2, 0.3, 0.1])).unwrap();
        let parts = separator.separate(&mix, &mut |_| true).unwrap();
        let named: Vec<_> = parts.iter().map(|(p, _)| *p).collect();
        assert_eq!(named, TIMBRICA.parts);
        let rest = &parts[3].1;
        let want: Vec<f32> = mix.left.iter().map(|x| x * (1.0 - 0.6)).collect();
        assert!(max_diff(&rest.left, &want) < 2e-3);
        // Parts plus the remainder add back up to the mix.
        for i in (0..mix.len()).step_by(997) {
            let sum: f32 = parts.iter().map(|(_, p)| p.left[i]).sum();
            assert!((sum - mix.left[i]).abs() < 1e-4);
        }
    }

    #[test]
    fn a_short_clip_is_padded_to_one_chunk() {
        let mix = signal(3_000);
        let mut separator = MaskSeparator::new(&LARSNET, nets(&[1.0, 1.0, 1.0, 1.0, 1.0])).unwrap();
        let parts = separator.separate(&mix, &mut |_| true).unwrap();
        assert!(parts.iter().all(|(_, p)| p.len() == 3_000));
        assert!(max_diff(&parts[1].1.right, &mix.right) < 1e-3);
    }

    #[test]
    fn cancelling_stops_between_chunks() {
        let mix = signal(700_000);
        let mut separator = MaskSeparator::new(&LARSNET, nets(&[1.0, 1.0, 1.0, 1.0, 1.0])).unwrap();
        let mut calls = 0;
        let result = separator.separate(&mix, &mut |_| {
            calls += 1;
            calls < 3
        });
        assert_eq!(result.err(), Some(SeparateError::Cancelled));
        assert_eq!(calls, 3);
    }

    #[test]
    fn the_wrong_number_of_nets_or_layout_is_refused() {
        assert!(MaskSeparator::new(&TIMBRICA, nets(&[1.0])).is_err());
        assert!(MaskSeparator::new(&crate::drum::DRUMSEP, nets(&[1.0])).is_err());
    }

    #[test]
    fn a_net_that_returns_the_wrong_size_is_an_error_not_a_panic() {
        struct Short;
        impl MaskNet for Short {
            fn run(&mut self, _: &[f32]) -> Result<Vec<f32>, SeparateError> {
                Ok(vec![0.0; 3])
            }
        }
        let nets: Vec<Box<dyn MaskNet>> = (0..5).map(|_| Box::new(Short) as _).collect();
        let mut separator = MaskSeparator::new(&LARSNET, nets).unwrap();
        let error = separator
            .separate(&signal(1000), &mut |_| true)
            .err()
            .unwrap();
        assert!(matches!(error, SeparateError::Failed(_)));
    }
}
