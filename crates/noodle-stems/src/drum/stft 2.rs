//! A short-time Fourier transform with a Hann window, for the model front
//! ends. The STFT of each model sits outside its ONNX graph, so it lives here.

use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

/// A spectrogram: `frames` columns of `bins` complex values, frame-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Spectrogram {
    pub bins: usize,
    pub frames: usize,
    pub data: Vec<Complex32>,
}

impl Spectrogram {
    pub fn zeros(bins: usize, frames: usize) -> Self {
        Self {
            bins,
            frames,
            data: vec![Complex32::new(0.0, 0.0); bins * frames],
        }
    }

    pub fn at(&self, bin: usize, frame: usize) -> Complex32 {
        self.data[frame * self.bins + bin]
    }

    pub fn set(&mut self, bin: usize, frame: usize, value: Complex32) {
        self.data[frame * self.bins + bin] = value;
    }
}

pub struct Stft {
    size: usize,
    hop: usize,
    window: Vec<f32>,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
}

/// `x` extended by `pad` samples at each end, mirrored without repeating the
/// edge sample (what `center=True` does). Short inputs repeat the mirror.
pub fn reflect_pad(x: &[f32], left: usize, right: usize) -> Vec<f32> {
    let n = x.len();
    let mut out = Vec::with_capacity(n + left + right);
    let at = |i: isize| -> f32 {
        if n == 0 {
            return 0.0;
        }
        if n == 1 {
            return x[0];
        }
        let period = 2 * (n as isize - 1);
        let mut i = i.rem_euclid(period);
        if i >= n as isize {
            i = period - i;
        }
        x[i as usize]
    };
    for i in 0..left {
        out.push(at(i as isize - left as isize));
    }
    out.extend_from_slice(x);
    for i in 0..right {
        out.push(at((n + i) as isize));
    }
    out
}

impl Stft {
    pub fn new(size: usize, hop: usize) -> Self {
        assert!(size >= 2 && size.is_multiple_of(2) && hop > 0 && hop <= size);
        let mut planner = RealFftPlanner::<f32>::new();
        let window = (0..size)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos())
            .collect();
        Self {
            size,
            hop,
            window,
            forward: planner.plan_fft_forward(size),
            inverse: planner.plan_fft_inverse(size),
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Bins in a frame: `size / 2 + 1`.
    pub fn bins(&self) -> usize {
        self.size / 2 + 1
    }

    /// Frames for `len` samples: the signal is centred, so the first frame is
    /// around sample 0.
    pub fn frames_for(&self, len: usize) -> usize {
        len / self.hop + 1
    }

    pub fn forward(&self, x: &[f32]) -> Spectrogram {
        let padded = reflect_pad(x, self.size / 2, self.size / 2);
        let frames = self.frames_for(x.len());
        let mut out = Spectrogram::zeros(self.bins(), frames);
        let mut input = vec![0.0f32; self.size];
        let mut spectrum = self.forward.make_output_vec();
        for f in 0..frames {
            let start = f * self.hop;
            for (i, v) in input.iter_mut().enumerate() {
                *v = padded.get(start + i).copied().unwrap_or(0.0) * self.window[i];
            }
            self.forward
                .process(&mut input, &mut spectrum)
                .expect("sizes match the plan");
            out.data[f * out.bins..(f + 1) * out.bins].copy_from_slice(&spectrum);
        }
        out
    }

    /// The signal of `len` samples that `spec` describes, by windowed overlap
    /// add. `forward` then `inverse` gives back the input.
    pub fn inverse(&self, spec: &Spectrogram, len: usize) -> Vec<f32> {
        let total = (spec.frames.saturating_sub(1)) * self.hop + self.size;
        let mut sum = vec![0.0f32; total];
        let mut norm = vec![0.0f32; total];
        let mut spectrum = self.inverse.make_input_vec();
        let mut output = vec![0.0f32; self.size];
        for f in 0..spec.frames {
            spectrum.copy_from_slice(&spec.data[f * spec.bins..(f + 1) * spec.bins]);
            // A real signal has no imaginary part at DC or Nyquist.
            spectrum[0].im = 0.0;
            spectrum[self.size / 2].im = 0.0;
            self.inverse
                .process(&mut spectrum, &mut output)
                .expect("sizes match the plan");
            let start = f * self.hop;
            for i in 0..self.size {
                sum[start + i] += output[i] / self.size as f32 * self.window[i];
                norm[start + i] += self.window[i] * self.window[i];
            }
        }
        let pad = self.size / 2;
        (0..len)
            .map(|i| {
                let j = pad + i;
                match (sum.get(j), norm.get(j)) {
                    (Some(&s), Some(&n)) if n > 1e-8 => s / n,
                    _ => 0.0,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (i as f32 * 0.05).sin() * 0.5 + (i as f32 * 0.31).sin() * 0.2)
            .collect()
    }

    #[test]
    fn forward_then_inverse_gives_back_the_signal() {
        let stft = Stft::new(1024, 256);
        let x = tone(5_000);
        let back = stft.inverse(&stft.forward(&x), x.len());
        assert_eq!(back.len(), x.len());
        for (a, b) in x.iter().zip(&back) {
            assert!((a - b).abs() < 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn a_tone_lands_in_its_bin() {
        let stft = Stft::new(1024, 256);
        let x: Vec<f32> = (0..4096)
            .map(|i| (std::f32::consts::TAU * 64.0 * i as f32 / 1024.0).sin())
            .collect();
        let spec = stft.forward(&x);
        let frame = 6;
        let loudest = (0..spec.bins)
            .max_by(|&a, &b| {
                spec.at(a, frame)
                    .norm()
                    .total_cmp(&spec.at(b, frame).norm())
            })
            .unwrap();
        assert_eq!(loudest, 64);
    }

    #[test]
    fn reflect_pad_mirrors_without_repeating_the_edge() {
        assert_eq!(
            reflect_pad(&[1.0, 2.0, 3.0, 4.0], 2, 2),
            vec![3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0, 2.0]
        );
    }
}
