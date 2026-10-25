//! Onset detection on log-compressed spectral flux.

use crate::stft::Spectrogram;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Onset {
    /// Seconds, on the signal's own timeline.
    pub time: f64,
    /// The flux at the peak.
    pub strength: f32,
}

/// Positive spectral flux per frame: how much the log-compressed magnitude
/// grew since the previous frame, summed over bins.
pub fn flux(s: &Spectrogram) -> Vec<f32> {
    let mut out = vec![0.0; s.frames];
    let c = |m: f32| (1.0 + 1000.0 * m).ln();
    for (f, o) in out.iter_mut().enumerate().skip(1) {
        let (a, b) = (s.row(f - 1), s.row(f));
        *o = a.iter().zip(b).map(|(&p, &q)| (c(q) - c(p)).max(0.0)).sum();
    }
    out
}

/// Peaks of `flux` above an adaptive threshold: the local mean over a second
/// plus a tenth of the largest peak anywhere, and the biggest within 50 ms.
pub fn pick(flux: &[f32], hop_seconds: f64) -> Vec<Onset> {
    let global = flux.iter().copied().fold(0.0, f32::max);
    if global <= 0.0 {
        return Vec::new();
    }
    let near = ((0.05 / hop_seconds).round() as usize).max(1);
    let wide = ((0.5 / hop_seconds).round() as usize).max(2);
    let mut out = Vec::new();
    for i in 1..flux.len() {
        let lo = i.saturating_sub(wide);
        let hi = (i + wide).min(flux.len() - 1);
        let mean = flux[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32;
        if flux[i] < mean + 0.1 * global {
            continue;
        }
        let (a, b) = (i.saturating_sub(near), (i + near).min(flux.len() - 1));
        let best = (a..=b).all(|j| flux[j] < flux[i] || (flux[j] == flux[i] && j >= i));
        if best {
            out.push(Onset {
                time: i as f64 * hop_seconds,
                strength: flux[i],
            });
        }
    }
    out
}

pub fn detect(s: &Spectrogram) -> Vec<Onset> {
    pick(&flux(s), s.hop_seconds)
}
