use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use noodle_core::{
    Command, Config, Connection, Endpoint, Node as ProjectNode, Project, TempoMap, Value,
};

use super::*;
use crate::{
    ConfigInfo, Event, EventKind, Instance, Io, Lane, LaneKernel, Layout, Node, NodeError,
    NodeInfo, NodeType, NoteId, OUTPUT_ID, ParamInfo, PerLane, Problem, Setup, Shape,
};

/// Smoothing for the `offset` parameter: 4 ms, which is 4 samples at the test
/// sample rate.
const SETTINGS: Settings = Settings {
    sample_rate: 1000.0,
    max_frames: 4,
    channels: 1,
};

/// The fade around a plan that changes what's audible: 5 ms, which is 5
/// samples at the test sample rate.
const FADE: usize = 5;

const fn info(id: &'static str) -> NodeInfo {
    NodeInfo {
        id,
        version: 1,
        name: id,
        category: "Test",
    }
}

/// Outputs 0, 1, 2, … (or from config `start`), so its state shows whether it
/// survived a plan swap.
struct Counter;

const START: ConfigInfo = ConfigInfo::int("start", "Start", 0);

impl NodeType for Counter {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("counter");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(CounterNode {
            next: START.get_int(setup.config) as f32,
        }))
    }
}

struct CounterNode {
    next: f32,
}

impl Node for CounterNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let lane = io.outputs[0].lane_mut(0, 0);
        for sample in lane.iter_mut() {
            *sample = self.next;
            self.next += 1.0;
        }
        debug_assert_eq!(lane.len(), ctx.frames);
    }
}

/// Outputs its input plus a smoothed `offset` parameter.
struct Offset;

impl NodeType for Offset {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("offset");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .input("in", "In")
            .param(
                "offset",
                "Offset",
                ParamInfo::new(-10.0, 10.0, 0.0).smoothing(4.0),
            )
            .output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(OffsetKernel, setup)))
    }
}

struct OffsetKernel;

impl LaneKernel for OffsetKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let (input, offset) = (lane.inputs.get(0), lane.inputs.get(1));
        for ((o, x), d) in lane.outputs.get_mut(0).iter_mut().zip(input).zip(offset) {
            *o = x + d;
        }
    }
}

/// Counts how many of its instances have been dropped.
struct Droppable(Arc<AtomicUsize>);

impl NodeType for Droppable {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("droppable");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(DroppableNode(Arc::clone(&self.0))))
    }
}

struct DroppableNode(Arc<AtomicUsize>);

impl Node for DroppableNode {
    fn process(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}
}

impl Drop for DroppableNode {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Always fails to instantiate.
struct Failing;

impl NodeType for Failing {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("failing");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Err(NodeError::config("no instance for you"))
    }
}

/// Two voices, holding 1.0 and 2.0.
struct TwoVoices;

impl NodeType for TwoVoices {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("two_voices");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        _inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(vec![Shape::new(2, 1)])
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(TwoVoicesNode))
    }
}

struct TwoVoicesNode;

impl Node for TwoVoicesNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        io.outputs[0].lane_mut(0, 0).fill(1.0);
        io.outputs[0].lane_mut(1, 0).fill(2.0);
    }
}

/// Fails to instantiate the first time, then outputs 7.
struct Flaky(AtomicUsize);

impl NodeType for Flaky {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("flaky");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        match self.0.fetch_add(1, Ordering::SeqCst) {
            0 => Err(NodeError::config("not yet")),
            _ => Ok(Instance::realtime(Constant(7.0))),
        }
    }
}

struct Constant(f32);

impl Node for Constant {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        io.outputs[0].fill(self.0);
    }
}

/// Emits one note per block: key 1 at frame 0 in the first block, key 2 at
/// frame 1 in the second, and so on.
struct NoteSource;

impl NodeType for NoteSource {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("note_source");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().event_output("notes", "Notes"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(NoteSourceNode { block: 0 }))
    }
}

struct NoteSourceNode {
    block: u32,
}

impl Node for NoteSourceNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let event = Event {
            time: self.block % ctx.frames as u32,
            kind: EventKind::NoteOn {
                note: NoteId(self.block),
                channel: 0,
                key: self.block as u8 + 1,
                velocity: 1.0,
            },
        };
        io.event_outputs[0].push(event).unwrap();
        self.block += 1;
    }
}

/// Passes notes through on output `a`, and on `b` with their keys doubled.
struct NoteThru;

impl NodeType for NoteThru {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("note_thru");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .event_input("notes", "Notes")
            .event_output("a", "A")
            .event_output("b", "B"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(NoteThruNode))
    }
}

struct NoteThruNode;

impl Node for NoteThruNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let [a, b] = io.event_outputs else {
            unreachable!()
        };
        for event in io.event_inputs[0] {
            a.push(*event).unwrap();
            let mut doubled = *event;
            if let EventKind::NoteOn { key, .. } = &mut doubled.kind {
                *key *= 2;
            }
            b.push(doubled).unwrap();
        }
    }
}

/// Turns notes into audio: each note adds `key × scale` at its frame.
struct NoteProbe;

const SCALE: ConfigInfo = ConfigInfo::int("scale", "Scale", 1);

impl NodeType for NoteProbe {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("note_probe");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .event_input("notes", "Notes")
            .output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let scale = SCALE.get_int(setup.config) as f32;
        Ok(Instance::realtime(NoteProbeNode(scale)))
    }
}

struct NoteProbeNode(f32);

impl Node for NoteProbeNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let out = io.outputs[0].lane_mut(0, 0);
        out.fill(0.0);
        for event in io.event_inputs[0] {
            if let EventKind::NoteOn { key, .. } = event.kind {
                out[event.time as usize] += key as f32 * self.0;
            }
        }
    }
}

/// Outputs a subnormal float, computed at run time.
struct Subnormal;

impl NodeType for Subnormal {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("subnormal");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(SubnormalNode))
    }
}

struct SubnormalNode;

impl Node for SubnormalNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let tiny = std::hint::black_box(f32::MIN_POSITIVE) * std::hint::black_box(0.25);
        io.outputs[0].fill(tiny);
    }
}

/// Outputs what the transport says, chosen by config `what`.
struct Probe;

const WHAT: ConfigInfo = ConfigInfo::int("what", "What", 0);

/// The values `what` can take.
const POSITION: i64 = 0;
const TICK: i64 = 1;
const BPM: i64 = 2;
const PLAYING: i64 = 3;
const SINCE_RESET: i64 = 4;

impl NodeType for Probe {
    fn info(&self) -> &NodeInfo {
        static INFO: NodeInfo = info("probe");
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ProbeNode {
            what: WHAT.get_int(setup.config),
            since_reset: 0,
        }))
    }
}

struct ProbeNode {
    what: i64,
    since_reset: u32,
}

impl Node for ProbeNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let transport = ctx.transport;
        for (i, sample) in io.outputs[0].lane_mut(0, 0).iter_mut().enumerate() {
            *sample = match self.what {
                POSITION => (transport.position + i as u64) as f32,
                TICK => transport.tick as f32,
                BPM => transport.bpm as f32,
                PLAYING => f32::from(u8::from(transport.playing)),
                _ => (self.since_reset + i as u32) as f32,
            };
        }
        self.since_reset += ctx.frames as u32;
    }

    fn reset(&mut self) {
        self.since_reset = 0;
    }
}

fn registry(dropped: &Arc<AtomicUsize>) -> Registry {
    let mut registry = Registry::with_builtins();
    registry.register(Counter);
    registry.register(Offset);
    registry.register(Droppable(Arc::clone(dropped)));
    registry.register(Failing);
    registry.register(Flaky(AtomicUsize::new(0)));
    registry.register(TwoVoices);
    registry.register(NoteSource);
    registry.register(NoteThru);
    registry.register(NoteProbe);
    registry.register(Subnormal);
    registry.register(Probe);
    registry
}

struct Rig {
    project: Project,
    registry: Registry,
    controller: Controller,
    processor: Processor,
    dropped: Arc<AtomicUsize>,
}

impl Rig {
    fn new(settings: Settings) -> Self {
        let dropped = Arc::new(AtomicUsize::new(0));
        let (controller, processor) = engine(settings).unwrap();
        Self {
            project: Project::new(),
            registry: registry(&dropped),
            controller,
            processor,
            dropped,
        }
    }

    fn add(&mut self, type_id: &str) -> NodeId {
        let id = self.project.new_node_id();
        self.edit(Command::AddNode {
            id,
            node: ProjectNode::new(type_id),
        });
        id
    }

    fn wire(&mut self, from: NodeId, to: NodeId, to_port: &str) {
        self.connect(from, "out", to, to_port);
    }

    fn connect(&mut self, from: NodeId, from_port: &str, to: NodeId, to_port: &str) {
        self.edit(Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        }));
    }

    fn edit(&mut self, command: Command) {
        command.apply(&mut self.project).unwrap();
    }

    fn update(&mut self) -> Vec<Diagnostic> {
        let lanes: Vec<_> = self.project.lanes().collect();
        self.controller
            .update_with_lanes(self.project.graph(), &lanes, &self.registry)
    }

    fn add_lane(&mut self, target: Endpoint, points: &[(i64, f32, noodle_core::Curve)]) {
        let id = self.project.new_lane_id();
        let points = points
            .iter()
            .map(|&(tick, value, curve)| noodle_core::AutomationPoint {
                tick: noodle_core::Tick(tick),
                value,
                curve,
            })
            .collect();
        self.edit(Command::AddLane {
            id,
            lane: noodle_core::AutomationLane::new(target, points),
        });
    }

    fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut output = vec![f32::NAN; frames * self.processor.settings().channels];
        self.processor.process(&mut output);
        output
    }
}

/// counter → output, compiled and sent.
fn counter_rig() -> (Rig, NodeId) {
    let mut rig = Rig::new(SETTINGS);
    let counter = rig.add("counter");
    let output = rig.add(OUTPUT_ID);
    rig.wire(counter, output, "in");
    assert!(rig.update().is_empty());
    (rig, counter)
}

/// offset → output, compiled and sent.
fn offset_rig() -> (Rig, NodeId) {
    let mut rig = Rig::new(SETTINGS);
    let offset = rig.add("offset");
    let output = rig.add(OUTPUT_ID);
    rig.wire(offset, output, "in");
    assert!(rig.update().is_empty());
    (rig, offset)
}

#[test]
fn silent_until_the_first_plan_arrives() {
    let mut rig = Rig::new(SETTINGS);
    assert_eq!(rig.render(6), [0.0; 6]);
}

#[test]
fn plays_the_output_node_in_blocks() {
    let mut rig = Rig::new(Settings {
        channels: 2,
        ..SETTINGS
    });
    let counter = rig.add("counter");
    let output = rig.add(OUTPUT_ID);
    rig.wire(counter, output, "in");
    rig.update();
    // Six frames is a block of four and then two; mono goes to both channels.
    assert_eq!(
        rig.render(6),
        [0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0, 5.0, 5.0]
    );
}

#[test]
fn unchanged_nodes_keep_their_state_across_updates() {
    let (mut rig, _) = counter_rig();
    assert_eq!(rig.render(4), [0.0, 1.0, 2.0, 3.0]);
    rig.add("counter");
    rig.update();
    assert_eq!(rig.render(4), [4.0, 5.0, 6.0, 7.0]);
}

#[test]
fn changing_config_rebuilds_the_node() {
    let (mut rig, counter) = counter_rig();
    rig.render(4);
    rig.edit(Command::SetConfig {
        node: counter,
        key: "start".into(),
        value: Some(Value::Int(100)),
    });
    rig.update();
    // The old counter fades out, then the new one fades in from 100.
    rig.render(2 * FADE);
    assert_eq!(rig.render(2), [105.0, 106.0]);
}

#[test]
fn a_plan_that_changes_the_output_fades_out_then_in() {
    let (mut rig, counter) = counter_rig();
    assert_eq!(rig.render(2), [0.0, 1.0]);
    rig.edit(Command::SetConfig {
        node: counter,
        key: "start".into(),
        value: Some(Value::Int(100)),
    });
    rig.update();
    // Out over five samples of the old counter, which ends silent, then in
    // over five of the new one. Gains are multiples of 1/5.
    let gains = [4, 3, 2, 1, 0, 1, 2, 3, 4, 5, 5];
    let values = [2, 3, 4, 5, 6, 100, 101, 102, 103, 104, 105];
    let expected: Vec<f32> = gains
        .iter()
        .zip(values)
        .map(|(&g, v)| g as f32 / FADE as f32 * v as f32)
        .collect();
    // In odd lengths, so the fade crosses block and call boundaries.
    let mut actual = rig.render(3);
    actual.extend(rig.render(7));
    actual.extend(rig.render(1));
    assert_eq!(actual, expected);
}

#[test]
fn rewiring_the_output_fades() {
    let mut rig = Rig::new(SETTINGS);
    let counter = rig.add("counter");
    let offset = rig.add("offset");
    let output = rig.add(OUTPUT_ID);
    rig.wire(counter, output, "in");
    rig.wire(counter, offset, "in");
    rig.update();
    rig.render(4);
    // Every node carries over; only the wire into the output changes.
    rig.wire(offset, output, "in");
    rig.update();
    let faded = rig.render(FADE);
    assert_eq!(faded[FADE - 1], 0.0, "{faded:?}");
}

#[test]
fn plans_that_leave_the_output_alone_go_in_at_once() {
    let (mut rig, offset) = offset_rig();
    // An unconnected node, and a value changed in the project.
    rig.add("counter");
    rig.edit(Command::SetParam {
        node: offset,
        key: "offset".into(),
        value: Some(2.0),
    });
    rig.update();
    assert_eq!(rig.render(4), [0.5, 1.0, 1.5, 2.0]);
}

#[test]
fn a_plan_that_arrives_mid_fade_goes_in_with_the_one_that_started_it() {
    let (mut rig, counter) = counter_rig();
    rig.render(4);
    rig.edit(Command::SetConfig {
        node: counter,
        key: "start".into(),
        value: Some(Value::Int(100)),
    });
    rig.update();
    rig.render(2);
    rig.edit(Command::SetConfig {
        node: counter,
        key: "start".into(),
        value: Some(Value::Int(200)),
    });
    rig.update();
    // The first fade carries on to silence, and both plans go in there.
    let out = rig.render(3 + FADE + 1);
    assert_eq!(out[2], 0.0);
    assert_eq!(out[3 + FADE], 205.0);
}

#[test]
fn parameter_changes_are_smoothed() {
    let (mut rig, offset) = offset_rig();
    assert_eq!(rig.render(2), [0.0, 0.0]);
    rig.controller.set_param(offset, "offset", 1.0);
    assert_eq!(rig.render(6), [0.25, 0.5, 0.75, 1.0, 1.0, 1.0]);
}

#[test]
fn a_ramp_carries_on_across_a_plan_swap() {
    let (mut rig, offset) = offset_rig();
    // As the UI would: record the value in the project and send it.
    rig.edit(Command::SetParam {
        node: offset,
        key: "offset".into(),
        value: Some(1.0),
    });
    rig.controller.set_param(offset, "offset", 1.0);
    assert_eq!(rig.render(2), [0.25, 0.5]);
    rig.add("counter");
    rig.update();
    assert_eq!(rig.render(3), [0.75, 1.0, 1.0]);
}

#[test]
fn updating_applies_values_changed_in_the_project() {
    // E.g. undoing a parameter change, which only touches the project.
    let (mut rig, offset) = offset_rig();
    rig.edit(Command::SetParam {
        node: offset,
        key: "offset".into(),
        value: Some(-2.0),
    });
    rig.update();
    assert_eq!(rig.render(5), [-0.5, -1.0, -1.5, -2.0, -2.0]);
}

#[test]
fn removed_nodes_are_freed_by_the_controller_not_the_processor() {
    let mut rig = Rig::new(SETTINGS);
    let node = rig.add("droppable");
    rig.update();
    rig.render(4);

    rig.edit(Command::RemoveNode { id: node });
    rig.update();
    rig.render(4);
    assert_eq!(
        rig.dropped.load(Ordering::SeqCst),
        0,
        "dropped on the audio thread"
    );

    rig.controller.maintain();
    assert_eq!(rig.dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn a_plan_waits_when_the_queue_is_full() {
    let (mut rig, counter) = counter_rig();
    // Far more updates than the queue holds, with nothing being rendered.
    for start in 1..=10 {
        rig.edit(Command::SetConfig {
            node: counter,
            key: "start".into(),
            value: Some(Value::Int(start)),
        });
        rig.update();
    }
    // Each plan rebuilds the counter, so the processor fades out first.
    rig.render(FADE);
    rig.controller.maintain();
    // The latest plan arrives, and takes over correctly from the last one the
    // processor installed.
    rig.render(FADE);
    assert_eq!(rig.render(2), [15.0, 16.0]);
}

#[test]
fn a_node_that_fails_to_instantiate_is_reported_and_silent() {
    let mut rig = Rig::new(SETTINGS);
    let failing = rig.add("failing");
    let output = rig.add(OUTPUT_ID);
    rig.wire(failing, output, "in");
    let diagnostics = rig.update();
    assert_eq!(
        diagnostics,
        [Diagnostic::node(
            failing,
            crate::Problem::Node(NodeError::config("no instance for you"))
        )]
    );
    assert_eq!(rig.render(4), [0.0; 4]);
}

#[test]
fn voices_are_summed_into_every_channel() {
    let mut rig = Rig::new(Settings {
        channels: 2,
        ..SETTINGS
    });
    let voices = rig.add("two_voices");
    let output = rig.add(OUTPUT_ID);
    rig.wire(voices, output, "in");
    rig.update();
    assert_eq!(rig.render(2), [3.0; 4]);
}

#[test]
fn a_failed_node_is_retried_and_reported_until_it_works() {
    let mut rig = Rig::new(SETTINGS);
    let flaky = rig.add("flaky");
    let output = rig.add(OUTPUT_ID);
    rig.wire(flaky, output, "in");
    let failed = [Diagnostic::node(
        flaky,
        crate::Problem::Node(NodeError::config("not yet")),
    )];
    assert_eq!(rig.update(), failed);
    assert_eq!(rig.render(2), [0.0; 2]);

    // Nothing in the graph changed, but the node is tried again. It's new on
    // the output's path, so it fades in.
    assert!(rig.update().is_empty());
    rig.render(2 * FADE);
    assert_eq!(rig.render(2), [7.0; 2]);
}

#[test]
fn a_failed_node_on_the_output_path_does_not_fade_every_update() {
    let mut rig = Rig::new(SETTINGS);
    let failing = rig.add("failing");
    let offset = rig.add("offset");
    let output = rig.add(OUTPUT_ID);
    rig.wire(failing, offset, "in");
    rig.wire(offset, output, "in");
    rig.update();
    rig.render(2);
    // An edit elsewhere: the failed node is retried, but it's still silent.
    rig.add("counter");
    rig.edit(Command::SetParam {
        node: offset,
        key: "offset".into(),
        value: Some(2.0),
    });
    rig.update();
    assert_eq!(rig.render(4), [0.5, 1.0, 1.5, 2.0]);
}

#[test]
fn a_failure_is_reported_on_every_update() {
    let mut rig = Rig::new(SETTINGS);
    rig.add("failing");
    let first = rig.update();
    assert_eq!(first.len(), 1);
    assert_eq!(rig.update(), first);
}

#[test]
fn events_reach_every_consumer_and_are_cleared_each_block() {
    let mut rig = Rig::new(SETTINGS);
    let source = rig.add("note_source");
    let thru = rig.add("note_thru");
    // Each probe feeds its own Output node; Output nodes are summed.
    let probe = |rig: &mut Rig, scale: i64| {
        let id = rig.project.new_node_id();
        let config = Config::new().with("scale", Value::Int(scale));
        rig.edit(Command::AddNode {
            id,
            node: ProjectNode::new("note_probe").with_config(config),
        });
        let output = rig.add(OUTPUT_ID);
        rig.wire(id, output, "in");
        id
    };
    let via_a = probe(&mut rig, 1);
    let via_b = probe(&mut rig, 100);
    let direct = probe(&mut rig, 10_000);
    rig.connect(source, "notes", thru, "notes");
    rig.connect(thru, "a", via_a, "notes");
    rig.connect(thru, "b", via_b, "notes");
    // Fan-out: the source is read by both `thru` and `direct`.
    rig.connect(source, "notes", direct, "notes");
    assert!(rig.update().is_empty());

    // Key k reaches `via_a` as k, `via_b` as 2k and `direct` as k, so the
    // total is k + 200k + 10000k = 10201k. Leftover events from an earlier
    // block, or events reaching the wrong consumer, would change it.
    assert_eq!(
        rig.render(8),
        [10201.0, 0.0, 0.0, 0.0, 0.0, 20402.0, 0.0, 0.0]
    );
}

#[test]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn subnormals_are_flushed_while_processing() {
    let mut rig = Rig::new(SETTINGS);
    let subnormal = rig.add("subnormal");
    let output = rig.add(OUTPUT_ID);
    rig.wire(subnormal, output, "in");
    rig.update();
    assert_eq!(rig.render(2), [0.0; 2]);
    // The thread's own mode is back afterwards.
    let tiny = std::hint::black_box(f32::MIN_POSITIVE) * std::hint::black_box(0.25);
    assert!(tiny.is_subnormal());
}

#[test]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn subnormals_are_flushed_while_processing_with_input() {
    // Live sessions with input take this path, not `process`.
    let mut rig = Rig::new(SETTINGS);
    let subnormal = rig.add("subnormal");
    let output = rig.add(OUTPUT_ID);
    rig.wire(subnormal, output, "in");
    rig.update();
    assert_eq!(rig.render_input(&[0.0, 0.0], 1), [0.0; 2]);
}

#[test]
fn non_finite_parameter_values_are_ignored() {
    let (mut rig, offset) = offset_rig();
    rig.controller.set_param(offset, "offset", f32::INFINITY);
    assert_eq!(rig.render(2), [0.0; 2]);
    rig.controller.set_param(offset, "offset", f32::NAN);
    assert_eq!(rig.render(2), [0.0; 2]);
    rig.controller.set_param(offset, "offset", 1.0);
    assert_eq!(rig.render(4), [0.25, 0.5, 0.75, 1.0]);
}

#[test]
fn invalid_settings_are_rejected() {
    let with = |change: fn(&mut Settings)| {
        let mut settings = SETTINGS;
        change(&mut settings);
        engine(settings).err()
    };
    assert_eq!(with(|s| s.channels = 0), Some(SettingsError::Channels));
    assert_eq!(with(|s| s.max_frames = 0), Some(SettingsError::MaxFrames));
    assert_eq!(
        with(|s| s.sample_rate = 0.0),
        Some(SettingsError::SampleRate(0.0))
    );
    assert!(matches!(
        with(|s| s.sample_rate = f32::NAN),
        Some(SettingsError::SampleRate(_))
    ));
    assert!(with(|_| {}).is_none());
}

/// input (with `channels` channels) → output, on a stereo engine.
fn input_rig(channels: i64) -> Rig {
    let mut rig = Rig::new(Settings {
        channels: 2,
        ..SETTINGS
    });
    let input = rig.project.new_node_id();
    rig.edit(Command::AddNode {
        id: input,
        node: ProjectNode::new(crate::INPUT_ID)
            .with_config(Config::new().with("channels", Value::Int(channels))),
    });
    let output = rig.add(OUTPUT_ID);
    rig.wire(input, output, "in");
    assert!(rig.update().is_empty());
    rig
}

impl Rig {
    fn render_input(&mut self, input: &[f32], input_channels: usize) -> Vec<f32> {
        let frames = input.len() / input_channels;
        let mut output = vec![f32::NAN; frames * self.processor.settings().channels];
        self.processor
            .process_with_input(input, input_channels, &mut output);
        output
    }
}

#[test]
fn an_input_node_plays_the_device_input_across_blocks() {
    let mut rig = input_rig(2);
    // Six frames is a block of four and then two.
    let input: Vec<f32> = (0..12).map(|x| x as f32).collect();
    assert_eq!(rig.render_input(&input, 2), input);
}

#[test]
fn a_mono_device_feeds_every_input_channel() {
    let mut rig = input_rig(2);
    assert_eq!(
        rig.render_input(&[1.0, 2.0, 3.0], 1),
        [1.0, 1.0, 2.0, 2.0, 3.0, 3.0]
    );
}

#[test]
fn input_channels_the_device_lacks_are_silent() {
    // A stereo node on a four-channel device takes the first two channels.
    let mut rig = input_rig(2);
    assert_eq!(
        rig.render_input(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], 4),
        [1.0, 2.0, 5.0, 6.0]
    );
    // A three-channel node on a stereo device: the third channel is silent,
    // so it doesn't reach the stereo output either way. Check it directly.
    let mut signal = vec![f32::NAN; 3 * 2];
    let mut out = crate::SignalOut::new(&mut signal, Shape::new(1, 3), 2);
    let input = Interleaved {
        samples: &[1.0, 2.0, 3.0, 4.0],
        channels: 2,
    };
    crate::plan::read_input(input, &mut out);
    assert_eq!(signal, [1.0, 3.0, 2.0, 4.0, 0.0, 0.0]);
}

#[test]
fn without_input_an_input_node_is_silent() {
    let mut rig = input_rig(2);
    assert_eq!(rig.render(3), [0.0; 6]);
}

#[test]
fn an_input_node_needs_a_sensible_channel_count() {
    for channels in [0, -1, crate::MAX_INPUT_CHANNELS as i64 + 1] {
        let mut rig = Rig::new(SETTINGS);
        let input = rig.project.new_node_id();
        rig.edit(Command::AddNode {
            id: input,
            node: ProjectNode::new(crate::INPUT_ID)
                .with_config(Config::new().with("channels", Value::Int(channels))),
        });
        assert_eq!(rig.update().len(), 1, "{channels} channels");
    }
}

// The transport.

/// A probe reporting `what`, wired to the output.
fn probe_rig(what: i64) -> Rig {
    let mut rig = Rig::new(SETTINGS);
    let probe = rig.add("probe");
    rig.edit(Command::SetConfig {
        node: probe,
        key: "what".into(),
        value: Some(Value::Int(what)),
    });
    let output = rig.add(OUTPUT_ID);
    rig.wire(probe, output, "in");
    assert!(rig.update().is_empty());
    rig
}

fn ticks(n: i64) -> noodle_core::Tick {
    noodle_core::Tick(n)
}

#[test]
fn a_new_transport_plays_and_counts_samples() {
    let mut rig = probe_rig(POSITION);
    let want: Vec<f32> = (0..10).map(|i| i as f32).collect();
    assert_eq!(rig.render(10), want);
    assert_eq!(rig.controller.transport().position(), 10);
}

#[test]
fn a_stopped_transport_holds_its_position_but_the_graph_keeps_rendering() {
    let mut rig = probe_rig(PLAYING);
    let transport = rig.controller.transport();
    assert_eq!(rig.render(4), [1.0; 4]);
    transport.stop();
    assert!(!transport.is_playing());
    assert_eq!(rig.render(8), [0.0; 8], "nodes are told it has stopped");
    assert_eq!(transport.position(), 4);
    transport.play();
    assert_eq!(rig.render(4), [1.0; 4]);
    assert_eq!(transport.position(), 8);
}

#[test]
fn seeking_fades_out_jumps_and_fades_back_in_with_nodes_reset() {
    let mut rig = probe_rig(SINCE_RESET);
    let transport = rig.controller.transport();
    rig.render(8);
    // 480 ticks is a quarter of a second at 120 bpm: 250 samples.
    transport.seek(ticks(480));
    let out = rig.render(20);
    // The output fades over FADE frames, jumps at silence, and comes back.
    assert_eq!(out[FADE], 0.0, "silent at the jump");
    // The jump is at frame 5 of this render, so the playhead was 8 + 5 then.
    assert_eq!(transport.position(), 250 + 20 - FADE as u64);
    // The probe was reset at the jump: it counts the frames since.
    assert_eq!(out[19], (20 - FADE - 1) as f32);
}

#[test]
fn a_seek_while_stopped_moves_the_playhead() {
    let mut rig = probe_rig(POSITION);
    let transport = rig.controller.transport();
    rig.render(4);
    transport.stop();
    transport.seek(ticks(960));
    rig.render(20);
    assert_eq!(transport.position(), 500, "a quarter note is half a second");
    transport.play();
    let out = rig.render(20);
    assert_eq!(out[19], 519.0);
}

#[test]
fn seeking_before_the_start_goes_to_the_start() {
    let mut rig = probe_rig(POSITION);
    let transport = rig.controller.transport();
    rig.render(8);
    transport.seek(ticks(-960));
    rig.render(12);
    assert_eq!(transport.position(), 12 - FADE as u64);
}

#[test]
fn a_loop_wraps_exactly_at_its_end() {
    let mut rig = probe_rig(POSITION);
    // 480 ticks is 250 samples; blocks are 4, so the wrap is mid-block.
    rig.controller
        .transport()
        .set_loop(Some((ticks(0), ticks(480))));
    let out = rig.render(600);
    for (i, &x) in out.iter().enumerate() {
        assert_eq!(x, (i % 250) as f32, "frame {i}");
    }
}

#[test]
fn a_loop_can_start_after_the_start() {
    let mut rig = probe_rig(POSITION);
    // 250 samples to 500.
    rig.controller
        .transport()
        .set_loop(Some((ticks(480), ticks(960))));
    let out = rig.render(600);
    assert_eq!(out[499], 499.0);
    assert_eq!(out[500], 250.0);
    assert_eq!(out[599], 349.0);
}

#[test]
fn an_empty_loop_is_no_loop_and_loops_turn_off() {
    let mut rig = probe_rig(POSITION);
    let transport = rig.controller.transport();
    transport.set_loop(Some((ticks(480), ticks(480))));
    assert_eq!(rig.render(300)[299], 299.0);
    transport.set_loop(Some((ticks(0), ticks(960))));
    transport.set_loop(None);
    assert_eq!(rig.render(300)[299], 599.0);
}

#[test]
fn nodes_see_the_musical_position() {
    let mut rig = probe_rig(TICK);
    // 960 ticks a half second at 120 bpm, so a tick is 1000 * 0.5 / 960
    // samples, and the first block starts at tick 0.
    let first = rig.render(4);
    assert_eq!(first[0], 0.0);
    let second = rig.render(4);
    assert!(
        (second[0] - 4.0 * 960.0 / 500.0).abs() < 1e-4,
        "{}",
        second[0]
    );
}

#[test]
fn a_tempo_change_reaches_nodes() {
    let mut rig = probe_rig(BPM);
    assert_eq!(rig.render(4)[0], 120.0);
    let map = TempoMap::constant(90.0, noodle_core::TimeSignature::COMMON).unwrap();
    rig.controller.set_tempo_map(&map);
    let out = rig.render(20);
    assert_eq!(out[19], 90.0);
}

#[test]
fn a_tempo_change_at_the_start_goes_in_without_a_fade() {
    let mut rig = probe_rig(BPM);
    let map = TempoMap::constant(90.0, noodle_core::TimeSignature::COMMON).unwrap();
    rig.controller.set_tempo_map(&map);
    assert_eq!(rig.render(8), [90.0; 8], "no dip, from the first block");
}

#[test]
fn a_tempo_change_keeps_the_playheads_tick() {
    let mut rig = probe_rig(POSITION);
    let transport = rig.controller.transport();
    transport.stop();
    transport.seek(ticks(480));
    rig.render(20);
    assert_eq!(transport.position(), 250);
    // Half the tempo takes twice as long to get to the same tick.
    let map = TempoMap::constant(60.0, noodle_core::TimeSignature::COMMON).unwrap();
    rig.controller.set_tempo_map(&map);
    rig.render(20);
    assert_eq!(transport.position(), 500);
}

#[test]
fn the_tempo_map_the_controller_reports_is_the_last_one_set() {
    let mut rig = probe_rig(BPM);
    assert_eq!(*rig.controller.tempo_map(), TempoMap::default());
    let map = TempoMap::constant(90.0, noodle_core::TimeSignature::COMMON).unwrap();
    rig.controller.set_tempo_map(&map);
    assert_eq!(*rig.controller.tempo_map(), map);
}

#[test]
fn old_tempo_tables_are_freed_by_the_controller() {
    let mut rig = probe_rig(BPM);
    for bpm in [100.0, 110.0, 120.0, 130.0, 140.0, 150.0] {
        let map = TempoMap::constant(bpm, noodle_core::TimeSignature::COMMON).unwrap();
        rig.controller.set_tempo_map(&map);
        rig.render(8);
        rig.controller.maintain();
    }
    assert_eq!(rig.render(8)[7], 150.0);
}

#[test]
fn a_tempo_change_mid_playback_neither_fades_nor_resets_nodes() {
    let mut rig = probe_rig(SINCE_RESET);
    rig.render(8);
    let map = TempoMap::constant(90.0, noodle_core::TimeSignature::COMMON).unwrap();
    rig.controller.set_tempo_map(&map);
    // The probe counts frames since its last reset, and keeps counting. A
    // fade would scale the first frames down.
    let want: Vec<f32> = (8..28).map(|i| i as f32).collect();
    assert_eq!(rig.render(20), want);
}

// At 1000 Hz and 120 bpm a sample is 1.92 ticks, so 1920 ticks is 1000 samples.
mod automation {
    use noodle_core::Curve::{Hold, Linear};

    use crate::{Location, Problem};

    use super::*;

    fn lane_rig(points: &[(i64, f32, noodle_core::Curve)]) -> (Rig, NodeId) {
        let mut rig = Rig::new(SETTINGS);
        let offset = rig.add("offset");
        let output = rig.add(OUTPUT_ID);
        rig.wire(offset, output, "in");
        rig.add_lane(Endpoint::new(offset, "offset"), points);
        (rig, offset)
    }

    #[test]
    fn a_linear_lane_drives_the_parameter_along_the_timeline() {
        let (mut rig, _) = lane_rig(&[(0, 0.0, Linear), (1920, 10.0, Linear)]);
        assert!(rig.update().is_empty());
        let out = rig.render(400);
        assert_eq!(out[0], 0.0);
        assert!((out[100] - 1.0).abs() < 1e-4, "{}", out[100]);
        assert!((out[399] - 3.99).abs() < 1e-3, "{}", out[399]);
    }

    #[test]
    fn a_hold_step_is_ramped_over_the_targets_smoothing() {
        // 4 ms of smoothing is 7.68 ticks, which is 4 samples. The step is
        // at tick 960, which is sample 500.
        let (mut rig, _) = lane_rig(&[(0, 0.0, Hold), (960, 4.0, Hold), (1920, 4.0, Hold)]);
        assert!(rig.update().is_empty());
        let out = rig.render(520);
        assert_eq!(out[499], 0.0);
        assert!((out[502] - 2.0).abs() < 1e-3, "{}", out[502]);
        assert_eq!(out[504], 4.0);
        assert_eq!(out[510], 4.0);
        // No sample jumps by more than a ramp step.
        let biggest = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(biggest < 1.1, "{biggest}");
    }

    #[test]
    fn a_stopped_transport_holds_the_lane_where_it_is() {
        let (mut rig, _) = lane_rig(&[(0, 3.0, Linear), (1920, 10.0, Linear)]);
        assert!(rig.update().is_empty());
        rig.controller.transport().stop();
        assert_eq!(rig.render(20), [3.0; 20]);
    }

    #[test]
    fn a_wire_wins_over_a_lane() {
        let mut rig = Rig::new(SETTINGS);
        let counter = rig.add("counter");
        let offset = rig.add("offset");
        let output = rig.add(OUTPUT_ID);
        rig.wire(counter, offset, "offset");
        rig.wire(offset, output, "in");
        rig.add_lane(Endpoint::new(offset, "offset"), &[(0, 9.0, Linear)]);
        let diagnostics = rig.update();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].problem, Problem::LaneOverridden);
        assert_eq!(
            diagnostics[0].location,
            Location::Wire(Endpoint::new(offset, "offset"))
        );
        assert_eq!(rig.render(3), [0.0, 1.0, 2.0]);
    }

    #[test]
    fn a_lane_on_a_missing_port_or_an_audio_input_is_reported() {
        let (mut rig, offset) = lane_rig(&[(0, 1.0, Linear)]);
        rig.add_lane(Endpoint::new(offset, "nope"), &[(0, 1.0, Linear)]);
        rig.add_lane(Endpoint::new(offset, "in"), &[(0, 1.0, Linear)]);
        let diagnostics = rig.update();
        let problems: Vec<_> = diagnostics.iter().map(|d| d.problem.clone()).collect();
        assert_eq!(
            problems,
            [
                Problem::UnknownPort(Endpoint::new(offset, "nope")),
                Problem::NotAParam("in".into())
            ]
        );
        // The good lane still works.
        assert_eq!(rig.render(2), [1.0, 1.0]);
    }

    #[test]
    fn editing_a_lane_swaps_its_points_without_a_fade() {
        let (mut rig, offset) = lane_rig(&[(0, 1.0, Linear)]);
        assert!(rig.update().is_empty());
        assert_eq!(rig.render(4), [1.0; 4]);
        let id = rig.project.lanes().next().unwrap().0;
        rig.edit(Command::SetLane {
            id,
            lane: noodle_core::AutomationLane::new(
                Endpoint::new(offset, "offset"),
                vec![noodle_core::AutomationPoint {
                    tick: noodle_core::Tick(0),
                    value: 2.0,
                    curve: Linear,
                }],
            ),
        });
        assert!(rig.update().is_empty());
        // The new points apply from the next block, with no dip.
        assert_eq!(rig.render(4), [2.0; 4]);
    }

    #[test]
    fn removing_a_lane_fades_because_the_wiring_changes() {
        let (mut rig, _) = lane_rig(&[(0, 1.0, Linear)]);
        assert!(rig.update().is_empty());
        rig.render(4);
        let id = rig.project.lanes().next().unwrap().0;
        rig.edit(Command::RemoveLane { id });
        assert!(rig.update().is_empty());
        let out = rig.render(4 + 2 * FADE);
        assert_eq!(out[FADE - 1], 0.0, "{out:?}");
    }

    #[test]
    fn the_same_lane_renders_the_same_however_the_blocks_fall() {
        let points = [
            (0, 0.0, Hold),
            (500, 5.0, Hold),
            (1000, 2.0, Linear),
            (1900, 8.0, Hold),
        ];
        let (mut a, _) = lane_rig(&points);
        assert!(a.update().is_empty());
        let whole = a.render(1000);
        let (mut b, _) = lane_rig(&points);
        assert!(b.update().is_empty());
        let mut pieces = Vec::new();
        for n in [1usize, 3, 7, 11, 100, 500, 378] {
            pieces.extend(b.render(n));
        }
        assert_eq!(whole, pieces);
    }
}

/// Two Output nodes, one on the main bus and one tied to `"other"`, fed by
/// different counters, on an engine of two buses: one channel each.
fn two_bus_rig() -> (Rig, NodeId, NodeId) {
    let mut rig = Rig::new(Settings {
        channels: 3,
        ..SETTINGS
    });
    rig.controller
        .set_buses(vec![
            Bus {
                device: "main".into(),
                channels: 2,
            },
            Bus {
                device: "other".into(),
                channels: 1,
            },
        ])
        .unwrap();
    let a = rig.add("counter");
    let b = rig.add("counter");
    rig.edit(Command::SetConfig {
        node: b,
        key: "start".into(),
        value: Some(Value::Int(100)),
    });
    let main = rig.add(OUTPUT_ID);
    let other = rig.add(OUTPUT_ID);
    rig.edit(Command::SetConfig {
        node: other,
        key: "device".into(),
        value: Some(Value::Text("other".into())),
    });
    rig.wire(a, main, "in");
    rig.wire(b, other, "in");
    (rig, main, other)
}

#[test]
fn output_nodes_play_on_the_bus_of_their_device() {
    let (mut rig, _, _) = two_bus_rig();
    assert!(rig.update().is_empty());
    // Main is channels 0 and 1 (a mono signal fills both); "other" is 2.
    assert_eq!(
        rig.render(3),
        [0.0, 0.0, 100.0, 1.0, 1.0, 101.0, 2.0, 2.0, 102.0]
    );
}

#[test]
fn naming_the_main_device_plays_on_the_main_bus() {
    let (mut rig, main, _) = two_bus_rig();
    rig.edit(Command::SetConfig {
        node: main,
        key: "device".into(),
        value: Some(Value::Text("main".into())),
    });
    assert!(rig.update().is_empty());
    assert_eq!(rig.render(1), [0.0, 0.0, 100.0]);
}

#[test]
fn an_output_on_a_device_that_is_not_open_plays_nothing() {
    let (mut rig, _, other) = two_bus_rig();
    rig.edit(Command::SetConfig {
        node: other,
        key: "device".into(),
        value: Some(Value::Text("unplugged".into())),
    });
    let diagnostics = rig.update();
    assert_eq!(
        diagnostics,
        [Diagnostic::node(
            other,
            Problem::DeviceUnavailable("unplugged".into())
        )]
    );
    assert_eq!(rig.render(1), [0.0, 0.0, 0.0]);
}

#[test]
fn only_one_output_may_play_on_a_device() {
    let (mut rig, _, _) = two_bus_rig();
    let second = rig.add(OUTPUT_ID);
    rig.edit(Command::SetConfig {
        node: second,
        key: "device".into(),
        value: Some(Value::Text("other".into())),
    });
    let counter = rig.add("counter");
    rig.wire(counter, second, "in");
    // The lower ID keeps the device; the newer one is ignored.
    assert_eq!(
        rig.update(),
        [Diagnostic::node(
            second,
            Problem::DeviceTaken("other".into())
        )]
    );
    assert_eq!(rig.render(1)[2], 100.0);
}

#[test]
fn unassigned_outputs_all_share_the_main_bus() {
    let (mut rig, _, _) = two_bus_rig();
    let extra = rig.add(OUTPUT_ID);
    let counter = rig.add("counter");
    rig.wire(counter, extra, "in");
    assert!(rig.update().is_empty());
    assert_eq!(rig.render(1)[..2], [0.0, 0.0]);
    assert_eq!(rig.render(1)[..2], [2.0, 2.0]);
}

#[test]
fn without_buses_every_output_mixes_into_every_channel() {
    let mut rig = Rig::new(Settings {
        channels: 2,
        ..SETTINGS
    });
    let counter = rig.add("counter");
    let output = rig.add(OUTPUT_ID);
    rig.edit(Command::SetConfig {
        node: output,
        key: "device".into(),
        value: Some(Value::Text("anything".into())),
    });
    rig.wire(counter, output, "in");
    assert!(rig.update().is_empty());
    assert_eq!(rig.render(2), [0.0, 0.0, 1.0, 1.0]);
}

#[test]
fn buses_must_add_up_to_the_engines_channels() {
    let (mut controller, _) = engine(Settings {
        channels: 2,
        ..SETTINGS
    })
    .unwrap();
    let bus = |channels| Bus {
        device: String::new(),
        channels,
    };
    assert_eq!(
        controller.set_buses(vec![bus(1)]),
        Err(SettingsError::Buses)
    );
    assert_eq!(controller.set_buses(vec![]), Err(SettingsError::Buses));
    assert!(controller.set_buses(vec![bus(1), bus(1)]).is_ok());
}
