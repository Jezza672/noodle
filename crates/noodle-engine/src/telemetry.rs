//! Data going from the audio thread back to the UI: meter levels and scope
//! samples.
//!
//! A [`Telemetry`] hub is shared between node types that report data, such as
//! the Meter and Scope nodes, and the UI. When a reporting node is
//! instantiated (off the audio thread), its node type opens a channel in the
//! hub for that node's ID, keeps the writing end in the instance, and the hub
//! keeps the reading end. The UI reads it every frame by node ID.
//!
//! The writing ends are lock-free and never allocate, so they're safe on the
//! audio thread:
//!
//! - **Meters** are atomics. The audio thread raises a peak with `fetch_max`
//!   and stores the latest RMS; the UI takes the peak (resetting it) and loads
//!   the RMS.
//! - **Scopes** are SPSC ring buffers of interleaved frames. When the UI falls
//!   behind and the ring is full, the newest frames are dropped.
//!
//! Only the hub itself has a lock, and only the UI and plan building touch it.
//! Every use of the hub closes the channels whose writing end is gone, which
//! happens once the controller frees the instance, e.g. after the node is
//! deleted, so the hub never holds more than the live nodes' channels.
//!
//! Instantiating a node again under the same ID, for example when its config
//! changes, replaces its channel, so the hub always holds the newest
//! instance's. That also means two engines instantiating the same graph from
//! one hub (say, live playback and an offline export) would fight over the
//! channels, so give an export a registry with its own hub.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use noodle_core::NodeId;

/// The shared hub of telemetry channels, keyed by node ID. Cloning it gives
/// another handle to the same hub.
#[derive(Clone, Default)]
pub struct Telemetry {
    inner: Arc<Mutex<Channels>>,
}

#[derive(Default)]
struct Channels {
    meters: HashMap<NodeId, Arc<MeterCells>>,
    scopes: HashMap<NodeId, ScopeReader>,
}

impl Channels {
    /// Closes the channels whose writing end has been dropped.
    fn prune(&mut self) {
        self.meters.retain(|_, cells| Arc::strong_count(cells) > 1);
        self.scopes
            .retain(|_, reader| !reader.consumer.is_abandoned());
    }
}

impl Telemetry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Channels> {
        // The hub holds no invariants a panic could break, so carry on.
        let mut channels = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        channels.prune();
        channels
    }

    /// Opens a meter channel for `node` with one level per channel, replacing
    /// any it had. Call this when instantiating, not on the audio thread.
    pub fn open_meter(&self, node: NodeId, channels: usize) -> MeterWriter {
        let cells = Arc::new(MeterCells(
            (0..channels).map(|_| LevelCells::default()).collect(),
        ));
        self.lock().meters.insert(node, cells.clone());
        MeterWriter(cells)
    }

    /// Opens a scope channel for `node` that buffers up to `capacity` frames of
    /// `channels` samples each, replacing any it had. Call this when
    /// instantiating, not on the audio thread.
    pub fn open_scope(&self, node: NodeId, channels: usize, capacity: usize) -> ScopeWriter {
        let channels = channels.max(1);
        let capacity = capacity.max(1);
        let (producer, consumer) = rtrb::RingBuffer::new(capacity * channels);
        let reader = ScopeReader {
            consumer,
            channels,
            history: VecDeque::with_capacity(capacity * channels),
            max: capacity * channels,
        };
        self.lock().scopes.insert(node, reader);
        ScopeWriter { producer, channels }
    }

    /// `node`'s meter levels, one per channel, or `None` if it has no meter.
    /// Each peak is the highest since the previous call, so only one reader
    /// should poll a meter.
    pub fn meter(&self, node: NodeId) -> Option<Vec<Level>> {
        let channels = self.lock();
        let cells = channels.meters.get(&node)?;
        Some(cells.0.iter().map(LevelCells::take).collect())
    }

    /// Copies `node`'s most recent scope frames into `view`, reusing its
    /// memory. Returns false, leaving `view` alone, if the node has no scope.
    pub fn read_scope(&self, node: NodeId, view: &mut ScopeView) -> bool {
        let mut channels = self.lock();
        let Some(reader) = channels.scopes.get_mut(&node) else {
            return false;
        };
        reader.drain();
        view.channels = reader.channels;
        view.samples.clear();
        let (front, back) = reader.history.as_slices();
        view.samples.extend_from_slice(front);
        view.samples.extend_from_slice(back);
        true
    }
}

/// One channel's level, as linear amplitudes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Level {
    /// The highest absolute sample since the previous read.
    pub peak: f32,
    /// The smoothed RMS level.
    pub rms: f32,
}

struct MeterCells(Box<[LevelCells]>);

#[derive(Default)]
struct LevelCells {
    /// The bits of a non-negative f32. For those, integer order is float
    /// order, so `fetch_max` on the bits takes the larger level.
    peak: AtomicU32,
    rms: AtomicU32,
}

impl LevelCells {
    fn take(&self) -> Level {
        Level {
            peak: f32::from_bits(self.peak.swap(0, Ordering::Relaxed)),
            rms: f32::from_bits(self.rms.load(Ordering::Relaxed)),
        }
    }
}

/// The audio thread's end of a meter channel.
pub struct MeterWriter(Arc<MeterCells>);

impl MeterWriter {
    pub fn channels(&self) -> usize {
        self.0.0.len()
    }

    /// Reports one block's level for `channel`. Negative and NaN levels count
    /// as zero. Out-of-range channels are ignored.
    pub fn write(&self, channel: usize, level: Level) {
        let Some(cells) = self.0.0.get(channel) else {
            return;
        };
        cells
            .peak
            .fetch_max(non_negative(level.peak).to_bits(), Ordering::Relaxed);
        cells
            .rms
            .store(non_negative(level.rms).to_bits(), Ordering::Relaxed);
    }
}

/// Maps NaN and negative values (including -0.0, whose bits are huge) to 0.
fn non_negative(x: f32) -> f32 {
    if x > 0.0 { x } else { 0.0 }
}

/// The audio thread's end of a scope channel.
pub struct ScopeWriter {
    producer: rtrb::Producer<f32>,
    channels: usize,
}

impl ScopeWriter {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Sends up to `frames` frames, as many as fit, and returns how many were
    /// sent. `sample(frame, channel)` gives each sample.
    pub fn write(&mut self, frames: usize, mut sample: impl FnMut(usize, usize) -> f32) -> usize {
        let fit = (self.producer.slots() / self.channels).min(frames);
        let Ok(chunk) = self.producer.write_chunk_uninit(fit * self.channels) else {
            return 0;
        };
        let channels = self.channels;
        chunk.fill_from_iter(
            (0..fit)
                .flat_map(|frame| (0..channels).map(move |channel| (frame, channel)))
                .map(|(frame, channel)| sample(frame, channel)),
        );
        fit
    }
}

/// The hub's end of a scope channel.
struct ScopeReader {
    consumer: rtrb::Consumer<f32>,
    channels: usize,
    /// The most recent samples, interleaved, oldest first.
    history: VecDeque<f32>,
    /// The most samples `history` keeps: the ring's size, a whole number of
    /// frames.
    max: usize,
}

impl ScopeReader {
    fn drain(&mut self) {
        let Ok(chunk) = self.consumer.read_chunk(self.consumer.slots()) else {
            return;
        };
        let max = self.max;
        // Only whole frames are ever written, so this keeps channels aligned.
        let skip = chunk.len().saturating_sub(max);
        let excess = (self.history.len() + chunk.len() - skip).saturating_sub(max);
        self.history.drain(..excess);
        self.history.extend(chunk.into_iter().skip(skip));
    }
}

/// A copy of a scope's most recent frames, for the UI to draw. Keep one per
/// scope and refill it with [`Telemetry::read_scope`] to reuse its memory.
#[derive(Clone, Debug, Default)]
pub struct ScopeView {
    channels: usize,
    samples: Vec<f32>,
}

impl ScopeView {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The frames, interleaved, oldest first.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// One channel's samples, oldest first, or `None` if there's no such
    /// channel.
    pub fn channel(&self, channel: usize) -> Option<impl Iterator<Item = f32> + '_> {
        (channel < self.channels).then(|| {
            self.samples
                .iter()
                .skip(channel)
                .step_by(self.channels)
                .copied()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: NodeId = NodeId(7);

    #[test]
    fn meter_peaks_hold_until_read() {
        let telemetry = Telemetry::new();
        let writer = telemetry.open_meter(NODE, 2);
        writer.write(
            0,
            Level {
                peak: 0.5,
                rms: 0.1,
            },
        );
        writer.write(
            0,
            Level {
                peak: 0.25,
                rms: 0.2,
            },
        );
        writer.write(
            1,
            Level {
                peak: f32::NAN,
                rms: -1.0,
            },
        );

        let levels = telemetry.meter(NODE).unwrap();
        assert_eq!(
            levels[0],
            Level {
                peak: 0.5,
                rms: 0.2
            }
        );
        assert_eq!(levels[1], Level::default());

        // The peak resets once read; the RMS stays.
        let levels = telemetry.meter(NODE).unwrap();
        assert_eq!(
            levels[0],
            Level {
                peak: 0.0,
                rms: 0.2
            }
        );
    }

    #[test]
    fn negative_zero_doesnt_pin_the_peak() {
        let telemetry = Telemetry::new();
        let writer = telemetry.open_meter(NODE, 1);
        writer.write(
            0,
            Level {
                peak: -0.0,
                rms: 0.0,
            },
        );
        writer.write(
            0,
            Level {
                peak: 0.5,
                rms: 0.0,
            },
        );
        assert_eq!(telemetry.meter(NODE).unwrap()[0].peak, 0.5);
    }

    fn read(telemetry: &Telemetry) -> ScopeView {
        let mut view = ScopeView::default();
        assert!(telemetry.read_scope(NODE, &mut view));
        view
    }

    #[test]
    fn scope_keeps_the_latest_frames() {
        let telemetry = Telemetry::new();
        let mut writer = telemetry.open_scope(NODE, 2, 4);
        assert_eq!(writer.write(3, |f, c| (f * 10 + c) as f32), 3);
        read(&telemetry);
        assert_eq!(writer.write(3, |f, c| (100 + f * 10 + c) as f32), 3);

        let view = read(&telemetry);
        assert_eq!(
            view.samples(),
            [20.0, 21.0, 100.0, 101.0, 110.0, 111.0, 120.0, 121.0]
        );
        let right: Vec<f32> = view.channel(1).unwrap().collect();
        assert_eq!(right, [21.0, 101.0, 111.0, 121.0]);
        assert!(view.channel(2).is_none());

        // A full ring's worth replaces the history outright.
        assert_eq!(writer.write(4, |f, c| (200 + f * 10 + c) as f32), 4);
        assert_eq!(read(&telemetry).samples()[..2], [200.0, 201.0]);
    }

    #[test]
    fn a_full_scope_drops_new_frames_whole() {
        let telemetry = Telemetry::new();
        let mut writer = telemetry.open_scope(NODE, 2, 4);
        assert_eq!(writer.write(3, |_, _| 1.0), 3);
        // Only one frame's room is left.
        assert_eq!(writer.write(3, |_, _| 2.0), 1);
        assert_eq!(writer.write(3, |_, _| 3.0), 0);
        assert_eq!(
            read(&telemetry).samples(),
            [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0]
        );
    }

    #[test]
    fn reopening_replaces() {
        let telemetry = Telemetry::new();
        let old = telemetry.open_meter(NODE, 1);
        let new = telemetry.open_meter(NODE, 1);
        old.write(
            0,
            Level {
                peak: 0.9,
                rms: 0.0,
            },
        );
        new.write(
            0,
            Level {
                peak: 0.1,
                rms: 0.0,
            },
        );
        assert_eq!(telemetry.meter(NODE).unwrap()[0].peak, 0.1);
    }

    #[test]
    fn dropping_the_writer_closes_the_channel() {
        let telemetry = Telemetry::new();
        let meter = telemetry.open_meter(NODE, 1);
        let scope = telemetry.open_scope(NODE, 1, 4);
        assert!(telemetry.meter(NODE).is_some());
        assert!(telemetry.read_scope(NODE, &mut ScopeView::default()));

        drop((meter, scope));
        assert!(telemetry.meter(NODE).is_none());
        assert!(!telemetry.read_scope(NODE, &mut ScopeView::default()));
        let channels = telemetry.lock();
        assert!(channels.meters.is_empty() && channels.scopes.is_empty());
    }
}
