//! Nodes that make and read note events: [`Key`] makes them from a gate, and
//! [`MonoNote`] turns them into the pitch, gate and velocity signals that a
//! monophonic patch plays from.
//!
//! The *Voices* node does the same for many notes at once, once polyphony
//! lands; this one plays the most recent note.

use noodle_engine::{
    Config, Context, Event, EventKind, Expression, Instance, Io, Layout, Node, NodeError, NodeInfo,
    NodeType, NoteId, ParamInfo, Ports, Setup,
};

/// Turns a gate parameter into notes: a note-on when the gate rises, a
/// note-off when it falls or the key changes. Pair it with a
/// [`Button`](crate::Button) for a key you can click, or wire in a
/// sequencer's gate.
pub struct Key;

pub const KEY_ID: &str = "noodle.event.key";

#[derive(Ports)]
struct KeyPorts {
    /// The MIDI key number, rounded to a whole key. Changing it while the
    /// gate is high ends the note and starts the new one.
    #[param("note", "Note", ParamInfo::new(0.0, 127.0, 60.0).smoothing(0.0))]
    note: (),
    #[param("gate", "Gate", ParamInfo::choice(["Off", "On"]))]
    gate: (),
    #[param("velocity", "Velocity", ParamInfo::new(0.0, 1.0, 0.8))]
    velocity: (),
    #[event_output("out", "Notes")]
    out: (),
}

static KEY: NodeInfo = NodeInfo {
    id: KEY_ID,
    version: 1,
    name: "Key",
    category: "Events",
};

impl NodeType for Key {
    fn info(&self) -> &NodeInfo {
        &KEY
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(KeyPorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(KeyNode {
            held: None,
            next_id: 0,
        }))
    }
}

struct KeyNode {
    /// The sounding note and its key.
    held: Option<(NoteId, u8)>,
    next_id: u32,
}

impl Node for KeyNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let note = io.inputs[KeyPorts::NOTE].lane(0, 0);
        let gate = io.inputs[KeyPorts::GATE].lane(0, 0);
        let velocity = io.inputs[KeyPorts::VELOCITY].lane(0, 0);
        let out = &mut io.event_outputs[KeyPorts::OUT];
        for time in 0..ctx.frames {
            let key = key_number(note[time]);
            let gate = gate[time] >= 0.5;
            if let Some((id, held)) = self.held
                && (!gate || key != held)
            {
                let off = Event {
                    time: time as u32,
                    kind: EventKind::NoteOff {
                        note: id,
                        velocity: 0.0,
                    },
                };
                // A full buffer drops the note-off; try again next frame
                // rather than lose track of the note.
                if out.push(off).is_ok() {
                    self.held = None;
                }
            }
            if self.held.is_none() && gate {
                let id = NoteId(self.next_id);
                let on = Event {
                    time: time as u32,
                    kind: EventKind::NoteOn {
                        note: id,
                        channel: 0,
                        key,
                        velocity: velocity[time].clamp(0.0, 1.0),
                    },
                };
                if out.push(on).is_ok() {
                    self.next_id = self.next_id.wrapping_add(1);
                    self.held = Some((id, key));
                }
            }
        }
    }

    fn reset(&mut self) {
        self.held = None;
    }
}

fn key_number(value: f32) -> u8 {
    // `as` saturates and turns NaN into 0.
    value.round().clamp(0.0, 127.0) as u8
}

/// Plays the most recent held note as signals: `pitch` in hertz, `gate`
/// (1 while any note is held) and `velocity`. Releasing a note falls back to
/// the one before it that is still held. After the last release, `pitch` and
/// `velocity` hold their values so an envelope's release stays in tune.
pub struct MonoNote;

pub const MONO_NOTE_ID: &str = "noodle.event.mono";

#[derive(Ports)]
struct MonoPorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[output("pitch", "Pitch")]
    pitch: (),
    #[output("gate", "Gate")]
    gate: (),
    #[output("velocity", "Velocity")]
    velocity: (),
}

static MONO: NodeInfo = NodeInfo {
    id: MONO_NOTE_ID,
    version: 1,
    name: "Mono Note",
    category: "Events",
};

impl NodeType for MonoNote {
    fn info(&self) -> &NodeInfo {
        &MONO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(MonoPorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(MonoNode::default()))
    }
}

/// How many notes can be held at once; more push out the oldest.
const HELD: usize = 32;

#[derive(Clone, Copy, Default)]
struct Held {
    id: u32,
    key: u8,
    velocity: f32,
    /// A pitch expression, in semitones.
    bend: f32,
}

#[derive(Default)]
struct MonoNode {
    held: [Held; HELD],
    len: usize,
    /// What the outputs hold when nothing is: the last note sounded.
    last: Held,
}

impl MonoNode {
    fn apply(&mut self, kind: EventKind) {
        match kind {
            EventKind::NoteOn {
                note,
                key,
                velocity,
                ..
            } => {
                if self.len == HELD {
                    self.held.copy_within(1.., 0);
                    self.len -= 1;
                }
                self.held[self.len] = Held {
                    id: note.0,
                    key,
                    velocity,
                    bend: 0.0,
                };
                self.len += 1;
            }
            EventKind::NoteOff { note, .. } => {
                if let Some(i) = self.held[..self.len].iter().position(|h| h.id == note.0) {
                    self.held.copy_within(i + 1..self.len, i);
                    self.len -= 1;
                }
            }
            EventKind::Expression {
                note,
                expression: Expression::Pitch,
                value,
            } => {
                for held in self.held[..self.len].iter_mut().filter(|h| h.id == note.0) {
                    held.bend = value;
                }
            }
            _ => {}
        }
        if let Some(top) = self.len.checked_sub(1) {
            self.last = self.held[top];
        }
    }

    fn pitch(&self) -> f32 {
        let semitones = f32::from(self.last.key) + self.last.bend - 69.0;
        440.0 * (semitones / 12.0).exp2()
    }
}

impl Node for MonoNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            event_inputs,
            outputs,
            ..
        } = io;
        let events = event_inputs[MonoPorts::NOTES];
        let [pitch, gate, velocity] = outputs
            .get_disjoint_mut([MonoPorts::PITCH, MonoPorts::GATE, MonoPorts::VELOCITY])
            .expect("mono note outputs are distinct");
        let (pitch, gate, velocity) = (
            pitch.lane_mut(0, 0),
            gate.lane_mut(0, 0),
            velocity.lane_mut(0, 0),
        );

        let mut next = 0;
        for time in 0..ctx.frames {
            while let Some(event) = events.get(next).filter(|e| e.time as usize <= time) {
                self.apply(event.kind);
                next += 1;
            }
            pitch[time] = self.pitch();
            gate[time] = if self.len > 0 { 1.0 } else { 0.0 };
            velocity[time] = self.last.velocity;
        }
        // Events past the block's end would be a bug upstream; apply them
        // anyway so a note-off is never lost.
        for event in &events[next..] {
            self.apply(event.kind);
        }
    }

    fn reset(&mut self) {
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn on(time: u32, id: u32, key: u8) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity: 0.5,
            },
        }
    }

    fn off(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        }
    }

    fn mono() -> Harness {
        Harness::new(&MonoNote, &Config::new(), &[], 48_000.0, 16).unwrap()
    }

    #[test]
    fn mono_note_follows_the_latest_note_and_falls_back() {
        let mut h = mono();
        h.send_events(MonoPorts::NOTES, &[on(2, 1, 69), on(6, 2, 81), off(10, 2)]);
        h.run(16).unwrap();
        let gate = h.output(MonoPorts::GATE).lane(0, 0).to_vec();
        let pitch = h.output(MonoPorts::PITCH).lane(0, 0).to_vec();
        assert_eq!(&gate[..2], &[0.0, 0.0]);
        assert!(gate[2..].iter().all(|&g| g == 1.0));
        assert!((pitch[2] - 440.0).abs() < 1e-3);
        assert!((pitch[6] - 880.0).abs() < 1e-2);
        // Releasing the upper note falls back to the one still held.
        assert!((pitch[10] - 440.0).abs() < 1e-3);
        assert_eq!(h.output(MonoPorts::VELOCITY).shape(), Shape::MONO);
    }

    #[test]
    fn mono_note_holds_pitch_after_release() {
        let mut h = mono();
        h.send_events(MonoPorts::NOTES, &[on(0, 1, 69), off(4, 1)]);
        h.run(16).unwrap();
        let gate = h.output(MonoPorts::GATE).lane(0, 0).to_vec();
        let pitch = h.output(MonoPorts::PITCH).lane(0, 0).to_vec();
        assert_eq!(gate[3], 1.0);
        assert!(gate[4..].iter().all(|&g| g == 0.0));
        assert!(pitch.iter().all(|&p| (p - 440.0).abs() < 1e-3));
        // And across the next block, with no events.
        h.send_events(MonoPorts::NOTES, &[]);
        h.run(16).unwrap();
        assert!((h.output(MonoPorts::PITCH).lane(0, 0)[0] - 440.0).abs() < 1e-3);
    }

    #[test]
    fn pitch_expression_bends_the_note() {
        let mut h = mono();
        let bend = Event {
            time: 4,
            kind: EventKind::Expression {
                note: NoteId(1),
                expression: Expression::Pitch,
                value: 12.0,
            },
        };
        h.send_events(MonoPorts::NOTES, &[on(0, 1, 69), bend]);
        h.run(16).unwrap();
        let pitch = h.output(MonoPorts::PITCH).lane(0, 0);
        assert!((pitch[3] - 440.0).abs() < 1e-3);
        // `apply` runs for the bend event itself, so it takes effect at its time.
        assert!((pitch[4] - 880.0).abs() < 1e-2);
    }

    #[test]
    fn a_full_stack_drops_the_oldest_note() {
        let mut h = mono();
        let events: Vec<_> = (0..HELD as u32 + 1)
            .map(|i| on(0, i, (i % 100) as u8))
            .collect();
        h.send_events(MonoPorts::NOTES, &events);
        h.run(4).unwrap();
        // Releasing every note but the oldest leaves the gate low: it was dropped.
        let offs: Vec<_> = (1..HELD as u32 + 1).map(|i| off(0, i)).collect();
        h.send_events(MonoPorts::NOTES, &offs);
        h.run(4).unwrap();
        assert!(
            h.output(MonoPorts::GATE)
                .lane(0, 0)
                .iter()
                .all(|&g| g == 0.0)
        );
    }

    fn key() -> Harness {
        Harness::new(&Key, &Config::new(), &[], 48_000.0, 8).unwrap()
    }

    #[test]
    fn key_emits_a_note_per_gate() {
        let mut h = key();
        h.run(8).unwrap();
        assert!(h.events(KeyPorts::OUT).is_empty());

        h.set(KeyPorts::GATE, 1.0);
        h.run(8).unwrap();
        let events = h.events(KeyPorts::OUT).to_vec();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], on_with(0, 0, 60, 0.8));

        // Held across blocks: nothing new.
        h.run(8).unwrap();
        assert!(h.events(KeyPorts::OUT).is_empty());

        h.set(KeyPorts::GATE, 0.0);
        h.run(8).unwrap();
        assert_eq!(h.events(KeyPorts::OUT), &[off(0, 0)]);

        // A second press is a new note.
        h.set(KeyPorts::GATE, 1.0);
        h.run(8).unwrap();
        assert_eq!(h.events(KeyPorts::OUT), &[on_with(0, 1, 60, 0.8)]);
    }

    #[test]
    fn changing_the_key_while_held_moves_the_note() {
        let mut h = key();
        h.set(KeyPorts::GATE, 1.0);
        h.run(8).unwrap();
        h.set(KeyPorts::NOTE, 64.0);
        h.run(8).unwrap();
        assert_eq!(
            h.events(KeyPorts::OUT),
            &[off(0, 0), on_with(0, 1, 64, 0.8)]
        );
    }

    fn on_with(time: u32, id: u32, key: u8, velocity: f32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity,
            },
        }
    }
}
