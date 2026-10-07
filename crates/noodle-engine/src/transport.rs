//! The transport as the UI controls it: play, stop, seek and loop. These are
//! shared atomics, so the UI thread sets them and the audio thread reads them
//! without a lock. The [`Processor`](crate::Processor) holds the rest of the
//! transport's state, which is the position.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

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
    seek_tick: AtomicI64,
    /// Counts seeks, so the audio thread notices a new one.
    seek_seq: AtomicU64,
    /// Where the audio thread has got to, in samples.
    position: AtomicU64,
}

impl TransportControl {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            playing: AtomicBool::new(true),
            looping: AtomicU64::new(0),
            seek_tick: AtomicI64::new(0),
            seek_seq: AtomicU64::new(0),
            position: AtomicU64::new(0),
        })
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
