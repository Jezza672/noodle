//! The Note Split node: routes notes to outputs by key and velocity.

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo,
    NodeType, NoteId, ParamInfo, Setup, Unit,
};

/// Sends each note to every output whose key range and velocity range it
/// falls in, so a drum rack is a Note Split in front of Samplers, and a
/// keyboard split or velocity layer is one with two outputs.
///
/// **Outputs.** How many (`outputs`, 1 to 16) is config, since each output is
/// a port. Output *n* has `low` and `high` keys and `vel_low` and `vel_high`
/// velocities (0 to 1, both ends included) and `transpose` in semitones,
/// which shifts the key it sends. With one output and no `rest` it is a note
/// filter: what matches passes, the rest is dropped. Set `low` and `high`
/// to the same key to pick one drum.
///
/// **Layering.** A note goes to every output that matches, not the first.
///
/// **Rest.** With the `rest` setting on there is one more output, `rest`,
/// that gets the notes no other output took (unshifted), so it is also the
/// inverse of a filter.
///
/// **Note-offs and expression** follow their note-on to the outputs that
/// took it, whatever the ranges have become since. Other events (raw MIDI
/// such as pitch bend and controllers) go to every output.
///
/// The ranges are read when the note starts.
pub struct NoteSplit;

pub const NOTE_SPLIT_ID: &str = "noodle.event.note_split";

const OUTPUTS: ConfigInfo = ConfigInfo::int("outputs", "Outputs", 2);
/// 1 to add the `rest` output.
const REST: ConfigInfo = ConfigInfo::int("rest", "Rest output", 0);
pub const MAX_OUTPUTS: i64 = 16;

static CONFIG: [ConfigInfo; 2] = [OUTPUTS, REST];

static INFO: NodeInfo = NodeInfo {
    id: NOTE_SPLIT_ID,
    version: 1,
    name: "Note Split",
    category: "Events",
};

/// The ports of output `n` (from 0), after the one event input: the five
/// parameters of each output in turn.
const PARAMS_PER_OUTPUT: usize = 5;
const LOW: usize = 0;
const HIGH: usize = 1;
const VEL_LOW: usize = 2;
const VEL_HIGH: usize = 3;
const TRANSPOSE: usize = 4;

/// The input index of a parameter of output `n`.
pub const fn param_index(output: usize, which: usize) -> usize {
    output * PARAMS_PER_OUTPUT + which
}

impl NoteSplit {
    fn counts(config: &Config) -> Result<(usize, bool), NodeError> {
        let outputs = OUTPUTS.get_int(config);
        if !(1..=MAX_OUTPUTS).contains(&outputs) {
            return Err(NodeError::config(format!(
                "Note Split needs between 1 and {MAX_OUTPUTS} outputs, not {outputs}"
            )));
        }
        Ok((outputs as usize, REST.get_int(config) != 0))
    }
}

impl NodeType for NoteSplit {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let (outputs, rest) = Self::counts(config)?;
        let mut layout = Layout::realtime().event_input("in", "Notes");
        for n in 1..=outputs {
            layout = layout
                .param(
                    format!("low{n}"),
                    format!("Low key {n}"),
                    ParamInfo::new(0.0, 127.0, 0.0).smoothing(0.0).featured(),
                )
                .param(
                    format!("high{n}"),
                    format!("High key {n}"),
                    ParamInfo::new(0.0, 127.0, 127.0).smoothing(0.0).featured(),
                )
                .param(
                    format!("vel_low{n}"),
                    format!("Low velocity {n}"),
                    ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0),
                )
                .param(
                    format!("vel_high{n}"),
                    format!("High velocity {n}"),
                    ParamInfo::new(0.0, 1.0, 1.0).smoothing(0.0),
                )
                .param(
                    format!("transpose{n}"),
                    format!("Transpose {n}"),
                    ParamInfo::new(-48.0, 48.0, 0.0)
                        .unit(Unit::Semitones)
                        .smoothing(0.0),
                );
        }
        for n in 1..=outputs {
            layout = layout.event_output(format!("out{n}"), format!("Notes {n}"));
        }
        if rest {
            layout = layout.event_output("rest", "Rest");
        }
        Ok(layout)
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let (outputs, rest) = Self::counts(setup.config)?;
        Ok(Instance::realtime(NoteSplitNode {
            outputs,
            rest,
            routes: [Route::default(); ROUTES],
            next: 0,
        }))
    }
}

/// Where one note went: a bit per output, bit `outputs` for the rest.
#[derive(Clone, Copy, Default)]
struct Route {
    note: u32,
    mask: u32,
}

/// The notes remembered at once. Past this the oldest are forgotten, and a
/// forgotten note's off goes nowhere.
const ROUTES: usize = 256;

struct NoteSplitNode {
    outputs: usize,
    rest: bool,
    routes: [Route; ROUTES],
    next: usize,
}

impl NoteSplitNode {
    fn remember(&mut self, note: NoteId, mask: u32) {
        self.routes[self.next] = Route { note: note.0, mask };
        self.next = (self.next + 1) % ROUTES;
    }

    /// Where a note went, if it is still remembered.
    fn lookup(&self, note: NoteId) -> u32 {
        // Newest first, so a reused ID finds its latest note.
        (0..ROUTES)
            .map(|k| &self.routes[(self.next + ROUTES - 1 - k) % ROUTES])
            .find(|r| r.mask != 0 && r.note == note.0)
            .map_or(0, |r| r.mask)
    }
}

impl Node for NoteSplitNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let value = |output: usize, which: usize| -> f32 {
            inputs[param_index(output, which)]
                .lane(0, 0)
                .first()
                .copied()
                .filter(|x| x.is_finite())
                .unwrap_or(0.0)
        };
        for event in event_inputs[0] {
            let mask = match event.kind {
                EventKind::NoteOn { key, velocity, .. } => {
                    let mut mask = 0u32;
                    for n in 0..self.outputs {
                        let key_in = f32::from(key) >= value(n, LOW).round()
                            && f32::from(key) <= value(n, HIGH).round();
                        let vel_in =
                            velocity >= value(n, VEL_LOW) && velocity <= value(n, VEL_HIGH);
                        if key_in && vel_in {
                            mask |= 1 << n;
                        }
                    }
                    if mask == 0 && self.rest {
                        mask = 1 << self.outputs;
                    }
                    mask
                }
                EventKind::NoteOff { note, .. } | EventKind::Expression { note, .. } => {
                    self.lookup(note)
                }
                EventKind::Midi(_) | EventKind::Channel { .. } => {
                    ((1 << self.outputs) - 1) | (u32::from(self.rest) << self.outputs)
                }
            };
            let mut sent = 0u32;
            #[allow(clippy::needless_range_loop)]
            for n in 0..self.outputs + usize::from(self.rest) {
                if mask & (1 << n) == 0 {
                    continue;
                }
                let kind = match event.kind {
                    EventKind::NoteOn {
                        note,
                        channel,
                        key,
                        velocity,
                    } if n < self.outputs => EventKind::NoteOn {
                        note,
                        channel,
                        key: shift(key, value(n, TRANSPOSE)),
                        velocity,
                    },
                    kind => kind,
                };
                let out = Event {
                    time: event.time,
                    kind,
                };
                if event_outputs[n].push(out).is_ok() {
                    sent |= 1 << n;
                }
            }
            if let EventKind::NoteOn { note, .. } = event.kind
                && sent != 0
            {
                self.remember(note, sent);
            }
        }
    }

    fn reset(&mut self) {
        self.routes = [Route::default(); ROUTES];
    }
}

fn shift(key: u8, semitones: f32) -> u8 {
    (f32::from(key) + semitones.round()).clamp(0.0, 127.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;

    fn on(time: u32, id: u32, key: u8, velocity: f32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 2,
                key,
                velocity,
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

    fn config(outputs: i64, rest: bool) -> Config {
        Config::new()
            .with("outputs", Value::Int(outputs))
            .with("rest", Value::Int(i64::from(rest)))
    }

    fn harness(outputs: i64, rest: bool) -> Harness {
        Harness::new(&NoteSplit, &config(outputs, rest), &[], 48_000.0, 64).unwrap()
    }

    fn set(h: &mut Harness, output: usize, which: usize, value: f32) {
        h.set(param_index(output, which), value);
    }

    fn keys(h: &Harness, port: usize) -> Vec<(u32, &'static str, u8)> {
        h.events(port)
            .iter()
            .map(|e| match e.kind {
                EventKind::NoteOn { key, .. } => (e.time, "on", key),
                EventKind::NoteOff { note, .. } => (e.time, "off", note.0 as u8),
                _ => (e.time, "other", 0),
            })
            .collect()
    }

    #[test]
    fn a_key_range_picks_the_notes_and_transposes_them() {
        let mut h = harness(2, true);
        set(&mut h, 0, LOW, 0.0);
        set(&mut h, 0, HIGH, 59.0);
        set(&mut h, 1, LOW, 60.0);
        set(&mut h, 1, TRANSPOSE, 12.0);
        h.send_events(0, &[on(1, 1, 40, 1.0), on(2, 2, 70, 1.0)]);
        h.run(64).unwrap();
        // outputs: out1, out2, rest
        assert_eq!(keys(&h, 0), [(1, "on", 40)]);
        assert_eq!(keys(&h, 1), [(2, "on", 82)]);
        assert!(h.events(2).is_empty());
    }

    #[test]
    fn what_matches_nothing_goes_to_rest() {
        let mut h = harness(1, true);
        set(&mut h, 0, LOW, 36.0);
        set(&mut h, 0, HIGH, 36.0);
        h.send_events(0, &[on(0, 1, 36, 1.0), on(1, 2, 37, 1.0)]);
        h.run(64).unwrap();
        assert_eq!(keys(&h, 0), [(0, "on", 36)]);
        assert_eq!(keys(&h, 1), [(1, "on", 37)]);
    }

    #[test]
    fn one_output_without_rest_is_a_filter() {
        let mut h = harness(1, false);
        set(&mut h, 0, LOW, 60.0);
        set(&mut h, 0, HIGH, 72.0);
        h.send_events(
            0,
            &[on(0, 1, 59, 1.0), on(1, 2, 65, 1.0), off(5, 1), off(6, 2)],
        );
        h.run(64).unwrap();
        assert_eq!(keys(&h, 0), [(1, "on", 65), (6, "off", 2)]);
    }

    #[test]
    fn velocity_ranges_layer_and_overlap() {
        let mut h = harness(2, false);
        set(&mut h, 0, VEL_HIGH, 0.6);
        set(&mut h, 1, VEL_LOW, 0.4);
        h.send_events(
            0,
            &[on(0, 1, 60, 0.2), on(1, 2, 60, 0.5), on(2, 3, 60, 0.9)],
        );
        h.run(64).unwrap();
        assert_eq!(keys(&h, 0), [(0, "on", 60), (1, "on", 60)]);
        assert_eq!(keys(&h, 1), [(1, "on", 60), (2, "on", 60)]);
    }

    #[test]
    fn a_note_off_follows_its_note_on_even_after_the_range_moves() {
        let mut h = harness(2, false);
        set(&mut h, 0, HIGH, 59.0);
        set(&mut h, 1, LOW, 60.0);
        h.send_events(0, &[on(0, 7, 40, 1.0)]);
        h.run(64).unwrap();
        // The ranges swap before the off.
        set(&mut h, 0, LOW, 100.0);
        set(&mut h, 0, HIGH, 127.0);
        h.send_events(0, &[off(3, 7)]);
        h.run(64).unwrap();
        assert_eq!(keys(&h, 0), [(3, "off", 7)]);
        assert!(h.events(1).is_empty());
    }

    #[test]
    fn other_events_reach_every_output() {
        let mut h = harness(2, true);
        let bend = Event {
            time: 4,
            kind: EventKind::Midi([0xE0, 0, 64]),
        };
        h.send_events(0, &[bend]);
        h.run(64).unwrap();
        for port in 0..3 {
            assert_eq!(h.events(port), [bend]);
        }
    }

    #[test]
    fn a_bad_output_count_is_an_error() {
        assert!(NoteSplit.layout(&config(0, false)).is_err());
        assert!(NoteSplit.layout(&config(17, false)).is_err());
    }
}
