//! Spectral signals: a stream of FFT frames.
//!
//! A spectral wire carries what a short-time Fourier transform of audio
//! produces: every [`HOP`] samples, one frame of [`BINS`] complex bins per
//! lane, covering the last [`FFT_SIZE`] samples under a Hann window. The size
//! and hop are the same everywhere (the engine has no per-wire resolution),
//! which is what lets any spectral node feed any other.
//!
//! Frames are timestamped like events: a frame's `time` is the frame offset
//! within the block of the last sample it covers. A block holds as many
//! frames as fit in it (a long block holds several, a short one usually
//! none), so a spectral node is written like an event node: for each frame
//! in, push a frame out. [`Analyzer`] turns audio into frames and
//! [`Synthesizer`] turns frames back into audio (with [`SYNTH_LATENCY`]
//! samples of delay); between them, nodes change the bins.
//!
//! Bins are scaled so that a sine of amplitude `a` on a bin's centre has
//! magnitude close to `a` (the window's sum is divided out), which makes
//! thresholds in decibels mean the same thing as on an audio signal.

use std::f32::consts::TAU;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::{Shape, SignalIn, SignalOut};

/// Samples per FFT frame.
pub const FFT_SIZE: usize = 2048;
/// Samples between frames: four frames overlap at any moment.
pub const HOP: usize = 512;
/// Bins per frame: 0 Hz up to the Nyquist frequency inclusive.
pub const BINS: usize = FFT_SIZE / 2 + 1;
/// How many samples later than its input a [`Synthesizer`]'s output sounds
/// (see [`Node::latency`](crate::Node::latency)).
pub const SYNTH_LATENCY: u32 = (FFT_SIZE - 1) as u32;

/// The frequency, in Hz, at the centre of bin `bin`.
pub fn bin_frequency(bin: usize, sample_rate: f32) -> f32 {
    bin as f32 * sample_rate / FFT_SIZE as f32
}

/// One complex bin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bin {
    pub re: f32,
    pub im: f32,
}

impl Bin {
    pub const ZERO: Bin = Bin { re: 0.0, im: 0.0 };

    pub fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    /// The bin with the given magnitude and phase (radians).
    pub fn polar(magnitude: f32, phase: f32) -> Self {
        let (sin, cos) = phase.sin_cos();
        Self::new(magnitude * cos, magnitude * sin)
    }

    pub fn magnitude(self) -> f32 {
        self.re.hypot(self.im)
    }

    pub fn power(self) -> f32 {
        self.re * self.re + self.im * self.im
    }

    pub fn phase(self) -> f32 {
        self.im.atan2(self.re)
    }

    pub fn scaled(self, gain: f32) -> Self {
        Self::new(self.re * gain, self.im * gain)
    }
}

/// How many frames a block of `max_frames` samples can hold.
pub fn frame_capacity(max_frames: usize) -> usize {
    max_frames / HOP + 2
}

/// Storage for the frames of one block of a spectral signal: room for
/// `frame_capacity(max_frames)` frames of up to `lanes` lanes, allocated once.
#[derive(Clone, Debug)]
pub struct SpectralBuf {
    lanes: usize,
    capacity: usize,
    times: Vec<u32>,
    data: Vec<Bin>,
}

impl SpectralBuf {
    pub fn new(lanes: usize, max_frames: usize) -> Self {
        let capacity = frame_capacity(max_frames);
        Self {
            lanes,
            capacity,
            times: Vec::with_capacity(capacity),
            data: vec![Bin::ZERO; capacity * lanes * BINS],
        }
    }

    /// The most lanes a frame in this buffer can have.
    pub fn lanes(&self) -> usize {
        self.lanes
    }

    pub fn clear(&mut self) {
        self.times.clear();
    }

    pub fn as_in(&self, shape: Shape) -> SpectralIn<'_> {
        debug_assert!(shape.lanes() <= self.lanes);
        let len = self.times.len() * shape.lanes() * BINS;
        SpectralIn {
            shape,
            times: &self.times,
            data: &self.data[..len],
        }
    }

    /// Empties the buffer and returns a writer for `shape`.
    pub fn as_out(&mut self, shape: Shape) -> SpectralOut<'_> {
        SpectralOut::new(self, shape)
    }
}

/// A read-only view of the frames of one block.
#[derive(Clone, Copy, Debug)]
pub struct SpectralIn<'a> {
    shape: Shape,
    times: &'a [u32],
    data: &'a [Bin],
}

impl<'a> SpectralIn<'a> {
    /// No frames: what an unconnected spectral input holds.
    pub fn empty() -> SpectralIn<'static> {
        SpectralIn {
            shape: Shape::MONO,
            times: &[],
            data: &[],
        }
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    /// How many frames this block has.
    pub fn frames(&self) -> usize {
        self.times.len()
    }

    /// The offset within the block of the last sample frame `frame` covers.
    pub fn time(&self, frame: usize) -> u32 {
        self.times[frame]
    }

    /// The bins of one lane of a frame, broadcasting like
    /// [`SignalIn::lane`]. Panics if the input has no frames.
    pub fn frame(&self, frame: usize, voice: usize, channel: usize) -> &'a [Bin] {
        let voice = if self.shape.voices == 1 { 0 } else { voice };
        let channel = if self.shape.channels == 1 { 0 } else { channel };
        let lane = voice * self.shape.channels + channel;
        let start = (frame * self.shape.lanes() + lane) * BINS;
        &self.data[start..start + BINS]
    }
}

/// Where a node writes the frames it produces in one block.
#[derive(Debug)]
pub struct SpectralOut<'a> {
    shape: Shape,
    buf: &'a mut SpectralBuf,
}

impl<'a> SpectralOut<'a> {
    /// Empties `buf` and writes frames of `shape` into it.
    pub fn new(buf: &'a mut SpectralBuf, shape: Shape) -> Self {
        assert!(
            shape.lanes() <= buf.lanes,
            "spectral output shape {shape} doesn't fit its buffer"
        );
        buf.times.clear();
        Self { shape, buf }
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    /// How many frames have been pushed.
    pub fn frames(&self) -> usize {
        self.buf.times.len()
    }

    /// Starts a frame at `time`, with every bin zero, and returns its index.
    /// Frames must be pushed in time order. `None` if the block is full.
    pub fn push(&mut self, time: u32) -> Option<usize> {
        debug_assert!(
            self.buf.times.last().is_none_or(|&last| last <= time),
            "frames must be pushed in time order"
        );
        let index = self.buf.times.len();
        if index == self.buf.capacity {
            return None;
        }
        self.buf.times.push(time);
        let stride = self.shape.lanes() * BINS;
        self.buf.data[index * stride..(index + 1) * stride].fill(Bin::ZERO);
        Some(index)
    }

    /// The bins of one lane of frame `frame`, which must have been pushed.
    pub fn frame_mut(&mut self, frame: usize, voice: usize, channel: usize) -> &mut [Bin] {
        assert!(frame < self.buf.times.len(), "frame not pushed");
        let lane = voice * self.shape.channels + channel;
        let start = (frame * self.shape.lanes() + lane) * BINS;
        &mut self.buf.data[start..start + BINS]
    }
}

/// The Hann window of [`FFT_SIZE`] samples (periodic).
fn hann() -> Box<[f32]> {
    (0..FFT_SIZE)
        .map(|n| 0.5 - 0.5 * (TAU * n as f32 / FFT_SIZE as f32).cos())
        .collect()
}

/// Turns audio into spectral frames, one set of rings per lane.
pub struct Analyzer {
    shape: Shape,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Box<[f32]>,
    /// The last `FFT_SIZE` samples of each lane, as a ring.
    rings: Vec<f32>,
    /// Where the next sample goes, and so where the oldest one is.
    pos: usize,
    /// Samples since the last frame.
    since: usize,
    time: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    scale: f32,
}

impl Analyzer {
    /// Allocates; call off the audio thread.
    pub fn new(shape: Shape) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let window = hann();
        let scale = 2.0 / window.iter().sum::<f32>();
        Self {
            shape,
            window,
            rings: vec![0.0; shape.lanes() * FFT_SIZE],
            pos: 0,
            since: 0,
            time: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            scale,
        }
    }

    pub fn reset(&mut self) {
        self.rings.fill(0.0);
        self.pos = 0;
        self.since = 0;
    }

    /// Reads one block and pushes the frames it completes. `input` has this
    /// analyzer's shape.
    pub fn process(&mut self, input: &SignalIn<'_>, out: &mut SpectralOut<'_>) {
        debug_assert_eq!(input.shape(), self.shape);
        let frames = input.frames();
        let mut t = 0;
        while t < frames {
            let n = (HOP - self.since).min(frames - t);
            for voice in 0..self.shape.voices {
                for channel in 0..self.shape.channels {
                    let lane = voice * self.shape.channels + channel;
                    let ring = &mut self.rings[lane * FFT_SIZE..(lane + 1) * FFT_SIZE];
                    let samples = &input.lane(voice, channel)[t..t + n];
                    let first = n.min(FFT_SIZE - self.pos);
                    ring[self.pos..self.pos + first].copy_from_slice(&samples[..first]);
                    ring[..n - first].copy_from_slice(&samples[first..]);
                }
            }
            self.pos = (self.pos + n) % FFT_SIZE;
            self.since += n;
            t += n;
            if self.since == HOP {
                self.since = 0;
                self.emit(t as u32 - 1, out);
            }
        }
    }

    fn emit(&mut self, time: u32, out: &mut SpectralOut<'_>) {
        let Some(frame) = out.push(time) else { return };
        for voice in 0..self.shape.voices {
            for channel in 0..self.shape.channels {
                let lane = voice * self.shape.channels + channel;
                let ring = &self.rings[lane * FFT_SIZE..(lane + 1) * FFT_SIZE];
                // The oldest sample is at `pos`.
                let (recent, oldest) = ring.split_at(self.pos);
                for (i, &x) in oldest.iter().chain(recent).enumerate() {
                    self.time[i] = x * self.window[i];
                }
                // The input and output vectors have the right lengths.
                let _ = self.fft.process_with_scratch(
                    &mut self.time,
                    &mut self.spectrum,
                    &mut self.scratch,
                );
                for (bin, x) in out
                    .frame_mut(frame, voice, channel)
                    .iter_mut()
                    .zip(&self.spectrum)
                {
                    *bin = Bin::new(x.re * self.scale, x.im * self.scale);
                }
            }
        }
    }
}

/// Turns spectral frames back into audio by overlap-adding them.
pub struct Synthesizer {
    shape: Shape,
    ifft: Arc<dyn ComplexToReal<f32>>,
    window: Box<[f32]>,
    /// Each lane's overlap-add accumulator, as a ring.
    rings: Vec<f32>,
    /// Where the sample being produced sits in the ring.
    pos: usize,
    spectrum: Vec<Complex<f32>>,
    time: Vec<f32>,
    scratch: Vec<Complex<f32>>,
    scale: f32,
}

impl Synthesizer {
    /// Allocates; call off the audio thread.
    pub fn new(shape: Shape) -> Self {
        let ifft = RealFftPlanner::<f32>::new().plan_fft_inverse(FFT_SIZE);
        let window = hann();
        // Undo the analysis scaling, the inverse transform's missing
        // 1 / FFT_SIZE, and the gain of the overlapping windows squared.
        let window_sum: f32 = window.iter().sum();
        let overlap: f32 = (0..FFT_SIZE / HOP).map(|k| window[k * HOP].powi(2)).sum();
        let scale = (window_sum / 2.0) / FFT_SIZE as f32 / overlap;
        Self {
            shape,
            window,
            rings: vec![0.0; shape.lanes() * FFT_SIZE],
            pos: 0,
            spectrum: ifft.make_input_vec(),
            time: ifft.make_output_vec(),
            scratch: ifft.make_scratch_vec(),
            ifft,
            scale,
        }
    }

    pub fn reset(&mut self) {
        self.rings.fill(0.0);
        self.pos = 0;
    }

    /// Reads one block of frames and writes the audio. `output` has this
    /// synthesizer's shape; `input` has it or is empty. The output is
    /// [`SYNTH_LATENCY`] samples later than the audio the frames came from.
    pub fn process(&mut self, input: &SpectralIn<'_>, output: &mut SignalOut<'_>) {
        debug_assert_eq!(output.shape(), self.shape);
        let frames = output.frames();
        let have = input.frames() > 0;
        for voice in 0..self.shape.voices {
            for channel in 0..self.shape.channels {
                let lane = voice * self.shape.channels + channel;
                let mut pos = self.pos;
                let mut next = 0;
                let out = output.lane_mut(voice, channel);
                for (t, sample) in out.iter_mut().enumerate().take(frames) {
                    while have && next < input.frames() && input.time(next) as usize <= t {
                        self.add(lane, pos, input.frame(next, voice, channel));
                        next += 1;
                    }
                    // The sample that no later frame can touch.
                    let at = (pos + 1) % FFT_SIZE;
                    let ring = &mut self.rings[lane * FFT_SIZE..(lane + 1) * FFT_SIZE];
                    *sample = std::mem::take(&mut ring[at]);
                    pos = at;
                }
            }
        }
        self.pos = (self.pos + frames) % FFT_SIZE;
    }

    /// Adds a frame ending at ring position `pos`.
    fn add(&mut self, lane: usize, pos: usize, bins: &[Bin]) {
        for (x, bin) in self.spectrum.iter_mut().zip(bins) {
            *x = Complex::new(bin.re, bin.im);
        }
        // The transform wants real DC and Nyquist bins.
        self.spectrum[0].im = 0.0;
        self.spectrum[BINS - 1].im = 0.0;
        let _ =
            self.ifft
                .process_with_scratch(&mut self.spectrum, &mut self.time, &mut self.scratch);
        let ring = &mut self.rings[lane * FFT_SIZE..(lane + 1) * FFT_SIZE];
        let start = (pos + 1) % FFT_SIZE;
        for (i, (&x, &w)) in self.time.iter().zip(&self.window).enumerate() {
            ring[(start + i) % FFT_SIZE] += x * w * self.scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[f32], block: usize) -> Vec<f32> {
        let shape = Shape::MONO;
        let mut analyzer = Analyzer::new(shape);
        let mut synth = Synthesizer::new(shape);
        let mut spectral = SpectralBuf::new(1, block);
        let mut out = Vec::new();
        for chunk in input.chunks(block) {
            let signal = SignalIn::new(chunk, shape, chunk.len());
            analyzer.process(&signal, &mut spectral.as_out(shape));
            let mut audio = vec![0.0; chunk.len()];
            synth.process(
                &spectral.as_in(shape),
                &mut SignalOut::new(&mut audio, shape, chunk.len()),
            );
            out.extend(audio);
        }
        out
    }

    fn noise(n: usize) -> Vec<f32> {
        let mut state = 12345u32;
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn analysis_then_synthesis_returns_the_input_late() {
        let input = noise(8192);
        let out = run(&input, 256);
        let delay = SYNTH_LATENCY as usize;
        // Frames start `HOP` samples in, so the first samples are only
        // partly covered; compare once the overlap is full.
        for n in (FFT_SIZE..input.len() - delay).step_by(7) {
            assert!(
                (out[n + delay] - input[n]).abs() < 1e-3,
                "sample {n}: {} vs {}",
                out[n + delay],
                input[n]
            );
        }
    }

    #[test]
    fn the_block_size_does_not_change_the_result() {
        let input = noise(6000);
        let small = run(&input, 37);
        let large = run(&input, 1024);
        let worst = small
            .iter()
            .zip(&large)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-5, "worst difference {worst}");
    }

    #[test]
    fn a_sine_has_its_amplitude_on_its_bin() {
        let shape = Shape::MONO;
        let bin = 100;
        let input: Vec<f32> = (0..4096)
            .map(|n| 0.5 * (TAU * bin as f32 * n as f32 / FFT_SIZE as f32).sin())
            .collect();
        let mut analyzer = Analyzer::new(shape);
        let mut spectral = SpectralBuf::new(1, 4096);
        analyzer.process(
            &SignalIn::new(&input, shape, 4096),
            &mut spectral.as_out(shape),
        );
        let frames = spectral.as_in(shape);
        assert_eq!(frames.frames(), 4096 / HOP);
        let magnitude = frames.frame(frames.frames() - 1, 0, 0)[bin].magnitude();
        assert!((magnitude - 0.5).abs() < 0.01, "magnitude {magnitude}");
        assert_eq!(frames.time(0) as usize, HOP - 1);
    }

    #[test]
    fn lanes_stay_apart() {
        let shape = Shape::STEREO;
        let left = noise(4096);
        let mut data = left.clone();
        data.extend(std::iter::repeat_n(0.0, 4096));
        let mut analyzer = Analyzer::new(shape);
        let mut synth = Synthesizer::new(shape);
        let mut spectral = SpectralBuf::new(2, 4096);
        analyzer.process(
            &SignalIn::new(&data, shape, 4096),
            &mut spectral.as_out(shape),
        );
        let mut audio = vec![0.0; 8192];
        synth.process(
            &spectral.as_in(shape),
            &mut SignalOut::new(&mut audio, shape, 4096),
        );
        assert!(audio[4096..].iter().all(|&x| x == 0.0));
        assert!(audio[..4096].iter().any(|&x| x.abs() > 0.1));
    }

    #[test]
    fn a_full_buffer_drops_frames_instead_of_growing() {
        let shape = Shape::MONO;
        let mut spectral = SpectralBuf::new(1, 64);
        let mut analyzer = Analyzer::new(shape);
        // A block longer than the buffer was sized for.
        let input = vec![0.1; 64 * HOP];
        analyzer.process(
            &SignalIn::new(&input, shape, input.len()),
            &mut spectral.as_out(shape),
        );
        assert_eq!(spectral.as_in(shape).frames(), frame_capacity(64));
    }
}
