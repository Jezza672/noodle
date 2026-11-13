//! A preallocated queue that holds events until a later sample, shared by the
//! event nodes that move notes in time (Swing and Humanise).

use noodle_engine::{Event, EventKind, EventsOut, NoteId};

/// How many events can wait at once; past this a new one is dropped.
const CAPACITY: usize = 2048;
/// How many notes' delays are remembered, so a note-off can follow its
/// note-on. Past this the oldest is forgotten and its off goes undelayed.
const NOTES: usize = 256;

#[derive(Clone, Copy)]
struct Pending {
    due: u64,
    event: Event,
}

#[derive(Clone, Copy, Default)]
struct Remembered {
    note: u32,
    delay: u64,
    live: bool,
}

/// Events waiting for their sample, in time order (first in, first out
/// among equal times), and the delay each sounding note was given.
pub(crate) struct EventDelay {
    queue: Box<[Option<Pending>]>,
    len: usize,
    notes: Box<[Remembered]>,
    next_note: usize,
    /// Samples since the start: the first sample of the next block.
    pub now: u64,
}

impl EventDelay {
    pub fn new() -> Self {
        Self {
            queue: vec![None; CAPACITY].into_boxed_slice(),
            len: 0,
            notes: vec![Remembered::default(); NOTES].into_boxed_slice(),
            next_note: 0,
            now: 0,
        }
    }

    /// Holds `event`, arriving `time` samples into the block, for `delay`
    /// more samples.
    pub fn push(&mut self, time: u32, delay: u64, event: EventKind) {
        if self.len == CAPACITY {
            // A note-off is worth more than the newest waiting event: a
            // dropped one leaves a note stuck.
            if !matches!(event, EventKind::NoteOff { .. }) {
                return;
            }
            self.len -= 1;
            self.queue[self.len] = None;
        }
        let due = self.now + u64::from(time) + delay;
        // After every event due at or before it.
        let at = self.queue[..self.len].partition_point(|p| p.is_some_and(|p| p.due <= due));
        self.queue.copy_within(at..self.len, at + 1);
        self.queue[at] = Some(Pending {
            due,
            event: Event {
                time: 0,
                kind: event,
            },
        });
        self.len += 1;
    }

    /// Writes the events due in this block of `frames` and moves on to the
    /// next block.
    pub fn drain(&mut self, frames: usize, out: &mut EventsOut<'_>) {
        let end = self.now + frames as u64;
        let mut n = 0;
        while n < self.len && self.queue[n].is_some_and(|p| p.due < end) {
            n += 1;
        }
        for p in self.queue[..n].iter().flatten() {
            let _ = out.push(Event {
                time: (p.due.max(self.now) - self.now) as u32,
                kind: p.event.kind,
            });
        }
        self.queue.copy_within(n..self.len, 0);
        for slot in &mut self.queue[self.len - n..self.len] {
            *slot = None;
        }
        self.len -= n;
        self.now = end;
    }

    /// Drops what is waiting and what is remembered, as a seek does.
    pub fn clear(&mut self) {
        self.queue.fill(None);
        self.len = 0;
        self.notes.fill(Remembered::default());
        self.next_note = 0;
    }

    /// Notes that a note was given `delay`.
    pub fn remember(&mut self, note: NoteId, delay: u64) {
        self.notes[self.next_note] = Remembered {
            note: note.0,
            delay,
            live: true,
        };
        self.next_note = (self.next_note + 1) % NOTES;
    }

    /// The delay a note was given, or `forgotten` if it has been pushed out
    /// of the memory (the delay every event gets, for a node that holds all
    /// of them back).
    pub fn delay_of(&self, note: NoteId, forgotten: u64) -> u64 {
        (0..NOTES)
            .map(|k| &self.notes[(self.next_note + NOTES - 1 - k) % NOTES])
            .find(|r| r.live && r.note == note.0)
            .map_or(forgotten, |r| r.delay)
    }
}
