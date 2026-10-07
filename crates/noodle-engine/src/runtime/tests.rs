use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use noodle_core::{Command, Config, Connection, Endpoint, Node as ProjectNode, Project, Value};

use super::*;
use crate::{
    ConfigInfo, Event, EventKind, Instance, Io, Lane, LaneKernel, Layout, Node, NodeError,
    NodeInfo, NodeType, NoteId, OUTPUT_ID, ParamInfo, PerLane, Setup, Shape,
};

/// Smoothing for the `offset` parameter: 4 ms, which is 4 samples at the test
/// sample rate.
const SETTINGS: Settings = Settings {
    sample_rate: 1000.0,
    max_frames: 4,
    channels: 1,
};

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
        self.controller.update(self.project.graph(), &self.registry)
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
    assert_eq!(rig.render(2), [100.0, 101.0]);
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
    rig.render(1);
    rig.controller.maintain();
    // The latest plan arrives, and takes over correctly from the last one the
    // processor installed.
    assert_eq!(rig.render(2), [10.0, 11.0]);
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

    // Nothing in the graph changed, but the node is tried again.
    assert!(rig.update().is_empty());
    assert_eq!(rig.render(2), [7.0; 2]);
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
