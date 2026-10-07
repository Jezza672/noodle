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
//! A node can have several channels open at once, because instances are
//! created when a plan is built, before it reaches the audio thread, and a
//! plan can be thrown away unsent (or its instance never take over, if a
//! later plan carries over the older one). The hub reads the newest channel
//! that has been written to, falling back to the newest, and closes the others
//! once their instances are freed. So readings follow whichever instance is
//! actually playing.
//!
//! Two engines instantiating the same graph from one hub (say, live playback
//! and an offline export) would both look live, so give an export a registry
//! with its own hub.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use noodle_core::NodeId;

/// The shared hub of telemetry channels, keyed by node ID. Cloning it gives
/// another handle to the same hub.
#[derive(Clone, Default)]
pub struct Telemetry {
    inner: Arc<Mutex<Channels>>,
}

/// Each node's open channels, oldest first.
#[derive(Default)]
struct Channels {
    meters: HashMap<NodeId, Vec<Arc<MeterCells>>>,
    scopes: HashMap<NodeId, Vec<ScopeReader>>,
}

impl Channels {
    /// Closes the channels whose writing end has been dropped.
    fn prune(&mut self) {
        self.meters.retain(|_, list| {
            list.retain(|cells| Arc::strong_count(cells) > 1);
            !list.is_empty()
        });
        self.scopes.retain(|_, list| {
            list.retain(|reader| !reader.consumer.is_abandoned());
            !list.is_empty()
        });
    }
}

/// The channel to read from `list` (oldest first): the newest one written to,
/// or else the newest.
fn live<T>(list: &[T], written: impl Fn(&T) -> bool) -> Option<&T> {
    list.iter().rev().find(|c| written(c)).or(list.last())
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

    /// Opens a meter channel for `node` with one level per channel. Call this
    /// when instantiating, not on the audio thread.
    pub fn open_meter(&self, node: NodeId, channels: usize) -> MeterWriter {
        let cells = Arc::new(MeterCells {
            levels: (0..channels).map(|_| LevelCells::default()).collect(),
            written: AtomicBool::new(false),
        });
        self.lock()
            .meters
            .entry(node)
            .or_default()
            .push(cells.clone());
        MeterWriter(cells)
    }

    /// Opens a scope channel for `node` that buffers up to `capacity` frames of
    /// `channels` samples each. Call this when instantiating, not on the audio
    /// thread.
    pub fn open_scope(&self, node: NodeId, channels: usize, capacity: usize) -> ScopeWriter {
        let channels = channels.max(1);
        let capacity = capacity.max(1);
        let (producer, consumer) = rtrb::RingBuffer::new(capacity * channels);
        let reader = ScopeReader {
            consumer,
            channels,
            history: VecDeque::with_capacity(capacity * channels),
            max: capacity * channels,
            written: false,
        };
        self.lock().scopes.entry(node).or_default().push(reader);
        ScopeWriter { producer, channels }
    }

    /// `node`'s meter levels, one per channel, or `None` if it has no meter.
    /// Each peak is the highest since the previous call, so only one reader
    /// should poll a meter.
    pub fn meter(&self, node: NodeId) -> Option<Vec<Level>> {
        let channels = self.lock();
        let list = channels.meters.get(&node)?;
        let cells = live(list, |cells| cells.written.load(Ordering::Relaxed))?;
        Some(cells.levels.iter().map(LevelCells::take).collect())
    }

    /// Copies `node`'s most recent scope frames into `view`, reusing its
    /// memory. Returns false, leaving `view` alone, if the node has no scope.
    pub fn read_scope(&self, node: NodeId, view: &mut ScopeView) -> bool {
        let mut channels = self.lock();
        let Some(list) = channels.scopes.get_mut(&node) else {
            return false;
        };
        list.iter_mut().for_each(ScopeReader::drain);
        let Some(reader) = live(list, |reader| reader.written) else {
            return false;
        };
        view.channels = reader.channels;
        view.samples.clear();
        let len = reader.history.len();
        let skip = view
            .limit
            .map_or(0, |frames| len.saturating_sub(frames * reader.channels));
        view.samples.extend(reader.history.range(skip..));
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

struct MeterCells {
    levels: Box<[LevelCells]>,
    /// Set once the writer has reported, which shows its instance is playing.
    written: AtomicBool,
}

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
        self.0.levels.len()
    }

    /// Reports one block's level for `channel`. Negative and NaN levels count
    /// as zero. Out-of-range channels are ignored.
    pub fn write(&self, channel: usize, level: Level) {
        let Some(cells) = self.0.levels.get(channel) else {
            return;
        };
        self.0.written.store(true, Ordering::Relaxed);
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
    /// Whether anything has arrived, which shows the writer's instance is
    /// playing.
    written: bool,
}

impl ScopeReader {
    fn drain(&mut self) {
        let Ok(chunk) = self.consumer.read_chunk(self.consumer.slots()) else {
            return;
        };
        let max = self.max;
        // Only whole frames are ever written, so this keeps channels aligned.
        self.written |= !chunk.is_empty();
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
    /// The most frames to copy, or `None` for everything the hub keeps.
    limit: Option<usize>,
}

impl ScopeView {
    /// A view that copies only the most recent `frames` frames, for drawing
    /// that never looks further back.
    pub fn tail(frames: usize) -> Self {
        Self {
            limit: Some(frames),
            ..Self::default()
        }
    }

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

        let mut tail = ScopeView::tail(2);
        assert!(telemetry.read_scope(NODE, &mut tail));
        assert_eq!(tail.samples(), [110.0, 111.0, 120.0, 121.0]);

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

    fn peak(peak: f32) -> Level {
        Level { peak, rms: 0.0 }
    }

    #[test]
    fn meters_follow_the_playing_instance() {
        let telemetry = Telemetry::new();
        let read = || telemetry.meter(NODE).unwrap()[0].peak;
        let playing = telemetry.open_meter(NODE, 1);
        playing.write(0, peak(0.9));

        // A rebuilt instance that hasn't started playing yet.
        let pending = telemetry.open_meter(NODE, 1);
        assert_eq!(read(), 0.9);
        // Its plan is thrown away, so the older instance plays on.
        drop(pending);
        playing.write(0, peak(0.8));
        assert_eq!(read(), 0.8);

        // A rebuilt instance that does take over.
        let rebuilt = telemetry.open_meter(NODE, 1);
        rebuilt.write(0, peak(0.1));
        playing.write(0, peak(0.7));
        assert_eq!(read(), 0.1);
        drop(playing);
        assert_eq!(telemetry.lock().meters[&NODE].len(), 1);
    }

    #[test]
    fn scopes_follow_the_playing_instance() {
        let telemetry = Telemetry::new();
        let mut playing = telemetry.open_scope(NODE, 1, 4);
        playing.write(1, |_, _| 1.0);
        let mut pending = telemetry.open_scope(NODE, 1, 4);
        assert_eq!(read(&telemetry).samples(), [1.0]);

        pending.write(1, |_, _| 2.0);
        assert_eq!(read(&telemetry).samples(), [2.0]);
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
