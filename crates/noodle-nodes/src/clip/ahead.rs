//! The worker that stretches warped clips ahead of the playhead.
//!
//! Repitch is a resampling the audio thread does itself. Grain and Beats are
//! too heavy for that, so a worker thread per track input node renders them
//! into a ring buffer, and the audio thread only copies out of it. This is
//! the same arrangement the hub has for decoding: the worker opens what the
//! node will need before the playhead gets there, hands it over through a
//! queue, and takes it back to free it.
//!
//! A [`Lane`] is the audio-thread end of one stretched section: a ring of
//! stereo frames and a small control block. The node tracks which frame of the
//! section the next ring frame is (`read_pos`). When the playhead is
//! somewhere else (a seek, a jump, a loop wrap, the section was edited) the
//! node asks the worker for a new position through the control block; the
//! worker restarts the stretch there, and tells the node how many frames of
//! the old position were already in the ring, which the node drops. Neither
//! side ever waits for the other, and nothing on the audio thread allocates
//! or locks.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use noodle_core::ClipId;
use noodle_io::{ClipStream, StreamSpec, StreamWorker, detect_onsets, open_stream};
use rtrb::{Consumer, Producer, RingBuffer};

use super::hub::{LOOKAHEAD, MAX_CHANNELS, Shared, wanted};
use super::read_frames;
use super::schedule::ScheduledClip;
use super::stretch::{Source, Stretcher};

/// Lanes a node can hold: the sections playing and about to, and the ones
/// opened ahead for the loop's start.
pub(super) const MAX_LANES: usize = 8;
/// Stereo frames a lane's ring holds, about 340 ms.
const RING_FRAMES: usize = 16_384;
/// The worker renders this many frames at a time.
const CHUNK_FRAMES: usize = 1_024;
const POLL: Duration = Duration::from_millis(2);
const IDLE_POLL: Duration = Duration::from_millis(20);
/// How long before a file that failed is tried again.
const RETRY: Duration = Duration::from_secs(2);
/// Source frames kept behind the stretcher's reading, for the grains that
/// reach back.
const HISTORY: i64 = 16_384;
/// How far ahead of the end of what is held the stream is read on instead of
/// being seeked.
const MAX_SKIP: i64 = 32_768;
const QUEUE: usize = 16;

/// What the node and the worker share about one lane.
struct Control {
    /// The node's latest request: restart at `want_pos`. A new `want_gen`
    /// makes a request.
    want_gen: AtomicU64,
    want_pos: AtomicU64,
    /// The worker's answer: it has restarted for `ack_gen`, and `ack_frames`
    /// frames were pushed to the ring before that.
    ack_gen: AtomicU64,
    ack_frames: AtomicU64,
    /// The node has played from a lane that was opened ahead for the loop's
    /// start, so it is an ordinary lane now.
    promoted: AtomicBool,
}

/// The audio-thread end of one stretched section.
pub(super) struct Lane {
    pub id: ClipId,
    /// Identifies the section as it sounds: its clip, source, mode and warp.
    pub key: u64,
    /// Tells this lane apart from others for the same section.
    pub serial: u64,
    /// Opened ahead for the loop's start, and not played from yet.
    pub head: bool,
    ring: Consumer<f32>,
    control: Arc<Control>,
    /// The section frame the next frame out of the ring is.
    read_pos: u64,
    /// How many frames the playhead has gone on since then: it played
    /// silence while the ring had nothing for it. They are dropped from the
    /// ring as it fills, so the lane catches up with the playhead.
    behind: u64,
    /// Frames taken out of the ring, ever.
    taken: u64,
    request: u64,
    /// How many frames of the ring were there before the worker restarted;
    /// they are dropped before playing on. None until the worker answers.
    stale: Option<u64>,
    synced: bool,
}

impl Lane {
    /// Makes this an ordinary lane, now that it is played from.
    pub fn promote(&mut self) {
        if self.head {
            self.head = false;
            self.control.promoted.store(true, Ordering::Relaxed);
        }
    }

    /// Asks the worker to restart the stretch at section frame `pos`.
    pub fn seek(&mut self, pos: u64) {
        self.request += 1;
        self.control.want_pos.store(pos, Ordering::Relaxed);
        self.control.want_gen.store(self.request, Ordering::Release);
        self.read_pos = pos;
        self.behind = 0;
        self.stale = None;
        self.synced = false;
    }

    /// The section frame the playhead should be at for the lane to be on
    /// time.
    pub fn expected(&self) -> u64 {
        self.read_pos + self.behind
    }

    /// Takes in the worker's answer to a seek and drops what the lane has to
    /// skip to be on time. True once the next frame out of the ring is the
    /// one the playhead is at.
    pub fn catch_up(&mut self) -> bool {
        self.sync();
        if !self.synced {
            return false;
        }
        let n = self.behind.min(self.ring.slots() as u64 / 2) as usize;
        if n > 0
            && let Ok(chunk) = self.ring.read_chunk(n * 2)
        {
            chunk.commit_all();
            self.taken += n as u64;
            self.read_pos += n as u64;
            self.behind -= n as u64;
        }
        self.behind == 0
    }

    /// Notes that the playhead went on `frames` without the lane.
    pub fn miss(&mut self, frames: usize) {
        self.behind += frames as u64;
    }

    /// Takes in the worker's answer to a seek, and drops what the ring still
    /// held from before it.
    fn sync(&mut self) {
        if self.synced {
            return;
        }
        if self.stale.is_none() && self.control.ack_gen.load(Ordering::Acquire) == self.request {
            self.stale = Some(self.control.ack_frames.load(Ordering::Relaxed));
        }
        let Some(upto) = self.stale else { return };
        let owed = upto.saturating_sub(self.taken) as usize;
        let n = owed.min(self.ring.slots() / 2);
        if n > 0
            && let Ok(chunk) = self.ring.read_chunk(n * 2)
        {
            chunk.commit_all();
            self.taken += n as u64;
        }
        self.synced = self.taken >= upto;
    }

    /// Copies up to `out.len() / 2` frames from the ring and returns how many.
    /// Call [`catch_up`](Self::catch_up) first.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        if !self.synced || self.behind > 0 {
            return 0;
        }
        let n = (out.len() / 2).min(self.ring.slots() / 2);
        if n == 0 {
            return 0;
        }
        let Ok(chunk) = self.ring.read_chunk(n * 2) else {
            return 0;
        };
        let (a, b) = chunk.as_slices();
        out[..a.len()].copy_from_slice(a);
        out[a.len()..n * 2].copy_from_slice(b);
        chunk.commit_all();
        self.taken += n as u64;
        self.read_pos += n as u64;
        n
    }
}

/// The node's ends of the queues.
pub(super) struct AheadLinks {
    pub lanes: Consumer<Box<Lane>>,
    pub returned: Producer<Box<Lane>>,
}

pub(super) fn spawn(shared: Arc<Shared>, rate: u32) -> AheadLinks {
    let (to_node, lanes) = RingBuffer::new(QUEUE);
    let (returned, from_node) = RingBuffer::new(QUEUE * 2);
    thread::Builder::new()
        .name("noodle-warp".into())
        .spawn(move || run(&shared, rate, to_node, from_node))
        .expect("can't start a thread");
    AheadLinks { lanes, returned }
}

/// The worker's end of one lane.
struct Job {
    serial: u64,
    id: ClipId,
    key: u64,
    head: bool,
    ring: Producer<f32>,
    control: Arc<Control>,
    stretcher: Stretcher<StreamSource>,
    /// How long the section is, and the next frame of it to render.
    length: u64,
    pos: u64,
    request: u64,
    /// Frames pushed to the ring, ever.
    pushed: u64,
    buffer: Vec<f32>,
}

impl Job {
    /// Follows a request to restart, then renders as much as the ring has
    /// room for. Returns whether it did anything.
    fn work(&mut self) -> bool {
        let wanted = self.control.want_gen.load(Ordering::Acquire);
        if wanted != self.request {
            self.request = wanted;
            self.pos = self
                .control
                .want_pos
                .load(Ordering::Relaxed)
                .min(self.length);
            self.stretcher.seek(self.pos);
            // Everything pushed so far belongs to the old position.
            self.control
                .ack_frames
                .store(self.pushed, Ordering::Relaxed);
            self.control.ack_gen.store(self.request, Ordering::Release);
        }
        let mut busy = false;
        while self.pos < self.length && self.ring.slots() >= CHUNK_FRAMES * 2 {
            let n = (CHUNK_FRAMES as u64).min(self.length - self.pos) as usize;
            let made = self.stretcher.read(&mut self.buffer[..n * 2]);
            // Whatever the stretcher didn't fill is silence.
            self.buffer[made * 2..n * 2].fill(0.0);
            let Ok(mut chunk) = self.ring.write_chunk(n * 2) else {
                break;
            };
            let (a, b) = chunk.as_mut_slices();
            let split = a.len();
            a.copy_from_slice(&self.buffer[..split]);
            b.copy_from_slice(&self.buffer[split..n * 2]);
            chunk.commit_all();
            self.pos += n as u64;
            self.pushed += n as u64;
            busy = true;
            if self.control.want_gen.load(Ordering::Relaxed) != self.request {
                break;
            }
        }
        busy
    }
}

/// The stretcher's view of a file: decoded frames from a stream, with a window
/// of them kept so that grains can reach back a little.
struct StreamSource {
    stream: ClipStream,
    channels: usize,
    total: i64,
    window: Vec<f32>,
    /// The window holds source frames `start..end`; the stream is at `end`.
    start: i64,
    end: i64,
    // Never read: dropping it stops and joins the stream's thread.
    _worker: Option<StreamWorker>,
}

impl StreamSource {
    fn new(stream: ClipStream, worker: StreamWorker) -> Self {
        let channels = stream.channels();
        let total = stream.total_frames() as i64;
        Self {
            stream,
            channels,
            total,
            window: Vec::new(),
            start: 0,
            end: 0,
            _worker: Some(worker),
        }
    }

    /// Makes sure source frames `lo..hi` (inside the file) are held.
    fn ensure(&mut self, lo: i64, hi: i64) {
        let channels = self.channels;
        if lo < self.start || lo > self.end + MAX_SKIP {
            self.stream.seek(lo as u64);
            self.window.clear();
            self.start = lo;
            self.end = lo;
        }
        // Read up to `hi` in pieces, forgetting what is too far behind.
        while self.end < hi {
            let want = ((hi - self.end) as usize).min(8192);
            let from = self.window.len();
            self.window.resize(from + want * channels, 0.0);
            let got = read_frames(&mut self.stream, &mut self.window[from..], true);
            self.window.truncate(from + got * channels);
            self.end += got as i64;
            if got < want {
                break;
            }
        }
        let behind = (lo - HISTORY - self.start).max(0);
        if behind > HISTORY {
            self.window.drain(..behind as usize * channels);
            self.start += behind;
        }
    }
}

impl Source for StreamSource {
    fn channels(&self) -> usize {
        self.channels
    }

    fn read(&mut self, start: i64, out: &mut [f32]) {
        let channels = self.channels;
        let frames = (out.len() / channels) as i64;
        let (lo, hi) = (start.max(0), (start + frames).min(self.total));
        out.fill(0.0);
        if hi <= lo {
            return;
        }
        self.ensure(lo, hi);
        // What the file really has may be less than it said.
        let hi = hi.min(self.end);
        if hi <= lo {
            return;
        }
        let from = (lo - self.start) as usize * channels;
        let to = (hi - self.start) as usize * channels;
        let at = (lo - start) as usize * channels;
        out[at..at + (to - from)].copy_from_slice(&self.window[from..to]);
    }
}

/// Where the transients of a clip's audio are, in frames of the clip's own
/// stream, found by reading the whole clip once.
fn analyse(clip: &ScheduledClip, rate: u32) -> Option<Vec<u64>> {
    let spec = StreamSpec {
        path: clip.source.path.clone(),
        rate,
        offset: clip.source.offset,
        length: clip.source.length,
        start: 0,
        chunks: 8,
    };
    let (mut stream, _worker) = open_stream(spec).ok()?;
    let channels = stream.channels();
    let mut mono: Vec<f32> = Vec::with_capacity(stream.total_frames() as usize);
    let mut buffer = vec![0.0f32; 4096 * channels];
    loop {
        let got = read_frames(&mut stream, &mut buffer, true);
        mono.extend(
            buffer[..got * channels]
                .chunks_exact(channels)
                .map(|f| f.iter().sum::<f32>() / channels as f32),
        );
        if got < 4096 {
            break;
        }
    }
    Some(detect_onsets(&mono, rate))
}

/// Opens a job for `clip`, starting at its frame `from`.
fn open(
    clip: &ScheduledClip,
    head: bool,
    from: u64,
    serial: u64,
    rate: u32,
    onsets: &mut HashMap<u64, Arc<Vec<u64>>>,
) -> Result<(Job, Box<Lane>), String> {
    let map = clip.warp.clone().ok_or("not warped")?;
    let spec = StreamSpec {
        path: clip.source.path.clone(),
        rate,
        offset: clip.source.offset,
        length: clip.source.length,
        start: 0,
        chunks: 8,
    };
    let (stream, worker) = open_stream(spec).map_err(|e| e.to_string())?;
    if stream.channels() > MAX_CHANNELS {
        return Err(format!("more than {MAX_CHANNELS} channels"));
    }
    let transients = if clip.mode == noodle_core::WarpMode::Beats {
        let key = clip.stream_key().1;
        match onsets.get(&key) {
            Some(found) => found.clone(),
            None => {
                let found = Arc::new(analyse(clip, rate).unwrap_or_default());
                onsets.insert(key, found.clone());
                found
            }
        }
    } else {
        Arc::new(Vec::new())
    };
    let source = StreamSource::new(stream, worker);
    let mut stretcher = Stretcher::new(source, map, &transients, clip.length, rate as f32);
    let from = from.min(clip.length);
    stretcher.seek(from);
    let (ring, consumer) = RingBuffer::new(RING_FRAMES * 2);
    let control = Arc::new(Control {
        want_gen: AtomicU64::new(0),
        want_pos: AtomicU64::new(0),
        ack_gen: AtomicU64::new(0),
        ack_frames: AtomicU64::new(0),
        promoted: AtomicBool::new(false),
    });
    let job = Job {
        serial,
        id: clip.id,
        key: clip.lane_key().1,
        head,
        ring,
        control: control.clone(),
        stretcher,
        length: clip.length,
        pos: from,
        request: 0,
        pushed: 0,
        buffer: vec![0.0; CHUNK_FRAMES * 2],
    };
    let lane = Box::new(Lane {
        id: clip.id,
        key: job.key,
        serial,
        head,
        ring: consumer,
        control,
        read_pos: from,
        behind: 0,
        taken: 0,
        request: 0,
        stale: None,
        synced: true,
    });
    Ok((job, lane))
}

fn run(
    shared: &Shared,
    rate: u32,
    mut to_node: Producer<Box<Lane>>,
    mut from_node: Consumer<Box<Lane>>,
) {
    let mut jobs: Vec<Job> = Vec::new();
    let mut next_serial = 0u64;
    let mut failed: HashMap<(ClipId, u64), Instant> = HashMap::new();
    let mut onsets: HashMap<u64, Arc<Vec<u64>>> = HashMap::new();
    // Lanes handed to the node and not yet taken back.
    let mut outstanding = 0usize;
    let lookahead = (f64::from(rate) * LOOKAHEAD) as u64;
    while !to_node.is_abandoned() {
        let mut busy = false;
        while let Ok(lane) = from_node.pop() {
            busy = true;
            outstanding = outstanding.saturating_sub(1);
            jobs.retain(|j| j.serial != lane.serial);
            // Dropped here, not on the audio thread.
            drop(lane);
        }
        let (schedule, preview) = (shared.schedule(), shared.preview.load(Ordering::Relaxed));
        let position = shared.position.load(Ordering::Relaxed);
        let looping = {
            let (start, end) = (
                shared.loop_start.load(Ordering::Relaxed),
                shared.loop_end.load(Ordering::Relaxed),
            );
            (start < end).then_some((start, end))
        };
        if !preview {
            for (clip, head) in wanted(&schedule, position, lookahead, looping) {
                if !clip.stretched() {
                    continue;
                }
                let key = clip.lane_key();
                if jobs.iter().any(|j| (j.id, j.key) == key && j.head == head) {
                    continue;
                }
                if failed.get(&key).is_some_and(|at| at.elapsed() < RETRY)
                    || outstanding >= MAX_LANES
                    || to_node.slots() == 0
                {
                    continue;
                }
                let from = match looping {
                    Some((start, _)) if head => start,
                    _ => position,
                }
                .saturating_sub(clip.start);
                match open(clip, head, from, next_serial, rate, &mut onsets) {
                    Ok((job, lane)) => {
                        next_serial += 1;
                        if to_node.push(lane).is_ok() {
                            jobs.push(job);
                            outstanding += 1;
                            failed.remove(&key);
                            busy = true;
                        }
                    }
                    Err(message) => {
                        failed.insert(key, Instant::now());
                        shared.fail(
                            clip.stream_key(),
                            super::schedule::FileError {
                                path: clip.source.path.clone(),
                                message,
                            },
                        );
                    }
                }
            }
        }
        for job in &mut jobs {
            if job.head && job.control.promoted.load(Ordering::Relaxed) {
                job.head = false;
            }
            busy |= job.work();
        }
        thread::sleep(if busy { POLL } else { IDLE_POLL });
    }
}
