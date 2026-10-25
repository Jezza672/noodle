//! Phasiness: the loss of vertical phase coherence that makes a phase
//! vocoder's output sound distant and reverberant.
//!
//! With a zero-phase window a steady sinusoid's bins in its main lobe share
//! one phase. Around each spectral peak this measures how far the
//! neighbouring bins' phases stray from the peak's, as a fraction of half a
//! turn, weighted by the peak's magnitude. 0 is perfectly coherent. (The paper
//! builds its phasiness from the same idea; its exact weighting is not
//! reproduced here.)

use crate::stft::Frames;
use std::f32::consts::PI;

/// Peaks quieter than this below the frame's loudest are ignored.
const PEAK_FLOOR_DB: f32 = -40.0;

/// Mean incoherence in `[0, 1]` over `x`'s frames, or `NaN` if nothing peaked.
pub fn phasiness(x: &[f32], rate: u32) -> f64 {
    let frames = Frames::standard(x, rate);
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for f in 0..frames.frames {
        let row = frames.row(f);
        let top = row.iter().map(|c| c.norm()).fold(0.0, f32::max);
        if top <= 0.0 {
            continue;
        }
        let floor = top * 10f32.powf(PEAK_FLOOR_DB / 20.0);
        for k in 2..frames.bins - 2 {
            let m = row[k].norm();
            if m < floor || m <= row[k - 1].norm() || m < row[k + 1].norm() {
                continue;
            }
            let step = |a: usize, b: usize| {
                let mut d = (row[b].arg() - row[a].arg()).abs();
                if d > PI {
                    d = 2.0 * PI - d;
                }
                d / PI
            };
            let incoherence = 0.5 * (step(k - 1, k) + step(k, k + 1));
            num += f64::from(m) * f64::from(incoherence);
            den += f64::from(m);
        }
    }
    if den > 0.0 { num / den } else { f64::NAN }
}
