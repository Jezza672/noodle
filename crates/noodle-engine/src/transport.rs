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
    loop_on: AtomicBool,
    loop_start: AtomicI64,
    loop_end: AtomicI64,
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
            loop_on: AtomicBool::new(false),
            loop_start: AtomicI64::new(0),
            loop_end: AtomicI64::new(0),
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
    /// or backwards range is the same as off.
    pub fn set_loop(&self, range: Option<(Tick, Tick)>) {
        match range {
            Some((start, end)) => {
                self.loop_on.store(false, Ordering::Release);
                self.loop_start.store(start.0, Ordering::Relaxed);
                self.loop_end.store(end.0, Ordering::Relaxed);
                self.loop_on.store(true, Ordering::Release);
            }
            None => self.loop_on.store(false, Ordering::Release),
        }
    }

    /// The playhead, in samples. It's the audio thread's position at the end
    /// of its last block, so it trails what's heard by up to a device buffer.
    /// Convert it with the tempo map for display.
    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    // The audio thread's side.

    pub(crate) fn loop_range(&self) -> Option<(Tick, Tick)> {
        if !self.loop_on.load(Ordering::Acquire) {
            return None;
        }
        let start = self.loop_start.load(Ordering::Relaxed);
        let end = self.loop_end.load(Ordering::Relaxed);
        (start < end).then_some((Tick(start), Tick(end)))
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
