//! The MIDI In node: notes and controllers from the MIDI input port the
//! app has open.
//!
//! The port itself belongs to the app (a choice in the settings, not part of
//! the project, like the audio device), and its messages reach every MIDI In
//! node through a [`MidiBus`]. A message that arrives during a block is
//! played at the start of the next one, so live playing is up to one block
//! (about 11 ms in the app) late and has that much jitter.
//!
//! Note-ons and note-offs become note events; everything else (controllers,
//! pitch bend, aftertouch, program changes) passes on as raw MIDI events.
//! The note ID is made from the channel and key, so a note-off finds its
//! note, and its top bit is set, which keeps it apart from the IDs notes in
//! MIDI clips have.

use noodle_engine::{
    Config, Context, Event, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    NoteId, ParamInfo, Ports, Setup,
};
use noodle_io::{MidiBus, MidiReceiver};

pub const MIDI_IN_ID: &str = "noodle.event.midi_in";

#[derive(Ports)]
struct MidiInPorts {
    /// Only this channel is played; 0 plays them all.
    #[param("channel", "Channel", ParamInfo::new(0.0, 16.0, 0.0).smoothing(0.0))]
    channel: (),
    #[event_output("out", "Events")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: MIDI_IN_ID,
    version: 1,
    name: "MIDI In",
    category: "Events",
};

/// Registered by [`register_library`](crate::register_library).
pub struct MidiIn {
    bus: MidiBus,
}

impl MidiIn {
    pub fn new(bus: &MidiBus) -> Self {
        Self { bus: bus.clone() }
    }
}

impl NodeType for MidiIn {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(MidiInPorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(MidiInNode {
            input: self.bus.subscribe(),
            held: [0; 16],
            ending: 0,
        }))
    }
}

struct MidiInNode {
    input: MidiReceiver,
    /// Per channel, the keys that have a note-on and no note-off yet, one
    /// bit each, so "all notes off" can end them.
    held: [u128; 16],
    /// Channels with an "all notes off" still to carry out.
    ending: u16,
}

fn note_id(channel: u8, key: u8) -> NoteId {
    NoteId(0x8000_0000 | u32::from(channel) << 8 | u32::from(key))
}

impl MidiInNode {
    /// The events a message stands for, in order. At most two kinds per
    /// message except "all notes off", which is handled by the caller.
    fn translate(&mut self, [status, a, b]: [u8; 3]) -> Option<EventKind> {
        let channel = status & 0x0F;
        Some(match status & 0xF0 {
            0x90 if b > 0 => {
                self.held[usize::from(channel)] |= 1 << a;
                EventKind::NoteOn {
                    note: note_id(channel, a),
                    channel,
                    key: a,
                    velocity: f32::from(b) / 127.0,
                }
            }
            0x80 | 0x90 => {
                self.held[usize::from(channel)] &= !(1 << a);
                EventKind::NoteOff {
                    note: note_id(channel, a),
                    velocity: f32::from(b) / 127.0,
                }
            }
            _ => EventKind::Midi([status, a, b]),
        })
    }
}

impl MidiInNode {
    /// Ends the held notes on channels with an "all notes off" to carry out.
    /// Each note is forgotten only once its note-off is in the buffer, so if
    /// the buffer fills the rest wait for the next block. Returns whether it
    /// finished.
    fn flush_ending(&mut self, out: &mut noodle_engine::EventsOut<'_>) -> bool {
        for channel in 0..16u8 {
            let bit = 1u16 << channel;
            if self.ending & bit == 0 {
                continue;
            }
            while self.held[usize::from(channel)] != 0 {
                let key = self.held[usize::from(channel)].trailing_zeros() as u8;
                let off = Event {
                    time: 0,
                    kind: EventKind::NoteOff {
                        note: note_id(channel, key),
                        velocity: 0.0,
                    },
                };
                if out.push(off).is_err() {
                    return false;
                }
                self.held[usize::from(channel)] &= !(1 << key);
            }
            self.ending &= !bit;
        }
        true
    }
}

impl Node for MidiInNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let wanted = io.inputs[MidiInPorts::CHANNEL].lane(0, 0)[0].round() as u8;
        let out = &mut io.event_outputs[MidiInPorts::OUT];
        // Left over from a block where the buffer filled.
        if !self.flush_ending(out) {
            return;
        }
        while let Some(message) = self.input.pop() {
            let channel = message[0] & 0x0F;
            // A note-off for a note that is held always gets through, so
            // changing the channel can't leave it sounding.
            let note_off =
                message[0] & 0xF0 == 0x80 || (message[0] & 0xF0 == 0x90 && message[2] == 0);
            let ends_held = note_off && self.held[usize::from(channel)] >> message[1] & 1 == 1;
            if wanted != 0 && channel + 1 != wanted && !ends_held {
                continue;
            }
            // Sound off (120) and all notes off (123) end what is held.
            if message[0] & 0xF0 == 0xB0 && matches!(message[1], 120 | 123) {
                self.ending |= 1 << channel;
                if !self.flush_ending(out) {
                    // The controller itself is dropped with the rest of the
                    // block; the notes still held end next block.
                    return;
                }
            }
            if let Some(kind) = self.translate(message)
                && out.push(Event { time: 0, kind }).is_err()
            {
                break;
            }
        }
    }

    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    fn node() -> (Harness, MidiBus) {
        let bus = MidiBus::new();
        let h = Harness::new(&MidiIn::new(&bus), &Config::new(), &[], 48_000.0, 16).unwrap();
        (h, bus)
    }

    fn key_on(channel: u8, key: u8, velocity: u8) -> [u8; 3] {
        [0x90 | channel, key, velocity]
    }

    #[test]
    fn notes_become_note_events_and_the_rest_passes_through() {
        let (mut h, bus) = node();
        bus.send(key_on(0, 60, 127));
        bus.send([0xB0, 1, 64]);
        bus.send([0x80, 60, 0]);
        h.run(16).unwrap();
        let events = h.events(MidiInPorts::OUT).to_vec();
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].kind,
            EventKind::NoteOn {
                note: note_id(0, 60),
                channel: 0,
                key: 60,
                velocity: 1.0
            }
        );
        assert_eq!(events[1].kind, EventKind::Midi([0xB0, 1, 64]));
        assert!(
            matches!(events[2].kind, EventKind::NoteOff { note, .. } if note == note_id(0, 60))
        );
        // A block with nothing new is empty.
        h.run(16).unwrap();
        assert!(h.events(MidiInPorts::OUT).is_empty());
    }

    #[test]
    fn a_note_on_with_no_velocity_is_a_note_off() {
        let (mut h, bus) = node();
        bus.send(key_on(2, 64, 100));
        bus.send(key_on(2, 64, 0));
        h.run(16).unwrap();
        let events = h.events(MidiInPorts::OUT);
        assert!(matches!(
            events[0].kind,
            EventKind::NoteOn { channel: 2, .. }
        ));
        assert!(
            matches!(events[1].kind, EventKind::NoteOff { note, .. } if note == note_id(2, 64))
        );
    }

    #[test]
    fn the_channel_parameter_filters() {
        let (mut h, bus) = node();
        h.set(MidiInPorts::CHANNEL, 3.0);
        bus.send(key_on(0, 60, 100));
        bus.send(key_on(2, 61, 100));
        bus.send(key_on(3, 62, 100));
        h.run(16).unwrap();
        let keys: Vec<u8> = h
            .events(MidiInPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOn { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(keys, [61]);
    }

    #[test]
    fn changing_the_channel_does_not_strand_a_held_note() {
        let (mut h, bus) = node();
        bus.send(key_on(2, 64, 100));
        h.run(16).unwrap();
        h.set(MidiInPorts::CHANNEL, 1.0);
        bus.send([0x82, 64, 0]);
        bus.send(key_on(2, 65, 100));
        h.run(16).unwrap();
        let events = h.events(MidiInPorts::OUT);
        // The held note ends; the new one is on a filtered channel.
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(
            matches!(events[0].kind, EventKind::NoteOff { note, .. } if note == note_id(2, 64))
        );
        // And a stray note-off for a note that isn't held is filtered.
        bus.send([0x82, 70, 0]);
        h.run(16).unwrap();
        assert!(h.events(MidiInPorts::OUT).is_empty());
    }

    #[test]
    fn all_notes_off_is_finished_in_a_later_block_if_the_buffer_fills() {
        let (mut h, bus) = node();
        for key in 0..60 {
            bus.send(key_on(0, key, 100));
        }
        h.run(16).unwrap();
        // 984 controllers leave room for 40 of the 60 note-offs.
        for i in 0..984u16 {
            bus.send([0xB0, 1, (i % 128) as u8]);
        }
        bus.send([0xB0, 123, 0]);
        h.run(16).unwrap();
        let count = |h: &Harness| {
            h.events(MidiInPorts::OUT)
                .iter()
                .filter(|e| matches!(e.kind, EventKind::NoteOff { .. }))
                .count()
        };
        let first = count(&h);
        assert_eq!(first, 40);
        h.run(16).unwrap();
        assert_eq!(first + count(&h), 60, "every held note ends, none is lost");
    }

    #[test]
    fn all_notes_off_ends_what_is_held() {
        let (mut h, bus) = node();
        bus.send(key_on(0, 60, 100));
        bus.send(key_on(0, 64, 100));
        bus.send(key_on(1, 67, 100));
        h.run(16).unwrap();
        bus.send([0xB0, 123, 0]);
        h.run(16).unwrap();
        let offs: Vec<NoteId> = h
            .events(MidiInPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOff { note, .. } => Some(note),
                _ => None,
            })
            .collect();
        // Channel 1's note is left alone.
        assert_eq!(offs, [note_id(0, 60), note_id(0, 64)]);
    }
}
