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

    pub fn as_slice(&self) -> &[Event] {
        self.events
    }
}
