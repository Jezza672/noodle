//! The engine and nodes recover from bad values and silence: an infinite or
//! NaN value doesn't leave a node stuck, and a silenced filter doesn't sit in
//! the slow subnormal range.

use noodle_core::{Command, Connection, Endpoint, Node, NodeId, Project};
use noodle_engine::{Controller, OUTPUT_ID, Processor, Registry, Settings, engine};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 512,
    channels: 1,
};

struct Session {
    project: Project,
    registry: Registry,
    controller: Controller,
    processor: Processor,
}

impl Session {
    fn new() -> Self {
        let mut registry = Registry::with_builtins();
        noodle_nodes::register_all(&mut registry);
        let (controller, processor) = engine(SETTINGS).unwrap();
        Self {
            project: Project::new(),
            registry,
            controller,
            processor,
        }
    }

    fn add(&mut self, node: Node) -> NodeId {
        let id = self.project.new_node_id();
        self.edit(Command::AddNode { id, node });
        id
    }

    fn wire(&mut self, from: NodeId, from_port: &str, to: NodeId, to_port: &str) {
        self.edit(Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        }));
    }

    fn edit(&mut self, command: Command) {
        command.apply(&mut self.project).unwrap();
    }

    fn update(&mut self) {
        let diagnostics = self.controller.update(self.project.graph(), &self.registry);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// Renders `seconds` of audio in blocks, returning the last block.
    fn render(&mut self, seconds: f32) -> Vec<f32> {
        let mut block = vec![0.0; SETTINGS.max_frames];
        let blocks = (seconds * SETTINGS.sample_rate / block.len() as f32).ceil() as usize;
        for _ in 0..blocks.max(1) {
            self.processor.process(&mut block);
        }
        block
    }
}

fn assert_sounding(block: &[f32]) {
    assert!(block.iter().all(|x| x.is_finite()), "{block:?}");
    assert!(block.iter().any(|&x| x != 0.0), "silent");
}

#[test]
fn a_sine_recovers_from_an_infinite_frequency() {
    let mut s = Session::new();
    let sine = s.add(Node::new("noodle.osc.sine"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(sine, "out", output, "in");
    s.update();
    s.render(0.1);

    s.controller.set_param(sine, "frequency", f32::INFINITY);
    s.render(0.0);
    s.controller.set_param(sine, "frequency", 440.0);
    assert_sounding(&s.render(1.0));
}

#[test]
fn a_filter_recovers_from_an_overflowing_gain() {
    let mut s = Session::new();
    let sine = s.add(Node::new("noodle.osc.sine"));
    let gain = s.add(Node::new("noodle.util.gain"));
    let svf = s.add(Node::new("noodle.filter.svf"));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(sine, "out", gain, "in");
    s.wire(gain, "out", svf, "in");
    s.wire(svf, "low", output, "in");
    s.update();
    s.render(0.1);

    // 10^50 overflows f32, so the gain outputs infinities.
    s.controller.set_param(gain, "gain", 1000.0);
    s.render(0.1);
    s.controller.set_param(gain, "gain", 0.0);
    assert_sounding(&s.render(1.0));
}

#[test]
fn infinite_values_in_the_project_are_replaced_by_defaults() {
    // As a hand-edited project file could hold.
    let mut s = Session::new();
    let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", f32::INFINITY));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(sine, "out", output, "in");
    s.update();
    assert_sounding(&s.render(0.1));
}

#[test]
fn a_silenced_filter_settles_to_zero() {
    let mut s = Session::new();
    let sine = s.add(Node::new("noodle.osc.sine").with_param("frequency", 100.0));
    let svf = s.add(Node::new("noodle.filter.svf").with_param("cutoff", 200.0));
    let output = s.add(Node::new(OUTPUT_ID));
    s.wire(sine, "out", svf, "in");
    s.wire(svf, "low", output, "in");
    s.update();
    assert_sounding(&s.render(0.5));

    s.edit(Command::Disconnect {
        input: Endpoint::new(svf, "in"),
    });
    s.update();
    // Subnormals are what cost CPU. The engine flushes them even for a node
    // that doesn't, and the SVF also flushes its own tiny state.
    let tail = s.render(20.0);
    assert!(
        tail.iter().all(|&x| !x.is_subnormal() && x.abs() < 1e-30),
        "stuck at {:e}",
        tail[tail.len() - 1]
    );
}
