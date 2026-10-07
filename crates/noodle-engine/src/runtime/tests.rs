use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use noodle_core::{Command, Config, Connection, Endpoint, Node as ProjectNode, Project, Value};

use super::*;
use crate::{
    ConfigInfo, Instance, Io, Lane, LaneKernel, Layout, Node, NodeError, NodeInfo, NodeType,
    OUTPUT_ID, ParamInfo, PerLane, Setup, Shape,
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

fn registry(dropped: &Arc<AtomicUsize>) -> Registry {
    let mut registry = Registry::with_builtins();
    registry.register(Counter);
    registry.register(Offset);
    registry.register(Droppable(Arc::clone(dropped)));
    registry.register(Failing);
    registry.register(TwoVoices);
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
        let (controller, processor) = engine(settings);
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
        self.edit(Command::Connect(Connection {
            from: Endpoint::new(from, "out"),
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
