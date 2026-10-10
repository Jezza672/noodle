//! The off-thread half of a track input: it hands the node its schedule and
//! opens the audio streams it will need, so the audio thread never opens a
//! file, allocates or frees anything.
//!
//! One hub thread runs per track input node instance. It talks to the node
//! through two queues: schedules and ready streams go to the node, and the
//! ones the node is done with come back to be dropped here. It stops when the
//! node is dropped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use noodle_core::ClipId;
use noodle_io::{ClipStream, StreamSpec, StreamWorker, open_stream};
use rtrb::{Consumer, Producer, RingBuffer};

use super::schedule::{ClipStatus, FileError, Notes, Schedule, ScheduledClip};

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
/// How often the hub looks while the playhead moves, and while it doesn't.
const POLL: Duration = Duration::from_millis(4);
const IDLE_POLL: Duration = Duration::from_millis(25);
/// How long before a file that failed to open is tried again, so one that
/// turns up later (restored, or a drive mounted) starts playing.
const RETRY: Duration = Duration::from_secs(2);
/// Things sent to the node and not yet returned. The return queue is bigger,
/// so the node can always hand a retired one back.
const MAX_OUTSTANDING: usize = 24;
const QUEUE: usize = 32;
const STREAM_CHUNKS: usize = 8;

/// What the UI side, the hub and the node share.
pub(super) struct Shared {
    latest: Mutex<(u64, Arc<Schedule>, Arc<Notes>)>,
    /// Offline rendering: the node waits for the hub and the disk instead of
    /// playing silence, so the output doesn't depend on timing.
    pub blocking: bool,
    /// Clips whose file couldn't be opened and why, for the offline node to
    /// skip instead of waiting on, and for the UI to name.
    failed: Mutex<Vec<((ClipId, u64), FileError)>>,
    /// The node's playhead at its last block, in samples.
    pub position: AtomicU64,
    /// The loop's start and end in samples as of the node's last block. An
    /// end of zero means no loop.
    pub loop_start: AtomicU64,
    pub loop_end: AtomicU64,
    pub bound: AtomicUsize,
    pub underruns: AtomicU64,
    /// The keys held down from the piano roll's keyboard, one bit each.
    pub audition: [AtomicU64; 2],
}

impl Shared {
    pub fn new(blocking: bool) -> Self {
        Self {
            blocking,
            latest: Mutex::new((0, Arc::new(Vec::new()), Arc::new(Vec::new()))),
            failed: Mutex::new(Vec::new()),
            position: AtomicU64::new(0),
            loop_start: AtomicU64::new(0),
            loop_end: AtomicU64::new(0),
            bound: AtomicUsize::new(0),
            underruns: AtomicU64::new(0),
            audition: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }

    pub fn set_audition(&self, key: u8, on: bool) {
        let key = key.min(127);
        let bit = 1u64 << (key % 64);
        let word = &self.audition[usize::from(key / 64)];
        if on {
            word.fetch_or(bit, Ordering::Relaxed);
        } else {
            word.fetch_and(!bit, Ordering::Relaxed);
        }
    }

    /// The version of the newest schedule set.
    pub fn version(&self) -> u64 {
        self.latest.lock().expect("schedule lock").0
    }

    pub fn set(&self, schedule: Schedule, notes: Notes) {
        let mut latest = self.latest.lock().expect("schedule lock");
        if *latest.1 != schedule || *latest.2 != notes {
            *latest = (latest.0 + 1, Arc::new(schedule), Arc::new(notes));
        }
    }

    pub fn has_failed(&self, key: (ClipId, u64)) -> bool {
        self.failed
            .lock()
            .expect("failed lock")
            .iter()
            .any(|(k, _)| *k == key)
    }

    pub fn is_empty(&self) -> bool {
        {
            let latest = self.latest.lock().expect("schedule lock");
            latest.1.is_empty() && latest.2.is_empty()
        }
    }

    /// The files that couldn't be opened, one entry each.
    pub fn errors(&self) -> Vec<FileError> {
        let mut errors: Vec<FileError> = Vec::new();
        for (_, error) in self.failed.lock().expect("failed lock").iter() {
            if !errors.contains(error) {
                errors.push(error.clone());
            }
        }
        errors
    }

    /// Notes that a clip's file failed; the newest message for a clip wins.
    fn fail(&self, key: (ClipId, u64), error: FileError) {
        let mut failed = self.failed.lock().expect("failed lock");
        match failed.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = error,
            None => failed.push((key, error)),
        }
    }

    pub fn status(&self) -> ClipStatus {
        ClipStatus {
            streams: self.bound.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            errors: self.errors(),
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
    /// A schedule, the MIDI notes to play with it, and their version.
    Schedule(u64, Box<Schedule>, Box<Notes>),
    Stream(Box<Prepared>),
}

pub(super) enum Retired {
    // Only kept to be dropped on the hub's thread.
    Schedule(#[allow(dead_code)] Box<Schedule>),
    /// The notes that came with a schedule. Sent back separately and not
    /// counted, so the return queue has room for two per schedule.
    Notes(#[allow(dead_code)] Box<Notes>),
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
    let (to_hub, from_node) = RingBuffer::new(QUEUE * 2);
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
    // Streams that failed to open, and when.
    let mut failed: HashMap<(ClipId, u64), Instant> = HashMap::new();
    let mut last_position = None;
    let lookahead = (f64::from(rate) * LOOKAHEAD) as u64;
    while !to_node.is_abandoned() {
        let mut busy = false;
        while let Ok(item) = from_node.pop() {
            busy = true;
            match &item {
                Retired::Promoted(serial) => {
                    if let Some(l) = live.iter_mut().find(|l| l.serial == *serial) {
                        l.head = false;
                    }
                    continue;
                }
                Retired::Stream(prepared) => live.retain(|l| l.serial != prepared.serial),
                Retired::Notes(_) => continue,
                Retired::Schedule(_) => {}
            }
            outstanding -= 1;
            drop(item);
        }
        let (version, schedule, notes) = {
            let latest = shared.latest.lock().expect("schedule lock");
            (latest.0, latest.1.clone(), latest.2.clone())
        };
        if sent != Some(version) && outstanding < MAX_OUTSTANDING {
            let message = ToNode::Schedule(
                version,
                Box::new(schedule.to_vec()),
                Box::new(notes.to_vec()),
            );
            if to_node.push(message).is_ok() {
                sent = Some(version);
                outstanding += 1;
                busy = true;
                failed.clear();
                // Files still in the schedule keep their error until they open,
                // so a status display doesn't flicker while the schedule is
                // being dragged about.
                shared
                    .failed
                    .lock()
                    .expect("failed lock")
                    .retain(|(k, _)| schedule.iter().any(|c| c.stream_key() == *k));
            }
        }
        if sent == Some(version) {
            let position = shared.position.load(Ordering::Relaxed);
            busy |= last_position != Some(position);
            last_position = Some(position);
            let looping = {
                let (start, end) = (
                    shared.loop_start.load(Ordering::Relaxed),
                    shared.loop_end.load(Ordering::Relaxed),
                );
                (start < end).then_some((start, end))
            };
            for (clip, head) in wanted(&schedule, position, lookahead, looping) {
                let key = clip.stream_key();
                if live.iter().any(|l| l.key == key && l.head == head) {
                    continue;
                }
                if failed.get(&key).is_some_and(|at| at.elapsed() < RETRY) {
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
                            busy = true;
                            if failed.remove(&key).is_some() {
                                shared
                                    .failed
                                    .lock()
                                    .expect("failed lock")
                                    .retain(|(k, _)| *k != key);
                            }
                            next_serial += 1;
                            live.push(Live { serial, key, head });
                            outstanding += 1;
                        }
                    }
                    Ok(_) => {
                        failed.insert(key, Instant::now());
                        shared.fail(
                            key,
                            FileError {
                                path: clip.source.path.clone(),
                                message: format!("more than {MAX_CHANNELS} channels"),
                            },
                        );
                    }
                    Err(e) => {
                        failed.insert(key, Instant::now());
                        shared.fail(
                            key,
                            FileError {
                                path: clip.source.path.clone(),
                                message: e.to_string(),
                            },
                        );
                    }
                }
            }
        }
        thread::sleep(if busy { POLL } else { IDLE_POLL });
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
