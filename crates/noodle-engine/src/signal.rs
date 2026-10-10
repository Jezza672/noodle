//! Audio signals: shapes, broadcasting, and the per-block views that nodes
//! read and write.

use std::fmt;

/// The shape of an audio signal: polyphonic voices × channels.
///
/// Each (voice, channel) pair is a *lane*: one stream of samples. A signal's
/// length in frames isn't part of its shape, since it changes from block to
/// block.
///
/// Shapes broadcast like numpy arrays: in each dimension the sizes must match
/// or one of them must be 1. That's what lets a mono LFO modulate an 8-voice
/// stereo filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shape {
    pub voices: usize,
    pub channels: usize,
}

impl Shape {
    pub const MONO: Shape = Shape::new(1, 1);
    pub const STEREO: Shape = Shape::new(1, 2);

    pub const fn new(voices: usize, channels: usize) -> Self {
        Self { voices, channels }
    }

    pub const fn lanes(self) -> usize {
        self.voices * self.channels
    }

    pub fn broadcast(self, other: Shape) -> Result<Shape, ShapeError> {
        let dim = |a: usize, b: usize| match (a, b) {
            _ if a == b => Some(a),
            (1, n) | (n, 1) => Some(n),
            _ => None,
        };
        match (
            dim(self.voices, other.voices),
            dim(self.channels, other.channels),
        ) {
            (Some(voices), Some(channels)) => Ok(Shape::new(voices, channels)),
            _ => Err(ShapeError(self, other)),
        }
    }

    /// Broadcasts any number of shapes together. No shapes at all gives
    /// [`Shape::MONO`].
    pub fn broadcast_all(shapes: impl IntoIterator<Item = Shape>) -> Result<Shape, ShapeError> {
        shapes.into_iter().try_fold(Shape::MONO, Shape::broadcast)
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}v×{}ch", self.voices, self.channels)
    }
}

/// Two shapes that can't be broadcast together, e.g. 8 voices meeting 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapeError(pub Shape, pub Shape);

impl fmt::Display for ShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "can't combine {} with {}: voices and channels must each match or be 1",
            self.0, self.1
        )
    }
}

impl std::error::Error for ShapeError {}

/// A read-only view of one block of a signal.
///
/// Samples are planar: each lane is a contiguous run of `frames` samples,
/// voice-major (voice 0's channels, then voice 1's, …).
#[derive(Clone, Copy, Debug)]
pub struct SignalIn<'a> {
    data: &'a [f32],
    shape: Shape,
    frames: usize,
    constant: Option<f32>,
    /// One flag per lane, or empty if the source doesn't report any.
    silent: &'a [bool],
}

impl<'a> SignalIn<'a> {
    pub fn new(data: &'a [f32], shape: Shape, frames: usize) -> Self {
        debug_assert_eq!(data.len(), shape.lanes() * frames);
        Self {
            data,
            shape,
            frames,
            constant: None,
            silent: &[],
        }
    }

    /// Attaches the producer's silence flags, one per lane (see
    /// [`SignalIn::is_silent`]).
    pub fn with_silent(self, silent: &'a [bool]) -> Self {
        debug_assert!(silent.is_empty() || silent.len() == self.shape.lanes());
        Self { silent, ..self }
    }

    /// Marks every sample as equal to `value`. The data must already hold it.
    pub fn with_constant(self, value: f32) -> Self {
        Self {
            constant: Some(value),
            ..self
        }
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// `Some(value)` if every sample in the block is `value`, e.g. an
    /// unconnected parameter that isn't mid-change. Nodes can use this to move
    /// work, such as computing filter coefficients, out of the per-sample loop.
    pub fn constant(&self) -> Option<f32> {
        self.constant
    }

    /// The samples of one lane, broadcasting: if this signal has a single
    /// voice (or channel), it serves every voice (or channel) index.
    pub fn lane(&self, voice: usize, channel: usize) -> &'a [f32] {
        let start = self.lane_index(voice, channel) * self.frames;
        &self.data[start..start + self.frames]
    }

    /// Whether every sample of the lane in this block is exactly 0.0, as far
    /// as is known without looking: a producer flagged it silent, or the whole
    /// signal is the constant 0. `false` means "not known to be", not "loud".
    /// Broadcasts like [`lane`](Self::lane).
    pub fn is_silent(&self, voice: usize, channel: usize) -> bool {
        self.constant == Some(0.0)
            || self
                .silent
                .get(self.lane_index(voice, channel))
                .copied()
                .unwrap_or(false)
    }

    fn lane_index(&self, voice: usize, channel: usize) -> usize {
        let voice = if self.shape.voices == 1 { 0 } else { voice };
        let channel = if self.shape.channels == 1 { 0 } else { channel };
        voice * self.shape.channels + channel
    }
}

/// A writable view of one block of a node output. Unlike inputs, outputs never
/// broadcast: they have exactly the shape the compiler gave them.
#[derive(Debug)]
pub struct SignalOut<'a> {
    data: &'a mut [f32],
    shape: Shape,
    frames: usize,
    /// One flag per lane, or empty if nothing downstream can read them.
    silent: &'a mut [bool],
}

impl<'a> SignalOut<'a> {
    pub fn new(data: &'a mut [f32], shape: Shape, frames: usize) -> Self {
        debug_assert_eq!(data.len(), shape.lanes() * frames);
        Self {
            data,
            shape,
            frames,
            silent: &mut [],
        }
    }

    /// Attaches the flags consumers read with [`SignalIn::is_silent`], one per
    /// lane. They are cleared here: a lane is not known to be silent until
    /// the node says so with [`set_silent`](Self::set_silent).
    pub fn with_silent(self, silent: &'a mut [bool]) -> Self {
        debug_assert!(silent.is_empty() || silent.len() == self.shape.lanes());
        silent.fill(false);
        Self { silent, ..self }
    }

    /// Tells consumers that the lane's samples are all exactly 0.0 this block.
    /// The node must have written them (or [`silence`](Self::silence)d the
    /// lane). It lets downstream nodes skip the lane, and is ignored where
    /// nothing reads it.
    pub fn set_silent(&mut self, voice: usize, channel: usize) {
        if let Some(flag) = self.silent.get_mut(voice * self.shape.channels + channel) {
            *flag = true;
        }
    }

    /// Writes zeros to the lane and flags it silent.
    pub fn silence(&mut self, voice: usize, channel: usize) {
        self.lane_mut(voice, channel).fill(0.0);
        self.set_silent(voice, channel);
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn lane_mut(&mut self, voice: usize, channel: usize) -> &mut [f32] {
        let start = (voice * self.shape.channels + channel) * self.frames;
        &mut self.data[start..start + self.frames]
    }

    pub fn fill(&mut self, value: f32) {
        self.data.fill(value);
    }
}

/// Owned storage for a signal, sized for a shape and a maximum block length.
#[derive(Clone, Debug)]
pub struct SignalBuffer {
    data: Vec<f32>,
    silent: Vec<bool>,
    shape: Shape,
    max_frames: usize,
}

impl SignalBuffer {
    pub fn new(shape: Shape, max_frames: usize) -> Self {
        Self {
            data: vec![0.0; shape.lanes() * max_frames],
            silent: vec![false; shape.lanes()],
            shape,
            max_frames,
        }
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Lanes are packed `frames` apart, so within a block a buffer must be
    /// written and read with the same `frames`.
    pub fn as_in(&self, frames: usize) -> SignalIn<'_> {
        SignalIn::new(
            &self.data[..self.shape.lanes() * frames],
            self.shape,
            frames,
        )
        .with_silent(&self.silent)
    }

    pub fn as_out(&mut self, frames: usize) -> SignalOut<'_> {
        let len = self.shape.lanes() * frames;
        SignalOut::new(&mut self.data[..len], self.shape, frames).with_silent(&mut self.silent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcasting() {
        let poly = Shape::new(8, 1);
        assert_eq!(Shape::MONO.broadcast(poly), Ok(poly));
        assert_eq!(poly.broadcast(Shape::STEREO), Ok(Shape::new(8, 2)));
        assert!(poly.broadcast(Shape::new(4, 1)).is_err());
        assert_eq!(Shape::broadcast_all([]), Ok(Shape::MONO));
    }

    #[test]
    fn silence_flags_broadcast_like_lanes_and_clear_on_write() {
        let mut buffer = SignalBuffer::new(Shape::new(3, 1), 4);
        let mut out = buffer.as_out(4);
        out.set_silent(1, 0);
        out.silence(2, 0);
        out.lane_mut(2, 0).fill(1.0);
        let signal = buffer.as_in(4);
        assert!(!signal.is_silent(0, 0));
        assert!(signal.is_silent(1, 0) && signal.is_silent(2, 0));

        // A mono source's flag serves every voice that reads it.
        let mut mono = SignalBuffer::new(Shape::MONO, 4);
        mono.as_out(4).set_silent(0, 0);
        assert!(mono.as_in(4).is_silent(5, 0));

        // Taking the buffer to write forgets the old flags.
        let _ = buffer.as_out(4);
        assert!(!buffer.as_in(4).is_silent(1, 0));
    }

    #[test]
    fn a_constant_zero_is_silent_and_other_constants_are_not() {
        let buffer = SignalBuffer::new(Shape::MONO, 4);
        assert!(!buffer.as_in(4).is_silent(0, 0));
        assert!(buffer.as_in(4).with_constant(0.0).is_silent(0, 0));
        assert!(!buffer.as_in(4).with_constant(0.5).is_silent(0, 0));
    }

    #[test]
    fn mono_input_serves_every_lane() {
        let mut buffer = SignalBuffer::new(Shape::MONO, 4);
        buffer.as_out(4).fill(0.5);
        let signal = buffer.as_in(4);
        assert_eq!(signal.lane(7, 1), &[0.5; 4]);
    }
}
