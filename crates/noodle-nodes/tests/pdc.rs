//! Plugin delay compensation, end to end: a node that reports latency delays
//! the signal it passes, and the paths it joins are held back to match, so
//! the render lines up with the timeline.

use noodle_core::{Connection, Endpoint, Graph, Node, NodeId};
use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node as EngineNode, NodeError, NodeInfo,
    NodeType, Registry, Settings, Setup, Value, render,
};
use noodle_nodes::register_all;

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 64,
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

/// Passes its input through `latency` samples late, and says so, as a
/// lookahead plugin does.
struct Latent;

static LATENT_INFO: NodeInfo = NodeInfo {
    id: "test.latent",
    version: 1,
    name: "Latent",
    category: "Test",
};

static LATENCY: ConfigInfo = ConfigInfo::int("latency", "Latency", 0);

impl NodeType for Latent {
    fn info(&self) -> &NodeInfo {
        &LATENT_INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        std::slice::from_ref(&LATENCY)
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In").output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let latency = LATENCY.get_int(setup.config).max(0) as usize;
        Ok(Instance::realtime(LatentNode {
            ring: vec![0.0; latency],
            pos: 0,
        }))
    }
}

struct LatentNode {
    ring: Vec<f32>,
    pos: usize,
}

impl EngineNode for LatentNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let input = io.inputs[0].lane(0, 0);
        let out = io.outputs[0].lane_mut(0, 0);
        if self.ring.is_empty() {
            out.copy_from_slice(input);
            return;
        }
        for (o, &x) in out.iter_mut().zip(input) {
            *o = std::mem::replace(&mut self.ring[self.pos], x);
            self.pos = (self.pos + 1) % self.ring.len();
        }
    }

    fn latency(&self) -> u32 {
        self.ring.len() as u32
    }
}

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    let _telemetry = register_all(&mut registry);
    registry.register(Impulse);
    registry.register(Latent);
    registry
}

fn wire(from: (u64, &str), to: (u64, &str)) -> Connection {
    Connection {
        from: Endpoint::new(NodeId(from.0), from.1),
        to: Endpoint::new(NodeId(to.0), to.1),
    }
}

/// The impulse straight into a mixer and through a Latent node into it.
fn parallel(latency: i64) -> Graph {
    Graph::from_parts(
        vec![
            (NodeId(1), Node::new("test.impulse")),
            (
                NodeId(2),
                Node::new("test.latent")
                    .with_config(Config::new().with("latency", Value::Int(latency))),
            ),
            (NodeId(3), Node::new("noodle.util.mix")),
            (NodeId(4), Node::new("noodle.io.output")),
        ],
        vec![
            wire((1, "out"), (2, "in")),
            wire((2, "out"), (3, "in1")),
            wire((1, "out"), (3, "in2")),
            wire((3, "out"), (4, "in")),
        ],
    )
    .unwrap()
}

fn peaks(samples: &[f32]) -> Vec<(usize, f32)> {
    samples
        .iter()
        .enumerate()
        .filter(|(_, x)| x.abs() > 1e-3)
        .map(|(n, &x)| (n, x))
        .collect()
}

#[test]
fn parallel_paths_line_up_and_the_render_starts_on_the_timeline() {
    // 150 samples is more than two blocks, and not a multiple of one.
    let rendered = render(&parallel(150), &registry(), SETTINGS, 1_000).unwrap();
    assert!(
        rendered.diagnostics.is_empty(),
        "{:?}",
        rendered.diagnostics
    );
    assert_eq!(rendered.samples.len(), 1_000);
    // Both copies of the impulse arrive together, at the timeline's start.
    assert_eq!(peaks(&rendered.samples), [(0, 2.0)]);
}

#[test]
fn without_latency_nothing_changes() {
    let rendered = render(&parallel(0), &registry(), SETTINGS, 200).unwrap();
    assert_eq!(peaks(&rendered.samples), [(0, 2.0)]);
}

#[test]
fn a_latent_path_alone_is_trimmed_to_the_timeline() {
    let graph = Graph::from_parts(
        vec![
            (NodeId(1), Node::new("test.impulse")),
            (
                NodeId(2),
                Node::new("test.latent")
                    .with_config(Config::new().with("latency", Value::Int(100))),
            ),
            (NodeId(3), Node::new("noodle.io.output")),
        ],
        vec![wire((1, "out"), (2, "in")), wire((2, "out"), (3, "in"))],
    )
    .unwrap();
    let rendered = render(&graph, &registry(), SETTINGS, 400).unwrap();
    // The plugin's latency is the whole mix's latency, which the render drops.
    assert_eq!(peaks(&rendered.samples), [(0, 1.0)]);
}

#[test]
fn the_controller_reports_the_output_latency() {
    let (mut controller, _processor) = noodle_engine::engine(SETTINGS).unwrap();
    controller.update(&parallel(150), &registry());
    assert_eq!(controller.output_latency(), 150);
    controller.update(&parallel(0), &registry());
    assert_eq!(controller.output_latency(), 0);
}

#[test]
fn two_latent_paths_align_to_the_slower() {
    // 1 -> latent(40) -> mix.in1, 1 -> latent(90) -> mix.in2.
    let graph = Graph::from_parts(
        vec![
            (NodeId(1), Node::new("test.impulse")),
            (
                NodeId(2),
                Node::new("test.latent").with_config(Config::new().with("latency", Value::Int(40))),
            ),
            (
                NodeId(3),
                Node::new("test.latent").with_config(Config::new().with("latency", Value::Int(90))),
            ),
            (NodeId(4), Node::new("noodle.util.mix")),
            (NodeId(5), Node::new("noodle.io.output")),
        ],
        vec![
            wire((1, "out"), (2, "in")),
            wire((1, "out"), (3, "in")),
            wire((2, "out"), (4, "in1")),
            wire((3, "out"), (4, "in2")),
            wire((4, "out"), (5, "in")),
        ],
    )
    .unwrap();
    let rendered = render(&graph, &registry(), SETTINGS, 300).unwrap();
    assert_eq!(peaks(&rendered.samples), [(0, 2.0)]);
}
