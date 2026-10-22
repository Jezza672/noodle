//! The worker that stretches warped clips ahead of the playhead.
//!
//! Repitch is a resampling the audio thread does itself. Grain and Beats are
//! too heavy for that, so a worker task per track input node renders them
//! into a ring buffer, and the audio thread only copies out of it. The worker
//! sleeps until it is rung: by the node when it has taken enough out of a ring
//! to make room, or the playhead has moved on, and by the UI side when the
//! schedule changes. This is the same arrangement the hub has for decoding: the worker opens what the
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Instant;

use noodle_core::ClipId;
use noodle_io::pool::{self, Bell, Handle, Next, Task};
use noodle_io::{
    ClipStream, StreamSpec, StreamWorker, detect_bass_onsets, detect_onsets, open_stream,
};
use rtrb::{Consumer, Producer, RingBuffer};

use super::hub::{LOOKAHEAD, MAX_CHANNELS, RETRY, Shared, wanted};
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
/// Source frames kept behind the stretcher's reading, for the grains that
/// reach back.
const HISTORY: i64 = 16_384;
/// How far ahead of the end of what is held the stream is read on instead of
/// being seeked.
const MAX_SKIP: i64 = 32_768;
const QUEUE: usize = 16;
/// Chunks a lane renders in one run, so a refill doesn't hold a pool thread
/// for long.
const CHUNKS_PER_RUN: usize = 4;

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
    /// Rings the worker, when there is room in the ring for it or it must
    /// seek.
    bell: Bell,
    /// Frames taken out of the ring since the worker was last rung.
    unrung: usize,
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
            self.bell.ring();
        }
    }

    /// Notes that `frames` left the ring, and rings the worker once there is
    /// room for it to render a chunk.
    fn took(&mut self, frames: usize) {
        self.unrung += frames;
        if self.unrung >= CHUNK_FRAMES {
            self.unrung = 0;
            self.bell.ring();
        }
    }

    /// Asks the worker to restart the stretch at section frame `pos`.
    pub fn seek(&mut self, pos: u64) {
        self.request += 1;
        self.control.want_pos.store(pos, Ordering::Relaxed);
        self.control.want_gen.store(self.request, Ordering::Release);
        self.bell.ring();
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
            self.took(n);
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
            self.took(n);
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
        self.took(n);
        n
    }
}

/// The node's ends of the queues.
pub(super) struct AheadLinks {
    pub lanes: Consumer<Box<Lane>>,
    pub returned: Producer<Box<Lane>>,
    pub bell: Bell,
    /// Stops the worker when the node is dropped.
    pub _handle: Handle,
}

pub(super) fn spawn(shared: Arc<Shared>, rate: u32) -> AheadLinks {
    let (to_node, lanes) = RingBuffer::new(QUEUE);
    let (returned, from_node) = RingBuffer::new(QUEUE * 2);
    let handle = pool::control().spawn_with(|bell| Ahead {
        shared: shared.clone(),
        bell,
        rate,
        to_node,
        from_node,
        jobs: Vec::new(),
        next_serial: 0,
        failed: HashMap::new(),
        onsets: HashMap::new(),
        outstanding: 0,
        lookahead: (f64::from(rate) * LOOKAHEAD) as u64,
    });
    let bell = handle.bell();
    shared.add_bell(bell.clone());
    AheadLinks {
        lanes,
        returned,
        bell,
        _handle: handle,
    }
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
        let mut budget = CHUNKS_PER_RUN;
        while budget > 0 && self.pos < self.length && self.ring.slots() >= CHUNK_FRAMES * 2 {
            budget -= 1;
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
fn analyse(clip: &ScheduledClip, rate: u32) -> Option<Transients> {
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
    Some(Transients {
        all: detect_onsets(&mono, rate),
        bass: detect_bass_onsets(&mono, rate),
    })
}

/// Where a clip's transients are: in the whole mix (Beats) and in its bass
/// (the bass band of both modes, and Grain's cuts).
#[derive(Default)]
struct Transients {
    all: Vec<u64>,
    bass: Vec<u64>,
}

/// A clip's transients: still being found on a thread of their own, or known.
/// Reading a whole file takes long enough to starve every other lane if the
/// render loop did it.
enum Onsets {
    Finding(mpsc::Receiver<Option<Transients>>),
    Known(Arc<Transients>),
}

/// The transients of `clip` if they are known, else starts finding them (once)
/// and says `None` until then.
fn onsets_of(
    clip: &ScheduledClip,
    rate: u32,
    known: &mut HashMap<u64, Onsets>,
    shared: &Arc<Shared>,
) -> Option<Arc<Transients>> {
    let key = clip.stream_key().1;
    let entry = known.entry(key).or_insert_with(|| {
        let (tx, rx) = mpsc::channel();
        let clip = clip.clone();
        let shared = shared.clone();
        thread::Builder::new()
            .name("noodle-onsets".into())
            .spawn(move || {
                let _ = tx.send(analyse(&clip, rate));
                // The worker is asleep until something rings it.
                shared.ring();
            })
            .map_or_else(
                |_| {
                    let (tx, rx) = mpsc::channel();
                    let _ = tx.send(None);
                    Onsets::Finding(rx)
                },
                |_| Onsets::Finding(rx),
            )
    });
    if let Onsets::Finding(rx) = entry {
        match rx.try_recv() {
            Ok(found) => *entry = Onsets::Known(Arc::new(found.unwrap_or_default())),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => *entry = Onsets::Known(Arc::default()),
        }
    }
    match entry {
        Onsets::Known(found) => Some(found.clone()),
        Onsets::Finding(_) => None,
    }
}

#[allow(clippy::too_many_arguments)]
/// Opens a job for `clip`, starting at its frame `from`.
fn open(
    clip: &ScheduledClip,
    head: bool,
    from: u64,
    serial: u64,
    rate: u32,
    onsets: &mut HashMap<u64, Onsets>,
    shared: &Arc<Shared>,
    bell: &Bell,
) -> Result<Option<(Job, Box<Lane>)>, String> {
    let map = clip.warp.clone().ok_or("not warped")?;
    let spec = StreamSpec {
        path: clip.source.path.clone(),
        rate,
        offset: clip.source.offset,
        length: clip.source.length,
        start: 0,
        chunks: 8,
    };
    // Not found yet: try again on the next pass.
    let Some(transients) = onsets_of(clip, rate, onsets, shared) else {
        return Ok(None);
    };
    // Beats cuts the mix at every transient; Grain only where the bass has
    // one, so that a kick's body and its click stay together.
    let cuts = if clip.mode == noodle_core::WarpMode::Beats {
        &transients.all
    } else {
        &transients.bass
    };
    let (stream, worker) = open_stream(spec).map_err(|e| e.to_string())?;
    if stream.channels() > MAX_CHANNELS {
        return Err(format!("more than {MAX_CHANNELS} channels"));
    }
    let source = StreamSource::new(stream, worker);
    let mut stretcher = Stretcher::new(
        source,
        map,
        (cuts, &transients.bass),
        clip.length,
        rate as f32,
    );
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
        bell: bell.clone(),
        unrung: 0,
        read_pos: from,
        behind: 0,
        taken: 0,
        request: 0,
        stale: None,
        synced: true,
    });
    Ok(Some((job, lane)))
}

struct Ahead {
    shared: Arc<Shared>,
    bell: Bell,
    rate: u32,
    to_node: Producer<Box<Lane>>,
    from_node: Consumer<Box<Lane>>,
    jobs: Vec<Job>,
    next_serial: u64,
    failed: HashMap<(ClipId, u64), Instant>,
    onsets: HashMap<u64, Onsets>,
    /// Lanes handed to the node and not yet taken back.
    outstanding: usize,
    lookahead: u64,
}

impl Task for Ahead {
    fn run(&mut self) -> Next {
        if self.to_node.is_abandoned() {
            return Next::Done;
        }
        if self.pass() {
            Next::Again
        } else if self.failed.is_empty() {
            Next::Idle
        } else {
            // A file that failed may turn up later.
            Next::After(RETRY)
        }
    }
}

impl Ahead {
    /// Looks at everything once and renders some. Returns whether it did
    /// something that may make more to do.
    fn pass(&mut self) -> bool {
        let Self {
            shared,
            bell,
            rate,
            to_node,
            from_node,
            jobs,
            next_serial,
            failed,
            onsets,
            outstanding,
            lookahead,
        } = self;
        let (rate, lookahead) = (*rate, *lookahead);
        let mut busy = false;
        while let Ok(lane) = from_node.pop() {
            busy = true;
            *outstanding = outstanding.saturating_sub(1);
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
                    || *outstanding >= MAX_LANES
                    || to_node.slots() == 0
                {
                    continue;
                }
                let from = match looping {
                    Some((start, _)) if head => start,
                    _ => position,
                }
                .saturating_sub(clip.start);
                match open(clip, head, from, *next_serial, rate, onsets, shared, bell) {
                    // Still finding its transients: rung when they are known.
                    Ok(None) => {}
                    Ok(Some((job, lane))) => {
                        *next_serial += 1;
                        if to_node.push(lane).is_ok() {
                            jobs.push(job);
                            *outstanding += 1;
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
        for job in jobs.iter_mut() {
            if job.head && job.control.promoted.load(Ordering::Relaxed) {
                job.head = false;
            }
            busy |= job.work();
        }
        busy
    }
}
