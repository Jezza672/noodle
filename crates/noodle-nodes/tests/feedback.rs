//! Feedback loops through a Delay, end to end: the loop compiles, echoes
//! decay as the loop's gain says, and a loop that doesn't pass through a
//! Delay's signal input is still refused.

use noodle_core::{Connection, Endpoint, Graph, Node, NodeId};
use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node as EngineNode, NodeError, NodeInfo, NodeType,
    Problem, Registry, Settings, Setup, render,
};
use noodle_nodes::{DELAY_ID, register_all};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 256,
    channels: 1,
};

/// One sample of 1.0 at the start of the render.
struct Impulse;

static IMPULSE_INFO: NodeInfo = NodeInfo {
    id: "test.impulse",
    version: 1,
    name: "Impulse",
    category: "Test",
};

impl NodeType for Impulse {
    fn info(&self) -> &NodeInfo {
        &IMPULSE_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().output("out", "Out"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ImpulseNode(true)))
    }
}

struct ImpulseNode(bool);

impl EngineNode for ImpulseNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let out = io.outputs[0].lane_mut(0, 0);
        out.fill(0.0);
        if self.0 {
            out[0] = 1.0;
            self.0 = false;
        }
    }
}

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    let _telemetry = register_all(&mut registry);
    registry.register(Impulse);
    registry
}

/// A graph under construction.
#[derive(Default)]
struct Patch {
    nodes: Vec<(NodeId, Node)>,
    wires: Vec<Connection>,
}

impl Patch {
    fn add(&mut self, id: u64, node: Node) {
        self.nodes.retain(|(n, _)| n.0 != id);
        self.nodes.push((NodeId(id), node));
    }

    fn wire(&mut self, from: (u64, &str), to: (u64, &str)) {
        self.wires.push(Connection {
            from: Endpoint::new(NodeId(from.0), from.1),
            to: Endpoint::new(NodeId(to.0), to.1),
        });
    }

    fn graph(&self) -> Graph {
        Graph::from_parts(self.nodes.clone(), self.wires.clone()).unwrap()
    }
}

/// Impulse and the echo's own output into a mixer, out through the Delay and
/// a gain of 0.5 (-6.0206 dB) back into the mixer.
fn echo(time: f32) -> Patch {
    let mut graph = Patch::default();
    graph.add(1, Node::new("test.impulse"));
    graph.add(2, Node::new("noodle.util.mix"));
    graph.add(3, Node::new(DELAY_ID).with_param("time", time));
    graph.add(4, Node::new("noodle.util.gain").with_param("gain", -6.0206));
    graph.add(5, Node::new("noodle.io.output"));
    graph.wire((1, "out"), (2, "in1"));
    graph.wire((2, "out"), (3, "in"));
    graph.wire((3, "out"), (4, "in"));
    graph.wire((4, "out"), (2, "in2"));
    graph.wire((2, "out"), (5, "in"));
    graph
}

#[test]
fn an_echo_repeats_at_the_delay_time_and_decays() {
    let rendered = render(
        &echo(1_200.0 / 48_000.0).graph(),
        &registry(),
        SETTINGS,
        6_000,
    )
    .unwrap();
    assert!(
        rendered.diagnostics.is_empty(),
        "{:?}",
        rendered.diagnostics
    );
    let s = &rendered.samples;
    for (n, expected) in [(0, 1.0), (1_200, 0.5), (2_400, 0.25), (3_600, 0.125)] {
        assert!((s[n] - expected).abs() < 1e-3, "sample {n} is {}", s[n]);
    }
    let others = s
        .iter()
        .enumerate()
        .filter(|(n, x)| n % 1_200 != 0 && x.abs() > 1e-3)
        .count();
    assert_eq!(others, 0, "only the echoes sound");
}

#[test]
fn inside_a_loop_the_delay_is_at_least_one_block() {
    // 100 samples asked for; the block is 256 frames, so the loop can only
    // do 257 (one block and a sample).
    let rendered = render(
        &echo(100.0 / 48_000.0).graph(),
        &registry(),
        SETTINGS,
        1_200,
    )
    .unwrap();
    assert!(rendered.diagnostics.is_empty());
    let s = &rendered.samples;
    assert!((s[0] - 1.0).abs() < 1e-3);
    assert!((s[257] - 0.5).abs() < 1e-3, "{}", s[257]);
    assert!(s[100].abs() < 1e-3);
}

#[test]
fn a_loop_with_no_delay_is_still_refused() {
    let mut graph = echo(0.01);
    // Replace the Delay with a plain gain.
    graph
        .wires
        .retain(|w| w.from.node.0 != 3 && w.to.node.0 != 3);
    graph.add(3, Node::new("noodle.util.gain"));
    graph.wire((2, "out"), (3, "in"));
    graph.wire((3, "out"), (4, "in"));
    let rendered = render(&graph.graph(), &registry(), SETTINGS, 512).unwrap();
    assert!(
        rendered
            .diagnostics
            .iter()
            .any(|d| d.problem == Problem::Loop),
        "{:?}",
        rendered.diagnostics
    );
}

#[test]
fn a_loop_into_the_delay_time_is_refused() {
    // The Delay reads `time` before it can write, so the loop can't be broken
    // there.
    let mut graph = Patch::default();
    graph.add(1, Node::new(DELAY_ID));
    graph.add(2, Node::new("noodle.util.gain"));
    graph.wire((1, "out"), (2, "in"));
    graph.wire((2, "out"), (1, "time"));
    let rendered = render(&graph.graph(), &registry(), SETTINGS, 512).unwrap();
    assert!(
        rendered
            .diagnostics
            .iter()
            .any(|d| d.problem == Problem::Loop),
        "{:?}",
        rendered.diagnostics
    );
}

#[test]
fn a_delay_outside_a_loop_can_be_shorter_than_a_block() {
    let mut graph = Patch::default();
    graph.add(1, Node::new("test.impulse"));
    graph.add(2, Node::new(DELAY_ID).with_param("time", 10.0 / 48_000.0));
    graph.add(3, Node::new("noodle.io.output"));
    graph.wire((1, "out"), (2, "in"));
    graph.wire((2, "out"), (3, "in"));
    let rendered = render(&graph.graph(), &registry(), SETTINGS, 512).unwrap();
    assert!(rendered.diagnostics.is_empty());
    assert!((rendered.samples[10] - 1.0).abs() < 1e-3);
}
