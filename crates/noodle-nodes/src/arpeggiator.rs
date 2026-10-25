//! The Arpeggiator node.

use noodle_engine::{
    Config, Context, Event, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    NoteId, ParamInfo, Ports, Setup, Unit,
};

/// Plays the notes held on its input one at a time, in time with the tempo.
///
/// - `pattern`: *Up* and *Down* walk the held notes by pitch; *Up-down* and
///   *Down-up* turn round at the ends without repeating them; *As played*
///   keeps the order the keys went down in; *Random* picks one each step, the
///   same picks every run.
/// - `rate` is the length of a step, as a note value: quarter, eighth,
///   eighth triplet, sixteenth, sixteenth triplet or thirty-second.
/// - `octaves` repeats the pattern that many octaves up.
/// - `gate` is how long each note lasts as a fraction of the step.
/// - `swing` delays every second step by that fraction of a step.
/// - `hold` keeps the pattern going after the keys are released, until the
///   next key goes down, which starts a new pattern.
///
/// The steps sit on the tempo's grid (the transport's ticks), so the arpeggio
/// stays on the beat however the notes were played. While the transport is
/// stopped it free-runs from where it left off. Each step's note has the
/// velocity of the key it came from. Controller and other raw MIDI events pass
/// through; per-note expression is dropped, since the notes it named are not
/// the ones that come out.
pub struct Arpeggiator;

pub const ARPEGGIATOR_ID: &str = "noodle.event.arpeggiator";

#[derive(Ports)]
struct ArpPorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param(
        "pattern",
        "Pattern",
        ParamInfo::choice([
            "Up",
            "Down",
            "Up-down",
            "Down-up",
            "As played",
            "Random"
        ])
    )]
    pattern: (),
    #[param("rate", "Rate", ParamInfo::choice(RATE_NAMES).default_to(3.0))]
    rate: (),
    #[param("octaves", "Octaves", ParamInfo::choice(["1", "2", "3", "4"]))]
    octaves: (),
    #[param("gate", "Gate", ParamInfo::new(0.05, 1.0, 0.6).unit(Unit::None))]
    gate: (),
    #[param("swing", "Swing", ParamInfo::new(0.0, 0.75, 0.0))]
    swing: (),
    #[param("hold", "Hold", ParamInfo::choice(["Off", "On"]))]
    hold: (),
    #[event_output("out", "Notes")]
    out: (),
}

pub const RATE_NAMES: [&str; 6] = ["1/4", "1/8", "1/8T", "1/16", "1/16T", "1/32"];

/// The length of each `rate` in ticks (960 to a quarter note).
pub const RATE_TICKS: [f64; 6] = [960.0, 480.0, 320.0, 240.0, 160.0, 120.0];

static INFO: NodeInfo = NodeInfo {
    id: ARPEGGIATOR_ID,
    version: 1,
    name: "Arpeggiator",
    category: "Events",
};

impl NodeType for Arpeggiator {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ArpPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ArpNode {
            sample_rate: setup.sample_rate,
            held: [Held::default(); MAX_HELD],
            held_count: 0,
            sounding: [Sounding::default(); SOUNDING],
            next_id: 0,
            step: 0,
            counter: 0,
            expected: None,
            free_tick: 0.0,
            now: 0,
            random: setup.seed | 1,
        }))
    }
}

const MAX_HELD: usize = 32;
/// Notes that can be sounding at once: one per step, and a gate of 1 ends each
/// as the next begins, so a couple are enough.
const SOUNDING: usize = 4;

#[derive(Clone, Copy, Default)]
struct Held {
    id: u32,
    key: u8,
    velocity: f32,
    /// The key is still down (it is kept after its release only by hold).
    down: bool,
}

#[derive(Clone, Copy, Default)]
struct Sounding {
    active: bool,
    note: u32,
    /// The sample (counting from the start) at which it ends.
    off_at: u64,
}

struct ArpNode {
    sample_rate: f32,
    held: [Held; MAX_HELD],
    held_count: usize,
    sounding: [Sounding; SOUNDING],
    next_id: u32,
    /// The grid step that fires next.
    step: i64,
    /// How many notes have played since the pattern began, to pick the next.
    counter: usize,
    /// Where the block should start if time runs on without a jump.
    expected: Option<f64>,
    free_tick: f64,
    /// Samples since the start, to date note-offs that fall in later blocks.
    now: u64,
    random: u64,
}

impl ArpNode {
    fn press(&mut self, id: NoteId, key: u8, velocity: f32, hold: bool) {
        // With hold, the first key after all were let go starts afresh.
        if hold && !self.any_down() {
            self.held_count = 0;
        }
        if self.held_count == MAX_HELD {
            return;
        }
        self.held[self.held_count] = Held {
            id: id.0,
            key,
            velocity,
            down: true,
        };
        self.held_count += 1;
    }

    fn release(&mut self, id: NoteId, hold: bool) {
        let Some(i) = self.held[..self.held_count]
            .iter()
            .position(|h| h.id == id.0)
        else {
            return;
        };
        if hold {
            self.held[i].down = false;
        } else {
            self.held.copy_within(i + 1..self.held_count, i);
            self.held_count -= 1;
        }
    }

    fn any_down(&self) -> bool {
        self.held[..self.held_count].iter().any(|h| h.down)
    }

    /// Ends the arpeggio when nothing is left to play: no keys, or latched
    /// ones with hold turned off.
    fn stop_if_empty(&mut self, out: &mut noodle_engine::EventsOut<'_>, time: usize, hold: bool) {
        if !hold && !self.any_down() {
            self.held_count = 0;
        }
        if self.held_count > 0 {
            return;
        }
        for s in self.sounding.iter_mut().filter(|s| s.active) {
            let _ = out.push(Event {
                time: time as u32,
                kind: EventKind::NoteOff {
                    note: NoteId(s.note),
                    velocity: 0.0,
                },
            });
            s.active = false;
        }
        self.counter = 0;
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64*
        self.random ^= self.random >> 12;
        self.random ^= self.random << 25;
        self.random ^= self.random >> 27;
        self.random.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33
    }

    /// The key and velocity of the `counter`th note of the pattern.
    fn pick(&mut self, pattern: usize, octaves: usize) -> Option<(u8, f32)> {
        let m = self.held_count;
        if m == 0 {
            return None;
        }
        // The held notes in the order the pattern walks them.
        let mut order = [Held::default(); MAX_HELD];
        order[..m].copy_from_slice(&self.held[..m]);
        if pattern != 4 {
            order[..m].sort_by_key(|h| h.key);
        }
        let n = m * octaves;
        let note = |i: usize| {
            let h = order[i % m];
            let key = (usize::from(h.key) + 12 * (i / m)).min(127) as u8;
            (key, h.velocity)
        };
        let c = self.counter;
        self.counter = self.counter.wrapping_add(1);
        let index = match pattern {
            0 | 4 => c % n,
            1 => n - 1 - c % n,
            // Up then back down, without playing the top or bottom twice.
            2 | 3 => {
                let cycle = (2 * n).saturating_sub(2).max(1);
                let i = c % cycle;
                let up = if i < n { i } else { cycle - i };
                if pattern == 2 { up } else { n - 1 - up }
            }
            _ => (self.next_random() as usize) % n,
        };
        Some(note(index))
    }
}

impl Node for ArpNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            event_outputs,
            ..
        } = io;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let pattern = first(ArpPorts::PATTERN).round().clamp(0.0, 5.0) as usize;
        let rate = first(ArpPorts::RATE).round().clamp(0.0, 5.0) as usize;
        let octaves = first(ArpPorts::OCTAVES).round().clamp(0.0, 3.0) as usize + 1;
        let gate = f64::from(first(ArpPorts::GATE).clamp(0.05, 1.0));
        let swing = f64::from(first(ArpPorts::SWING).clamp(0.0, 0.75));
        let hold = first(ArpPorts::HOLD) >= 0.5;
        let out = &mut event_outputs[ArpPorts::OUT];

        let frames = ctx.frames;
        let ticks_per_sample = ctx.transport.bpm * 960.0 / 60.0 / f64::from(self.sample_rate);
        let start_tick = if ctx.transport.playing {
            ctx.transport.tick
        } else {
            self.free_tick
        };
        let end_tick = start_tick + frames as f64 * ticks_per_sample;
        let step_ticks = RATE_TICKS[rate];
        // The grid step k falls at k·step, every other one swung later.
        let tick_of = |k: i64| {
            let late = if k.rem_euclid(2) == 1 {
                swing * step_ticks
            } else {
                0.0
            };
            k as f64 * step_ticks + late
        };
        // A jump of the playhead restarts the pattern on the new grid.
        if self.expected.is_none_or(|e| (e - start_tick).abs() > 1.0) {
            self.step = (start_tick / step_ticks - 0.01).ceil() as i64;
            while tick_of(self.step - 1) >= start_tick {
                self.step -= 1;
            }
            self.counter = 0;
        }
        self.expected = Some(end_tick);
        self.free_tick = end_tick;

        let sample_of = |tick: f64| ((tick - start_tick) / ticks_per_sample).max(0.0) as usize;
        let push = |out: &mut noodle_engine::EventsOut<'_>, time: usize, kind| {
            let _ = out.push(Event {
                time: time.min(frames.saturating_sub(1)) as u32,
                kind,
            });
        };

        self.stop_if_empty(out, 0, hold);

        // Input events and the arpeggio's own, in time order: the input's
        // changes apply at their time, between steps.
        let mut input = event_inputs[ArpPorts::NOTES].iter().peekable();
        let mut apply_until =
            |me: &mut Self, out: &mut noodle_engine::EventsOut<'_>, time: usize| {
                while let Some(event) = input.next_if(|e| e.time as usize <= time) {
                    match event.kind {
                        EventKind::NoteOn {
                            note,
                            key,
                            velocity,
                            ..
                        } => me.press(note, key, velocity, hold),
                        EventKind::NoteOff { note, .. } => {
                            me.release(note, hold);
                            me.stop_if_empty(out, event.time as usize, hold);
                        }
                        EventKind::Midi(_) | EventKind::Channel { .. } => {
                            push(out, event.time as usize, event.kind)
                        }
                        EventKind::Expression { .. } => {}
                    }
                }
            };

        loop {
            let tick = tick_of(self.step);
            let step_at = (tick < end_tick).then(|| sample_of(tick));
            // Note-offs that fall before the next step, or in the block.
            let next_off = self
                .sounding
                .iter()
                .filter(|s| s.active)
                .map(|s| s.off_at)
                .min();
            let off_in_block = next_off
                .filter(|&at| at < self.now + frames as u64)
                .map(|at| at.saturating_sub(self.now) as usize);
            let (time, is_step) = match (step_at, off_in_block) {
                (Some(s), Some(o)) if o <= s => (o, false),
                (Some(s), _) => (s, true),
                (None, Some(o)) => (o, false),
                (None, None) => break,
            };
            apply_until(self, out, time);
            if is_step {
                let note = self.pick(pattern, octaves);
                if let Some((key, velocity)) = note {
                    let id = NoteId(self.next_id);
                    self.next_id = self.next_id.wrapping_add(1);
                    let length = (step_ticks * gate / ticks_per_sample).max(1.0) as u64;
                    let slot = self.sounding.iter().position(|s| !s.active).unwrap_or(0);
                    if self.sounding[slot].active {
                        // Out of room: end the oldest first.
                        push(
                            out,
                            time,
                            EventKind::NoteOff {
                                note: NoteId(self.sounding[slot].note),
                                velocity: 0.0,
                            },
                        );
                    }
                    self.sounding[slot] = Sounding {
                        active: true,
                        note: id.0,
                        off_at: self.now + time as u64 + length,
                    };
                    push(
                        out,
                        time,
                        EventKind::NoteOn {
                            note: id,
                            channel: 0,
                            key,
                            velocity,
                        },
                    );
                }
                self.step += 1;
            } else {
                let at = self.now + time as u64;
                for s in self
                    .sounding
                    .iter_mut()
                    .filter(|s| s.active && s.off_at <= at)
                {
                    push(
                        out,
                        time,
                        EventKind::NoteOff {
                            note: NoteId(s.note),
                            velocity: 0.0,
                        },
                    );
                    s.active = false;
                }
            }
        }
        apply_until(self, out, frames);
        self.now += frames as u64;
    }

    fn reset(&mut self) {
        self.held_count = 0;
        self.sounding = [Sounding::default(); SOUNDING];
        self.expected = None;
        self.counter = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;
    /// A sixteenth note at 120 beats per minute, in samples.
    const STEP: u32 = 6_000;

    fn on(time: u32, id: u32, key: u8) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity: 0.8,
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

    fn harness(frames: usize) -> Harness {
        let mut h = Harness::new(&Arpeggiator, &Config::new(), &[], RATE, frames).unwrap();
        h.set(ArpPorts::RATE, 3.0);
        h
    }

    /// (time, key) of each note-on in the last block.
    fn played(h: &Harness) -> Vec<(u32, u8)> {
        h.events(ArpPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOn { key, .. } => Some((e.time, key)),
                _ => None,
            })
            .collect()
    }

    fn chord(h: &mut Harness) {
        h.send_events(0, &[on(0, 1, 64), on(0, 2, 60), on(0, 3, 67)]);
    }

    fn keys(h: &mut Harness, pattern: f32, octaves: f32, steps: u32) -> Vec<u8> {
        h.set(ArpPorts::PATTERN, pattern);
        h.set(ArpPorts::OCTAVES, octaves);
        chord(h);
        h.run((STEP * steps) as usize).unwrap();
        played(h).iter().map(|&(_, k)| k).collect()
    }

    #[test]
    fn up_walks_the_chord_on_the_grid_and_wraps() {
        let mut h = harness(24_000);
        chord(&mut h);
        h.run(24_000).unwrap();
        assert_eq!(
            played(&h),
            [(0, 60), (STEP, 64), (2 * STEP, 67), (3 * STEP, 60)]
        );
    }

    #[test]
    fn patterns_order_the_notes() {
        assert_eq!(
            keys(&mut harness(48_000), 1.0, 0.0, 6),
            [67, 64, 60, 67, 64, 60]
        );
        assert_eq!(
            keys(&mut harness(48_000), 2.0, 0.0, 6),
            [60, 64, 67, 64, 60, 64]
        );
        assert_eq!(
            keys(&mut harness(48_000), 3.0, 0.0, 6),
            [67, 64, 60, 64, 67, 64]
        );
        assert_eq!(keys(&mut harness(48_000), 4.0, 0.0, 3), [64, 60, 67]);
        let random = keys(&mut harness(96_000), 5.0, 0.0, 12);
        assert_eq!(random.len(), 12);
        assert!(random.iter().all(|k| [60, 64, 67].contains(k)));
        assert_eq!(
            random,
            keys(&mut harness(96_000), 5.0, 0.0, 12),
            "repeatable"
        );
    }

    #[test]
    fn octaves_stack_the_pattern_upward() {
        assert_eq!(
            keys(&mut harness(48_000), 0.0, 1.0, 8),
            [60, 64, 67, 72, 76, 79, 60, 64]
        );
    }

    #[test]
    fn each_note_lasts_the_gate_fraction_of_a_step() {
        let mut h = harness(12_000);
        h.set(ArpPorts::GATE, 0.5);
        chord(&mut h);
        h.run(12_000).unwrap();
        let offs: Vec<u32> = h
            .events(ArpPorts::OUT)
            .iter()
            .filter(|e| matches!(e.kind, EventKind::NoteOff { .. }))
            .map(|e| e.time)
            .collect();
        assert_eq!(offs, [STEP / 2, STEP + STEP / 2]);
    }

    #[test]
    fn a_note_off_that_falls_in_a_later_block_is_sent_then() {
        let mut h = harness(4_800);
        chord(&mut h);
        h.run(4_800).unwrap();
        assert_eq!(played(&h), [(0, 60)]);
        h.send_events(0, &[]);
        h.run(4_800).unwrap();
        // The default gate of 0.6 ends the note at 3_600, inside the first
        // block; the second has the next on at 6_000 - 4_800 = 1_200.
        assert_eq!(played(&h), [(1_200, 64)]);
    }

    #[test]
    fn releasing_every_key_ends_the_sounding_note_and_stops() {
        let mut h = harness(12_000);
        h.set(ArpPorts::GATE, 1.0);
        h.send_events(0, &[on(0, 1, 60)]);
        h.run(4_800).unwrap();
        h.send_events(0, &[off(100, 1)]);
        h.run(4_800).unwrap();
        let kinds: Vec<_> = h
            .events(ArpPorts::OUT)
            .iter()
            .map(|e| (e.time, e.kind))
            .collect();
        assert_eq!(kinds.len(), 1, "{kinds:?}");
        assert_eq!(kinds[0].0, 100);
        assert!(matches!(kinds[0].1, EventKind::NoteOff { .. }));
        h.send_events(0, &[]);
        h.run(12_000).unwrap();
        assert!(h.events(ArpPorts::OUT).is_empty());
    }

    #[test]
    fn hold_keeps_the_pattern_until_a_new_key() {
        let mut h = harness(12_000);
        h.set(ArpPorts::HOLD, 1.0);
        h.send_events(0, &[on(0, 1, 60), off(500, 1)]);
        h.run(12_000).unwrap();
        assert_eq!(played(&h), [(0, 60), (STEP, 60)]);
        h.send_events(0, &[on(100, 2, 72)]);
        h.run(12_000).unwrap();
        let keys: Vec<u8> = played(&h).iter().map(|p| p.1).collect();
        // The step at the block's start comes before the key goes down.
        assert_eq!(keys, [60, 72], "the old key is gone");
    }

    #[test]
    fn swing_delays_the_odd_steps() {
        let mut h = harness(24_000);
        h.set(ArpPorts::SWING, 0.5);
        chord(&mut h);
        h.run(24_000).unwrap();
        let times: Vec<u32> = played(&h).iter().map(|p| p.0).collect();
        assert_eq!(times, [0, STEP + STEP / 2, 2 * STEP, 3 * STEP + STEP / 2]);
    }

    #[test]
    fn steps_follow_the_rate() {
        let mut h = harness(48_000);
        h.set(ArpPorts::RATE, 0.0);
        chord(&mut h);
        h.run(48_000).unwrap();
        let times: Vec<u32> = played(&h).iter().map(|p| p.0).collect();
        assert_eq!(times, [0, 24_000]);
    }
}
