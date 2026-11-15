//! Spectral signals: a stream of FFT frames.
//!
//! A spectral wire carries what a short-time Fourier transform of audio
//! produces: every hop samples, one frame of `size / 2 + 1` complex bins per
//! lane, covering the last `size` samples under a Hann window. The hop is a
//! quarter of the size. A wire has one size, which Analyze chooses (its
//! `size` config) and every node downstream of it adopts; [`FFT_SIZE`],
//! [`HOP`] and [`BINS`] are the default, 2048 (ADR 0007).
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

/// Samples per FFT frame by default.
pub const FFT_SIZE: usize = 2048;
/// Samples between frames by default: four frames overlap at any moment.
pub const HOP: usize = fft_hop(FFT_SIZE);
/// Bins per frame by default: 0 Hz up to the Nyquist frequency inclusive.
pub const BINS: usize = fft_bins(FFT_SIZE);
/// How many samples later than its input a default-size [`Synthesizer`]'s
/// output sounds (see [`Node::latency`](crate::Node::latency)).
pub const SYNTH_LATENCY: u32 = synth_latency(FFT_SIZE);
/// The smallest and largest frame sizes a wire can have.
pub const MIN_FFT_SIZE: usize = 256;
pub const MAX_FFT_SIZE: usize = 8192;

/// Samples between the frames of a wire of `size` samples.
pub const fn fft_hop(size: usize) -> usize {
    size / 4
}

/// Bins in a frame of a wire of `size` samples.
pub const fn fft_bins(size: usize) -> usize {
    size / 2 + 1
}

/// How many samples later than its input a [`Synthesizer`] of `size` sounds.
pub const fn synth_latency(size: usize) -> u32 {
    (size - 1) as u32
}

/// The frame size nearest to `wanted`: a power of two from
/// [`MIN_FFT_SIZE`] to [`MAX_FFT_SIZE`] (nearest on a log scale).
pub fn clamp_fft_size(wanted: i64) -> usize {
    let wanted = wanted.clamp(MIN_FFT_SIZE as i64, MAX_FFT_SIZE as i64) as f64;
    let power = wanted.log2().round() as u32;
    1usize << power
}

/// The frequency, in Hz, at the centre of bin `bin` of a default-size frame.
pub fn bin_frequency(bin: usize, sample_rate: f32) -> f32 {
    bin_frequency_in(bin, sample_rate, FFT_SIZE)
}

/// The frequency, in Hz, at the centre of bin `bin` of a frame of `size`.
pub fn bin_frequency_in(bin: usize, sample_rate: f32, size: usize) -> f32 {
    bin as f32 * sample_rate / size as f32
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

/// How many default-size frames a block of `max_frames` samples can hold.
pub fn frame_capacity(max_frames: usize) -> usize {
    frame_capacity_for(max_frames, FFT_SIZE)
}

/// How many frames of a wire of `size` a block of `max_frames` samples can hold.
pub fn frame_capacity_for(max_frames: usize, size: usize) -> usize {
    max_frames / fft_hop(size) + 2
}

/// Storage for the frames of one block of a spectral signal: room for
/// `frame_capacity_for(max_frames, size)` frames of up to `lanes` lanes,
/// allocated once.
#[derive(Clone, Debug)]
pub struct SpectralBuf {
    lanes: usize,
    size: usize,
    bins: usize,
    capacity: usize,
    times: Vec<u32>,
    data: Vec<Bin>,
}

impl SpectralBuf {
    /// A buffer of default-size frames.
    pub fn new(lanes: usize, max_frames: usize) -> Self {
        Self::with_size(lanes, max_frames, FFT_SIZE)
    }

    /// A buffer of frames of `size` samples.
    pub fn with_size(lanes: usize, max_frames: usize, size: usize) -> Self {
        let capacity = frame_capacity_for(max_frames, size);
        let bins = fft_bins(size);
        Self {
            lanes,
            size,
            bins,
            capacity,
            times: Vec::with_capacity(capacity),
            data: vec![Bin::ZERO; capacity * lanes * bins],
        }
    }

    /// The FFT size of the frames in this buffer.
    pub fn size(&self) -> usize {
        self.size
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
        let len = self.times.len() * shape.lanes() * self.bins;
        SpectralIn {
            shape,
            bins: self.bins,
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
    bins: usize,
    times: &'a [u32],
    data: &'a [Bin],
}

impl<'a> SpectralIn<'a> {
    /// No frames: what an unconnected spectral input holds.
    pub fn empty() -> SpectralIn<'static> {
        SpectralIn {
            shape: Shape::MONO,
            bins: 0,
            times: &[],
            data: &[],
        }
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    /// Bins in each frame (zero for [`empty`](Self::empty)).
    pub fn bins(&self) -> usize {
        self.bins
    }

    /// How many frames this block has.
    pub fn frames(&self) -> usize {
        self.times.len()
    }

    /// The offset within the block of the last sample frame `frame` covers.
    pub fn time(&self, frame: usize) -> u32 {
        self.times[frame]
    }

    /// Every lane of a frame, back to back: `shape().lanes() * bins()` bins.
    pub(crate) fn frame_lanes(&self, frame: usize) -> &'a [Bin] {
        let stride = self.shape.lanes() * self.bins;
        &self.data[frame * stride..(frame + 1) * stride]
    }

    /// The bins of one lane of a frame, broadcasting like
    /// [`SignalIn::lane`]. Panics if the input has no frames.
    pub fn frame(&self, frame: usize, voice: usize, channel: usize) -> &'a [Bin] {
        let voice = if self.shape.voices == 1 { 0 } else { voice };
        let channel = if self.shape.channels == 1 { 0 } else { channel };
        let lane = voice * self.shape.channels + channel;
        let start = (frame * self.shape.lanes() + lane) * self.bins;
        &self.data[start..start + self.bins]
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

    /// Bins in each frame.
    pub fn bins(&self) -> usize {
        self.buf.bins
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
        let stride = self.shape.lanes() * self.buf.bins;
        self.buf.data[index * stride..(index + 1) * stride].fill(Bin::ZERO);
        Some(index)
    }

    /// Every lane of frame `frame`, back to back, which must have been pushed.
    pub(crate) fn frame_lanes_mut(&mut self, frame: usize) -> &mut [Bin] {
        assert!(frame < self.buf.times.len(), "frame not pushed");
        let stride = self.shape.lanes() * self.buf.bins;
        &mut self.buf.data[frame * stride..(frame + 1) * stride]
    }

    /// The bins of one lane of frame `frame`, which must have been pushed.
    pub fn frame_mut(&mut self, frame: usize, voice: usize, channel: usize) -> &mut [Bin] {
        assert!(frame < self.buf.times.len(), "frame not pushed");
        let lane = voice * self.shape.channels + channel;
        let start = (frame * self.shape.lanes() + lane) * self.buf.bins;
        &mut self.buf.data[start..start + self.buf.bins]
    }
}

/// Holds a spectral signal back by a fixed number of samples, so the frames
/// of an earlier path line up with those of a later one (delay compensation).
/// A frame is stored with the time it is due and comes out in the block that
/// time falls in, so any delay works, not only multiples of [`HOP`]. All room
/// is allocated up front.
#[derive(Debug)]
pub struct SpectralDelay {
    delay: u64,
    stride: usize,
    capacity: usize,
    /// When each stored frame is due, in samples since the start (or reset).
    due: Box<[u64]>,
    data: Box<[Bin]>,
    head: usize,
    len: usize,
    elapsed: u64,
}

impl SpectralDelay {
    /// A delay of `delay` samples for frames of `lanes` lanes of a wire of
    /// `size`, in blocks of up to `max_frames` samples.
    pub fn new(lanes: usize, delay: usize, max_frames: usize, size: usize) -> Self {
        let capacity = delay / fft_hop(size) + frame_capacity_for(max_frames, size) + 2;
        let stride = lanes * fft_bins(size);
        Self {
            delay: delay as u64,
            stride,
            capacity,
            due: vec![0; capacity].into_boxed_slice(),
            data: vec![Bin::ZERO; capacity * stride].into_boxed_slice(),
            head: 0,
            len: 0,
            elapsed: 0,
        }
    }

    pub fn delay(&self) -> usize {
        self.delay as usize
    }

    /// Forgets the frames held and starts counting from zero.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.elapsed = 0;
    }

    /// Takes over the frames `old` holds, if it is the same size.
    pub fn continue_from(&mut self, old: &mut SpectralDelay) {
        if self.delay == old.delay && self.stride == old.stride && self.capacity == old.capacity {
            std::mem::swap(&mut self.due, &mut old.due);
            std::mem::swap(&mut self.data, &mut old.data);
            self.head = old.head;
            self.len = old.len;
            self.elapsed = old.elapsed;
        }
    }

    /// Takes in this block's frames and writes the ones now due.
    /// `frames` is the block's length in samples.
    pub fn process(&mut self, input: &SpectralIn<'_>, frames: usize, out: &mut SpectralOut<'_>) {
        debug_assert_eq!(input.shape().lanes() * input.bins(), self.stride);
        for frame in 0..input.frames() {
            if self.len == self.capacity {
                // Can't happen with the room made, but never overwrite
                // silently: the oldest frame goes.
                self.head = (self.head + 1) % self.capacity;
                self.len -= 1;
            }
            let slot = (self.head + self.len) % self.capacity;
            self.due[slot] = self.elapsed + u64::from(input.time(frame)) + self.delay;
            self.data[slot * self.stride..(slot + 1) * self.stride]
                .copy_from_slice(input.frame_lanes(frame));
            self.len += 1;
        }
        let end = self.elapsed + frames as u64;
        while self.len > 0 && self.due[self.head] < end {
            let time = self.due[self.head].saturating_sub(self.elapsed) as u32;
            let Some(at) = out.push(time) else { break };
            out.frame_lanes_mut(at).copy_from_slice(
                &self.data[self.head * self.stride..(self.head + 1) * self.stride],
            );
            self.head = (self.head + 1) % self.capacity;
            self.len -= 1;
        }
        self.elapsed = end;
    }
}

/// The Hann window of `size` samples (periodic).
fn hann(size: usize) -> Box<[f32]> {
    (0..size)
        .map(|n| 0.5 - 0.5 * (TAU * n as f32 / size as f32).cos())
        .collect()
}

/// Turns audio into spectral frames, one set of rings per lane.
pub struct Analyzer {
    shape: Shape,
    size: usize,
    hop: usize,
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
        Self::with_size(shape, FFT_SIZE)
    }

    /// An analyzer making frames of `size` samples. Allocates.
    pub fn with_size(shape: Shape, size: usize) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(size);
        let window = hann(size);
        let scale = 2.0 / window.iter().sum::<f32>();
        Self {
            shape,
            size,
            hop: fft_hop(size),
            window,
            rings: vec![0.0; shape.lanes() * size],
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
            let n = (self.hop - self.since).min(frames - t);
            for voice in 0..self.shape.voices {
                for channel in 0..self.shape.channels {
                    let lane = voice * self.shape.channels + channel;
                    let ring = &mut self.rings[lane * self.size..(lane + 1) * self.size];
                    let samples = &input.lane(voice, channel)[t..t + n];
                    let first = n.min(self.size - self.pos);
                    ring[self.pos..self.pos + first].copy_from_slice(&samples[..first]);
                    ring[..n - first].copy_from_slice(&samples[first..]);
                }
            }
            self.pos = (self.pos + n) % self.size;
            self.since += n;
            t += n;
            if self.since == self.hop {
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
                let ring = &self.rings[lane * self.size..(lane + 1) * self.size];
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
    size: usize,
    bins: usize,
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
        Self::with_size(shape, FFT_SIZE)
    }

    /// A synthesizer for frames of `size` samples. Allocates.
    pub fn with_size(shape: Shape, size: usize) -> Self {
        let ifft = RealFftPlanner::<f32>::new().plan_fft_inverse(size);
        let window = hann(size);
        let hop = fft_hop(size);
        // Undo the analysis scaling, the inverse transform's missing
        // 1 / size, and the gain of the overlapping windows squared.
        let window_sum: f32 = window.iter().sum();
        let overlap: f32 = (0..size / hop).map(|k| window[k * hop].powi(2)).sum();
        let scale = (window_sum / 2.0) / size as f32 / overlap;
        Self {
            shape,
            size,
            bins: fft_bins(size),
            window,
            rings: vec![0.0; shape.lanes() * size],
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
                    let at = (pos + 1) % self.size;
                    let ring = &mut self.rings[lane * self.size..(lane + 1) * self.size];
                    *sample = std::mem::take(&mut ring[at]);
                    pos = at;
                }
            }
        }
        self.pos = (self.pos + frames) % self.size;
    }

    /// Adds a frame ending at ring position `pos`.
    fn add(&mut self, lane: usize, pos: usize, bins: &[Bin]) {
        for (x, bin) in self.spectrum.iter_mut().zip(bins) {
            *x = Complex::new(bin.re, bin.im);
        }
        // The transform wants real DC and Nyquist bins.
        self.spectrum[0].im = 0.0;
        self.spectrum[self.bins - 1].im = 0.0;
        let _ =
            self.ifft
                .process_with_scratch(&mut self.spectrum, &mut self.time, &mut self.scratch);
        let ring = &mut self.rings[lane * self.size..(lane + 1) * self.size];
        let start = (pos + 1) % self.size;
        for (i, (&x, &w)) in self.time.iter().zip(&self.window).enumerate() {
            ring[(start + i) % self.size] += x * w * self.scale;
        }
    }
}

/// The farthest [`FormantWarp`] lifts or cuts a bin, as an amplitude ratio
/// (24 dB), so a hole in the envelope never becomes a spike.
const FORMANT_GAIN_LIMIT: f32 = 16.0;

/// Moves a frame's spectral envelope (its formants) up or down in frequency
/// and leaves the fine structure (the harmonics, so the pitch) where it is.
///
/// The envelope is the magnitude smoothed over `width` bins either side.
/// Moving it by `ratio` means scaling each bin by `E(f / ratio) / E(f)`, which
/// keeps the phases. Following a pitch shift of `r` with a warp of `1 / r`
/// puts the formants back where they were.
pub struct FormantWarp {
    /// Running totals of the magnitudes, one longer than the bins.
    prefix: Vec<f32>,
    envelope: Vec<f32>,
}

impl Default for FormantWarp {
    fn default() -> Self {
        Self::new()
    }
}

impl FormantWarp {
    /// A warp for default-size frames. Allocates; call off the audio thread.
    pub fn new() -> Self {
        Self::with_size(FFT_SIZE)
    }

    /// A warp for frames of `size` samples. Allocates.
    pub fn with_size(size: usize) -> Self {
        let bins = fft_bins(size);
        Self {
            prefix: vec![0.0; bins + 1],
            envelope: vec![0.0; bins],
        }
    }

    /// Writes `input` to `out` with its envelope moved by `ratio` (above 1
    /// is up). A ratio near 1 copies the frame.
    pub fn apply(&mut self, input: &[Bin], out: &mut [Bin], ratio: f32, width: usize) {
        let bins = self.envelope.len();
        debug_assert!(input.len() == bins && out.len() == bins);
        if (ratio - 1.0).abs() < 1e-4 || !ratio.is_finite() || ratio <= 0.0 {
            out.copy_from_slice(input);
            return;
        }
        let width = width.max(1);
        let mut sum = 0.0;
        for (k, bin) in input.iter().enumerate() {
            self.prefix[k] = sum;
            sum += bin.magnitude();
        }
        self.prefix[bins] = sum;
        for (k, e) in self.envelope.iter_mut().enumerate() {
            let lo = k.saturating_sub(width);
            let hi = (k + width + 1).min(bins);
            *e = (self.prefix[hi] - self.prefix[lo]) / (hi - lo) as f32;
        }
        // A floor under the envelope, so silence stays silent.
        let floor = self.envelope.iter().copied().fold(0.0, f32::max) * 1e-5 + 1e-12;
        for (k, (bin, slot)) in input.iter().zip(out.iter_mut()).enumerate() {
            let at = k as f32 / ratio;
            let below = at.floor() as usize;
            let e_new = if below >= bins - 1 {
                // Above the top the envelope has nothing to move down.
                self.envelope[bins - 1]
            } else {
                let t = at - below as f32;
                self.envelope[below] * (1.0 - t) + self.envelope[below + 1] * t
            };
            let gain = ((e_new + floor) / (self.envelope[k] + floor))
                .clamp(1.0 / FORMANT_GAIN_LIMIT, FORMANT_GAIN_LIMIT);
            *slot = bin.scaled(gain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spectral_delay_moves_frames_by_whole_samples_across_blocks() {
        let shape = Shape::MONO;
        let block = 100;
        let delay = 1_300; // not a multiple of a hop or a block
        let mut line = SpectralDelay::new(1, delay, block, FFT_SIZE);
        let mut source = SpectralBuf::new(1, block);
        let mut dest = SpectralBuf::new(1, block);
        let mut heard = Vec::new();
        for b in 0..40u64 {
            let start = b * block as u64;
            let mut out = source.as_out(shape);
            // A frame every hop, marked by its start time in bin 0.
            for t in 0..block as u64 {
                if (start + t).is_multiple_of(HOP as u64) {
                    let at = out.push(t as u32).unwrap();
                    out.frame_mut(at, 0, 0)[0] = Bin::new((start + t) as f32, 0.0);
                }
            }
            let input = source.as_in(shape);
            let mut out = dest.as_out(shape);
            line.process(&input, block, &mut out);
            let got = dest.as_in(shape);
            for f in 0..got.frames() {
                heard.push((
                    start + u64::from(got.time(f)),
                    got.frame(f, 0, 0)[0].re as u64,
                ));
            }
        }
        assert!(heard.len() > 5);
        for (at, marked) in heard {
            assert_eq!(at, marked + delay as u64);
        }
    }

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

    /// A frame with harmonics every `spacing` bins under a single broad
    /// peak (a formant) centred on `centre`.
    fn voiced(spacing: usize, centre: f32) -> Vec<Bin> {
        (0..BINS)
            .map(|k| {
                if k % spacing != 0 {
                    return Bin::default();
                }
                let d = (k as f32 - centre) / 80.0;
                Bin::new((-d * d).exp(), 0.0)
            })
            .collect()
    }

    fn peak_bin(frame: &[Bin]) -> usize {
        // The middle of the busiest 41 bins, which follows the formant and
        // ignores which harmonic happens to be largest.
        let mag: Vec<f32> = frame.iter().map(|b| b.magnitude()).collect();
        (20..BINS - 20)
            .max_by(|&a, &b| {
                let sum = |k: usize| mag[k - 20..=k + 20].iter().sum::<f32>();
                sum(a).total_cmp(&sum(b))
            })
            .unwrap()
    }

    #[test]
    fn a_formant_warp_moves_the_envelope_and_keeps_the_harmonics() {
        let frame = voiced(10, 200.0);
        let mut warp = FormantWarp::new();
        let mut out = vec![Bin::default(); BINS];
        warp.apply(&frame, &mut out, 1.25, 20);
        let before = peak_bin(&frame);
        let after = peak_bin(&out);
        assert!(
            (before as f32 * 1.25 - after as f32).abs() < 12.0,
            "{before} -> {after}"
        );
        // Bins between the harmonics stay empty: the pitch did not move.
        assert!(
            out.iter()
                .enumerate()
                .all(|(k, b)| k % 10 == 0 || b.magnitude() == 0.0)
        );
    }

    #[test]
    fn a_formant_warp_of_one_copies_the_frame() {
        let frame = voiced(7, 100.0);
        let mut out = vec![Bin::default(); BINS];
        FormantWarp::new().apply(&frame, &mut out, 1.0, 20);
        assert_eq!(out, frame);
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
        let mut data = left;
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
