//! The Selector node.

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Instance, Io, Layout, Memory, Node, NodeError,
    NodeInfo, NodeType, NoteId, ParamInfo, SampleChooser, Setup, Shape,
};

use crate::random_select::MAX_WAIT_MS;
use crate::sample_set::CHOICE_CUT;

/// Routes one of its inputs to its output, chosen by `which`: a deterministic
/// A/B (or N-way) selector, where [`RandomSelect`](crate::RandomSelect) picks
/// at random on a trigger.
///
/// **Kind.** What the inputs carry is config (`kind`), because it changes the
/// ports: 0 is audio or control signal, 1 is note events, 2 is samples (or
/// sample sets), 3 is spectral frames. The editor sets it from the first wire
/// and refuses a different kind once one is wired.
///
/// **Inputs.** How many (`inputs`, 2 to 64) is config too, and grows with the
/// wires like a mixer's.
///
/// **Choosing.** `which` is a stepped parameter (`In 1`…`In N`), numbered from
/// 0 internally. Its value is rounded and clamped to an input, so automation
/// and wires can choose one; a wire changes the choice per sample.
///
/// **Switching signals.** `Instant` swaps at the sample the choice changes on.
/// `Zero crossing` waits until the old input crosses zero (at most
/// [`MAX_WAIT_MS`]), so a switch between waveforms doesn't click. Every voice
/// and channel switches at its own crossing. The choice is read from the first
/// voice.
///
/// **Switching notes.** The old input's new note-ons are closed at the switch.
/// With `Finish held`, the notes it already sent keep their note-offs, which
/// pass when they arrive; with `Cut held`, they are ended with a note-off at
/// the switch. Every other event follows the selected input.
///
/// **Samples.** The output is a sample port, and the node's `choice` signal
/// carries the number of the input in force (plus [`CHOICE_CUT`] when held
/// notes are cut), for the compiler to wire to the receiver's `sample_choice`.
///
/// **Spectra.** The chosen input's frames are passed, and a change takes
/// effect at the first frame at or after it. Spectra have no `switch`.
pub struct Selector;

/// The Selector's type ID.
pub const SELECTOR_ID: &str = "noodle.util.selector";

/// The signal output a Selector of samples puts its choice on.
pub const CHOICE_PORT: &str = "choice";

const INPUTS: ConfigInfo = ConfigInfo::int("inputs", "Inputs", 2);
const KIND: ConfigInfo =
    ConfigInfo::int("kind", "Kind (0 signal, 1 notes, 2 samples, 3 spectral)", 0);
pub const MIN_INPUTS: i64 = 2;
pub const MAX_INPUTS: i64 = 64;
const OUT: usize = 0;
const WHICH: &str = "which";
const SWITCH: &str = "switch";
/// Held notes remembered at once; past this, new note-ons are dropped.
const HELD: usize = 256;

static CONFIG: [ConfigInfo; 2] = [INPUTS, KIND];

static INFO: NodeInfo = NodeInfo {
    id: SELECTOR_ID,
    version: 1,
    name: "Selector",
    category: "Utilities",
};

/// Offsets from the first parameter input, which follows the signal inputs
/// `in1`…`inN` for the signal kind and comes first for the other kinds.
const WHICH_OFFSET: usize = 0;
const SWITCH_OFFSET: usize = 1;

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
        if !(MIN_INPUTS..=MAX_INPUTS).contains(&inputs) {
            return Err(NodeError::config(format!(
                "Selector needs between {MIN_INPUTS} and {MAX_INPUTS} inputs, not {inputs}"
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

    /// The index of `which` among the signal inputs.
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
}

impl NodeType for Selector {
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
        layout = layout.param(
            WHICH,
            "Which",
            ParamInfo::choice((1..=counts.inputs).map(|i| format!("In {i}"))),
        );
        if counts.has_switch() {
            let switch = match counts.kind {
                Kind::Signal => ParamInfo::choice(["Instant", "Zero crossing"]),
                _ => ParamInfo::choice(["Finish held", "Cut held"]),
            };
            layout = layout.param(SWITCH, "Switch", switch);
        }
        let memory = match counts.kind {
            // A zero-crossing wait remembers the old input for up to
            // MAX_WAIT_MS.
            Kind::Signal => Memory::Bounded(MAX_WAIT_MS as u32),
            // A held note can last as long as its gate.
            Kind::Notes => Memory::Unbounded,
            // Samples and spectra follow the current choice and frame.
            Kind::Samples | Kind::Spectral => Memory::None,
        };
        Ok(match counts.kind {
            Kind::Signal => layout.output("out", "Out"),
            Kind::Notes => layout.event_output("out", "Out"),
            // The signal is the choice itself, wired to the receiver by the
            // compiler; the editor keeps it off the node.
            Kind::Samples => layout
                .sample_output("out", "Out")
                .output(CHOICE_PORT, "Choice"),
            Kind::Spectral => layout.spectral_output("out", "Out"),
        }
        .memory(memory))
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
        let base = counts.base();
        Ok(Instance::realtime(SelectorNode {
            kind: counts.kind,
            inputs: counts.inputs,
            which_port: base + WHICH_OFFSET,
            switch_port: base + SWITCH_OFFSET,
            target: 0,
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

struct SelectorNode {
    kind: Kind,
    inputs: usize,
    which_port: usize,
    switch_port: usize,
    /// The input in force at the start of the next block.
    target: usize,
    max_wait: u32,
    lanes: Vec<Lane>,
    /// The frame and new input of each change of `which` in this block.
    edges: Vec<(usize, usize)>,
    cursors: Vec<usize>,
    /// Notes sent on, as (input, note), awaiting their note-off.
    held: Vec<(usize, NoteId)>,
}

impl SelectorNode {
    /// The value of a parameter at the start of the block.
    fn param(io: &Io<'_, '_>, index: usize) -> f32 {
        let x = io.inputs[index].lane(0, 0).first().copied().unwrap_or(0.0);
        if x.is_finite() { x } else { 0.0 }
    }

    /// Finds where `which` changes this block. It reads the first voice, as
    /// Random Select reads its trigger.
    fn find_changes(&mut self, io: &Io<'_, '_>, frames: usize) {
        let which = io.inputs[self.which_port].lane(0, 0);
        self.edges.clear();
        let mut now = self.target;
        for (i, &value) in which.iter().enumerate().take(frames) {
            let chosen = chosen_input(value, self.inputs);
            if chosen != now {
                now = chosen;
                self.edges.push((i, chosen));
            }
        }
        self.target = now;
    }

    fn process_signals(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let mut now = self.target;
        self.find_changes(&io, ctx.frames);
        let instant = Self::param(&io, self.switch_port) < 0.5;
        let shape = io.outputs[OUT].shape();
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
                    io.outputs[OUT].lane_mut(voice, channel)[i] = x;
                }
            }
        }
    }

    /// The choice, as the number of the input in force at each frame, with
    /// [`CHOICE_CUT`] added when held notes are to be cut.
    fn process_samples(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let mut now = self.target;
        self.find_changes(&io, ctx.frames);
        let cut = Self::param(&io, self.switch_port) >= 0.5;
        let flag = if cut { CHOICE_CUT } else { 0.0 };
        let out = io.outputs[OUT].lane_mut(0, 0);
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
        self.find_changes(&io, ctx.frames);
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
        self.find_changes(&io, ctx.frames);
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

impl Node for SelectorNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        match self.kind {
            Kind::Signal => self.process_signals(ctx, io),
            Kind::Notes => self.process_events(ctx, io),
            Kind::Samples => self.process_samples(ctx, io),
            Kind::Spectral => self.process_spectral(ctx, io),
        }
    }

    fn reset(&mut self) {
        self.target = 0;
        self.lanes.fill(Lane::default());
        self.held.clear();
    }
}

/// The input a `which` value chooses: rounded, clamped to `0..inputs`, and
/// input 1 for a value that isn't a number.
fn chosen_input(which: f32, inputs: usize) -> usize {
    if which.is_finite() {
        which.round().clamp(0.0, (inputs - 1) as f32) as usize
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{InputKind, ParamKind, Value};

    const FRAMES: usize = 64;

    const MONO: Shape = Shape::MONO;

    fn config(inputs: i64, kind: i64) -> Config {
        Config::new()
            .with("inputs", Value::Int(inputs))
            .with("kind", Value::Int(kind))
    }

    /// A signal selector with `inputs` audio inputs, all wired mono, and an
    /// unconnected `which` and `switch`.
    fn signal(inputs: i64) -> Harness {
        let connected: Vec<_> = (0..inputs as usize).map(|i| (i, MONO)).collect();
        Harness::new(&Selector, &config(inputs, 0), &connected, 48_000.0, FRAMES).unwrap()
    }

    /// Fills each input with its 1-based number, so the output says which was
    /// chosen.
    fn numbered(h: &mut Harness, inputs: usize) {
        for i in 0..inputs {
            h.input(i, FRAMES).fill((i + 1) as f32);
        }
    }

    #[test]
    fn selects_the_input_which_names() {
        let mut h = signal(3);
        numbered(&mut h, 3);
        for (which, want) in [(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)] {
            h.set(3, which);
            h.run(FRAMES).unwrap();
            assert_eq!(h.output(OUT).lane(0, 0), &[want; FRAMES]);
        }
    }

    #[test]
    fn which_rounds_and_clamps_to_an_input() {
        let mut h = signal(3);
        numbered(&mut h, 3);
        for (which, want) in [
            (-5.0, 1.0),
            (0.4, 1.0),
            (0.5, 2.0),
            (1.49, 2.0),
            (1.5, 3.0),
            (99.0, 3.0),
            (f32::NAN, 1.0),
        ] {
            h.set(3, which);
            h.run(FRAMES).unwrap();
            assert_eq!(h.output(OUT).lane(0, 0), &[want; FRAMES], "which {which}");
        }
    }

    #[test]
    fn a_wired_which_selects_each_sample() {
        let connected = [(0, MONO), (1, MONO), (2, MONO)];
        let mut h = Harness::new(&Selector, &config(2, 0), &connected, 48_000.0, 4).unwrap();
        h.input(0, 4).fill(1.0);
        h.input(1, 4).fill(2.0);
        // The last value rounds to input 2 and the last clamps to it.
        h.input(2, 4)
            .lane_mut(0, 0)
            .copy_from_slice(&[0.0, 1.0, 1.4, 5.0]);
        h.run(4).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[1.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn zero_crossing_waits_for_the_old_input_to_cross() {
        // Input 1 is a ramp through zero at frame 30; input 2 is constant. The
        // first block leaves the old input on a negative sample, so the switch
        // at frame 10 must wait for the crossing.
        let connected = [(0, MONO), (1, MONO), (2, MONO)];
        let mut h = Harness::new(&Selector, &config(2, 0), &connected, 48_000.0, FRAMES).unwrap();
        h.set(3, 1.0);
        h.input(1, FRAMES).fill(2.0);
        h.input(0, FRAMES).fill(-1.0);
        h.input(2, FRAMES).fill(0.0);
        h.run(FRAMES).unwrap();

        let ramp: Vec<f32> = (0..FRAMES).map(|i| i as f32 - 30.0).collect();
        h.input(0, FRAMES).lane_mut(0, 0).copy_from_slice(&ramp);
        let mut which = h.input(2, FRAMES);
        which.fill(0.0);
        which.lane_mut(0, 0)[10..].fill(1.0);
        h.run(FRAMES).unwrap();

        let out = h.output(OUT).lane(0, 0);
        assert_eq!(out[9], -21.0, "still on the old input");
        assert_eq!(out[10], -20.0, "not switched at the choice change");
        assert_eq!(out[29], -1.0);
        assert_eq!(out[30], 2.0, "switches at the crossing");
    }

    #[test]
    fn zero_crossing_gives_up_waiting_on_dc() {
        let mut h = signal(2);
        h.input(0, FRAMES).fill(1.0);
        h.input(1, FRAMES).fill(2.0);
        // Settle on input 1 with a nonzero previous sample.
        h.set(2, 0.0);
        h.set(3, 0.0);
        h.run(FRAMES).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[1.0; FRAMES]);

        // A DC input never crosses, so the switch waits out MAX_WAIT_MS.
        h.set(2, 1.0);
        h.set(3, 1.0);
        let wait = (48_000.0 * MAX_WAIT_MS / 1000.0) as usize;
        let mut seen = 0;
        loop {
            h.run(FRAMES).unwrap();
            seen += FRAMES;
            if h.output(OUT).lane(0, 0)[FRAMES - 1] == 2.0 {
                break;
            }
            assert!(seen < wait + 4 * FRAMES, "never switched");
        }
        assert!(seen >= wait, "switched before giving up");
    }

    #[test]
    fn a_mono_input_broadcasts_across_the_output_shape() {
        let poly = Shape::new(3, 2);
        let connected = [(0, poly), (1, MONO)];
        let mut h = Harness::new(&Selector, &config(2, 0), &connected, 48_000.0, FRAMES).unwrap();
        let mut input = h.input(0, FRAMES);
        for voice in 0..3 {
            for channel in 0..2 {
                input
                    .lane_mut(voice, channel)
                    .fill((voice * 2 + channel + 1) as f32);
            }
        }
        h.input(1, FRAMES).fill(-1.0);

        h.set(2, 0.0);
        h.run(FRAMES).unwrap();
        assert_eq!(h.output(OUT).shape(), poly);
        assert_eq!(h.output(OUT).lane(2, 1), &[6.0; FRAMES]);

        // A mono input plays on every voice and channel.
        h.set(2, 1.0);
        h.run(FRAMES).unwrap();
        assert_eq!(h.output(OUT).shape(), poly);
        assert_eq!(h.output(OUT).lane(2, 1), &[-1.0; FRAMES]);
    }

    #[test]
    fn an_unconnected_selected_input_is_silent() {
        // Input 2 is configured but nothing is wired to it.
        let connected = [(0, MONO), (2, MONO)];
        let mut h = Harness::new(&Selector, &config(3, 0), &connected, 48_000.0, FRAMES).unwrap();
        h.input(0, FRAMES).fill(1.0);
        h.input(2, FRAMES).fill(3.0);

        h.set(3, 1.0);
        h.run(FRAMES).unwrap();
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 0.0));

        // The other inputs still pass while selected.
        h.set(3, 2.0);
        h.run(FRAMES).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[3.0; FRAMES]);
        h.set(3, 0.0);
        h.run(FRAMES).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[1.0; FRAMES]);
    }

    #[test]
    fn the_default_is_two_named_inputs() {
        let layout = Selector.layout(&Config::new()).unwrap();
        assert_eq!(layout.inputs.len(), 4, "in1, in2, which and switch");
        assert_eq!(layout.outputs.len(), 1);
        assert_eq!(layout.memory, Memory::Bounded(MAX_WAIT_MS as u32));
        let which = layout.inputs.iter().find(|port| port.key == WHICH).unwrap();
        let InputKind::Param(info) = &which.kind else {
            panic!("which should be a parameter");
        };
        assert_eq!(info.max, 1.0);
        let ParamKind::Stepped { labels } = &info.kind else {
            panic!("which should be stepped");
        };
        assert_eq!(labels.as_slice(), ["In 1", "In 2"]);
        let switch = layout
            .inputs
            .iter()
            .find(|port| port.key == SWITCH)
            .unwrap();
        let InputKind::Param(info) = &switch.kind else {
            panic!("switch should be a parameter");
        };
        assert_eq!(info.max, 1.0, "Instant or Zero crossing");
    }

    #[test]
    fn shapes_follow_the_kind() {
        let signal = config(2, 0);
        let layout = Selector.layout(&signal).unwrap();
        let poly = Shape::new(2, 1);
        let inputs = [MONO, poly, MONO, MONO];
        assert_eq!(
            Selector.output_shapes(&signal, &layout, &inputs).unwrap(),
            [poly]
        );

        let notes = config(2, 1);
        let layout = Selector.layout(&notes).unwrap();
        assert_eq!(layout.memory, Memory::Unbounded, "a held note can last");
        assert!(
            Selector
                .output_shapes(&notes, &layout, &[MONO, MONO])
                .unwrap()
                .is_empty(),
            "notes have no signal outputs"
        );

        let samples = config(2, 2);
        let layout = Selector.layout(&samples).unwrap();
        assert_eq!(layout.memory, Memory::None);
        assert_eq!(
            Selector
                .output_shapes(&samples, &layout, &[poly, MONO])
                .unwrap(),
            [Shape::MONO],
            "the choice signal is mono"
        );

        let spectral = config(2, 3);
        let layout = Selector.layout(&spectral).unwrap();
        assert_eq!(layout.memory, Memory::None);
        assert_eq!(
            Selector
                .output_shapes(&spectral, &layout, &[MONO, poly, MONO])
                .unwrap(),
            [poly]
        );
    }

    #[test]
    fn rejects_a_bad_input_count() {
        assert!(matches!(
            Selector.layout(&config(1, 0)),
            Err(NodeError::Config(_))
        ));
        assert!(matches!(
            Selector.layout(&config(65, 0)),
            Err(NodeError::Config(_))
        ));
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

    /// A note selector with `inputs` event inputs and `which` wired, so a
    /// block can switch mid-way.
    fn notes(inputs: i64) -> Harness {
        Harness::new(
            &Selector,
            &config(inputs, 1),
            &[(0, MONO)],
            48_000.0,
            FRAMES,
        )
        .unwrap()
    }

    #[test]
    fn notes_follow_the_chosen_input() {
        let mut h = notes(2);
        h.input(0, FRAMES).fill(1.0);
        h.send_events(0, &[on(1, 1), on(3, 4)]);
        h.send_events(
            1,
            &[
                on(2, 2),
                Event {
                    time: 4,
                    kind: EventKind::Midi([0xB0, 1, 64]),
                },
            ],
        );
        h.run(FRAMES).unwrap();
        assert_eq!(
            h.events(0),
            [
                on(2, 2),
                Event {
                    time: 4,
                    kind: EventKind::Midi([0xB0, 1, 64]),
                }
            ]
        );
    }

    #[test]
    fn finish_held_lets_note_offs_through_but_closes_new_note_ons() {
        let mut h = notes(2);
        h.input(0, FRAMES).fill(1.0);
        h.send_events(1, &[on(1, 2)]);
        h.run(FRAMES).unwrap();
        assert_eq!(h.events(0), [on(1, 2)]);

        // Switch to input 1 at frame 5; input 2's held note finishes, and
        // input 1's new note-ons pass.
        let mut which = h.input(0, FRAMES);
        which.fill(1.0);
        which.lane_mut(0, 0)[5..].fill(0.0);
        h.send_events(0, &[on(8, 1)]);
        h.send_events(1, &[on(2, 3), off(10, 2)]);
        h.run(FRAMES).unwrap();
        let out = h.events(0).to_vec();
        assert_eq!(out, [on(2, 3), on(8, 1), off(10, 2)]);
    }

    #[test]
    fn cut_held_ends_notes_at_the_switch() {
        let mut h = notes(2);
        h.set(1, 1.0);
        h.input(0, FRAMES).fill(1.0);
        h.send_events(1, &[on(1, 2)]);
        h.run(FRAMES).unwrap();

        let mut which = h.input(0, FRAMES);
        which.fill(1.0);
        which.lane_mut(0, 0)[5..].fill(0.0);
        h.send_events(0, &[on(8, 1)]);
        h.send_events(1, &[off(10, 2)]);
        h.run(FRAMES).unwrap();
        assert_eq!(h.events(0), [off(5, 2), on(8, 1)]);
    }

    #[test]
    fn a_sample_selector_names_its_candidates_and_hides_its_pick() {
        let samples = config(3, 2);
        let chooser = Selector.sample_chooser(&samples).unwrap();
        assert_eq!(chooser.output, "out");
        assert_eq!(chooser.inputs, ["in1", "in2", "in3"]);
        let layout = Selector.layout(&samples).unwrap();
        assert!(layout.outputs.iter().any(|p| p.key == chooser.index));
        assert_eq!(layout.sample_inputs.len(), 3);
        assert!(
            Selector.sample_chooser(&config(3, 0)).is_none(),
            "only samples are chosen"
        );
    }

    #[test]
    fn samples_put_the_chosen_input_on_the_choice_signal() {
        let mut h = Harness::new(&Selector, &config(2, 2), &[(0, MONO)], 48_000.0, FRAMES).unwrap();
        h.input(0, FRAMES).fill(1.0);
        h.run(FRAMES).unwrap();
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 1.0));

        // Cut held adds the flag the Sampler reads.
        h.set(1, 1.0);
        h.run(FRAMES).unwrap();
        assert!(
            h.output(OUT)
                .lane(0, 0)
                .iter()
                .all(|&x| x == 1.0 + CHOICE_CUT)
        );
    }

    #[test]
    fn spectra_pass_the_chosen_inputs_frames() {
        use noodle_engine::Bin;
        const BLOCK: usize = 4096;
        let config = config(2, 3);
        let mut h = Harness::with_spectral(
            &Selector,
            &config,
            &[(0, MONO)],
            &[MONO, MONO],
            48_000.0,
            BLOCK,
        )
        .unwrap();
        let mut which = h.input(0, BLOCK);
        which.fill(0.0);
        which.lane_mut(0, 0)[1500..].fill(1.0);
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
