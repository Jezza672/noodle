//! The cached-audio player: a source node that plays a stored render in place
//! of the subgraph that made it. A frozen node or group, and an offline node,
//! play this way.
//!
//! Live, the audio comes off the disk through a worker thread that keeps a
//! few chunks ready ahead of the playhead, in the manner of
//! `noodle_io::ClipStream`: full chunks go to the audio thread through one
//! lock-free queue and spent ones come back through another, so the audio
//! side never allocates, locks or touches the disk. The worker follows the
//! transport's loop, so a loop wraps without a gap. A jump asks the worker to
//! restart at the new chunk, and the block is silent until it has.
//!
//! In an offline render ([`CachedPlayer::blocking`]) the node reads the file
//! itself in `process`, so the output doesn't depend on timing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use noodle_core::{CacheKey, Config, Value};
use noodle_engine::{
    Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Replacement, Setup, Shape,
};
use noodle_io::{CacheStore, CachedAudio};
use rtrb::{Consumer, Producer, RingBuffer};

pub const CACHED_ID: &str = "noodle.internal.cached";

/// Frames in a chunk.
const CHUNK: usize = 4096;
/// Chunks the worker keeps ahead: about 1.4 s at 48 kHz.
const CHUNKS: usize = 16;
/// Chunks read on the spot when a player is built, from the playhead on, so
/// a render swapped in mid-playback has its first blocks ready: about 0.34 s
/// at 48 kHz, longer than the worker needs to catch up.
const PRELOAD: u64 = 4;
const IDLE: Duration = Duration::from_millis(2);

static INFO: NodeInfo = NodeInfo {
    id: CACHED_ID,
    version: 1,
    name: "Cached audio",
    category: noodle_engine::INTERNAL_CATEGORY,
};

/// Plays a render from the cache, or silence where there is none (an offline
/// node whose render isn't done).
pub struct CachedPlayer {
    store: CacheStore,
    key: Option<CacheKey>,
    shape: Shape,
    blocking: bool,
}

impl CachedPlayer {
    /// Plays the entry for `key`, which holds `shape.lanes()` channels. If
    /// the entry is missing or the wrong shape when the node is built, it
    /// plays silence.
    pub fn new(store: CacheStore, key: CacheKey, shape: Shape) -> Self {
        Self {
            store,
            key: Some(key),
            shape,
            blocking: false,
        }
    }

    /// Plays silence of `shape`: what stands in for an offline node until its
    /// render is done.
    pub fn silent(store: CacheStore, shape: Shape) -> Self {
        Self {
            store,
            key: None,
            shape,
            blocking: false,
        }
    }

    /// Reads on the calling thread instead of through a worker, for offline
    /// renders.
    #[must_use]
    pub fn blocking(self) -> Self {
        Self {
            blocking: true,
            ..self
        }
    }

    /// This player as a [`Replacement`] for the compiler. Its config names
    /// the entry, so a plan built for a different render replaces the node.
    pub fn replacement(self) -> Replacement {
        let name = self.key.map_or_else(|| "silent".into(), |key| key.to_hex());
        Replacement {
            config: Config::new().with("render", Value::Text(name)),
            node_type: Arc::new(self),
        }
    }
}

impl NodeType for CachedPlayer {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        _inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(vec![self.shape])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let Some(key) = self.key else {
            return Ok(Instance::realtime(Silent));
        };
        let audio = self
            .store
            .get(&key)
            .ok_or_else(|| NodeError::config("the cached render is gone"))?;
        let lanes = self.shape.lanes();
        if audio.info().channels != lanes {
            return Err(NodeError::config("the cached render has the wrong shape"));
        }
        Ok(Instance::realtime(if self.blocking {
            Player::Blocking(Blocking {
                audio,
                scratch: Vec::new(),
                lanes,
            })
        } else {
            Player::Streaming(Streaming::start(audio, lanes, setup.position))
        }))
    }
}

struct Silent;

impl Node for Silent {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        for output in io.outputs {
            output.fill(0.0);
        }
    }
}

enum Player {
    Streaming(Streaming),
    Blocking(Blocking),
}

impl Node for Player {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        match self {
            Self::Streaming(player) => player.process(ctx, io),
            Self::Blocking(player) => player.process(ctx, io),
        }
    }
}

/// Reads straight from the file.
struct Blocking {
    audio: CachedAudio,
    scratch: Vec<f32>,
    lanes: usize,
}

impl Blocking {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let out = &mut io.outputs[0];
        let shape = out.shape();
        let frames = ctx.frames;
        out.fill(0.0);
        if !ctx.transport.playing {
            return;
        }
        self.scratch.resize(frames * self.lanes, 0.0);
        let read = self
            .audio
            .read_frames(ctx.transport.position, &mut self.scratch)
            .unwrap_or(0);
        for lane in 0..shape.lanes() {
            let samples = out.lane_mut(lane / shape.channels, lane % shape.channels);
            for (i, sample) in samples.iter_mut().take(read).enumerate() {
                *sample = self.scratch[i * self.lanes + lane];
            }
        }
    }
}

struct Chunk {
    /// Which seek this chunk answers.
    generation: u64,
    index: u64,
    /// Lane-major: `CHUNK` samples per lane.
    data: Vec<f32>,
}

struct Shared {
    /// Bumped by the audio thread to restart the worker, after `target`.
    generation: AtomicU64,
    target: AtomicU64,
    stop: AtomicBool,
    /// The loop in frames as of the last block; an end of 0 means none.
    loop_start: AtomicU64,
    loop_end: AtomicU64,
}

/// The chunk after `index` in the order the worker makes them: the next one,
/// or the loop's first when `index` holds the loop's last frame.
fn next_index(index: u64, loop_start: u64, loop_end: u64) -> u64 {
    if loop_end > 0 && loop_end > loop_start && index == (loop_end - 1) / CHUNK as u64 {
        loop_start / CHUNK as u64
    } else {
        index + 1
    }
}

struct Streaming {
    lanes: usize,
    shared: Arc<Shared>,
    full: Consumer<Chunk>,
    spent: Producer<Chunk>,
    generation: u64,
    /// The chunk the worker delivers next, as far as this side can tell.
    expected: u64,
    current: Option<Chunk>,
    loop_range: (u64, u64),
}

impl Streaming {
    /// Starts the worker at the chunk holding `position`, and has the next
    /// few chunks read before returning (this runs off the audio thread), so
    /// playback that is already under way doesn't open with a silent block.
    fn start(audio: CachedAudio, lanes: usize, position: u64) -> Self {
        let first = position / CHUNK as u64;
        let (mut spent_tx, spent_rx) = RingBuffer::new(CHUNKS + 1);
        let (full_tx, full_rx) = RingBuffer::new(CHUNKS + 1);
        // One more than the worker can have in flight, for the chunk the
        // audio thread is playing.
        for _ in 0..=CHUNKS {
            let _ = spent_tx.push(Chunk {
                generation: 0,
                index: 0,
                data: vec![0.0; CHUNK * lanes],
            });
        }
        let shared = Arc::new(Shared {
            generation: AtomicU64::new(0),
            target: AtomicU64::new(first),
            stop: AtomicBool::new(false),
            loop_start: AtomicU64::new(0),
            loop_end: AtomicU64::new(0),
        });
        let mut worker = Worker {
            audio,
            lanes,
            shared: Arc::clone(&shared),
            spent: spent_rx,
            full: full_tx,
            scratch: vec![0.0; CHUNK * lanes],
            next: first,
        };
        worker.preload(PRELOAD);
        thread::Builder::new()
            .name("noodle-cached".into())
            .spawn(move || worker.run())
            .expect("can't start a thread");
        Self {
            lanes,
            shared,
            full: full_rx,
            spent: spent_tx,
            generation: 0,
            expected: first,
            current: None,
            loop_range: (0, 0),
        }
    }

    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let out = &mut io.outputs[0];
        let shape = out.shape();
        out.fill(0.0);
        let transport = &ctx.transport;
        let range = transport.loop_range.unwrap_or((0, 0));
        if range != self.loop_range {
            self.loop_range = range;
            self.shared.loop_start.store(range.0, Ordering::Relaxed);
            self.shared.loop_end.store(range.1, Ordering::Relaxed);
        }
        if !transport.playing {
            return;
        }
        let mut done = 0;
        while done < ctx.frames {
            let position = transport.position + done as u64;
            let index = position / CHUNK as u64;
            let offset = (position % CHUNK as u64) as usize;
            if !self.ensure(index) {
                break;
            }
            let chunk = self.current.as_ref().expect("ensure left a chunk");
            let n = (ctx.frames - done).min(CHUNK - offset);
            for lane in 0..self.lanes {
                let source = &chunk.data[lane * CHUNK + offset..lane * CHUNK + offset + n];
                let samples = out.lane_mut(lane / shape.channels, lane % shape.channels);
                samples[done..done + n].copy_from_slice(source);
            }
            done += n;
        }
    }

    /// Makes the chunk `index` current, if it is ready. If not, makes sure it
    /// is on its way, and returns false so the caller plays silence.
    fn ensure(&mut self, index: u64) -> bool {
        if self
            .current
            .as_ref()
            .is_some_and(|c| c.generation == self.generation && c.index == index)
        {
            return true;
        }
        if let Some(chunk) = self.current.take() {
            let _ = self.spent.push(chunk);
        }
        let (start, end) = self.loop_range;
        loop {
            let Ok(head) = self.full.peek() else {
                // Nothing ready. If the worker is heading for the chunk,
                // wait; if not, send it there.
                let ahead = index.wrapping_sub(self.expected);
                if ahead >= CHUNKS as u64 {
                    self.seek(index);
                }
                return false;
            };
            let (generation, head_index) = (head.generation, head.index);
            if generation != self.generation {
                self.recycle();
                continue;
            }
            if head_index == index {
                let chunk = self.full.pop().expect("peeked");
                self.expected = next_index(index, start, end);
                self.current = Some(chunk);
                return true;
            }
            // Skipping forward within what the worker already made is cheaper
            // than a seek; anything else needs one.
            if head_index < index && index - head_index < CHUNKS as u64 {
                self.expected = next_index(head_index, start, end);
                self.recycle();
                continue;
            }
            self.seek(index);
            return false;
        }
    }

    fn recycle(&mut self) {
        if let Ok(chunk) = self.full.pop() {
            let _ = self.spent.push(chunk);
        }
    }

    fn seek(&mut self, index: u64) {
        self.generation += 1;
        self.expected = index;
        self.shared.target.store(index, Ordering::Release);
        self.shared
            .generation
            .store(self.generation, Ordering::Release);
    }
}

impl Drop for Streaming {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

struct Worker {
    audio: CachedAudio,
    lanes: usize,
    shared: Arc<Shared>,
    spent: Consumer<Chunk>,
    full: Producer<Chunk>,
    scratch: Vec<f32>,
    /// The chunk to make next.
    next: u64,
}

impl Worker {
    /// Makes the next `count` chunks now, on the calling thread.
    fn preload(&mut self, count: u64) {
        for _ in 0..count {
            let Ok(mut chunk) = self.spent.pop() else {
                return;
            };
            self.fill(&mut chunk, self.next);
            chunk.generation = 0;
            chunk.index = self.next;
            self.next += 1;
            let _ = self.full.push(chunk);
        }
    }

    fn run(mut self) {
        let mut generation = 0;
        let mut next = self.next;
        while !self.shared.stop.load(Ordering::Relaxed) {
            let latest = self.shared.generation.load(Ordering::Acquire);
            if latest != generation {
                generation = latest;
                next = self.shared.target.load(Ordering::Acquire);
            }
            let Ok(mut chunk) = self.spent.pop() else {
                thread::sleep(IDLE);
                continue;
            };
            self.fill(&mut chunk, next);
            chunk.generation = generation;
            chunk.index = next;
            let (start, end) = (
                self.shared.loop_start.load(Ordering::Relaxed),
                self.shared.loop_end.load(Ordering::Relaxed),
            );
            next = next_index(next, start, end);
            let _ = self.full.push(chunk);
        }
    }

    /// Reads chunk `index` and lays it out lane by lane. Whatever the entry
    /// doesn't cover is silence.
    fn fill(&mut self, chunk: &mut Chunk, index: u64) {
        let read = self
            .audio
            .read_frames(index * CHUNK as u64, &mut self.scratch)
            .unwrap_or(0);
        for lane in 0..self.lanes {
            let lane_data = &mut chunk.data[lane * CHUNK..(lane + 1) * CHUNK];
            for (i, sample) in lane_data.iter_mut().enumerate() {
                *sample = if i < read {
                    self.scratch[i * self.lanes + lane]
                } else {
                    0.0
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::KeyBuilder;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;

    /// A render of 100 000 frames of a ramp, in a store of its own.
    fn ramp() -> (tempfile::TempDir, CacheStore, CacheKey) {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::open(dir.path()).unwrap();
        let key = KeyBuilder::new("cached-test").u64(1).finish();
        let mut writer = store.writer(&key, 1, RATE as u32).unwrap();
        let samples: Vec<f32> = (0..100_000).map(|i| (i + 1) as f32).collect();
        writer.write(&samples).unwrap();
        writer.commit().unwrap();
        (dir, store, key)
    }

    #[test]
    fn a_player_built_mid_playback_has_its_first_block_ready() {
        let (_dir, store, key) = ramp();
        let player = CachedPlayer::new(store, key, Shape::MONO);
        // Built with the playhead at 60 000 and run at once, as when a
        // render finishes during playback: no waiting for the worker.
        let mut h = Harness::starting_at(&player, &Config::new(), &[], RATE, 64, 60_000).unwrap();
        h.run(64).unwrap();
        let out = h.output(0).lane(0, 0).to_vec();
        let expected: Vec<f32> = (60_000..60_064).map(|i| (i + 1) as f32).collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn a_player_still_follows_the_playhead_after_the_preload() {
        let (_dir, store, key) = ramp();
        let player = CachedPlayer::new(store, key, Shape::MONO);
        let mut h = Harness::starting_at(&player, &Config::new(), &[], RATE, 4096, 40_000).unwrap();
        // Well past the preloaded chunks: the worker catches up.
        let mut heard = Vec::new();
        for _ in 0..4 {
            h.run(4096).unwrap();
            heard.extend_from_slice(h.output(0).lane(0, 0));
            std::thread::sleep(Duration::from_millis(20));
        }
        // The first block is ready at once; later ones may be silent while
        // the worker catches up, but never wrong.
        assert_eq!(heard[0], 40_001.0);
        for (i, &x) in heard.iter().enumerate() {
            if x != 0.0 {
                assert_eq!(x, (40_000 + i + 1) as f32, "at {i}");
            }
        }
    }
}
