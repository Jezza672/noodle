//! Short-time Fourier transforms.

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Window length and hop shared by the spectral metrics.
pub const FFT_SIZE: usize = 2048;
pub const HOP: usize = 512;

/// Complex frames with zero-phase windowing: the window's centre is the
/// frame's time origin, so a steady sinusoid has the same phase at every bin
/// of its main lobe. Frame `i` is centred on sample `i * hop`.
pub struct Frames {
    pub bins: usize,
    pub hop: usize,
    pub rate: u32,
    pub frames: usize,
    pub data: Vec<Complex<f32>>,
}

impl Frames {
    pub fn analyse(x: &[f32], rate: u32, size: usize, hop: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft: Arc<dyn RealToComplex<f32>> = planner.plan_fft_forward(size);
        let bins = size / 2 + 1;
        let frames = x.len() / hop + 1;
        let window: Vec<f32> = (0..size)
            .map(|n| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / size as f32).cos())
            .collect();
        let mut data = vec![Complex::new(0.0, 0.0); frames * bins];
        let mut input = fft.make_input_vec();
        let mut scratch = fft.make_scratch_vec();
        let half = size / 2;
        for f in 0..frames {
            let centre = (f * hop) as isize;
            // Rotate so the window's centre lands on sample 0.
            for n in 0..size {
                let src = centre + n as isize - half as isize;
                let v = if src >= 0 && (src as usize) < x.len() {
                    x[src as usize]
                } else {
                    0.0
                };
                input[(n + half) % size] = v * window[n];
            }
            fft.process_with_scratch(
                &mut input,
                &mut data[f * bins..(f + 1) * bins],
                &mut scratch,
            )
            .expect("fft sizes match");
        }
        Self {
            bins,
            hop,
            rate,
            frames,
            data,
        }
    }

    pub fn standard(x: &[f32], rate: u32) -> Self {
        Self::analyse(x, rate, FFT_SIZE, HOP)
    }

    pub fn row(&self, frame: usize) -> &[Complex<f32>] {
        &self.data[frame * self.bins..(frame + 1) * self.bins]
    }

    pub fn hop_seconds(&self) -> f64 {
        self.hop as f64 / f64::from(self.rate)
    }

    pub fn magnitudes(&self) -> Spectrogram {
        Spectrogram {
            bins: self.bins,
            frames: self.frames,
            hop_seconds: self.hop_seconds(),
            rate: self.rate,
            mag: self.data.iter().map(|c| c.norm()).collect(),
        }
    }
}

/// Magnitudes only, frame-major.
#[derive(Clone)]
pub struct Spectrogram {
    pub bins: usize,
    pub frames: usize,
    pub hop_seconds: f64,
    pub rate: u32,
    pub mag: Vec<f32>,
}

impl Spectrogram {
    pub fn standard(x: &[f32], rate: u32) -> Self {
        Frames::standard(x, rate).magnitudes()
    }

    /// A spectrogram for finding when things happen: a window of about 10 ms
    /// and a hop of a quarter of that, so onset times are good to a few
    /// milliseconds. Frequency resolution is poor, which timing does not need.
    pub fn for_onsets(x: &[f32], rate: u32) -> Self {
        let size = (rate as usize / 100).next_power_of_two().max(128);
        Frames::analyse(x, rate, size, size / 4).magnitudes()
    }

    pub fn row(&self, frame: usize) -> &[f32] {
        &self.mag[frame * self.bins..(frame + 1) * self.bins]
    }

    /// The row at `seconds`, linearly interpolated, or `None` outside the
    /// spectrogram.
    pub fn row_at(&self, seconds: f64, out: &mut [f32]) -> bool {
        let pos = seconds / self.hop_seconds;
        if pos < 0.0 || pos > (self.frames - 1) as f64 {
            return false;
        }
        let i = (pos.floor() as usize).min(self.frames - 1);
        let j = (i + 1).min(self.frames - 1);
        let t = (pos - i as f64) as f32;
        for ((o, a), b) in out.iter_mut().zip(self.row(i)).zip(self.row(j)) {
            *o = a + (b - a) * t;
        }
        true
    }

    /// Frequency of `bin` in hertz.
    pub fn hz(&self, bin: usize) -> f64 {
        bin as f64 * f64::from(self.rate) / ((self.bins - 1) * 2) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_peaks_at_its_bin_with_flat_phase() {
        let rate = 44_100;
        let f = 20.0 * f64::from(rate) / FFT_SIZE as f64; // exactly bin 20
        let x: Vec<f32> = (0..rate)
            .map(|n| (2.0 * std::f64::consts::PI * f * f64::from(n) / f64::from(rate)).sin() as f32)
            .collect();
        let s = Frames::standard(&x, rate);
        let row = s.row(10);
        let peak = (0..s.bins)
            .max_by(|&a, &b| row[a].norm().total_cmp(&row[b].norm()))
            .unwrap();
        assert_eq!(peak, 20);
        let d = (row[21].arg() - row[20].arg()).abs();
        assert!(
            d < 0.05 || (d - 2.0 * std::f32::consts::PI).abs() < 0.05,
            "phase step {d}"
        );
    }
}
