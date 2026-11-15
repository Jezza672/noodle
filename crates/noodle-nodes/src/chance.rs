//! The Chance node.

use noodle_engine::{
    Config, Context, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Ports, Setup,
};

use crate::held_notes::{HeldNotes, Out};

/// Lets each note through with a set probability, the same way every run.
///
/// - `probability` (0 to 1) is the chance that a note passes: 1 passes
///   everything, 0 nothing.
/// - `seed` picks the run of random numbers; another seed, another set of
///   notes dropped.
///
/// There is one draw per note-on, from the generator Humanise uses, started
/// from the project's seed mixed with `seed` and again at every reset (a
/// seek or a loop restart), so a render is the same every time. A note-off
/// and the expression follow their note-on: a dropped note's are dropped
/// too, and a note that passed always gets its note-off, whatever
/// `probability` is by then. Other events pass through. To drop a whole beat
/// at once, put the node before the Chord or after a clock-like source
/// that makes one note per beat.
pub struct Chance;

pub const CHANCE_ID: &str = "noodle.event.chance";

#[derive(Ports)]
struct ChancePorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param(
        "probability",
        "Probability",
        ParamInfo::new(0.0, 1.0, 1.0).smoothing(0.0).featured()
    )]
    probability: (),
    #[param("seed", "Seed", ParamInfo::new(0.0, 999.0, 0.0).smoothing(0.0))]
    seed: (),
    #[event_output("out", "Notes")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: CHANCE_ID,
    version: 1,
    name: "Chance",
    category: "Events",
};

impl NodeType for Chance {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ChancePorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ChanceNode {
            held: HeldNotes::new(),
            random: 1,
            seeded_with: u32::MAX,
            base: setup.seed,
        }))
    }
}

struct ChanceNode {
    /// Which notes passed (one output with their own ID) and which did not.
    held: HeldNotes,
    random: u64,
    /// The `seed` parameter `random` was last started from.
    seeded_with: u32,
    /// The project seed, mixed with the parameter.
    base: u64,
}

impl ChanceNode {
    /// A number from 0 up to (not including) 1.
    fn next(&mut self) -> f32 {
        // xorshift64*
        self.random ^= self.random >> 12;
        self.random ^= self.random << 25;
        self.random ^= self.random >> 27;
        let bits = self.random.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        bits as f32 / (1u64 << 24) as f32
    }
}

impl Node for ChanceNode {
    fn reset(&mut self) {
        self.held.clear();
        // The next block starts the run of random numbers again.
        self.seeded_with = u32::MAX;
    }

    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let probability = first(ChancePorts::PROBABILITY).clamp(0.0, 1.0);
        let seed = first(ChancePorts::SEED).round().clamp(0.0, 999.0) as u32;
        if seed != self.seeded_with {
            self.seeded_with = seed;
            self.random =
                (self.base ^ (u64::from(seed) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1;
        }
        for event in event_inputs[ChancePorts::NOTES] {
            match event.kind {
                EventKind::NoteOn { note, .. } => {
                    let passes = self.next() < probability;
                    let outs = [Out {
                        id: note.0,
                        delay: 0,
                    }];
                    self.held
                        .insert(note.0, if passes { &outs[..] } else { &[] });
                    if !passes {
                        continue;
                    }
                }
                EventKind::NoteOff { note, .. } => {
                    // A note that was never seen (or was forgotten) passes:
                    // an off with nothing to end is harmless.
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
            let _ = event_outputs[ChancePorts::OUT].push(*event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{Event, NoteId};

    fn on(id: u32) -> Event {
        Event {
            time: 0,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key: 60,
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

    fn rig(probability: f32, seed: f32) -> Harness {
        let mut h = Harness::new(&Chance, &Config::new(), &[], 48_000.0, 64).unwrap();
        h.set(ChancePorts::PROBABILITY, probability);
        h.set(ChancePorts::SEED, seed);
        h
    }

    /// Sends notes 0 to 199 (an on and an off each) one block apiece and
    /// returns which notes passed, with how many offs came out.
    fn run(h: &mut Harness) -> (Vec<u32>, Vec<u32>) {
        let (mut ons, mut offs) = (Vec::new(), Vec::new());
        for id in 0..200 {
            h.send_events(ChancePorts::NOTES, &[on(id), off(id)]);
            h.run(64).unwrap();
            for e in h.events(ChancePorts::OUT) {
                match e.kind {
                    EventKind::NoteOn { note, .. } => ons.push(note.0),
                    EventKind::NoteOff { note, .. } => offs.push(note.0),
                    _ => {}
                }
            }
        }
        (ons, offs)
    }

    #[test]
    fn probability_one_passes_everything_and_zero_nothing() {
        let (ons, offs) = run(&mut rig(1.0, 0.0));
        assert_eq!(ons.len(), 200);
        assert_eq!(offs.len(), 200);
        let (ons, offs) = run(&mut rig(0.0, 0.0));
        assert!(ons.is_empty() && offs.is_empty());
    }

    #[test]
    fn about_that_share_passes_the_same_way_every_run() {
        let (a, offs) = run(&mut rig(0.5, 3.0));
        let (b, _) = run(&mut rig(0.5, 3.0));
        let (c, _) = run(&mut rig(0.5, 4.0));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!((70..=130).contains(&a.len()), "{}", a.len());
        // Every note that passed ended, and no other did.
        assert_eq!(a, offs);
    }

    #[test]
    fn a_reset_starts_the_run_again() {
        let mut h = rig(0.5, 1.0);
        let (first, _) = run(&mut h);
        h.seek(0);
        let (again, _) = run(&mut h);
        assert_eq!(first, again);
    }

    #[test]
    fn a_note_off_follows_its_note_on_whatever_the_probability_is_by_then() {
        let mut h = rig(1.0, 0.0);
        h.send_events(ChancePorts::NOTES, &[on(1)]);
        h.run(64).unwrap();
        assert_eq!(h.events(ChancePorts::OUT).len(), 1);
        h.set(ChancePorts::PROBABILITY, 0.0);
        h.send_events(ChancePorts::NOTES, &[off(1), on(2), off(2)]);
        h.run(64).unwrap();
        // Note 1's off passes; note 2 was dropped, with its off.
        assert_eq!(h.events(ChancePorts::OUT), &[off(1)]);
    }
}
