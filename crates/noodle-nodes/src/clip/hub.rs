//! The off-thread half of a track input: it hands the node its schedule and
//! opens the audio streams it will need, so the audio thread never opens a
//! file, allocates or frees anything.
//!
//! One hub thread runs per track input node instance. It talks to the node
//! through two queues: schedules and ready streams go to the node, and the
//! ones the node is done with come back to be dropped here. It stops when the
//! node is dropped.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use noodle_core::ClipId;
use noodle_io::{ClipStream, StreamSpec, StreamWorker, open_stream};
use rtrb::{Consumer, Producer, RingBuffer};

use super::schedule::{ClipStatus, Schedule};

/// Streams a node can hold at once: the clip playing and the ones about to.
pub(super) const MAX_STREAMS: usize = 4;
/// The most channels a file can have and still play.
pub(super) const MAX_CHANNELS: usize = 8;
/// How far ahead of the playhead streams are opened, in seconds.
const LOOKAHEAD: f64 = 1.0;
const POLL: Duration = Duration::from_millis(4);
/// Things sent to the node and not yet returned. The return queue is bigger,
/// so the node can always hand a retired one back.
const MAX_OUTSTANDING: usize = 24;
const QUEUE: usize = 32;
const STREAM_CHUNKS: usize = 8;

/// What the UI side, the hub and the node share.
pub(super) struct Shared {
    latest: Mutex<(u64, Arc<Schedule>)>,
    error: Mutex<Option<String>>,
    /// The node's playhead at its last block, in samples.
    pub position: AtomicU64,
    pub bound: AtomicUsize,
    pub underruns: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            latest: Mutex::new((0, Arc::new(Vec::new()))),
            error: Mutex::new(None),
            position: AtomicU64::new(0),
            bound: AtomicUsize::new(0),
            underruns: AtomicU64::new(0),
        }
    }
}

impl Shared {
    pub fn set(&self, schedule: Schedule) {
        let mut latest = self.latest.lock().expect("schedule lock");
        if *latest.1 != schedule {
            *latest = (latest.0 + 1, Arc::new(schedule));
        }
    }

    pub fn status(&self) -> ClipStatus {
        ClipStatus {
            streams: self.bound.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            error: self.error.lock().expect("error lock").clone(),
        }
    }
}

/// A stream opened for a clip, with the worker that feeds it.
pub(super) struct Prepared {
    pub id: ClipId,
    pub key: u64,
    pub stream: ClipStream,
    // Never read: dropping it stops and joins the thread, which must happen
    // here and not on the audio thread.
    _worker: StreamWorker,
}

pub(super) enum ToNode {
    Schedule(Box<Schedule>),
    Stream(Box<Prepared>),
}

pub(super) enum Retired {
    // Only kept to be dropped on the hub's thread.
    Schedule(#[allow(dead_code)] Box<Schedule>),
    Stream(Box<Prepared>),
}

/// The node's ends of the queues.
pub(super) struct Links {
    pub from_hub: Consumer<ToNode>,
    pub to_hub: Producer<Retired>,
}

pub(super) fn spawn(shared: Arc<Shared>, rate: u32) -> Links {
    let (to_node, from_hub) = RingBuffer::new(QUEUE);
    let (to_hub, from_node) = RingBuffer::new(QUEUE);
    thread::Builder::new()
        .name("noodle-clips".into())
        .spawn(move || run(&shared, rate, to_node, from_node))
        .expect("can't start a thread");
    Links { from_hub, to_hub }
}

fn run(
    shared: &Shared,
    rate: u32,
    mut to_node: Producer<ToNode>,
    mut from_node: Consumer<Retired>,
) {
    let mut sent: Option<u64> = None;
    let mut outstanding = 0usize;
    let mut live: Vec<(ClipId, u64)> = Vec::new();
    // Streams that failed to open for this version of the schedule.
    let mut failed: HashSet<(ClipId, u64)> = HashSet::new();
    let lookahead = (f64::from(rate) * LOOKAHEAD) as u64;
    while !to_node.is_abandoned() {
        while let Ok(item) = from_node.pop() {
            outstanding -= 1;
            if let Retired::Stream(prepared) = &item {
                live.retain(|key| *key != (prepared.id, prepared.key));
            }
            drop(item);
        }
        let (version, schedule) = {
            let latest = shared.latest.lock().expect("schedule lock");
            (latest.0, latest.1.clone())
        };
        if sent != Some(version) && outstanding < MAX_OUTSTANDING {
            let message = ToNode::Schedule(Box::new(schedule.to_vec()));
            if to_node.push(message).is_ok() {
                sent = Some(version);
                outstanding += 1;
                failed.clear();
                *shared.error.lock().expect("error lock") = None;
            }
        }
        if sent == Some(version) {
            let position = shared.position.load(Ordering::Relaxed);
            for clip in schedule.iter() {
                if clip.end() <= position || clip.start >= position.saturating_add(lookahead) {
                    continue;
                }
                let key = clip.stream_key();
                if live.contains(&key) || failed.contains(&key) {
                    continue;
                }
                if live.len() >= MAX_STREAMS || outstanding >= MAX_OUTSTANDING {
                    break;
                }
                let spec = StreamSpec {
                    path: clip.source.path.clone(),
                    rate,
                    offset: clip.source.offset,
                    length: clip.source.length,
                    start: position.saturating_sub(clip.start),
                    chunks: STREAM_CHUNKS,
                };
                match open_stream(spec) {
                    Ok((stream, worker)) if stream.channels() <= MAX_CHANNELS => {
                        let prepared = Prepared {
                            id: clip.id,
                            key: key.1,
                            stream,
                            _worker: worker,
                        };
                        if to_node.push(ToNode::Stream(Box::new(prepared))).is_ok() {
                            live.push(key);
                            outstanding += 1;
                        }
                    }
                    Ok(_) => {
                        failed.insert(key);
                        *shared.error.lock().expect("error lock") = Some(format!(
                            "{}: more than {MAX_CHANNELS} channels",
                            clip.source.path.display()
                        ));
                    }
                    Err(e) => {
                        failed.insert(key);
                        *shared.error.lock().expect("error lock") =
                            Some(format!("{}: {e}", clip.source.path.display()));
                    }
                }
            }
        }
        thread::sleep(POLL);
    }
}
