//! The Random Select node.

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo,
    NodeType, NoteId, ParamInfo, SampleChooser, Setup, Shape,
};

use crate::sample_set::CHOICE_CUT;

/// Routes one of its inputs to its output, and picks a new one at random each
/// time `trigger` rises through 0.5 (a clock, a key's gate, a comparator).
/// The output holds its choice until the next trigger. It starts on input 1.
///
/// **Kind.** What the inputs carry is config (`kind`), because it changes the
/// ports: 0 is audio or control signal, 1 is note events, 2 is samples (or
/// sample sets), 3 is spectral frames. The editor sets it from the first wire.
///
/// **Inputs.** How many (`inputs`, 1 to 64) is config too, and grows with the
/// wires like a mixer's.
///
/// **Picking.** Uniform. With `avoid_repeat` on, the pick is never the input
/// already chosen (unless there is only one). The choices come from the
/// node's own seed, so a render repeats; `seed` mixes a number of the user's
/// into it.
///
/// **Switching signals.** `Instant` swaps at the trigger's sample.
/// `Zero crossing` waits until the old input crosses zero (at most
/// [`MAX_WAIT_MS`]), so a switch between waveforms doesn't click. Every voice
/// and channel switches at its own crossing. The trigger is read from the
/// first voice.
///
/// **Switching notes.** The old input's new note-ons are closed at the
/// trigger. With `Finish held`, the notes it already sent keep their
/// note-offs, which pass when they arrive; with `Cut held`, they are ended
/// with a note-off at the trigger. Every other event follows the new input.
pub struct RandomSelect;

pub const RANDOM_SELECT_ID: &str = "noodle.util.randselect";

const INPUTS: ConfigInfo = ConfigInfo::int("inputs", "Inputs", 2);
const KIND: ConfigInfo =
    ConfigInfo::int("kind", "Kind (0 signal, 1 notes, 2 samples, 3 spectral)", 0);
pub const MAX_INPUTS: i64 = 64;
/// The longest a zero-crossing switch waits for a crossing.
pub const MAX_WAIT_MS: f32 = 20.0;
/// Held notes remembered at once; past this, new note-ons are dropped.
const HELD: usize = 256;

static CONFIG: [ConfigInfo; 2] = [KIND, INPUTS];

static INFO: NodeInfo = NodeInfo {
    id: RANDOM_SELECT_ID,
    version: 1,
    name: "Random Select",
    category: "Utilities",
};

/// Offsets from the first parameter-ish input, which follows the signal
/// inputs `in1`…`inN` for the signal kind and comes first for notes.
const TRIGGER: usize = 0;
const AVOID_REPEAT: usize = 1;
const SWITCH: usize = 2;
const SEED: usize = 3;
/// The signal output a Random Select of samples puts its pick on.
pub const CHOICE_PORT: &str = "choice";

/// What the inputs carry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Signal,
    Notes,
    Samples,
    Spectral,
}

struct Counts {
    kind: Kind,
    inputs: usize,
}

impl Counts {
    fn of(config: &Config) -> Result<Self, NodeError> {
        let inputs = INPUTS.get_int(config);
        if !(1..=MAX_INPUTS).contains(&inputs) {
            return Err(NodeError::config(format!(
                "Random Select needs between 1 and {MAX_INPUTS} inputs, not {inputs}"
            )));
        }
        let kind = match KIND.get_int(config) {
            1 => Kind::Notes,
            2 => Kind::Samples,
            3 => Kind::Spectral,
            _ => Kind::Signal,
        };
        Ok(Self {
            kind,
            inputs: inputs as usize,
        })
    }

    /// The index of the trigger among the signal inputs.
    fn base(&self) -> usize {
        if self.kind == Kind::Signal {
            self.inputs
        } else {
            0
        }
    }

    /// Whether there is a `switch` parameter. Spectra just switch at a frame.
    fn has_switch(&self) -> bool {
        self.kind != Kind::Spectral
    }

    /// Where `seed` is among the parameters after the inputs.
    fn seed_offset(&self) -> usize {
        if self.has_switch() { SEED } else { SEED - 1 }
    }
}

impl NodeType for RandomSelect {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let counts = Counts::of(config)?;
        let mut layout = Layout::realtime();
        for i in 1..=counts.inputs {
            let (key, name) = (format!("in{i}"), format!("In {i}"));
            layout = match counts.kind {
                Kind::Signal => layout.input(key, name),
                Kind::Notes => layout.event_input(key, name),
                Kind::Samples => layout.sample_input(key, name),
                Kind::Spectral => layout.spectral_input(key, name),
            };
        }
        layout = layout.input("trigger", "Trigger").param(
            "avoid_repeat",
            "Avoid repeat",
            ParamInfo::choice(["Off", "On"]),
        );
        if counts.has_switch() {
            let switch = if counts.kind == Kind::Signal {
                ParamInfo::choice(["Instant", "Zero crossing"])
            } else {
                ParamInfo::choice(["Finish held", "Cut held"])
            };
            layout = layout.param("switch", "Switch", switch);
        }
        layout = layout.param(
            "seed",
            "Seed",
            ParamInfo::new(0.0, 9999.0, 0.0).smoothing(0.0),
        );
        Ok(match counts.kind {
            Kind::Signal => layout.output("out", "Out"),
            Kind::Notes => layout.event_output("out", "Out"),
            // The signal is the pick itself, wired to the receiver by the
            // compiler; the editor keeps it off the node.
            Kind::Samples => layout
                .sample_output("out", "Out")
                .output(CHOICE_PORT, "Choice"),
            Kind::Spectral => layout.spectral_output("out", "Out"),
        })
    }

    fn sample_chooser(&self, config: &Config) -> Option<SampleChooser> {
        let counts = Counts::of(config)
            .ok()
            .filter(|c| c.kind == Kind::Samples)?;
        Some(SampleChooser {
            output: "out".into(),
            inputs: (1..=counts.inputs).map(|i| format!("in{i}")).collect(),
            index: CHOICE_PORT.into(),
        })
    }

    fn output_shapes(
        &self,
        config: &Config,
        layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(match Counts::of(config)?.kind {
            Kind::Samples => vec![Shape::MONO],
            Kind::Spectral => {
                // The spectral inputs' shapes follow the signal inputs'.
                let shape = Shape::broadcast_all(inputs[layout.inputs.len()..].iter().copied())?;
                vec![shape]
            }
            _ => {
                let shape = Shape::broadcast_all(inputs.iter().copied())?;
                vec![shape; layout.outputs.len()]
            }
        })
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let counts = Counts::of(setup.config)?;
        let shape = setup.output_shapes.first().copied().unwrap_or(Shape::MONO);
        Ok(Instance::realtime(RandomSelectNode {
            kind: counts.kind,
            inputs: counts.inputs,
            base: counts.base(),
            seed_port: counts.base() + counts.seed_offset(),
            switch_port: counts.base() + SWITCH,
            base_seed: setup.seed,
            seed: 0,
            rng: Rng::new(setup.seed),
            target: 0,
            gate: false,
            max_wait: (setup.sample_rate * MAX_WAIT_MS / 1000.0) as u32,
            lanes: vec![Lane::default(); shape.lanes()],
            edges: Vec::with_capacity(setup.max_frames),
            cursors: vec![0; counts.inputs],
            held: Vec::with_capacity(HELD),
        }))
    }
}

/// One voice and channel's place in a zero-crossing switch.
#[derive(Clone, Copy, Default)]
struct Lane {
    active: usize,
    prev: f32,
    waited: u32,
}

struct RandomSelectNode {
    kind: Kind,
    inputs: usize,
    base: usize,
    seed_port: usize,
    switch_port: usize,
    base_seed: u64,
    seed: u32,
    rng: Rng,
    /// The input the last trigger chose.
    target: usize,
    gate: bool,
    max_wait: u32,
    lanes: Vec<Lane>,
    /// The frame and new target of each trigger in this block.
    edges: Vec<(usize, usize)>,
    cursors: Vec<usize>,
    /// Notes sent on, as (input, note), awaiting their note-off.
    held: Vec<(usize, NoteId)>,
}

impl RandomSelectNode {
    /// The value of a parameter at the start of the block.
    fn param(io: &Io<'_, '_>, index: usize) -> f32 {
        let x = io.inputs[index].lane(0, 0).first().copied().unwrap_or(0.0);
        if x.is_finite() { x } else { 0.0 }
    }

    /// Finds the block's triggers and picks for each.
    fn find_edges(&mut self, io: &Io<'_, '_>, frames: usize) {
        let seed = Self::param(io, self.seed_port).round().max(0.0) as u32;
        if seed != self.seed {
            self.seed = seed;
            self.rng = Rng::new(mix_seed(self.base_seed, seed));
        }
        let avoid = Self::param(io, self.base + AVOID_REPEAT) >= 0.5;
        let trigger = io.inputs[self.base + TRIGGER].lane(0, 0);
        self.edges.clear();
        for (i, &x) in trigger.iter().enumerate().take(frames) {
            let high = x >= 0.5;
            if high && !self.gate {
                self.target = self.rng.pick(self.inputs, avoid.then_some(self.target));
                self.edges.push((i, self.target));
            }
            self.gate = high;
        }
    }

    fn process_signals(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let mut now = self.target;
        self.find_edges(&io, ctx.frames);
        let instant = Self::param(&io, self.switch_port) < 0.5;
        let shape = io.outputs[0].shape();
        let mut edge = 0;
        for i in 0..ctx.frames {
            while edge < self.edges.len() && self.edges[edge].0 == i {
                now = self.edges[edge].1;
                edge += 1;
            }
            for voice in 0..shape.voices {
                for channel in 0..shape.channels {
                    let lane = &mut self.lanes[voice * shape.channels + channel];
                    if lane.active != now {
                        let old = io.inputs[lane.active].lane(voice, channel)[i];
                        if instant || lane.prev * old <= 0.0 || lane.waited >= self.max_wait {
                            lane.active = now;
                            lane.waited = 0;
                        } else {
                            lane.waited += 1;
                        }
                    }
                    let x = io.inputs[lane.active].lane(voice, channel)[i];
                    lane.prev = x;
                    io.outputs[0].lane_mut(voice, channel)[i] = x;
                }
            }
        }
    }

    /// The pick, as the number of the input in force at each frame, with
    /// [`CHOICE_CUT`] added when held notes are to be cut.
    fn process_samples(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let mut now = self.target;
        self.find_edges(&io, ctx.frames);
        let cut = Self::param(&io, self.switch_port) >= 0.5;
        let flag = if cut { CHOICE_CUT } else { 0.0 };
        let out = io.outputs[0].lane_mut(0, 0);
        let mut edge = 0;
        for (i, slot) in out.iter_mut().enumerate().take(ctx.frames) {
            while edge < self.edges.len() && self.edges[edge].0 == i {
                now = self.edges[edge].1;
                edge += 1;
            }
            *slot = now as f32 + flag;
        }
    }

    fn process_spectral(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let mut now = self.target;
        self.find_edges(&io, ctx.frames);
        self.cursors.fill(0);
        let out = &mut io.spectral_outputs[0];
        let shape = out.shape();
        let mut edge = 0;
        let mut start = 0;
        while start < ctx.frames {
            while edge < self.edges.len() && self.edges[edge].0 == start {
                now = self.edges[edge].1;
                edge += 1;
            }
            let end = self.edges.get(edge).map_or(ctx.frames, |&(frame, _)| frame);
            for (input, frames) in io.spectral_inputs.iter().enumerate().take(self.inputs) {
                while self.cursors[input] < frames.frames()
                    && (frames.time(self.cursors[input]) as usize) < end
                {
                    let frame = self.cursors[input];
                    self.cursors[input] += 1;
                    if input != now {
                        continue;
                    }
                    let Some(at) = out.push(frames.time(frame)) else {
                        continue;
                    };
                    for voice in 0..shape.voices {
                        for channel in 0..shape.channels {
                            out.frame_mut(at, voice, channel)
                                .copy_from_slice(frames.frame(frame, voice, channel));
                        }
                    }
                }
            }
            start = end;
        }
    }

    fn process_events(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let mut now = self.target;
        self.find_edges(&io, ctx.frames);
        let cut = Self::param(&io, self.switch_port) >= 0.5;
        self.cursors.fill(0);
        let out = &mut io.event_outputs[0];
        let mut edge = 0;
        let mut start = 0;
        while start < ctx.frames {
            while edge < self.edges.len() && self.edges[edge].0 == start {
                now = self.edges[edge].1;
                edge += 1;
                if cut {
                    for &(_, note) in &self.held {
                        let _ = out.insert(Event {
                            time: start as u32,
                            kind: EventKind::NoteOff {
                                note,
                                velocity: 0.0,
                            },
                        });
                    }
                    self.held.clear();
                }
            }
            let end = self.edges.get(edge).map_or(ctx.frames, |&(frame, _)| frame);
            for (input, events) in io.event_inputs.iter().enumerate().take(self.inputs) {
                while let Some(&event) = events.get(self.cursors[input]) {
                    if event.time as usize >= end {
                        break;
                    }
                    self.cursors[input] += 1;
                    let current = input == now;
                    let pass = match event.kind {
                        EventKind::NoteOn { note, .. } => {
                            // A note-on that can't be remembered is dropped, so
                            // it can't be left without its note-off.
                            let room = self.held.len() < HELD;
                            if current && room {
                                self.held.push((input, note));
                            }
                            current && room
                        }
                        EventKind::NoteOff { note, .. } => {
                            let at = self.held.iter().position(|&h| h == (input, note));
                            if let Some(at) = at {
                                self.held.swap_remove(at);
                            }
                            at.is_some()
                        }
                        EventKind::Expression { note, .. } => self.held.contains(&(input, note)),
                        EventKind::Midi(_) | EventKind::Channel { .. } => current,
                    };
                    if pass {
                        let _ = out.insert(event);
                    }
                }
            }
            start = end;
        }
    }
}

impl Node for RandomSelectNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        match self.kind {
            Kind::Signal => self.process_signals(ctx, io),
            Kind::Notes => self.process_events(ctx, io),
            Kind::Samples => self.process_samples(ctx, io),
            Kind::Spectral => self.process_spectral(ctx, io),
        }
    }

    fn reset(&mut self) {
        self.rng = Rng::new(mix_seed(self.base_seed, self.seed));
        self.target = 0;
        self.gate = false;
        self.lanes.fill(Lane::default());
        self.held.clear();
    }
}

/// xorshift64*.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn below(&mut self, n: usize) -> usize {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let bits = x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32;
        ((bits * n as u64) >> 32) as usize
    }

    /// Uniform in `0..n`, never `avoid` if there is another to pick.
    fn pick(&mut self, n: usize, avoid: Option<usize>) -> usize {
        match avoid {
            Some(last) if n > 1 && last < n => {
                let r = self.below(n - 1);
                if r >= last { r + 1 } else { r }
            }
            _ => self.below(n),
        }
    }
}

/// The node's seed with the user's, so seed 0 is the node's own.
fn mix_seed(node: u64, user: u32) -> u64 {
    node ^ u64::from(user).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;

    const FRAMES: usize = 64;

    fn config(inputs: i64, events: bool) -> Config {
        Config::new()
            .with("inputs", Value::Int(inputs))
            .with("kind", Value::Int(i64::from(events)))
    }

    /// Params of a signal node with `n` inputs.
    fn avoid(n: usize) -> usize {
        n + AVOID_REPEAT
    }
    fn switch(n: usize) -> usize {
        n + SWITCH
    }
    fn seed(n: usize) -> usize {
        n + SEED
    }

    /// A signal node with inputs holding the constants 1, 2, 3…, and a
    /// connected trigger.
    fn signal(n: usize, h_seed: u64) -> Harness {
        let connected: Vec<_> = (0..=n).map(|i| (i, Shape::MONO)).collect();
        let mut h = Harness::with_seed(
            &RandomSelect,
            &config(n as i64, false),
            &connected,
            48_000.0,
            FRAMES,
            h_seed,
        )
        .unwrap();
        for i in 0..n {
            h.input(i, FRAMES).fill((i + 1) as f32);
        }
        h
    }

    fn pulse(h: &mut Harness, n: usize, at: &[usize]) {
        let mut input = h.input(n, FRAMES);
        input.fill(0.0);
        let lane = input.lane_mut(0, 0);
        for &i in at {
            lane[i] = 1.0;
        }
    }

    /// The input (1-based) chosen at each trigger over `triggers` blocks.
    fn picks(h: &mut Harness, n: usize, triggers: usize) -> Vec<f32> {
        let mut picks = Vec::new();
        for _ in 0..triggers {
            pulse(h, n, &[3]);
            h.run(FRAMES).unwrap();
            picks.push(h.output(0).lane(0, 0)[10]);
            pulse(h, n, &[]);
            h.run(FRAMES).unwrap();
        }
        picks
    }

    #[test]
    fn holds_input_one_until_triggered_then_holds_the_choice() {
        let mut h = signal(3, 7);
        pulse(&mut h, 3, &[20]);
        h.run(FRAMES).unwrap();
        let out = h.output(0).lane(0, 0).to_vec();
        assert!(out[..20].iter().all(|&x| x == 1.0));
        let chosen = out[20];
        assert!(out[20..].iter().all(|&x| x == chosen));
        // No trigger in the next block: the choice is held.
        pulse(&mut h, 3, &[]);
        h.run(FRAMES).unwrap();
        assert!(h.output(0).lane(0, 0).iter().all(|&x| x == chosen));
    }

    #[test]
    fn a_held_gate_is_one_trigger() {
        let mut h = signal(4, 11);
        h.input(4, FRAMES).fill(1.0);
        h.run(FRAMES).unwrap();
        let first = h.output(0).lane(0, 0)[0];
        assert!(h.output(0).lane(0, 0).iter().all(|&x| x == first));
        h.run(FRAMES).unwrap();
        assert!(h.output(0).lane(0, 0).iter().all(|&x| x == first));
    }

    #[test]
    fn picks_repeat_for_a_seed_and_differ_between_seeds() {
        let a = picks(&mut signal(4, 5), 4, 24);
        let b = picks(&mut signal(4, 5), 4, 24);
        assert_eq!(a, b);
        assert_ne!(a, picks(&mut signal(4, 6), 4, 24));
        let mut user = signal(4, 5);
        user.set(seed(4), 3.0);
        assert_ne!(a, picks(&mut user, 4, 24));
    }

    #[test]
    fn every_input_is_picked() {
        let picks = picks(&mut signal(4, 1), 4, 80);
        for want in 1..=4 {
            assert!(picks.contains(&(want as f32)), "never picked {want}");
        }
    }

    #[test]
    fn avoid_repeat_never_picks_the_same_input_twice() {
        let mut h = signal(3, 9);
        h.set(avoid(3), 1.0);
        let avoided = picks(&mut h, 3, 60);
        assert!(avoided.windows(2).all(|w| w[0] != w[1]), "{avoided:?}");
        assert_ne!(avoided[0], 1.0);
        // Without it, repeats happen.
        let free = picks(&mut signal(3, 9), 3, 60);
        assert!(free.windows(2).any(|w| w[0] == w[1]));
    }

    #[test]
    fn a_single_input_with_avoid_repeat_stays_put() {
        let mut h = signal(1, 2);
        h.set(avoid(1), 1.0);
        assert!(picks(&mut h, 1, 5).iter().all(|&x| x == 1.0));
    }

    #[test]
    fn zero_crossing_waits_for_the_old_input_to_cross() {
        // Input 1 is a ramp through zero at frame 30; input 2 is constant.
        let mut h = signal(2, 3);
        h.set(switch(2), 1.0);
        h.set(avoid(2), 1.0);
        let ramp: Vec<f32> = (0..FRAMES).map(|i| i as f32 - 30.0).collect();
        h.input(0, FRAMES).lane_mut(0, 0).copy_from_slice(&ramp);
        pulse(&mut h, 2, &[10]);
        h.run(FRAMES).unwrap();
        let out = h.output(0).lane(0, 0).to_vec();
        // Avoiding a repeat from input 1 means input 2 is chosen.
        assert_eq!(out[29], -1.0);
        assert_eq!(out[30], 2.0, "switches on the crossing, not the trigger");
        assert_eq!(out[10], -20.0);
    }

    #[test]
    fn zero_crossing_gives_up_waiting_on_dc() {
        let mut h = signal(2, 3);
        h.set(switch(2), 1.0);
        h.set(avoid(2), 1.0);
        pulse(&mut h, 2, &[0]);
        let wait = (48_000.0 * MAX_WAIT_MS / 1000.0) as usize;
        let mut seen = 0;
        while seen <= wait {
            h.run(FRAMES).unwrap();
            if h.output(0).lane(0, 0)[FRAMES - 1] == 2.0 {
                return;
            }
            pulse(&mut h, 2, &[]);
            seen += FRAMES;
        }
        panic!("never switched");
    }

    #[test]
    fn a_full_held_list_drops_note_ons_rather_than_their_offs() {
        let mut h = notes(1);
        let ons: Vec<Event> = (0..HELD as u32 + 4).map(|i| on(1, i)).collect();
        // The harness's event buffer holds 1024.
        h.send_events(0, &ons);
        h.run(FRAMES).unwrap();
        assert_eq!(h.events(0).len(), HELD);
        let offs: Vec<Event> = (0..HELD as u32 + 4).map(|i| off(2, i)).collect();
        h.send_events(0, &offs);
        h.run(FRAMES).unwrap();
        assert_eq!(h.events(0).len(), HELD);
    }

    #[test]
    fn rejects_a_bad_input_count() {
        assert!(RandomSelect.layout(&config(0, false)).is_err());
        assert!(RandomSelect.layout(&config(65, true)).is_err());
    }

    #[test]
    fn the_kind_sets_the_port_types() {
        let notes = RandomSelect.layout(&config(3, true)).unwrap();
        assert_eq!(notes.event_inputs.len(), 3);
        assert_eq!(notes.event_outputs.len(), 1);
        assert!(notes.outputs.is_empty());
        let signal = RandomSelect.layout(&config(3, false)).unwrap();
        assert_eq!(signal.inputs.len(), 3 + 4);
        assert!(signal.event_inputs.is_empty());
    }

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

    fn notes(n: i64) -> Harness {
        Harness::new(
            &RandomSelect,
            &config(n, true),
            &[(0, Shape::MONO)],
            48_000.0,
            FRAMES,
        )
        .unwrap()
    }

    fn fire(h: &mut Harness, at: usize) {
        let mut input = h.input(0, FRAMES);
        input.fill(0.0);
        input.lane_mut(0, 0)[at] = 1.0;
    }

    /// Triggers until input 2 is the one chosen (avoiding repeats makes two
    /// inputs alternate), with the held-note choice set to `cut`.
    fn on_input_two(cut: bool) -> Harness {
        let mut h = notes(2);
        h.set(1, 1.0);
        h.set(2, f32::from(u8::from(cut)));
        fire(&mut h, 0);
        h.run(FRAMES).unwrap();
        fire(&mut h, FRAMES - 1);
        h.input(0, FRAMES).fill(0.0);
        h
    }

    #[test]
    fn notes_follow_the_chosen_input() {
        let mut h = on_input_two(false);
        h.send_events(0, &[on(1, 1)]);
        h.send_events(1, &[on(2, 2)]);
        h.run(FRAMES).unwrap();
        assert_eq!(h.events(0), [on(2, 2)]);
    }

    #[test]
    fn finish_held_lets_note_offs_through_but_closes_new_note_ons() {
        let mut h = on_input_two(false);
        h.send_events(1, &[on(1, 2)]);
        h.run(FRAMES).unwrap();
        assert_eq!(h.events(0), [on(1, 2)]);
        // Trigger at 5 sends us back to input 1; input 2's off still passes.
        fire(&mut h, 5);
        h.send_events(0, &[on(8, 1)]);
        h.send_events(1, &[on(2, 3), off(10, 2)]);
        h.run(FRAMES).unwrap();
        let out = h.events(0).to_vec();
        assert_eq!(out, [on(2, 3), on(8, 1), off(10, 2)]);
    }

    #[test]
    fn cut_held_ends_notes_at_the_switch() {
        let mut h = on_input_two(true);
        h.send_events(1, &[on(1, 2)]);
        h.run(FRAMES).unwrap();
        fire(&mut h, 5);
        h.send_events(0, &[on(8, 1)]);
        h.send_events(1, &[off(10, 2)]);
        h.run(FRAMES).unwrap();
        assert_eq!(h.events(0), [off(5, 2), on(8, 1)]);
    }

    /// A Random Select of samples, whose output is the number of the pick.
    fn samples(n: i64) -> Harness {
        let config = config(n, false).with("kind", Value::Int(2));
        Harness::new(
            &RandomSelect,
            &config,
            &[(0, Shape::MONO)],
            48_000.0,
            FRAMES,
        )
        .unwrap()
    }

    #[test]
    fn samples_put_the_number_of_the_pick_on_the_signal_from_the_trigger() {
        let mut h = samples(2);
        h.set(AVOID_REPEAT, 1.0);
        fire(&mut h, 20);
        h.run(FRAMES).unwrap();
        // The out is a sample port; the signal is the only output buffer.
        let pick = h.output(0).lane(0, 0).to_vec();
        assert!(pick[..20].iter().all(|&x| x == 0.0), "starts on the first");
        assert!(pick[20..].iter().all(|&x| x == 1.0), "{pick:?}");
    }

    #[test]
    fn cutting_held_samples_adds_the_cut_flag() {
        let mut h = samples(2);
        h.set(SWITCH, 1.0);
        h.input(0, FRAMES).fill(0.0);
        h.run(FRAMES).unwrap();
        assert!(h.output(0).lane(0, 0).iter().all(|&x| x == CHOICE_CUT));
    }

    #[test]
    fn a_pick_of_samples_names_its_candidates_in_input_order() {
        let config = config(3, false).with("kind", Value::Int(2));
        let chooser = RandomSelect.sample_chooser(&config).unwrap();
        assert_eq!(chooser.output, "out");
        assert_eq!(chooser.inputs, ["in1", "in2", "in3"]);
        let layout = RandomSelect.layout(&config).unwrap();
        assert!(layout.outputs.iter().any(|p| p.key == chooser.index));
        assert_eq!(layout.sample_inputs.len(), 3);
        assert!(
            RandomSelect
                .sample_chooser(&self::config(3, false))
                .is_none()
        );
    }

    #[test]
    fn spectra_switch_on_the_first_frame_after_the_trigger() {
        use noodle_engine::Bin;
        const BLOCK: usize = 4096;
        let config = Config::new().with("kind", Value::Int(3));
        let mut h = Harness::with_spectral(
            &RandomSelect,
            &config,
            &[(0, Shape::MONO)],
            &[Shape::MONO, Shape::MONO],
            48_000.0,
            BLOCK,
        )
        .unwrap();
        h.set(1, 1.0);
        let mut trigger = h.input(0, BLOCK);
        trigger.fill(0.0);
        trigger.lane_mut(0, 0)[1500] = 1.0;
        for (port, re) in [(0, 1.0), (1, 2.0)] {
            let mut input = h.spectral_input(port);
            for time in [100, 2000, 3500] {
                let at = input.push(time).unwrap();
                input.frame_mut(at, 0, 0)[0] = Bin::new(re, 0.0);
            }
        }
        h.run(BLOCK).unwrap();
        let out = h.spectral(0);
        let got: Vec<(u32, f32)> = (0..out.frames())
            .map(|i| (out.time(i), out.frame(i, 0, 0)[0].re))
            .collect();
        assert_eq!(got, [(100, 1.0), (2000, 2.0), (3500, 2.0)]);
    }
}
