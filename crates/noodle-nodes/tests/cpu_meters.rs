//! Per-node CPU meters: a node that takes a known share of the block's time
//! reads as about that share, and a cheap one reads far lower.

use std::time::{Duration, Instant};

use noodle_core::{Connection, Endpoint, Graph, Node, NodeId};
use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node as EngineNode, NodeError, NodeInfo, NodeType,
    Registry, Settings, Setup, Telemetry, engine,
};
use noodle_nodes::register_all;

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 64,
    channels: 1,
};

/// Spins for 0.8 ms per block, 60% of a 64-frame block at 48 kHz.
struct Busy;

static BUSY_INFO: NodeInfo = NodeInfo {
    id: "test.busy",
    version: 1,
    name: "Busy",
    category: "Test",
};

impl NodeType for Busy {
    fn info(&self) -> &NodeInfo {
        &BUSY_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In").output("out", "Out"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(BusyNode))
    }
}

struct BusyNode;

impl EngineNode for BusyNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_micros(800) {
            std::hint::spin_loop();
        }
        io.outputs[0]
            .lane_mut(0, 0)
            .copy_from_slice(io.inputs[0].lane(0, 0));
    }
}

fn wire(from: (u64, &str), to: (u64, &str)) -> Connection {
    Connection {
        from: Endpoint::new(NodeId(from.0), from.1),
        to: Endpoint::new(NodeId(to.0), to.1),
    }
}

#[test]
fn a_busy_node_reads_as_its_share_of_the_block() {
    let mut registry = Registry::with_builtins();
    let _telemetry = register_all(&mut registry);
    registry.register(Busy);
    let graph = Graph::from_parts(
        vec![
            (NodeId(1), Node::new("noodle.osc.sine")),
            (NodeId(2), Node::new("test.busy")),
            (NodeId(3), Node::new("noodle.io.output")),
        ],
        vec![wire((1, "out"), (2, "in")), wire((2, "out"), (3, "in"))],
    )
    .unwrap();
    let telemetry = Telemetry::new();
    let (mut controller, mut processor) = engine(SETTINGS).unwrap();
    controller.set_telemetry(&telemetry);
    assert!(controller.update(&graph, &registry).is_empty());
    assert_eq!(telemetry.read_cpu(NodeId(2)), None, "nothing has run yet");

    let mut out = vec![0.0; 64];
    for _ in 0..300 {
        processor.process(&mut out);
    }
    let busy = telemetry.read_cpu(NodeId(2)).expect("busy node reports");
    let sine = telemetry.read_cpu(NodeId(1)).expect("sine reports");
    assert!(
        (0.45..1.2).contains(&busy.load),
        "0.8 ms of 1.33 ms is about 0.6, got {}",
        busy.load
    );
    assert!(busy.peak >= busy.load);
    assert!(
        sine.load < busy.load / 4.0,
        "{} vs {}",
        sine.load,
        busy.load
    );
    let all = telemetry.read_all_cpu();
    assert_eq!(all.len(), 3, "{all:?}");
    assert!(all.windows(2).all(|w| w[0].0 < w[1].0), "sorted by node");

    // A node that's gone from the plan stops reporting once its plan is freed.
    let empty = Graph::default();
    controller.update(&empty, &registry);
    for _ in 0..4 {
        processor.process(&mut out);
        controller.maintain();
    }
    assert_eq!(telemetry.read_cpu(NodeId(2)), None);
}

#[test]
fn without_telemetry_nothing_is_timed() {
    // No hub: the plan is built without writers, and rendering works as before.
    let mut registry = Registry::with_builtins();
    let _telemetry = register_all(&mut registry);
    let graph = Graph::from_parts(
        vec![
            (NodeId(1), Node::new("noodle.osc.sine")),
            (NodeId(2), Node::new("noodle.io.output")),
        ],
        vec![wire((1, "out"), (2, "in"))],
    )
    .unwrap();
    let rendered = noodle_engine::render(&graph, &registry, SETTINGS, 256).unwrap();
    assert!(rendered.diagnostics.is_empty());
}
