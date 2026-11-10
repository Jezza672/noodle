//! The Choke Gate node: lets notes through, and cuts them short when a note
//! arrives on its `choke` input.

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo,
    NodeType, NoteId, Setup,
};

use crate::note_split::CHOKE;

/// Passes the notes on `in` to `out`. A note-on arriving on `choke` ends
/// every note passed so far that is still sounding, with a note-off whose
/// velocity is negative ([`CHOKE`]), which a Sampler fades out in 10 ms
/// whatever its mode.
///
/// **Choking one drum with another.** Wire the notes a drum plays (the
/// output of its key filter) into another drum's `choke`. Wire as many
/// drums as you like to one `choke`, and one drum's notes to as many
/// `choke` inputs as you like. Two drums can choke each other (a closed hat
/// and an open hat) without a loop in the graph: each gate reads the other
/// drum's *incoming* notes, not its output.
///
/// The notes on `choke` are not passed on. Events on the two inputs are
/// taken in time order, and a choke at the same moment as a note on `in`
/// comes first, so the new note is not cut.
pub struct ChokeGate;

pub const CHOKE_GATE_ID: &str = "noodle.event.choke_gate";

static INFO: NodeInfo = NodeInfo {
    id: CHOKE_GATE_ID,
    version: 1,
    name: "Choke Gate",
    category: "Events",
};

/// The notes remembered at once. Past this the oldest are forgotten, and a
/// forgotten note is not choked.
const LIVE: usize = 128;

impl NodeType for ChokeGate {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &[]
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .event_input("in", "Notes")
            .event_input("choke", "Choke")
            .event_output("out", "Notes"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ChokeGateNode {
            live: [None; LIVE],
            next: 0,
        }))
    }
}

struct ChokeGateNode {
    /// The notes passed and not yet ended, oldest overwritten first.
    live: [Option<u32>; LIVE],
    next: usize,
}

impl Node for ChokeGateNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            event_inputs,
            event_outputs,
            ..
        } = io;
        let (notes, chokes) = (event_inputs[0], event_inputs[1]);
        let mut chokes = chokes
            .iter()
            .filter(|e| matches!(e.kind, EventKind::NoteOn { .. }))
            .peekable();
        for event in notes {
            // Chokes at or before this event's time go first.
            while let Some(choke) = chokes.next_if(|c| c.time <= event.time) {
                self.cut(choke.time, &mut event_outputs[0]);
            }
            match event.kind {
                EventKind::NoteOn { note, .. } => {
                    self.live[self.next] = Some(note.0);
                    self.next = (self.next + 1) % LIVE;
                }
                EventKind::NoteOff { note, .. } => {
                    for slot in self.live.iter_mut().filter(|s| **s == Some(note.0)) {
                        *slot = None;
                    }
                }
                _ => {}
            }
            let _ = event_outputs[0].push(*event);
        }
        for choke in chokes {
            self.cut(choke.time, &mut event_outputs[0]);
        }
    }

    fn reset(&mut self) {
        self.live = [None; LIVE];
    }
}

impl ChokeGateNode {
    fn cut(&mut self, time: u32, out: &mut noodle_engine::EventsOut<'_>) {
        for slot in self.live.iter_mut() {
            if let Some(note) = slot.take() {
                let _ = out.push(Event {
                    time,
                    kind: EventKind::NoteOff {
                        note: NoteId(note),
                        velocity: CHOKE,
                    },
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    fn on(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key: 46,
                velocity: 1.0,
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

    fn harness() -> Harness {
        Harness::new(&ChokeGate, &Config::new(), &[], 48_000.0, 64).unwrap()
    }

    fn summary(h: &Harness) -> Vec<(u32, &'static str, u32)> {
        h.events(0)
            .iter()
            .map(|e| match e.kind {
                EventKind::NoteOn { note, .. } => (e.time, "on", note.0),
                EventKind::NoteOff { note, velocity } if velocity < 0.0 => {
                    (e.time, "choke", note.0)
                }
                EventKind::NoteOff { note, .. } => (e.time, "off", note.0),
                _ => (e.time, "other", 0),
            })
            .collect()
    }

    #[test]
    fn a_note_on_the_choke_input_ends_the_notes_still_sounding() {
        let mut h = harness();
        h.send_events(0, &[on(0, 1), on(2, 2), off(3, 2)]);
        h.send_events(1, &[on(10, 9)]);
        h.run(64).unwrap();
        // Note 2 had ended already, so only note 1 is cut, and the choke
        // note itself is not passed on.
        assert_eq!(
            summary(&h),
            [(0, "on", 1), (2, "on", 2), (3, "off", 2), (10, "choke", 1)]
        );
    }

    #[test]
    fn a_choke_at_the_same_moment_comes_before_the_new_note() {
        let mut h = harness();
        h.send_events(0, &[on(0, 1), on(5, 2)]);
        h.send_events(1, &[on(5, 9)]);
        h.run(64).unwrap();
        assert_eq!(summary(&h), [(0, "on", 1), (5, "choke", 1), (5, "on", 2)]);
    }

    #[test]
    fn without_a_choke_everything_passes_unchanged() {
        let mut h = harness();
        h.send_events(0, &[on(0, 1), off(4, 1)]);
        h.run(64).unwrap();
        assert_eq!(summary(&h), [(0, "on", 1), (4, "off", 1)]);
    }
}
