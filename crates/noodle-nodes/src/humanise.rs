//! The Humanise node.

use noodle_engine::{
    Config, Context, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Ports, Setup, Unit,
};

use crate::event_delay::EventDelay;

/// Loosens the notes that pass through it: each note plays a little early
/// or late and a little harder or softer, by a random amount that is the
/// same every run.
///
/// - `timing` is the most, in milliseconds, a note moves either way (up to
///   [`MAX_TIMING_MS`]).
/// - `velocity` is the most a note's velocity moves, as a fraction of full
///   scale.
/// - `seed` picks the run of random numbers; another seed, another feel.
///
/// To play early as well as late, every note is held back by [`MAX_TIMING_MS`]
/// and then moved either way from there, so the node reports that as its
/// latency and the engine delays what runs in parallel to match. Note-offs
/// and expression follow their note-on. Automate `timing` over a stretch of
/// the song to humanise only those notes; the amount is read when each note
/// starts. For stored, per-note humanising of a clip, see the clip's
/// Humanise in the piano roll.
pub struct Humanise;

pub const HUMANISE_ID: &str = "noodle.event.humanise";

/// The most `timing` can ask for, and the delay the node always adds.
pub const MAX_TIMING_MS: f32 = 50.0;

#[derive(Ports)]
struct HumanisePorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param(
        "timing",
        "Timing",
        ParamInfo::new(0.0, MAX_TIMING_MS, 0.0)
            .unit(Unit::None)
            .smoothing(0.0)
    )]
    timing: (),
    #[param("velocity", "Velocity", ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0))]
    velocity: (),
    #[param("seed", "Seed", ParamInfo::new(0.0, 999.0, 0.0).smoothing(0.0))]
    seed: (),
    #[event_output("out", "Notes")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: HUMANISE_ID,
    version: 1,
    name: "Humanise",
    category: "Events",
};

impl NodeType for Humanise {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(HumanisePorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(HumaniseNode {
            delay: EventDelay::new(),
            hold: (MAX_TIMING_MS * 0.001 * setup.sample_rate) as u64,
            sample_rate: setup.sample_rate,
            random: 1,
            seeded_with: u32::MAX,
            base: setup.seed,
        }))
    }
}

struct HumaniseNode {
    delay: EventDelay,
    /// The delay every event gets, in samples.
    hold: u64,
    sample_rate: f32,
    random: u64,
    /// The `seed` parameter `random` was last started from.
    seeded_with: u32,
    /// The project seed, mixed with the parameter.
    base: u64,
}

impl HumaniseNode {
    /// A number from -1 to 1.
    fn next(&mut self) -> f32 {
        // xorshift64*
        self.random ^= self.random >> 12;
        self.random ^= self.random << 25;
        self.random ^= self.random >> 27;
        let bits = self.random.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        bits as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

impl Node for HumaniseNode {
    fn latency(&self) -> u32 {
        self.hold as u32
    }

    fn reset(&mut self) {
        self.delay.clear();
        // The next block starts the run of random numbers again.
        self.seeded_with = u32::MAX;
    }

    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let timing = first(HumanisePorts::TIMING).clamp(0.0, MAX_TIMING_MS);
        let velocity = first(HumanisePorts::VELOCITY).clamp(0.0, 1.0);
        let seed = first(HumanisePorts::SEED).round().clamp(0.0, 999.0) as u32;
        if seed != self.seeded_with {
            self.seeded_with = seed;
            self.random =
                (self.base ^ (u64::from(seed) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1;
        }
        let spread = f64::from(timing * 0.001 * self.sample_rate);
        for event in event_inputs[HumanisePorts::NOTES] {
            let mut kind = event.kind;
            let delay = match kind {
                EventKind::NoteOn {
                    note,
                    channel,
                    key,
                    velocity: v,
                } => {
                    let (t, dv) = (self.next(), self.next());
                    let moved = (v + dv * velocity * 0.5).clamp(0.01, 1.0);
                    kind = EventKind::NoteOn {
                        note,
                        channel,
                        key,
                        velocity: if v > 0.0 { moved } else { 0.0 },
                    };
                    let delay = (self.hold as f64 + f64::from(t) * spread).max(0.0) as u64;
                    self.delay.remember(note, delay);
                    delay
                }
                EventKind::NoteOff { note, .. } | EventKind::Expression { note, .. } => {
                    self.delay.delay_of(note)
                }
                _ => self.hold,
            };
            self.delay.push(event.time, delay, kind);
        }
        self.delay
            .drain(ctx.frames, &mut event_outputs[HumanisePorts::OUT]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{Event, NoteId};

    const RATE: f32 = 48_000.0;

    fn on(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key: 60,
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

    /// Plays notes 0.5 s apart (an off 1000 samples after each) and returns
    /// the events with absolute times.
    fn play(timing: f32, velocity: f32, seed: f32) -> Vec<(u64, EventKind)> {
        let mut h = Harness::new(&Humanise, &Config::new(), &[], RATE, 1000).unwrap();
        h.set(HumanisePorts::TIMING, timing);
        h.set(HumanisePorts::VELOCITY, velocity);
        h.set(HumanisePorts::SEED, seed);
        let mut out = Vec::new();
        for b in 0..120u64 {
            let mut events = Vec::new();
            if b % 24 == 0 {
                events.push(on(0, b as u32 / 24 + 1));
            }
            if b % 24 == 1 {
                events.push(off(0, b as u32 / 24 + 1));
            }
            h.send_events(HumanisePorts::NOTES, &events);
            h.run(1000).unwrap();
            for e in h.events(HumanisePorts::OUT) {
                out.push((b * 1000 + u64::from(e.time), e.kind));
            }
        }
        out
    }

    fn starts(out: &[(u64, EventKind)]) -> Vec<u64> {
        out.iter()
            .filter(|(_, k)| matches!(k, EventKind::NoteOn { .. }))
            .map(|(t, _)| *t)
            .collect()
    }

    #[test]
    fn it_reports_its_hold_as_latency_and_delays_by_exactly_that_at_zero() {
        let out = play(0.0, 0.0, 0.0);
        let hold = (MAX_TIMING_MS * 0.001 * RATE) as u64;
        assert_eq!(
            starts(&out),
            [
                hold,
                hold + 24_000,
                hold + 48_000,
                hold + 72_000,
                hold + 96_000
            ]
        );
    }

    #[test]
    fn timing_moves_notes_within_its_range_and_is_repeatable() {
        let hold = (MAX_TIMING_MS * 0.001 * RATE) as i64;
        let a = starts(&play(10.0, 0.0, 3.0));
        let b = starts(&play(10.0, 0.0, 3.0));
        let c = starts(&play(10.0, 0.0, 4.0));
        assert_eq!(a, b);
        assert_ne!(a, c);
        let spread = (10.0 * 0.001 * RATE) as i64;
        let mut moved = false;
        for (i, t) in a.iter().enumerate() {
            let off = *t as i64 - hold - 24_000 * i as i64;
            assert!(off.abs() <= spread + 1, "{off}");
            moved |= off != 0;
        }
        assert!(moved);
    }

    #[test]
    fn velocity_moves_within_its_range_and_offs_follow_their_notes() {
        let out = play(10.0, 0.4, 1.0);
        let mut changed = false;
        for (_, k) in &out {
            if let EventKind::NoteOn { velocity, .. } = k {
                assert!((0.3..=0.7).contains(velocity), "{velocity}");
                changed |= (*velocity - 0.5).abs() > 1e-3;
            }
        }
        assert!(changed);
        let ons = starts(&out);
        let offs: Vec<u64> = out
            .iter()
            .filter(|(_, k)| matches!(k, EventKind::NoteOff { .. }))
            .map(|(t, _)| *t)
            .collect();
        for (on, off) in ons.iter().zip(&offs) {
            assert_eq!(off - on, 1000);
        }
    }
}
