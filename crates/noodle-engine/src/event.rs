//! Note and MIDI events, timestamped within a block.

/// Identifies one note from note-on to note-off, so expressions and note-offs
/// reach the right voice even when the same key is held twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NoteId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    /// Frame offset within the current block.
    pub time: u32,
    pub kind: EventKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventKind {
    NoteOn {
        note: NoteId,
        channel: u8,
        key: u8,
        velocity: f32,
    },
    NoteOff {
        note: NoteId,
        velocity: f32,
    },
    /// Per-note modulation (MPE, MIDI 2.0 and CLAP note expressions).
    Expression {
        note: NoteId,
        expression: Expression,
        value: f32,
    },
    /// A raw MIDI 1.0 message, for anything not covered above.
    Midi([u8; 3]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Expression {
    /// In semitones.
    Pitch,
    Pressure,
    Timbre,
    Volume,
    Pan,
}

/// Where a node writes the events it produces in one block. The capacity is
/// fixed when the plan is built, so pushing never allocates.
#[derive(Debug)]
pub struct EventsOut<'a> {
    events: &'a mut Vec<Event>,
}

impl<'a> EventsOut<'a> {
    /// Clears `events`, keeping its capacity.
    pub fn new(events: &'a mut Vec<Event>) -> Self {
        events.clear();
        Self { events }
    }

    /// Events must be pushed in time order. When the buffer is full, the
    /// event is handed back.
    pub fn push(&mut self, event: Event) -> Result<(), Event> {
        debug_assert!(
            self.events
                .last()
                .is_none_or(|last| last.time <= event.time),
            "events must be pushed in time order"
        );
        if self.events.len() == self.events.capacity() {
            return Err(event);
        }
        self.events.push(event);
        Ok(())
    }

    /// Adds an event in its place in time order, after any at the same time,
    /// for a node that merges events from several sources into one block.
    /// When the buffer is full, the event is handed back.
    pub fn insert(&mut self, event: Event) -> Result<(), Event> {
        if self.events.len() == self.events.capacity() {
            return Err(event);
        }
        let at = self.events.partition_point(|e| e.time <= event.time);
        self.events.insert(at, event);
        Ok(())
    }

    pub fn as_slice(&self) -> &[Event] {
        self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(time: u32, key: u8) -> Event {
        Event {
            time,
            kind: EventKind::Midi([0x90, key, 0]),
        }
    }

    #[test]
    fn inserting_keeps_time_order_and_puts_ties_after_what_is_there() {
        let mut events = Vec::with_capacity(4);
        let mut out = EventsOut::new(&mut events);
        out.push(at(5, 1)).unwrap();
        out.push(at(9, 2)).unwrap();
        out.insert(at(2, 3)).unwrap();
        out.insert(at(5, 4)).unwrap();
        let order: Vec<_> = out.as_slice().iter().map(|e| (e.time, e.kind)).collect();
        assert_eq!(
            order,
            [
                (2, EventKind::Midi([0x90, 3, 0])),
                (5, EventKind::Midi([0x90, 1, 0])),
                (5, EventKind::Midi([0x90, 4, 0])),
                (9, EventKind::Midi([0x90, 2, 0])),
            ]
        );
    }

    #[test]
    fn inserting_into_a_full_buffer_hands_the_event_back_without_growing_it() {
        let mut events = Vec::with_capacity(1);
        let mut out = EventsOut::new(&mut events);
        out.insert(at(1, 1)).unwrap();
        assert_eq!(out.insert(at(0, 2)), Err(at(0, 2)));
        assert_eq!(out.as_slice().len(), 1);
        assert_eq!(events.capacity(), 1);
    }
}
