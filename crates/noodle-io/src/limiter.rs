//! A true-peak limiter for the end of an export: a lookahead limiter that
//! keeps the signal between its samples (BS.1770 Annex 2, 4× oversampled, the
//! way [`LoudnessMeter`](crate::LoudnessMeter) measures it) under a ceiling.
//!
//! It is delivery processing, like dither: it runs offline in the writer, so
//! it can look ahead freely, and it is not a node. The gain for each frame is
//! the smallest gain any frame within `window` of it needs, smoothed by a
//! moving average of the same length, so the gain falls before a peak
//! arrives and rises again afterwards, and never exceeds what the frame needs.

use std::collections::VecDeque;

use crate::loudness::{OVERSAMPLING, SIDE, TAPS, interpolators};

/// How long the gain takes to fall to a peak and to come back, in seconds.
const WINDOW_SECONDS: f64 = 0.005;

/// Limits interleaved audio pushed in chunks. Its output is the input
/// delayed by [`TruePeakLimiter::latency`] frames; [`finish`](Self::finish)
/// flushes the end, so as many frames come out as went in.
pub struct TruePeakLimiter {
    channels: usize,
    ceiling: f32,
    interpolators: [[f64; TAPS]; OVERSAMPLING - 1],
    /// The last `TAPS` samples of each channel, oldest first.
    history: Vec<[f32; TAPS]>,
    /// Frames pushed so far.
    pushed: u64,
    /// The inter-sample peak between the previous frame and the one before.
    previous: f32,
    /// Frames (interleaved) that have not come out yet.
    pending: VecDeque<f32>,
    window: usize,
    /// The gain each recent frame needs: (frame, gain), rising in gain, so
    /// the front is the smallest over the window.
    smallest: VecDeque<(u64, f32)>,
    /// The last `window` window minima, and their sum.
    recent: VecDeque<f32>,
    sum: f64,
}

impl TruePeakLimiter {
    /// A limiter holding the true peak under `ceiling_db` dBTP.
    pub fn new(sample_rate: u32, channels: usize, ceiling_db: f64) -> Self {
        let window = ((f64::from(sample_rate) * WINDOW_SECONDS).round() as usize).max(1);
        Self {
            channels,
            ceiling: 10f64.powf(ceiling_db / 20.0) as f32,
            interpolators: interpolators(),
            history: vec![[0.0; TAPS]; channels],
            pushed: 0,
            previous: 0.0,
            pending: VecDeque::new(),
            window,
            smallest: VecDeque::new(),
            recent: VecDeque::new(),
            sum: 0.0,
        }
    }

    /// Frames between a sample going in and coming out.
    pub fn latency(&self) -> usize {
        SIDE + self.window - 1
    }

    /// Takes interleaved samples and appends the limited frames that are
    /// ready to `out`.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        for frame in samples.chunks_exact(self.channels) {
            self.frame(frame, out);
        }
    }

    /// Flushes the frames still held back into `out`.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let silence = vec![0.0; self.channels];
        for _ in 0..self.latency() {
            self.frame(&silence, out);
        }
    }

    fn frame(&mut self, frame: &[f32], out: &mut Vec<f32>) {
        let mut sample_peak = 0.0f32;
        let mut between = 0.0f32;
        for (history, &x) in self.history.iter_mut().zip(frame) {
            let x = if x.is_finite() { x } else { 0.0 };
            history.rotate_left(1);
            history[TAPS - 1] = x;
            self.pending.push_back(x);
        }
        // The frame `SIDE` back is at the middle of the filters; the peak
        // between it and the next frame is found now.
        for history in &self.history {
            for filter in &self.interpolators {
                let y: f64 = filter
                    .iter()
                    .zip(history)
                    .map(|(tap, &x)| tap * f64::from(x))
                    .sum();
                between = between.max(y.abs() as f32);
            }
        }
        self.pushed += 1;
        if self.pushed <= SIDE as u64 {
            // Nothing is centred yet; the frames wait for the filters.
            return;
        }
        // Frame `m` is the one now `SIDE` back. Its peak is the highest of its
        // own samples and the stretches of signal on either side of it.
        let first = self.pending.len() - self.channels * (SIDE + 1);
        for c in 0..self.channels {
            sample_peak = sample_peak.max(self.pending[first + c].abs());
        }
        let peak = sample_peak.max(self.previous).max(between);
        self.previous = between;
        let need = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        let q = self.pushed - SIDE as u64 - 1;
        while self.smallest.back().is_some_and(|&(_, g)| g >= need) {
            self.smallest.pop_back();
        }
        self.smallest.push_back((q, need));
        while self
            .smallest
            .front()
            .is_some_and(|&(i, _)| i + (self.window as u64) <= q)
        {
            self.smallest.pop_front();
        }
        // The smallest gain over the last `window` frames, which belongs to
        // the frame `window - 1` before `q`.
        let minimum = self.smallest.front().map_or(1.0, |&(_, g)| g);
        self.recent.push_back(minimum);
        self.sum += f64::from(minimum);
        if self.recent.len() > self.window {
            self.sum -= f64::from(self.recent.pop_front().unwrap_or(1.0));
        }
        if self.recent.len() == self.window {
            let gain = (self.sum / self.window as f64) as f32;
            for _ in 0..self.channels {
                let x = self.pending.pop_front().unwrap_or(0.0);
                out.push(x * gain);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LoudnessMeter;

    fn limit(samples: &[f32], channels: usize, ceiling: f64) -> Vec<f32> {
        let mut limiter = TruePeakLimiter::new(48_000, channels, ceiling);
        let mut out = Vec::new();
        for piece in samples.chunks(channels * 777) {
            limiter.push(piece, &mut out);
        }
        limiter.finish(&mut out);
        out
    }

    fn true_peak(samples: &[f32], channels: usize) -> f64 {
        let mut meter = LoudnessMeter::new(48_000, channels);
        meter.push(samples);
        meter.finish().true_peak_db
    }

    /// A loud tone near a quarter of the sample rate, whose peaks fall
    /// between samples, so its true peak is above its sample peak.
    fn hot(frames: usize, channels: usize) -> Vec<f32> {
        (0..frames * channels)
            .map(|i| {
                let f = (i / channels) as f32;
                1.4 * (f * 1.1 + 0.3 * (i % channels) as f32).sin()
            })
            .collect()
    }

    #[test]
    fn the_true_peak_ends_up_under_the_ceiling() {
        for channels in [1, 2] {
            let input = hot(20_000, channels);
            assert!(true_peak(&input, channels) > 2.0);
            let out = limit(&input, channels, -1.0);
            assert_eq!(out.len(), input.len(), "every frame comes out");
            let peak = true_peak(&out, channels);
            assert!(peak <= -0.95, "{channels} channels: {peak} dBTP");
            // Not squashed much further than it had to be.
            assert!(peak > -1.6, "{channels} channels: {peak} dBTP");
        }
    }

    #[test]
    fn a_signal_under_the_ceiling_comes_out_unchanged_but_delayed() {
        let input: Vec<f32> = (0..5_000).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect();
        let out = limit(&input, 1, -1.0);
        assert_eq!(out.len(), input.len());
        assert_eq!(out, input);
    }

    #[test]
    fn a_single_loud_click_is_caught_before_it_arrives() {
        let mut input = vec![0.0f32; 4_000];
        input[2_000] = 4.0;
        let out = limit(&input, 1, -1.0);
        let peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak <= 0.9, "{peak}");
        assert!(peak > 0.1, "the click is limited, not removed: {peak}");
    }

    #[test]
    fn a_short_input_and_a_silent_one_still_come_out_whole() {
        for frames in [0usize, 1, 5, 100] {
            let out = limit(&vec![0.5; frames], 1, -1.0);
            assert_eq!(out.len(), frames);
        }
        let out = limit(&vec![0.0; 3_000], 2, -1.0);
        assert!(out.iter().all(|&x| x == 0.0));
    }
}
