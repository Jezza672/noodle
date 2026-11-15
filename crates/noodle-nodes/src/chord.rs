//! The Chord node.

use noodle_engine::{
    Config, Context, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, NoteId,
    ParamInfo, Ports, Setup, Unit,
};

use crate::event_delay::EventDelay;
use crate::held_notes::{HeldNotes, MAX_OUT, Out};

/// Turns each note into a chord.
///
/// - `type` is the chord built on the note: major, minor, diminished,
///   augmented, sus2, sus4, sixth, dominant seventh, major seventh, minor
///   seventh, ninth, power chord or octave.
/// - `inversion` moves that many of the lowest notes up an octave.
/// - `spread` is how many octaves every second note (from the second
///   lowest) moves up, to open the voicing out.
/// - `strum` is the time in milliseconds from the first note to the last
///   (the lowest first), 0 to [`MAX_STRUM_MS`]; 0 plays them together.
///
/// The lowest note keeps the played note's ID and the others get IDs derived
/// from it, so each is its own note downstream. Each has the played note's
/// velocity. A note-off ends every note of its chord, each as late as it
/// began, so a strummed chord is released in the order it was played; the
/// chord is whatever it was at the note-on, so changing `type` while a chord
/// is held strands nothing. Notes that fall outside 0 to 127 are left out. A
/// chord that does not fit the node's queue is dropped whole, with its
/// note-off. Expression follows the note to all of its chord. Other events
/// pass through.
///
/// Strumming only moves the later notes later: the first note stays on time,
/// so the node adds no latency (as Swing).
pub struct Chord;

pub const CHORD_ID: &str = "noodle.event.chord";

/// The longest `strum`.
pub const MAX_STRUM_MS: f32 = 100.0;

/// The chords: a name and the semitones above the played note.
pub const CHORDS: [(&str, &[u8]); 13] = [
    ("Major", &[0, 4, 7]),
    ("Minor", &[0, 3, 7]),
    ("Diminished", &[0, 3, 6]),
    ("Augmented", &[0, 4, 8]),
    ("Sus2", &[0, 2, 7]),
    ("Sus4", &[0, 5, 7]),
    ("Sixth", &[0, 4, 7, 9]),
    ("7th", &[0, 4, 7, 10]),
    ("Major 7th", &[0, 4, 7, 11]),
    ("Minor 7th", &[0, 3, 7, 10]),
    ("9th", &[0, 4, 7, 10, 14]),
    ("Power", &[0, 7]),
    ("Octave", &[0, 12]),
];

#[derive(Ports)]
struct ChordPorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param("type", "Type", ParamInfo::choice(CHORDS.map(|(name, _)| name)))]
    kind: (),
    #[param(
        "inversion",
        "Inversion",
        ParamInfo::choice(["Root", "1st", "2nd", "3rd"])
    )]
    inversion: (),
    #[param("spread", "Spread", ParamInfo::choice(["0", "1", "2"]))]
    spread: (),
    #[param(
        "strum",
        "Strum",
        ParamInfo::new(0.0, MAX_STRUM_MS, 0.0)
            .unit(Unit::None)
            .smoothing(0.0)
    )]
    strum: (),
    #[event_output("out", "Notes")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: CHORD_ID,
    version: 1,
    name: "Chord",
    category: "Events",
};

impl NodeType for Chord {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ChordPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ChordNode {
            delay: EventDelay::new(),
            held: HeldNotes::new(),
            sample_rate: setup.sample_rate,
        }))
    }
}

struct ChordNode {
    delay: EventDelay,
    held: HeldNotes,
    sample_rate: f32,
}

/// The ID of the `index`th note of the chord on `note`; the lowest keeps it.
fn derived(note: u32, index: usize) -> u32 {
    if index == 0 {
        return note;
    }
    let mut x = u64::from(note) << 8 | index as u64;
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (x ^ (x >> 31)) as u32
}

/// The keys of the chord of `kind` on `key`, lowest first, and how many.
pub(crate) fn voicing(
    key: u8,
    kind: usize,
    inversion: usize,
    spread: usize,
) -> ([u8; MAX_OUT], usize) {
    let intervals = CHORDS[kind.min(CHORDS.len() - 1)].1;
    let mut steps = [0u8; MAX_OUT];
    let n = intervals.len().min(MAX_OUT);
    steps[..n].copy_from_slice(&intervals[..n]);
    // Each inversion lifts the lowest note an octave.
    for _ in 0..inversion.min(n - 1) {
        steps[..n].sort_unstable();
        steps[0] += 12;
    }
    steps[..n].sort_unstable();
    for step in steps[..n].iter_mut().skip(1).step_by(2) {
        *step += 12 * spread as u8;
    }
    steps[..n].sort_unstable();
    let mut keys = [0u8; MAX_OUT];
    let mut count = 0;
    for &step in &steps[..n] {
        let k = u16::from(key) + u16::from(step);
        if k <= 127 {
            keys[count] = k as u8;
            count += 1;
        }
    }
    (keys, count)
}

impl Node for ChordNode {
    fn reset(&mut self) {
        self.delay.clear();
        self.held.clear();
    }

    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let kind = first(ChordPorts::KIND)
            .round()
            .clamp(0.0, (CHORDS.len() - 1) as f32) as usize;
        let inversion = first(ChordPorts::INVERSION).round().clamp(0.0, 3.0) as usize;
        let spread = first(ChordPorts::SPREAD).round().clamp(0.0, 2.0) as usize;
        let strum = f64::from(first(ChordPorts::STRUM).clamp(0.0, MAX_STRUM_MS))
            * 0.001
            * f64::from(self.sample_rate);
        for event in event_inputs[ChordPorts::NOTES] {
            match event.kind {
                EventKind::NoteOn {
                    note,
                    channel,
                    key,
                    velocity,
                } => {
                    let (keys, count) = voicing(key, kind, inversion, spread);
                    // A chord that doesn't fit is dropped whole, and not
                    // recorded, so its note-off finds nothing to release.
                    if count == 0 || self.delay.room() < count {
                        continue;
                    }
                    let mut outs = [Out::default(); MAX_OUT];
                    for (i, &k) in keys[..count].iter().enumerate() {
                        let delay = if count > 1 {
                            (strum * i as f64 / (count - 1) as f64) as u32
                        } else {
                            0
                        };
                        outs[i] = Out {
                            id: derived(note.0, i),
                            delay,
                        };
                        self.delay.push(
                            event.time,
                            u64::from(delay),
                            EventKind::NoteOn {
                                note: NoteId(outs[i].id),
                                channel,
                                key: k,
                                velocity,
                            },
                        );
                    }
                    self.held.insert(note.0, &outs[..count]);
                }
                EventKind::NoteOff { note, velocity } => {
                    // Dropped or forgotten: nothing sounds for it.
                    if let Some(held) = self.held.release(note.0) {
                        for out in held.outs() {
                            self.delay.push(
                                event.time,
                                u64::from(out.delay),
                                EventKind::NoteOff {
                                    note: NoteId(out.id),
                                    velocity,
                                },
                            );
                        }
                    }
                }
                EventKind::Expression {
                    note,
                    expression,
                    value,
                } => {
                    if let Some(held) = self.held.get(note.0) {
                        for out in held.outs() {
                            self.delay.push(
                                event.time,
                                u64::from(out.delay),
                                EventKind::Expression {
                                    note: NoteId(out.id),
                                    expression,
                                    value,
                                },
                            );
                        }
                    }
                }
                other => self.delay.push(event.time, 0, other),
            }
        }
        self.delay
            .drain(ctx.frames, &mut event_outputs[ChordPorts::OUT]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{Event, Expression};

    const RATE: f32 = 48_000.0;

    fn on(id: u32, key: u8) -> Event {
        Event {
            time: 0,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity: 0.6,
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

    fn rig() -> Harness {
        Harness::new(&Chord, &Config::new(), &[], RATE, 1000).unwrap()
    }

    /// Runs one block and returns (time, kind) of what came out.
    fn block(h: &mut Harness, events: &[Event]) -> Vec<(u32, EventKind)> {
        h.send_events(ChordPorts::NOTES, events);
        h.run(1000).unwrap();
        h.events(ChordPorts::OUT)
            .iter()
            .map(|e| (e.time, e.kind))
            .collect()
    }

    fn keys(out: &[(u32, EventKind)]) -> Vec<u8> {
        out.iter()
            .filter_map(|(_, k)| match k {
                EventKind::NoteOn { key, .. } => Some(*key),
                _ => None,
            })
            .collect()
    }

    fn off_ids(out: &[(u32, EventKind)]) -> Vec<u32> {
        out.iter()
            .filter_map(|(_, k)| match k {
                EventKind::NoteOff { note, .. } => Some(note.0),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_note_becomes_its_chord_with_distinct_ids_and_the_velocity() {
        let mut h = rig();
        h.set(ChordPorts::KIND, 7.0);
        let out = block(&mut h, &[on(5, 60)]);
        assert_eq!(keys(&out), [60, 64, 67, 70]);
        let mut ids: Vec<u32> = out
            .iter()
            .map(|(_, k)| match k {
                EventKind::NoteOn { note, velocity, .. } => {
                    assert_eq!(*velocity, 0.6);
                    note.0
                }
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(ids[0], 5);
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn inversion_and_spread_move_the_voicing() {
        assert_eq!(voicing(60, 0, 1, 0).0[..3], [64, 67, 72]);
        assert_eq!(voicing(60, 0, 2, 0).0[..3], [67, 72, 76]);
        // Spread 1 lifts the second lowest an octave: 60, 67, 76.
        assert_eq!(voicing(60, 0, 0, 1).0[..3], [60, 67, 76]);
        // Notes past the top are left out.
        assert_eq!(voicing(125, 0, 0, 0).1, 1);
    }

    #[test]
    fn the_off_ends_every_note_of_the_chord() {
        let mut h = rig();
        let on_out = block(&mut h, &[on(5, 60)]);
        let off_out = block(&mut h, &[off(5)]);
        let mut started: Vec<u32> = on_out
            .iter()
            .map(|(_, k)| match k {
                EventKind::NoteOn { note, .. } => note.0,
                _ => unreachable!(),
            })
            .collect();
        let mut ended = off_ids(&off_out);
        started.sort_unstable();
        ended.sort_unstable();
        assert_eq!(started, ended);
    }

    #[test]
    fn changing_the_type_while_held_strands_no_note() {
        let mut h = rig();
        h.set(ChordPorts::KIND, 0.0);
        let on_out = block(&mut h, &[on(5, 60)]);
        assert_eq!(keys(&on_out).len(), 3);
        // Now the node would make a four-note chord (or a different one).
        h.set(ChordPorts::KIND, 10.0);
        let off_out = block(&mut h, &[off(5)]);
        assert_eq!(off_ids(&off_out).len(), 3);
    }

    #[test]
    fn a_strum_spreads_the_notes_and_the_offs_follow() {
        let mut h = rig();
        h.set(ChordPorts::STRUM, 20.0);
        // 20 ms is 960 samples: the last note of three at 960, the middle
        // at 480. All fit in the 1000-sample block.
        let out = block(&mut h, &[on(5, 60)]);
        let times: Vec<u32> = out.iter().map(|(t, _)| *t).collect();
        assert_eq!(times, [0, 480, 960]);
        let off_out = block(&mut h, &[off(5)]);
        let times: Vec<u32> = off_out.iter().map(|(t, _)| *t).collect();
        assert_eq!(times, [0, 480, 960]);
    }

    #[test]
    fn expression_reaches_every_note_and_other_events_pass() {
        let mut h = rig();
        block(&mut h, &[on(5, 60)]);
        let out = block(
            &mut h,
            &[
                Event {
                    time: 0,
                    kind: EventKind::Expression {
                        note: NoteId(5),
                        expression: Expression::Pressure,
                        value: 0.5,
                    },
                },
                Event {
                    time: 1,
                    kind: EventKind::Midi([0xB0, 1, 2]),
                },
            ],
        );
        assert_eq!(out.len(), 4);
        assert!(matches!(out[3].1, EventKind::Midi(_)));
    }
}
