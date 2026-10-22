//! The off-thread half of the Sampler: what it takes to play a warp that
//! keeps the pitch (Grain and Beats), and to take new settings without being
//! rebuilt.
//!
//! A clip's Grain and Beats are rendered by a worker ahead of the playhead.
//! A Sampler has no playhead to follow: its notes can come at any moment. So
//! it asks for the whole sample at once: [`render`] runs the clip's own
//! [`Stretcher`] over it, to a copy of the sample stretched as the warp says,
//! and the voices play that copy at the pitch of their note, with the same
//! loop and pitch logic as the original. The [`WarpMap`] that did it stays
//! with the copy, to turn a position in it back into a place in the sample.
//!
//! One [`Service`] thread per Sampler node type does the rendering for all its
//! instances, shares a copy between instances that want the same one, and
//! carries the live settings to them. The audio thread talks to it through a
//! pair of queues per instance, as the track inputs do with their hubs, and
//! frees nothing: whatever it is done with goes back to be dropped here.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use noodle_core::{ClipWarp, NodeId, TempoMap, Tick, TimeSignature, WarpAxis};
use noodle_engine::TempoTable;
use noodle_io::{detect_bass_onsets, detect_onsets};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::clip::{StretchSource, Stretcher, WarpMap};
use crate::sample::Sample;
use crate::sampler::Tune;

/// What the audio thread and the service pass to each other.
pub(crate) enum Msg {
    /// New live settings, to the node; one it is done with, back.
    Tune(Arc<Tune>),
    /// A freshly stretched copy, to the node; one it is done with, back.
    Stretched(Arc<Stretched>),
}

/// A copy of a sample stretched by its warp, and how.
pub(crate) struct Stretched {
    /// The warp's identity (see [`warp_key`]) and the tempo it was made at
    /// (see [`tempo_key`]).
    pub key: u64,
    pub tempo: u64,
    /// The stretched audio, at the sample's own rate.
    pub sample: Sample,
    /// Output frame to source frame, both at the sample's rate.
    pub map: WarpMap,
}

/// A number for `warp` that differs whenever the stretch would.
pub(crate) fn warp_key(warp: &ClipWarp) -> u64 {
    let mut hasher = DefaultHasher::new();
    warp.to_text().hash(&mut hasher);
    // Never 0, which is how a Tune says it has nothing to stretch.
    hasher.finish() | 1
}

/// The tempo a stretch is made at, in hundredths of a beat a minute, which is
/// as exact as it is worth following. A warp that doesn't follow the timeline's
/// tempo has the same stretch at any tempo, and a key of 0.
pub(crate) fn tempo_key(warp: &ClipWarp, bpm: f64) -> u64 {
    if warp.curve().axis == WarpAxis::Beats {
        (bpm.max(1.0) * 100.0).round() as u64
    } else {
        0
    }
}

/// Stretches `sample` as `warp` says at `bpm`, as the clip engine would.
pub(crate) fn render(sample: &Sample, warp: &ClipWarp, bpm: f64) -> Option<Stretched> {
    let frames = sample.frames();
    if frames == 0 {
        return None;
    }
    let rate = sample.sample_rate.max(1);
    let seconds = frames as f64 / f64::from(rate);
    let tempo = TempoMap::constant(bpm.max(1.0), TimeSignature::COMMON).ok()?;
    let table = TempoTable::new(&tempo, rate as f32);
    let map = WarpMap::build(
        &warp.curve(),
        &table,
        Tick(0),
        f64::from(rate),
        0.0,
        seconds,
        frames as u64,
    );
    let total = map.out_len().round().max(1.0) as u64;

    let mono: Vec<f32> = sample
        .left
        .iter()
        .zip(&sample.right)
        .map(|(l, r)| (l + r) * 0.5)
        .collect();
    // As the engine finds them: the bass transients for both modes, those of
    // the whole mix for Beats only.
    let bass = detect_bass_onsets(&mono, rate);
    let all = if warp.mode == noodle_core::WarpMode::Beats {
        detect_onsets(&mono, rate)
    } else {
        Vec::new()
    };
    let source = Memory {
        left: &sample.left,
        right: &sample.right,
    };
    let mut stretcher = Stretcher::new(
        source,
        Arc::new(map.clone()),
        (&all, &bass),
        total,
        rate as f32,
    );
    const CHUNK: usize = 4096;
    let mut buffer = vec![0.0f32; CHUNK * 2];
    let mut left = Vec::with_capacity(total as usize);
    let mut right = Vec::with_capacity(total as usize);
    loop {
        let n = stretcher.read(&mut buffer);
        for frame in buffer[..n * 2].as_chunks::<2>().0 {
            left.push(frame[0]);
            right.push(frame[1]);
        }
        if n < CHUNK {
            break;
        }
    }
    Some(Stretched {
        key: warp_key(warp),
        tempo: tempo_key(warp, bpm),
        sample: Sample {
            left: left.into(),
            right: right.into(),
            sample_rate: sample.sample_rate,
        },
        map,
    })
}

/// A sample, as the stretcher reads its source.
struct Memory<'a> {
    left: &'a [f32],
    right: &'a [f32],
}

impl StretchSource for Memory<'_> {
    fn channels(&self) -> usize {
        2
    }

    fn read(&mut self, start: i64, out: &mut [f32]) {
        for (i, frame) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let at = usize::try_from(start + i as i64)
                .ok()
                .filter(|&at| at < self.left.len());
            frame[0] = at.map_or(0.0, |at| self.left[at]);
            frame[1] = at.map_or(0.0, |at| self.right[at]);
        }
    }
}

/// How long a stretch's inputs must stay as they are before it is rendered,
/// so that dragging a warp marker (or the project's tempo) doesn't render at
/// every step. Until then the Sampler plays the warp as Repitch.
const SETTLE: Duration = Duration::from_millis(150);
const POLL: Duration = Duration::from_millis(5);
const IDLE_POLL: Duration = Duration::from_millis(20);
/// Messages each way per instance.
const QUEUE: usize = 16;
/// Copies kept for other instances to share.
const KEPT: usize = 4;

/// The node's ends of its queues, and what it reports.
pub(crate) struct NodeLinks {
    pub inbox: Consumer<Msg>,
    outbox: Producer<Msg>,
    /// Retired messages the full outbox would not take yet.
    limbo: Vec<Msg>,
    /// The tempo as of the node's last block, as `f64` bits.
    pub bpm: Arc<AtomicU64>,
}

impl NodeLinks {
    /// Hands `message` back to the service to drop. If the queue is full it
    /// waits in a list made room for in advance, and is offered again by
    /// [`flush`](Self::flush); if that is full too it is leaked, since
    /// freeing it here would be on the audio thread.
    pub fn retire(&mut self, message: Msg) {
        if let Err(rtrb::PushError::Full(message)) = self.outbox.push(message) {
            if self.limbo.len() < self.limbo.capacity() {
                self.limbo.push(message);
            } else {
                std::mem::forget(message);
            }
        }
    }

    /// Offers the retired messages that had to wait to the service again.
    pub fn flush(&mut self) {
        while self.limbo.last().is_some() && self.outbox.slots() > 0 {
            if let Some(message) = self.limbo.pop() {
                let _ = self.outbox.push(message);
            }
        }
    }
}

struct Link {
    serial: u64,
    node: NodeId,
    /// Which sample the instance plays, to tell what can be shared.
    source: u64,
    to_node: Producer<Msg>,
    from_node: Consumer<Msg>,
    sample: Option<Arc<Sample>>,
    bpm: Arc<AtomicU64>,
    /// The settings wanted, and whether the node has them yet.
    tune: Arc<Tune>,
    tune_sent: bool,
    /// What the node was last sent, and what is wanted since when.
    sent: Option<(u64, u64)>,
    wanted: Option<((u64, u64), Instant)>,
    /// A copy waiting for room in the queue.
    ready: Option<Arc<Stretched>>,
}

struct Shared {
    links: Mutex<Vec<Link>>,
    cache: Mutex<Vec<(u64, Arc<Stretched>)>>,
}

/// The worker of one Sampler node type. See the module.
pub(crate) struct Service {
    shared: Arc<Shared>,
    started: Mutex<bool>,
    next: AtomicU64,
}

impl Service {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                links: Mutex::new(Vec::new()),
                cache: Mutex::new(Vec::new()),
            }),
            started: Mutex::new(false),
            next: AtomicU64::new(0),
        }
    }

    /// Adds an instance of `node` that plays `sample` (named by `source`) with
    /// `tune`, and starts the thread if this is the first. Call this when
    /// building a plan.
    pub fn register(
        &self,
        node: NodeId,
        source: u64,
        sample: Option<Arc<Sample>>,
        tune: Arc<Tune>,
    ) -> NodeLinks {
        let (to_node, inbox) = RingBuffer::new(QUEUE);
        let (outbox, from_node) = RingBuffer::new(QUEUE * 4);
        let bpm = Arc::new(AtomicU64::new(120.0f64.to_bits()));
        let link = Link {
            serial: self.next.fetch_add(1, Ordering::Relaxed),
            node,
            source,
            to_node,
            from_node,
            sample,
            bpm: bpm.clone(),
            tune,
            tune_sent: true,
            sent: None,
            wanted: None,
            ready: None,
        };
        lock(&self.shared.links).push(link);
        let mut started = lock(&self.started);
        if !*started {
            let shared = self.shared.clone();
            // If the thread can't start, nothing is ever stretched and the
            // Sampler plays warps as Repitch.
            *started = thread::Builder::new()
                .name("noodle-sampler".into())
                .spawn(move || run(&shared))
                .is_ok();
        }
        NodeLinks {
            inbox,
            outbox,
            limbo: Vec::with_capacity(QUEUE * 4),
            bpm,
        }
    }

    /// Gives every instance of `node` new settings.
    pub fn retune(&self, node: NodeId, gain_text: &str, warp_text: &str) {
        let mut read: Option<Arc<Tune>> = None;
        for link in lock(&self.shared.links)
            .iter_mut()
            .filter(|l| l.node == node)
        {
            if !link.tune.is(gain_text, warp_text) {
                let tune = read.get_or_insert_with(|| Arc::new(Tune::read(gain_text, warp_text)));
                link.tune = tune.clone();
                link.tune_sent = false;
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A stretch to make.
struct Job {
    serial: u64,
    cache_key: u64,
    sample: Arc<Sample>,
    warp: ClipWarp,
    bpm: f64,
    key: (u64, u64),
}

fn run(shared: &Arc<Shared>) {
    // Stops when the node type, the only other holder, is gone.
    while Arc::strong_count(shared) > 1 {
        let mut busy = false;
        let job = {
            let mut links = lock(&shared.links);
            links.retain(|l| !l.to_node.is_abandoned());
            let mut job = None;
            for link in links.iter_mut() {
                busy |= service(link);
                if job.is_none() {
                    job = wants(link, shared);
                }
            }
            job
        };
        if let Some(job) = job {
            busy = true;
            let made = render(&job.sample, &job.warp, job.bpm).map(Arc::new);
            if let Some(made) = &made {
                let mut cache = lock(&shared.cache);
                cache.retain(|(key, _)| *key != job.cache_key);
                cache.push((job.cache_key, made.clone()));
                if cache.len() > KEPT {
                    cache.remove(0);
                }
            }
            if let Some(link) = lock(&shared.links)
                .iter_mut()
                .find(|l| l.serial == job.serial)
            {
                // A render that fails is not asked for again until the
                // settings change.
                link.sent = Some(job.key);
                link.ready = made;
            }
        }
        thread::sleep(if busy { POLL } else { IDLE_POLL });
    }
}

/// Drops what the node is done with and sends what it is owed. Returns
/// whether it did anything.
fn service(link: &mut Link) -> bool {
    let mut busy = false;
    while link.from_node.pop().is_ok() {
        busy = true;
    }
    if !link.tune_sent
        && link.to_node.slots() > 0
        && link.to_node.push(Msg::Tune(link.tune.clone())).is_ok()
    {
        link.tune_sent = true;
        busy = true;
    }
    if let Some(ready) = link.ready.take() {
        match link.to_node.push(Msg::Stretched(ready)) {
            Ok(()) => busy = true,
            Err(rtrb::PushError::Full(Msg::Stretched(back))) => link.ready = Some(back),
            Err(_) => {}
        }
    }
    busy
}

/// The stretch `link` needs next, if it needs one: from the cache, which
/// queues it for the node, or else a job to render.
fn wants(link: &mut Link, shared: &Shared) -> Option<Job> {
    let (Some(sample), Some(warp)) = (link.sample.clone(), link.tune.stretch_warp()) else {
        link.wanted = None;
        return None;
    };
    let bpm = f64::from_bits(link.bpm.load(Ordering::Relaxed));
    let key = (warp_key(warp), tempo_key(warp, bpm));
    if link.sent == Some(key) || link.ready.is_some() {
        link.wanted = None;
        return None;
    }
    let cache_key = {
        let mut hasher = DefaultHasher::new();
        (link.source, key).hash(&mut hasher);
        hasher.finish()
    };
    let cached = lock(&shared.cache)
        .iter()
        .find(|(k, _)| *k == cache_key)
        .map(|(_, s)| s.clone());
    if let Some(cached) = cached {
        link.ready = Some(cached);
        link.sent = Some(key);
        link.wanted = None;
        return None;
    }
    match link.wanted {
        Some((wanted, since)) if wanted == key => {
            if since.elapsed() < SETTLE {
                return None;
            }
        }
        _ => {
            link.wanted = Some((key, Instant::now()));
            return None;
        }
    }
    link.wanted = None;
    Some(Job {
        serial: link.serial,
        cache_key,
        sample,
        warp: warp.clone(),
        bpm,
        key,
    })
}
