//! The Euclidean node.

use noodle_engine::{
    Config, Context, Event, EventKind, EventsOut, Instance, Io, Layout, Node, NodeError, NodeInfo,
    NodeType, NoteId, ParamInfo, Ports, Setup, Unit,
};

use crate::clock::{DIVISION_TICKS, DIVISIONS};

/// A tempo-synced rhythm: `pulses` hits spread as evenly as they can go over
/// `steps` steps (Bjorklund's algorithm, the "Euclidean rhythm": 3 in 8 is
/// `x..x..x.`, 5 in 8 is `x.xx.xx.`), one note per hit.
///
/// - `steps` (1 to 32) and `pulses` (0 to `steps`) make the pattern.
/// - `rotation` moves it later by that many steps, wrapping round.
/// - `rate` is the length of a step, a division of the whole note as Clock's.
/// - `gate` is how long each note lasts as a fraction of the step.
/// - `key` and `velocity` are what plays; `accent` is how much louder a hit
///   on the first step of the cycle is.
///
/// Steps sit on the transport's tick grid, counted from the start of the
/// timeline, so the pattern stays on the beat through seeks, tempo changes
/// and loops and agrees with an Arpeggiator or Clock on the same rate. It is
/// silent while the transport is stopped. A note ends at the time it was
/// given when it began, so changing `pulses`, `steps` or `gate` while it
/// sounds never strands it.
pub struct Euclidean;

pub const EUCLIDEAN_ID: &str = "noodle.event.euclidean";

#[derive(Ports)]
struct EuclideanPorts {
    #[param(
        "steps",
        "Steps",
        ParamInfo::new(1.0, 32.0, 16.0).smoothing(0.0).featured()
    )]
    steps: (),
    #[param(
        "pulses",
        "Pulses",
        ParamInfo::new(0.0, 32.0, 5.0).smoothing(0.0).featured()
    )]
    pulses: (),
    #[param(
        "rotation",
        "Rotation",
        ParamInfo::new(0.0, 31.0, 0.0).smoothing(0.0)
    )]
    rotation: (),
    #[param(
        "rate",
        "Rate",
        ParamInfo {
            default: 4.0,
            featured: true,
            ..ParamInfo::choice(DIVISIONS)
        }
    )]
    rate: (),
    #[param("gate", "Gate", ParamInfo::new(0.05, 1.0, 0.5).unit(Unit::None))]
    gate: (),
    #[param(
        "key",
        "Key",
        ParamInfo::new(0.0, 127.0, 60.0)
            .unit(Unit::Note)
            .smoothing(0.0)
    )]
    key: (),
    #[param("velocity", "Velocity", ParamInfo::new(0.0, 1.0, 0.8))]
    velocity: (),
    #[param("accent", "Accent", ParamInfo::new(0.0, 1.0, 0.0))]
    accent: (),
    #[event_output("out", "Notes")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: EUCLIDEAN_ID,
    version: 1,
    name: "Euclidean",
    category: "Events",
};

impl NodeType for Euclidean {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(EuclideanPorts::layout())
    }

    fn pulses_on_beat(&self) -> bool {
        true
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(EuclideanNode::new(setup.sample_rate)))
    }
}

/// The pattern of `pulses` hits in `steps` steps, as bits (bit `i` is step
/// `i`), by Bjorklund's algorithm: groups of hits and rests are paired up
/// until at most one rest group is left over.
pub(crate) fn bjorklund(steps: u32, pulses: u32) -> u64 {
    let steps = steps.clamp(1, 32);
    let pulses = pulses.min(steps);
    // A group is its bits (first step lowest) and its length.
    #[derive(Clone, Copy)]
    struct Group {
        bits: u64,
        len: u32,
    }
    // Hit `a` then `b`.
    let join = |a: Group, b: Group| Group {
        bits: a.bits | b.bits << a.len,
        len: a.len + b.len,
    };
    let (mut a, mut count_a) = (Group { bits: 1, len: 1 }, pulses);
    let (mut b, mut count_b) = (Group { bits: 0, len: 1 }, steps - pulses);
    while count_b > 1 && count_a > 0 {
        if count_a >= count_b {
            let pair = join(a, b);
            (a, b) = (pair, a);
            (count_a, count_b) = (count_b, count_a - count_b);
        } else {
            let pair = join(a, b);
            a = pair;
            count_b -= count_a;
        }
    }
    let mut bits = 0u64;
    let mut len = 0;
    for _ in 0..count_a {
        bits |= a.bits << len;
        len += a.len;
    }
    for _ in 0..count_b {
        bits |= b.bits << len;
        len += b.len;
    }
    debug_assert_eq!(len, steps);
    bits
}

/// Notes that can sound at once: a gate of 1 ends each as the next begins.
const SOUNDING: usize = 4;

#[derive(Clone, Copy, Default)]
struct Sounding {
    active: bool,
    note: u32,
    /// The sample (counting from the start) at which it ends.
    off_at: u64,
}

struct EuclideanNode {
    sample_rate: f32,
    sounding: [Sounding; SOUNDING],
    next_id: u32,
    /// The grid step that fires next.
    step: i64,
    /// Where the block should start if time runs on without a jump.
    expected: Option<f64>,
    /// Samples since the start, to date note-offs that fall in later blocks.
    now: u64,
}

impl EuclideanNode {
    fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            sounding: [Sounding::default(); SOUNDING],
            next_id: 0,
            step: 0,
            expected: None,
            now: 0,
        }
    }

    /// Ends the notes due at or before `at` (samples since the start), the
    /// earliest first, `time` being the block offset to write them at.
    fn end_due(&mut self, at: u64, frames: usize, out: &mut EventsOut<'_>) {
        while let Some(s) = self
            .sounding
            .iter_mut()
            .filter(|s| s.active && s.off_at <= at)
            .min_by_key(|s| s.off_at)
        {
            let time = s.off_at.saturating_sub(self.now) as usize;
            let _ = out.push(Event {
                time: time.min(frames.saturating_sub(1)) as u32,
                kind: EventKind::NoteOff {
                    note: NoteId(s.note),
                    velocity: 0.0,
                },
            });
            s.active = false;
        }
    }
}

impl Node for EuclideanNode {
    fn reset(&mut self) {
        *self = Self::new(self.sample_rate);
    }

    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_outputs,
            ..
        } = io;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let steps = first(EuclideanPorts::STEPS).round().clamp(1.0, 32.0) as u32;
        let pulses = first(EuclideanPorts::PULSES).round().clamp(0.0, 32.0) as u32;
        let rotation = first(EuclideanPorts::ROTATION).round().clamp(0.0, 31.0) as u32;
        let rate = first(EuclideanPorts::RATE)
            .round()
            .clamp(0.0, (DIVISIONS.len() - 1) as f32) as usize;
        let gate = f64::from(first(EuclideanPorts::GATE).clamp(0.05, 1.0));
        let key = first(EuclideanPorts::KEY).round().clamp(0.0, 127.0) as u8;
        let velocity = first(EuclideanPorts::VELOCITY).clamp(0.0, 1.0);
        let accent = first(EuclideanPorts::ACCENT).clamp(0.0, 1.0);
        let out = &mut event_outputs[EuclideanPorts::OUT];

        let frames = ctx.frames;
        let ticks_per_sample = ctx.transport.bpm * 960.0 / 60.0 / f64::from(self.sample_rate);
        let start_tick = ctx.transport.tick;
        let end_tick = start_tick + frames as f64 * ticks_per_sample;
        let step_ticks = DIVISION_TICKS[rate];
        let pattern = bjorklund(steps, pulses);

        if !ctx.transport.playing {
            // Silent, but a note that sounds still ends when it was meant to.
            self.expected = None;
            self.end_due(self.now + frames as u64, frames, out);
            self.now += frames as u64;
            return;
        }
        // A jump of the playhead puts the pattern on the new grid.
        if self.expected.is_none_or(|e| (e - start_tick).abs() > 1.0) {
            self.step = (start_tick / step_ticks - 0.01).ceil() as i64;
            while (self.step - 1) as f64 * step_ticks >= start_tick {
                self.step -= 1;
            }
        }
        self.expected = Some(end_tick);

        loop {
            let tick = self.step as f64 * step_ticks;
            if tick >= end_tick {
                break;
            }
            let time = (((tick - start_tick) / ticks_per_sample).max(0.0) as usize)
                .min(frames.saturating_sub(1));
            let at = self.now + time as u64;
            self.end_due(at, frames, out);
            let k = self.step.rem_euclid(i64::from(steps)) as u32;
            let index = (k + steps - rotation % steps) % steps;
            if pattern >> index & 1 == 1 {
                let id = NoteId(self.next_id);
                self.next_id = self.next_id.wrapping_add(1);
                let length = (step_ticks * gate / ticks_per_sample).max(1.0) as u64;
                let slot = self.sounding.iter().position(|s| !s.active).unwrap_or(0);
                if self.sounding[slot].active {
                    // Out of room: end the oldest first.
                    let _ = out.push(Event {
                        time: time as u32,
                        kind: EventKind::NoteOff {
                            note: NoteId(self.sounding[slot].note),
                            velocity: 0.0,
                        },
                    });
                }
                self.sounding[slot] = Sounding {
                    active: true,
                    note: id.0,
                    off_at: at + length,
                };
                let loud = if k == 0 { velocity + accent } else { velocity };
                let _ = out.push(Event {
                    time: time as u32,
                    kind: EventKind::NoteOn {
                        note: id,
                        channel: 0,
                        key,
                        velocity: loud.clamp(0.01, 1.0),
                    },
                });
            }
            self.step += 1;
        }
        self.end_due(self.now + frames as u64 - 1, frames, out);
        self.now += frames as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;
    /// A sixteenth at 120 bpm, in samples.
    const STEP: u64 = 6_000;

    fn pattern(steps: u32, pulses: u32) -> String {
        let bits = bjorklund(steps, pulses);
        (0..steps)
            .map(|i| if bits >> i & 1 == 1 { 'x' } else { '.' })
            .collect()
    }

    #[test]
    fn bjorklund_makes_the_known_rhythms() {
        for (steps, pulses, want) in [
            (8, 3, "x..x..x."),
            (8, 5, "x.xx.xx."),
            (7, 4, "x.x.x.x"),
            (5, 2, "x.x.."),
            (4, 3, "xxx."),
            (12, 5, "x..x.x..x.x."),
            (12, 7, "x.xx.x.xx.x."),
            (16, 5, "x..x..x..x..x..."),
            (8, 0, "........"),
            (8, 8, "xxxxxxxx"),
            (1, 1, "x"),
            (9, 4, "x.x.x.x.."),
        ] {
            let got = pattern(steps, pulses);
            assert_eq!(got.len(), steps as usize);
            assert_eq!(got.matches('x').count(), pulses as usize);
            // Rhythms are the same up to rotation, but Bjorklund's start on
            // a hit and these are the standard spellings.
            assert_eq!(got, want, "E({pulses},{steps})");
        }
    }

    /// Runs `blocks` blocks of 1000 frames and returns every event with its
    /// absolute time.
    fn play(h: &mut Harness, blocks: u64) -> Vec<(u64, EventKind)> {
        let mut out = Vec::new();
        let start = h.position();
        for b in 0..blocks {
            h.run(1000).unwrap();
            for e in h.events(EuclideanPorts::OUT) {
                out.push((start + b * 1000 + u64::from(e.time), e.kind));
            }
        }
        out
    }

    fn rig(steps: f32, pulses: f32, rotation: f32) -> Harness {
        let mut h = Harness::new(&Euclidean, &Config::new(), &[], RATE, 1000).unwrap();
        h.set_playing(true);
        h.set(EuclideanPorts::STEPS, steps);
        h.set(EuclideanPorts::PULSES, pulses);
        h.set(EuclideanPorts::ROTATION, rotation);
        h
    }

    fn starts(out: &[(u64, EventKind)]) -> Vec<u64> {
        out.iter()
            .filter(|(_, k)| matches!(k, EventKind::NoteOn { .. }))
            .map(|(t, _)| *t)
            .collect()
    }

    #[test]
    fn hits_sit_on_the_grid_where_the_pattern_says() {
        // E(3,8) at sixteenths: steps 0, 3 and 6 of every 8.
        let mut h = rig(8.0, 3.0, 0.0);
        let out = play(&mut h, 100);
        let want: Vec<u64> = [0, 3, 6, 8, 11, 14, 16]
            .iter()
            .map(|k| k * STEP)
            .filter(|&t| t < 100_000)
            .collect();
        assert_eq!(starts(&out), want);
    }

    #[test]
    fn rotation_moves_the_pattern_later() {
        let mut h = rig(8.0, 3.0, 1.0);
        let out = play(&mut h, 60);
        // x..x..x. moved later by one step: .x..x..x
        assert_eq!(starts(&out), [STEP, 4 * STEP, 7 * STEP, 9 * STEP]);
    }

    #[test]
    fn notes_end_after_their_gate_and_a_change_strands_nothing() {
        let mut h = rig(8.0, 8.0, 0.0);
        h.set(EuclideanPorts::GATE, 0.5);
        // Notes at 0 and 6000; the second sounds when everything changes.
        let mut out = play(&mut h, 7);
        h.set(EuclideanPorts::PULSES, 0.0);
        h.set(EuclideanPorts::GATE, 1.0);
        out.extend(play(&mut h, 10));
        let ons: Vec<u32> = out
            .iter()
            .filter_map(|(_, k)| match k {
                EventKind::NoteOn { note, .. } => Some(note.0),
                _ => None,
            })
            .collect();
        let offs: Vec<(u64, u32)> = out
            .iter()
            .filter_map(|(t, k)| match k {
                EventKind::NoteOff { note, .. } => Some((*t, note.0)),
                _ => None,
            })
            .collect();
        assert_eq!(ons, [0, 1]);
        // The second ends half a step after it began, as its gate was then.
        assert_eq!(offs, [(3000, 0), (STEP + 3000, 1)]);
    }

    #[test]
    fn it_is_silent_while_stopped_and_picks_up_on_the_grid() {
        let mut h = rig(8.0, 8.0, 0.0);
        h.set_playing(false);
        assert!(play(&mut h, 20).is_empty());
        h.set_playing(true);
        let out = play(&mut h, 20);
        // 20000 samples in: the next grid step is 4 at 24000.
        assert_eq!(starts(&out)[0], 4 * STEP);
    }

    #[test]
    fn velocity_key_and_accent_apply() {
        let mut h = rig(4.0, 4.0, 0.0);
        h.set(EuclideanPorts::KEY, 36.0);
        h.set(EuclideanPorts::VELOCITY, 0.5);
        h.set(EuclideanPorts::ACCENT, 0.3);
        let out = play(&mut h, 30);
        let ons: Vec<(u8, f32)> = out
            .iter()
            .filter_map(|(_, k)| match k {
                EventKind::NoteOn { key, velocity, .. } => Some((*key, *velocity)),
                _ => None,
            })
            .collect();
        assert_eq!(ons[0], (36, 0.8));
        assert_eq!(ons[1], (36, 0.5));
    }
}
