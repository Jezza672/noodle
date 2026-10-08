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

use super::schedule::{ClipStatus, Schedule, ScheduledClip};

/// Streams a node can hold at once: the clip playing and the ones about to.
pub(super) const MAX_STREAMS: usize = 6;
/// Streams opened ahead for the loop's start, on top of those. They have their
/// own room so a busy loop can't keep them from opening, or the other way
/// round.
pub(super) const MAX_HEADS: usize = 4;
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
    /// The loop's start and end in samples as of the node's last block. An
    /// end of zero means no loop.
    pub loop_start: AtomicU64,
    pub loop_end: AtomicU64,
    pub bound: AtomicUsize,
    pub underruns: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            latest: Mutex::new((0, Arc::new(Vec::new()))),
            error: Mutex::new(None),
            position: AtomicU64::new(0),
            loop_start: AtomicU64::new(0),
            loop_end: AtomicU64::new(0),
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
    /// Tells this stream apart from others for the same clip.
    pub serial: u64,
    /// Opened ahead for the loop's start, and not played from yet.
    pub head: bool,
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
    /// The node has started playing from a stream that was opened ahead for
    /// the loop's start (its serial), so it is an ordinary stream now. Not
    /// something to drop, and it needs no room in the queue.
    Promoted(u64),
}

/// What the hub knows about a stream it has handed over.
struct Live {
    serial: u64,
    key: (ClipId, u64),
    head: bool,
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
    let mut next_serial = 0u64;
    let mut live: Vec<Live> = Vec::new();
    // Streams that failed to open for this version of the schedule.
    let mut failed: HashSet<(ClipId, u64)> = HashSet::new();
    let lookahead = (f64::from(rate) * LOOKAHEAD) as u64;
    while !to_node.is_abandoned() {
        while let Ok(item) = from_node.pop() {
            match &item {
                Retired::Promoted(serial) => {
                    if let Some(l) = live.iter_mut().find(|l| l.serial == *serial) {
                        l.head = false;
                    }
                    continue;
                }
                Retired::Stream(prepared) => live.retain(|l| l.serial != prepared.serial),
                Retired::Schedule(_) => {}
            }
            outstanding -= 1;
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
            let looping = {
                let (start, end) = (
                    shared.loop_start.load(Ordering::Relaxed),
                    shared.loop_end.load(Ordering::Relaxed),
                );
                (start < end).then_some((start, end))
            };
            for (clip, head) in wanted(&schedule, position, lookahead, looping) {
                let key = clip.stream_key();
                if live.iter().any(|l| l.key == key && l.head == head) || failed.contains(&key) {
                    continue;
                }
                if outstanding >= MAX_OUTSTANDING {
                    break;
                }
                let cap = if head { MAX_HEADS } else { MAX_STREAMS };
                if live.iter().filter(|l| l.head == head).count() >= cap {
                    continue;
                }
                // A stream opened for the loop's start begins there; any other
                // begins where the playhead is, or at the clip's start.
                let from = match looping {
                    Some((start, _)) if head => start,
                    _ => position,
                };
                let spec = StreamSpec {
                    path: clip.source.path.clone(),
                    rate,
                    offset: clip.source.offset,
                    length: clip.source.length,
                    start: from.saturating_sub(clip.start),
                    chunks: STREAM_CHUNKS,
                };
                match open_stream(spec) {
                    Ok((stream, worker)) if stream.channels() <= MAX_CHANNELS => {
                        let serial = next_serial;
                        let prepared = Prepared {
                            id: clip.id,
                            key: key.1,
                            serial,
                            head,
                            stream,
                            _worker: worker,
                        };
                        if to_node.push(ToNode::Stream(Box::new(prepared))).is_ok() {
                            next_serial += 1;
                            live.push(Live { serial, key, head });
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

/// The clips that need a stream now, soonest first, and whether each is for
/// the loop's start (`true`) or for where the playhead is going (`false`).
///
/// Clips starting within `lookahead` of the playhead are wanted. When the
/// loop's end is within that distance too, so are the clips at the start of
/// the loop, including the one already playing there, each with a stream
/// opened at the loop's start, so the audio is ready before the playhead
/// wraps.
fn wanted(
    schedule: &[ScheduledClip],
    position: u64,
    lookahead: u64,
    looping: Option<(u64, u64)>,
) -> Vec<(&ScheduledClip, bool)> {
    let mut reach = position.saturating_add(lookahead);
    let wrap = looping.filter(|&(_, end)| position < end);
    if let Some((_, end)) = wrap {
        reach = reach.min(end);
    }
    let mut out: Vec<_> = schedule
        .iter()
        .filter(|c| c.end() > position && c.start < reach)
        .map(|c| (c, false))
        .collect();
    if let Some((start, end)) = wrap
        && position.saturating_add(lookahead) > end
    {
        let ahead = start + (position + lookahead - end);
        out.extend(
            schedule
                .iter()
                .filter(|c| c.end() > start && c.start < ahead)
                .map(|c| (c, true)),
        );
    }
    out
}
