//! Profiling: the numbers a profile reports match nodes that take a known
//! time, block statistics see deadline misses, and helper threads really do
//! speed up a graph with branches that can run side by side.

use std::time::{Duration, Instant};

use noodle_core::{Command, Connection, Endpoint, Node, NodeId, Project};
use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node as EngineNode, NodeError, NodeInfo, NodeType,
    OUTPUT_ID, Registry, Settings, Setup, profile_project,
};
use noodle_nodes::register_all;

/// These tests measure how long nodes spin against the clock, so they take
/// turns: another test's helper threads (which ask for priority over ordinary
/// threads) would otherwise take the cores the spinning node needs.
static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn turn() -> std::sync::MutexGuard<'static, ()> {
    TURN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 64,
    channels: 1,
};

/// Spins for 0.4 ms per block: 30% of a 64-frame block at 48 kHz.
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
        while start.elapsed() < Duration::from_micros(400) {
            std::hint::spin_loop();
        }
        io.outputs[0]
            .lane_mut(0, 0)
            .copy_from_slice(io.inputs[0].lane(0, 0));
    }
}

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    let _hub = register_all(&mut registry);
    registry.register(Busy);
    registry
}

/// `branches` chains of sine → busy, mixed into one output.
fn project(branches: usize) -> (Project, Vec<NodeId>) {
    let mut project = Project::new();
    let mut add = |node: Node| {
        let id = project.new_node_id();
        Command::AddNode { id, node }.apply(&mut project).unwrap();
        id
    };
    let mix = add(Node::new("noodle.util.mix").with_config(
        noodle_core::Config::new().with("inputs", noodle_core::Value::Int(branches as i64)),
    ));
    let output = add(Node::new(OUTPUT_ID));
    let mut busy = Vec::new();
    let mut wires = vec![(mix, "out".to_owned(), output, "in".to_owned())];
    for i in 0..branches {
        let sine = add(Node::new("noodle.osc.sine"));
        let node = add(Node::new("test.busy"));
        busy.push(node);
        wires.push((sine, "out".into(), node, "in".into()));
        wires.push((node, "out".into(), mix, format!("in{}", i + 1)));
    }
    for (from, from_port, to, to_port) in wires {
        Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        })
        .apply(&mut project)
        .unwrap();
    }
    (project, busy)
}

/// Panics every block.
struct Boom;

static BOOM_INFO: NodeInfo = NodeInfo {
    id: "test.boom",
    version: 1,
    name: "Boom",
    category: "Test",
};

impl NodeType for Boom {
    fn info(&self) -> &NodeInfo {
        &BOOM_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In").output("out", "Out"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(BoomNode))
    }
}

struct BoomNode;

impl EngineNode for BoomNode {
    fn process(&mut self, _ctx: &Context, _io: Io<'_, '_>) {
        panic!("node bug (expected by the test)");
    }
}

const FRAMES: usize = 64 * 400;

#[test]
fn a_profile_reports_each_nodes_share_and_the_blocks() {
    let _turn = turn();
    let (project, busy) = project(1);
    let profile = profile_project(&project, &registry(), SETTINGS, FRAMES, 0, false).unwrap();
    assert!(profile.diagnostics.is_empty());
    assert_eq!(profile.frames, FRAMES);
    assert_eq!(profile.nodes[0].id, busy[0], "slowest first");
    let load = profile.load(&profile.nodes[0]);
    assert!(
        (0.25..0.9).contains(&load),
        "0.4 ms of 1.33 ms is about 0.3, got {load}"
    );
    assert!(profile.nodes[0].blocks >= 399, "{:?}", profile.nodes[0]);
    // The rest take a small part of what the busy node does.
    assert!(profile.nodes[1].total < profile.nodes[0].total / 4);
    assert!(profile.blocks.blocks >= 399, "{:?}", profile.blocks);
    assert!(profile.blocks.mean >= profile.load(&profile.nodes[0]) as f32 * 0.9);
    assert!(profile.blocks.worst >= profile.blocks.mean);
    assert!(
        profile.blocks.percentile(0.5) >= 0.3,
        "{:?}",
        profile.blocks
    );
    assert_eq!(profile.shape.width, 1, "a single chain overlaps nothing");
}

#[test]
fn blocks_over_their_budget_are_counted() {
    let _turn = turn();
    // Four branches take at least 1.6 ms of a 1.33 ms block.
    let (project, _) = project(4);
    let profile = profile_project(&project, &registry(), SETTINGS, FRAMES, 0, false).unwrap();
    assert_eq!(
        profile.blocks.misses, profile.blocks.blocks,
        "{:?}",
        profile.blocks
    );
    assert!(profile.blocks.worst > 1.2);
    assert!(profile.blocks.histogram[10] + profile.blocks.histogram[11] == profile.blocks.blocks);
}

#[test]
fn helper_threads_speed_up_side_by_side_branches() {
    let _turn = turn();
    if std::thread::available_parallelism().map_or(1, |n| n.get()) < 4 {
        return;
    }
    let (project, _) = project(4);
    // A shared CI machine only ever makes a run slower (a busy node spins on
    // the wall clock, so a thread that loses its core still burns its time,
    // and a helper that is late falls back to the caller doing its work). The
    // fastest of a few runs is the one the machine disturbed least; a single
    // run was seen to flake on Windows runners (parallel took 86% of serial).
    let best = |threads: usize| {
        (0..RUNS)
            .map(|_| profile_project(&project, &registry(), SETTINGS, FRAMES, threads, false))
            .map(Result::unwrap)
            .min_by_key(|profile| profile.wall)
            .unwrap()
    };
    let serial = best(0);
    let parallel = best(3);
    assert!(parallel.shape.width >= 4, "{:?}", parallel.shape);
    assert!(
        parallel.wall < serial.wall.mul_f64(0.85),
        "4 busy branches on 4 threads: {:?} vs {:?} on one",
        parallel.wall,
        serial.wall
    );
    assert!(
        parallel.blocks.misses < parallel.blocks.blocks,
        "{:?}",
        parallel.blocks
    );
}

/// Runs of each configuration whose fastest counts in the speed-up test.
const RUNS: usize = 4;

#[test]
fn a_panicking_node_on_workers_is_silenced_and_counted() {
    let _turn = turn();
    let mut registry = registry();
    registry.register(Boom);
    let (mut project, _) = project(2);
    let mut add = |node: Node| {
        let id = project.new_node_id();
        Command::AddNode { id, node }.apply(&mut project).unwrap();
        id
    };
    let sine = add(Node::new("noodle.osc.sine"));
    let boom = add(Node::new("test.boom"));
    let output = add(Node::new(OUTPUT_ID));
    for (from, to) in [(sine, boom), (boom, output)] {
        Command::Connect(Connection {
            from: Endpoint::new(from, "out"),
            to: Endpoint::new(to, "in"),
        })
        .apply(&mut project)
        .unwrap();
    }
    let profile = profile_project(&project, &registry, SETTINGS, FRAMES, 2, true).unwrap();
    assert!(profile.parallel);
    assert!(profile.blocks.panics >= 399, "{:?}", profile.blocks);
}

#[test]
fn the_shape_shows_the_width_threads_could_use_even_on_one_thread() {
    let _turn = turn();
    let (project, _) = project(4);
    let profile = profile_project(&project, &registry(), SETTINGS, FRAMES, 0, false).unwrap();
    assert!(profile.shape.width >= 4, "{:?}", profile.shape);
    assert!(!profile.parallel);
}
