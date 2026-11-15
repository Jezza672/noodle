//! The Transpose node.

use noodle_engine::{
    Config, Context, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Ports, Setup, Unit,
};

use crate::held_notes::{HeldNotes, Out};

/// Shifts the notes that pass through it by a number of semitones.
///
/// `semitones` (-48 to 48) is read at each note-on and kept for the note:
/// a note shifted outside 0 to 127 is dropped, with its note-off and
/// expression, and changing `semitones` while a note is held never strands
/// or invents one. A note keeps its ID and velocity.
///
/// It has no follow mode of its own. To follow the song's key, wire a Song
/// Key node's `shift` into `semitones`: a wire is the value here, so no
/// conversion is needed. (`semitones` is a number that is rounded; a wire
/// between whole numbers passes them as they are.)
pub struct Transpose;

pub const TRANSPOSE_ID: &str = "noodle.event.transpose";

#[derive(Ports)]
struct TransposePorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param(
        "semitones",
        "Semitones",
        ParamInfo::new(-48.0, 48.0, 0.0)
            .unit(Unit::Semitones)
            .smoothing(0.0)
    )]
    semitones: (),
    #[event_output("out", "Notes")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: TRANSPOSE_ID,
    version: 1,
    name: "Transpose",
    category: "Events",
};

impl NodeType for Transpose {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(TransposePorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(TransposeNode {
            held: HeldNotes::new(),
        }))
    }
}

struct TransposeNode {
    /// Which notes were dropped (the rest passed with their own ID).
    held: HeldNotes,
}

impl Node for TransposeNode {
    fn reset(&mut self) {
        self.held.clear();
    }

    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let semitones = inputs[TransposePorts::SEMITONES]
            .lane(0, 0)
            .first()
            .copied()
            .unwrap_or(0.0)
            .round()
            .clamp(-48.0, 48.0) as i32;
        for event in event_inputs[TransposePorts::NOTES] {
            let mut event = *event;
            match &mut event.kind {
                EventKind::NoteOn { note, key, .. } => {
                    let moved = i32::from(*key) + semitones;
                    if (0..=127).contains(&moved) {
                        *key = moved as u8;
                        self.held.insert(
                            note.0,
                            &[Out {
                                id: note.0,
                                delay: 0,
                            }],
                        );
                    } else {
                        self.held.insert(note.0, &[]);
                        continue;
                    }
                }
                EventKind::NoteOff { note, .. } => {
                    if self
                        .held
                        .release(note.0)
                        .is_some_and(|held| held.outs().is_empty())
                    {
                        continue;
                    }
                }
                EventKind::Expression { note, .. }
                    if self
                        .held
                        .get(note.0)
                        .is_some_and(|held| held.outs().is_empty()) =>
                {
                    continue;
                }
                _ => {}
            }
            let _ = event_outputs[TransposePorts::OUT].push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{Event, NoteId};

    fn on(id: u32, key: u8) -> Event {
        Event {
            time: 0,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity: 0.5,
            },
        }
    }

    fn off(id: u32) -> Event {
        Event {
            time: 0,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        }
    }

    fn step(h: &mut Harness, events: &[Event]) -> Vec<EventKind> {
        h.send_events(TransposePorts::NOTES, events);
        h.run(64).unwrap();
        h.events(TransposePorts::OUT)
            .iter()
            .map(|e| e.kind)
            .collect()
    }

    fn rig(semitones: f32) -> Harness {
        let mut h = Harness::new(&Transpose, &Config::new(), &[], 48_000.0, 64).unwrap();
        h.set(TransposePorts::SEMITONES, semitones);
        h
    }

    #[test]
    fn it_shifts_the_key_and_keeps_the_rest() {
        let mut h = rig(-2.0);
        assert_eq!(
            step(&mut h, &[on(1, 60), off(1)]),
            [
                EventKind::NoteOn {
                    note: NoteId(1),
                    channel: 0,
                    key: 58,
                    velocity: 0.5
                },
                off(1).kind
            ]
        );
    }

    #[test]
    fn a_note_shifted_out_of_range_is_dropped_with_its_off() {
        let mut h = rig(12.0);
        assert!(step(&mut h, &[on(1, 120), on(2, 60)]).len() == 1);
        // Held across a parameter change: the dropped one stays dropped (its
        // off is not invented) and the sounding one still ends.
        h.set(TransposePorts::SEMITONES, 0.0);
        let out = step(&mut h, &[off(1), off(2)]);
        assert_eq!(out, [off(2).kind]);
    }

    #[test]
    fn a_parameter_change_never_strands_a_held_note() {
        let mut h = rig(0.0);
        // Sounding, then the shift would drop it: its off must still pass.
        step(&mut h, &[on(1, 120)]);
        h.set(TransposePorts::SEMITONES, 48.0);
        assert_eq!(step(&mut h, &[off(1)]), [off(1).kind]);
    }
}
