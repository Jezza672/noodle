//! Keeping a transposed clip's formants where they were.
//!
//! Reading a stretched clip back at `r` frames per output frame moves the
//! pitch by `r`, and the formants with it, which is what makes a vocal
//! shifted up sound like a smaller singer. This stage runs on the shifted
//! audio and moves the spectral envelope back by `1 / r`
//! ([`FormantWarp`]), frame by frame, so the pitch stays shifted and the
//! formants stay put.
//!
//! It runs on the stretch worker, not the audio thread, so it may allocate.
//! Its output is [`SYNTH_LATENCY`] frames behind its input; the caller
//! drops that many from the front and feeds that many zeros at the end.

use noodle_engine::{
    Analyzer, BINS, Bin, FFT_SIZE, FormantWarp, SYNTH_LATENCY, Shape, SignalIn, SignalOut,
    SpectralBuf, Synthesizer,
};

/// Bins either side of a bin the envelope is smoothed over (about 700 Hz at
/// 44.1 kHz in all): wider than the gap between a voice's harmonics, narrower
/// than a formant.
const SMOOTH_BINS: usize = 16;
/// Frames handled at a time.
pub const BLOCK: usize = 2048;

/// How far back, in frames, a seek has to start so that a frame reaches the
/// position asked for with its whole window behind it.
pub const WARMUP: usize = FFT_SIZE;
/// How many frames the stage's output lags its input.
pub const LATENCY: usize = SYNTH_LATENCY as usize;

pub struct FormantStage {
    analyzer: Analyzer,
    synth: Synthesizer,
    warp: FormantWarp,
    frames: SpectralBuf,
    warped: SpectralBuf,
    planar: Vec<f32>,
    result: Vec<f32>,
}

impl FormantStage {
    pub fn new() -> Self {
        Self {
            analyzer: Analyzer::new(Shape::STEREO),
            synth: Synthesizer::new(Shape::STEREO),
            warp: FormantWarp::new(),
            frames: SpectralBuf::new(2, BLOCK),
            warped: SpectralBuf::new(2, BLOCK),
            planar: vec![0.0; BLOCK * 2],
            result: vec![0.0; BLOCK * 2],
        }
    }

    pub fn reset(&mut self) {
        self.analyzer.reset();
        self.synth.reset();
    }

    /// Replaces `io` (stereo, interleaved, at most [`BLOCK`] frames) with its
    /// formant-moved self, [`LATENCY`] frames late. `ratio_at(t)` is how far
    /// to move the formants (above 1 is up) for a frame ending `t` frames
    /// into this block.
    pub fn process(&mut self, io: &mut [f32], ratio_at: impl Fn(u32) -> f32) {
        let n = io.len() / 2;
        assert!(n <= BLOCK);
        for (i, pair) in io.chunks_exact(2).enumerate() {
            self.planar[i] = pair[0];
            self.planar[n + i] = pair[1];
        }
        self.analyzer.process(
            &SignalIn::new(&self.planar[..n * 2], Shape::STEREO, n),
            &mut self.frames.as_out(Shape::STEREO),
        );
        let input = self.frames.as_in(Shape::STEREO);
        let mut out = self.warped.as_out(Shape::STEREO);
        let mut scratch = [Bin::default(); BINS];
        for f in 0..input.frames() {
            let time = input.time(f);
            let Some(at) = out.push(time) else { break };
            let ratio = ratio_at(time);
            for channel in 0..2 {
                self.warp
                    .apply(input.frame(f, 0, channel), &mut scratch, ratio, SMOOTH_BINS);
                out.frame_mut(at, 0, channel).copy_from_slice(&scratch);
            }
        }
        self.synth.process(
            &self.warped.as_in(Shape::STEREO),
            &mut SignalOut::new(&mut self.result[..n * 2], Shape::STEREO, n),
        );
        for (i, pair) in io.chunks_exact_mut(2).enumerate() {
            pair[0] = self.result[i];
            pair[1] = self.result[n + i];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pushes `input` (mono, as stereo) through the stage in blocks and
    /// returns the left channel, aligned with the input.
    fn run(input: &[f32], ratio: f32) -> Vec<f32> {
        let mut stage = FormantStage::new();
        let mut padded = input.to_vec();
        padded.extend(std::iter::repeat_n(0.0, LATENCY));
        let mut out = Vec::new();
        for chunk in padded.chunks(BLOCK) {
            let mut io: Vec<f32> = chunk.iter().flat_map(|&x| [x, x]).collect();
            stage.process(&mut io, |_| ratio);
            out.extend(io.chunks_exact(2).map(|p| p[0]));
        }
        out.drain(..LATENCY);
        out
    }

    #[test]
    fn a_ratio_of_one_returns_the_audio() {
        let input: Vec<f32> = (0..16_384)
            .map(|n| (n as f32 * 0.07).sin() * 0.5 + (n as f32 * 0.31).sin() * 0.2)
            .collect();
        let out = run(&input, 1.0);
        assert_eq!(out.len(), input.len());
        let err = input[4096..12_000]
            .iter()
            .zip(&out[4096..12_000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 1e-3, "{err}");
    }

    fn voiced(f0: f32, centre: f32, width: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                (1..=30)
                    .map(|h| {
                        let f = f0 * h as f32;
                        let d = (f - centre) / width;
                        (-d * d).exp() * (f * t * std::f32::consts::TAU).sin()
                    })
                    .sum::<f32>()
                    * 0.05
            })
            .collect()
    }

    fn centroid(signal: &[f32]) -> f32 {
        let (mut total, mut weighted) = (0.0, 0.0);
        for step in 4..240 {
            let f = step as f32 * 25.0;
            let w = std::f32::consts::TAU * f / 48_000.0;
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (n, &x) in signal.iter().enumerate() {
                re += x * (w * n as f32).cos();
                im += x * (w * n as f32).sin();
            }
            let power = re * re + im * im;
            total += power;
            weighted += power * f;
        }
        weighted / total
    }

    #[test]
    fn the_stage_puts_a_shifted_voices_formant_back() {
        // A voice shifted up 4 semitones by resampling: the pitch and the
        // formant both moved by 1.26.
        let r = 2f32.powf(4.0 / 12.0);
        let shifted = voiced(150.0 * r, 1000.0 * r, 600.0 * r, 48_000);
        // What the shifted pitch would sound like with the formant kept.
        let ideal = voiced(150.0 * r, 1000.0, 600.0, 48_000);
        let fixed = run(&shifted, 1.0 / r);
        let window = 16_000..32_000;
        let before = centroid(&ideal[window.clone()]);
        let moved = centroid(&shifted[window.clone()]);
        let after = centroid(&fixed[window]);
        assert!(moved > before * 1.15, "{before} {moved}");
        assert!(
            (after / before - 1.0).abs() < 0.06,
            "{before} -> {after} (shifted {moved})"
        );
    }
}
