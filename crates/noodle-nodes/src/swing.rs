//! The Swing node.

use noodle_engine::{
    Config, Context, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Ports, Setup,
};

use crate::event_delay::EventDelay;

/// Delays the notes that fall on the off-beats of a grid, so a straight
/// pattern shuffles. It sits on the note wire between the track and the
/// instrument, and works on drawn, recorded or arpeggiated notes alike.
///
/// - `amount` is how late the off-beat notes play, as a fraction of a grid
///   step (the Arpeggiator's `swing`): 0 is straight, a third is a triplet
///   feel, and the most is three quarters.
/// - `grid` is the step: eighths, sixteenths or thirty-seconds.
///
/// A note whose start lies within a quarter of a step of an odd-numbered
/// step is delayed; the others pass on time. Its note-off and expression move
/// by the same amount, so lengths are kept. It reads the transport, so while
/// the transport is stopped (live playing) notes pass through unchanged. At
/// an amount of 0 every event passes on time.
pub struct Swing;

pub const SWING_ID: &str = "noodle.event.swing";

#[derive(Ports)]
struct SwingPorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param("amount", "Amount", ParamInfo::new(0.0, 0.75, 0.0))]
    amount: (),
    #[param("grid", "Grid", ParamInfo::choice(GRID_NAMES).default_to(1.0))]
    grid: (),
    #[event_output("out", "Notes")]
    out: (),
}

pub const GRID_NAMES: [&str; 3] = ["1/8", "1/16", "1/32"];
/// The length of each `grid` in ticks (960 to a quarter note).
pub const GRID_TICKS: [f64; 3] = [480.0, 240.0, 120.0];

static INFO: NodeInfo = NodeInfo {
    id: SWING_ID,
    version: 1,
    name: "Swing",
    category: "Events",
};

impl NodeType for Swing {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(SwingPorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(SwingNode {
            delay: EventDelay::new(),
        }))
    }
}

struct SwingNode {
    delay: EventDelay,
}

impl Node for SwingNode {
    fn reset(&mut self) {
        self.delay.clear();
    }

    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let amount = f64::from(first(SwingPorts::AMOUNT).clamp(0.0, 0.75));
        let grid = first(SwingPorts::GRID).round().clamp(0.0, 2.0) as usize;
        let step = GRID_TICKS[grid];
        let ticks_per_sample =
            ctx.transport.bpm * 960.0 / 60.0 / f64::from(ctx.sample_rate).max(1.0);
        let swinging = ctx.transport.playing && amount > 0.0;
        let late = (amount * step / ticks_per_sample) as u64;

        for event in event_inputs[SwingPorts::NOTES] {
            let delay = match event.kind {
                EventKind::NoteOn { note, .. } => {
                    let tick = ctx.transport.tick + f64::from(event.time) * ticks_per_sample;
                    let k = (tick / step).round();
                    let on_odd = k.rem_euclid(2.0) == 1.0 && (tick - k * step).abs() <= step * 0.25;
                    let delay = if swinging && on_odd { late } else { 0 };
                    self.delay.remember(note, delay);
                    delay
                }
                EventKind::NoteOff { note, .. } | EventKind::Expression { note, .. } => {
                    self.delay.delay_of(note, 0)
                }
                _ => 0,
            };
            self.delay.push(event.time, delay, event.kind);
        }
        self.delay
            .drain(ctx.frames, &mut event_outputs[SwingPorts::OUT]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{Event, NoteId};

    const RATE: f32 = 48_000.0;
    // At 120 bpm one sample is 0.04 ticks, so a 1/16 (240 ticks) is 6000
    // samples.
    const SIXTEENTH: u64 = 6000;

    fn on(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key: 60,
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

    /// Runs the harness over `blocks` blocks of 1000 frames with `input` at
    /// absolute sample times, and returns each output event's absolute time.
    fn play(amount: f32, input: &[(u64, Event)], blocks: u64) -> Vec<(u64, EventKind)> {
        let mut h = Harness::new(&Swing, &Config::new(), &[], RATE, 1000).unwrap();
        h.set(SwingPorts::AMOUNT, amount);
        let mut out = Vec::new();
        for b in 0..blocks {
            let events: Vec<Event> = input
                .iter()
                .filter(|(t, _)| *t / 1000 == b)
                .map(|(t, e)| Event {
                    time: (*t % 1000) as u32,
                    ..*e
                })
                .collect();
            h.send_events(SwingPorts::NOTES, &events);
            h.run(1000).unwrap();
            for e in h.events(SwingPorts::OUT) {
                out.push((b * 1000 + u64::from(e.time), e.kind));
            }
        }
        out
    }

    fn times(out: &[(u64, EventKind)]) -> Vec<u64> {
        out.iter().map(|(t, _)| *t).collect()
    }

    #[test]
    fn off_beats_play_late_and_the_rest_on_time() {
        let input = [
            (0, on(0, 1)),
            (SIXTEENTH, on(0, 2)),
            (2 * SIXTEENTH, on(0, 3)),
        ];
        let out = play(0.5, &input, 20);
        assert_eq!(times(&out), [0, SIXTEENTH + SIXTEENTH / 2, 2 * SIXTEENTH]);
    }

    #[test]
    fn a_note_off_moves_with_its_note() {
        let input = [(SIXTEENTH, on(0, 2)), (SIXTEENTH + 1000, off(0, 2))];
        let out = play(0.5, &input, 20);
        assert_eq!(times(&out), [9000, 10000]);
    }

    #[test]
    fn zero_amount_is_straight() {
        let input = [(SIXTEENTH, on(0, 2)), (SIXTEENTH + 1000, off(0, 2))];
        let out = play(0.0, &input, 20);
        assert_eq!(times(&out), [6000, 7000]);
    }

    #[test]
    fn notes_off_the_grid_are_left_alone() {
        let input = [(SIXTEENTH + 2000, on(0, 5))];
        let out = play(0.5, &input, 20);
        assert_eq!(times(&out), [SIXTEENTH + 2000]);
    }
}
