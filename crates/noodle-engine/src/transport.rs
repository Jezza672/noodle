//! The transport as the UI controls it: play, stop, seek and loop. These are
//! shared atomics, so the UI thread sets them and the audio thread reads them
//! without a lock. The [`Processor`](crate::Processor) holds the rest of the
//! transport's state, which is the position.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use noodle_core::Tick;

/// What the UI asks of the transport. Get one from
/// [`Controller::transport`](crate::Controller::transport).
///
/// A new transport is playing from the start, which is how a graph with no
/// timeline runs: time just goes on. Stopping holds the position and tells
/// nodes the transport isn't playing, but the graph keeps rendering, so
/// effect tails ring out and live input still comes through.
pub struct TransportControl {
    playing: AtomicBool,
    /// The loop's start in the high half and end in the low half, in ticks,
    /// so the audio thread never sees a start from one loop and an end from
    /// another. Zero means no loop.
    looping: AtomicU64,
    /// Whether a recording is counting in (see [`TransportControl::set_count_in`]).
    count_in: AtomicBool,
    seek_tick: AtomicI64,
    /// Counts seeks, so the audio thread notices a new one.
    seek_seq: AtomicU64,
    /// Where the audio thread has got to, in samples.
    position: AtomicU64,
    input: Arc<InputTap>,
}

/// The most device input channels an [`InputTap`] keeps.
pub const TAP_CHANNELS: usize = 16;

/// The device input of the block being rendered, for nodes that monitor it.
/// The audio thread writes it before the plan runs and nodes read it during
/// the run, so no lock is needed: samples are atomics, read and written
/// relaxed. A block's frames never outlast it; a node that reads a channel
/// after the next block began would hear the next block's audio, which can't
/// happen because blocks run one after another.
pub struct InputTap {
    channels: AtomicUsize,
    frames: AtomicUsize,
    /// `TAP_CHANNELS` stripes of `capacity` samples.
    data: Box<[AtomicU32]>,
    capacity: usize,
}

impl InputTap {
    /// A tap that holds blocks of up to `max_frames` frames. The engine makes
    /// its own; this is for hosts that run nodes by hand.
    pub fn new(max_frames: usize) -> Self {
        Self {
            channels: AtomicUsize::new(0),
            frames: AtomicUsize::new(0),
            data: (0..max_frames * TAP_CHANNELS)
                .map(|_| AtomicU32::new(0))
                .collect(),
            capacity: max_frames,
        }
    }

    /// Stores a block of interleaved input of `channels` channels. Channels
    /// past [`TAP_CHANNELS`] and frames past the capacity are not kept.
    pub fn write(&self, samples: &[f32], channels: usize) {
        if channels == 0 {
            self.clear();
            return;
        }
        let frames = (samples.len() / channels).min(self.capacity);
        for channel in 0..channels.min(TAP_CHANNELS) {
            let stripe = &self.data[channel * self.capacity..][..frames];
            for (slot, &x) in stripe
                .iter()
                .zip(samples.iter().skip(channel).step_by(channels))
            {
                slot.store(x.to_bits(), Ordering::Relaxed);
            }
        }
        self.frames.store(frames, Ordering::Relaxed);
        self.channels.store(channels, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        self.frames.store(0, Ordering::Relaxed);
        self.channels.store(0, Ordering::Relaxed);
    }

    /// How many channels the device is giving, or 0 with no input.
    pub fn channels(&self) -> usize {
        self.channels.load(Ordering::Relaxed)
    }

    /// Adds `gain` times device channel `channel` (from 0) into `out`, for
    /// as many frames as `out` has and the block gave. A mono device feeds
    /// every channel. Does nothing for a channel the device doesn't have.
    pub fn add_channel(&self, channel: usize, gain: f32, out: &mut [f32]) {
        let channels = self.channels();
        let source = match channels {
            0 => return,
            1 => 0,
            n if channel < n => channel,
            _ => return,
        };
        if source >= TAP_CHANNELS {
            return;
        }
        let frames = self.frames.load(Ordering::Relaxed).min(out.len());
        let stripe = &self.data[source * self.capacity..][..frames];
        for (o, slot) in out.iter_mut().zip(stripe) {
            *o += gain * f32::from_bits(slot.load(Ordering::Relaxed));
        }
    }
}

impl std::fmt::Debug for InputTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputTap")
            .field("channels", &self.channels())
            .finish()
    }
}

impl TransportControl {
    pub(crate) fn new(max_frames: usize) -> Arc<Self> {
        Arc::new(Self {
            playing: AtomicBool::new(true),
            looping: AtomicU64::new(0),
            count_in: AtomicBool::new(false),
            seek_tick: AtomicI64::new(0),
            seek_seq: AtomicU64::new(0),
            position: AtomicU64::new(0),
            input: Arc::new(InputTap::new(max_frames)),
        })
    }

    /// The device input of the block being rendered, for monitoring.
    pub fn input_tap(&self) -> Arc<InputTap> {
        self.input.clone()
    }

    pub(crate) fn tap(&self) -> &InputTap {
        &self.input
    }

    pub fn play(&self) {
        self.playing.store(true, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.playing.store(false, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Makes the metronome click while on, whether or not it is switched on:
    /// the count-in before a recording.
    pub fn set_count_in(&self, on: bool) {
        self.count_in.store(on, Ordering::Relaxed);
    }

    pub(crate) fn counting_in(&self) -> bool {
        self.count_in.load(Ordering::Relaxed)
    }

    /// Moves the playhead to `tick`. The output fades out and back in around
    /// the jump, and nodes are reset. Ticks before the start go to the start.
    pub fn seek(&self, tick: Tick) {
        self.seek_tick.store(tick.0, Ordering::Relaxed);
        self.seek_seq.fetch_add(1, Ordering::Release);
    }

    /// Loops between two ticks while playing, or turns looping off. An empty
    /// or backwards range is the same as off. A playhead already past the end
    /// when the loop is set plays on and never wraps, as in most DAWs. Ticks
    /// are held in 32 bits each, which reaches about 26 days at 120 bpm;
    /// later ones are clamped.
    pub fn set_loop(&self, range: Option<(Tick, Tick)>) {
        let pack = |tick: Tick| u64::from(u32::try_from(tick.0.max(0)).unwrap_or(u32::MAX));
        let packed = match range {
            Some((start, end)) if start < end => pack(start) << 32 | pack(end),
            _ => 0,
        };
        self.looping.store(packed, Ordering::Relaxed);
    }

    /// The playhead, in samples. It's the audio thread's position at the end
    /// of its last block, so it trails what's heard by up to a device buffer.
    /// Convert it with the tempo map for display.
    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    // The audio thread's side.

    pub(crate) fn loop_range(&self) -> Option<(Tick, Tick)> {
        let packed = self.looping.load(Ordering::Relaxed);
        let (start, end) = (packed >> 32, packed & u64::from(u32::MAX));
        (start < end).then_some((Tick(start as i64), Tick(end as i64)))
    }

    /// A seek made since `seen`, which is updated.
    pub(crate) fn take_seek(&self, seen: &mut u64) -> Option<Tick> {
        let seq = self.seek_seq.load(Ordering::Acquire);
        if seq == *seen {
            return None;
        }
        *seen = seq;
        Some(Tick(self.seek_tick.load(Ordering::Relaxed)))
    }

    pub(crate) fn publish(&self, position: u64) {
        self.position.store(position, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_is_read_back_and_a_mono_device_feeds_every_channel() {
        let tap = InputTap::new(4);
        // Two channels, three frames: left 1 2 3, right 10 20 30.
        tap.write(&[1.0, 10.0, 2.0, 20.0, 3.0, 30.0], 2);
        assert_eq!(tap.channels(), 2);
        let mut out = [0.5; 4];
        tap.add_channel(1, 2.0, &mut out);
        // Three frames were given; the fourth is untouched.
        assert_eq!(out, [20.5, 40.5, 60.5, 0.5]);
        // A channel the device doesn't have adds nothing.
        let mut out = [0.0; 3];
        tap.add_channel(2, 1.0, &mut out);
        assert_eq!(out, [0.0; 3]);
        // Mono feeds channel 0 and 1 alike.
        tap.write(&[7.0, 8.0], 1);
        let (mut a, mut b) = ([0.0; 2], [0.0; 2]);
        tap.add_channel(0, 1.0, &mut a);
        tap.add_channel(1, 1.0, &mut b);
        assert_eq!((a, b), ([7.0, 8.0], [7.0, 8.0]));
        // No input is silence.
        tap.clear();
        let mut out = [1.0; 2];
        tap.add_channel(0, 1.0, &mut out);
        assert_eq!((tap.channels(), out), (0, [1.0, 1.0]));
    }
}
