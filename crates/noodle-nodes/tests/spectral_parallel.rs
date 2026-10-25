//! Spectral chains on worker threads sound exactly like one thread: the
//! plan's step graph orders steps by the spectral buffers they share as well
//! as by the signal ones.

use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{Controller, OUTPUT_ID, Processor, Registry, Settings, engine_with_workers};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 128,
    channels: 2,
};

const CHAINS: usize = 6;

struct Rig {
    project: Project,
    registry: Registry,
    controller: Controller,
    processor: Processor,
    mix: NodeId,
}

impl Rig {
    fn new(workers: usize) -> Self {
        let mut registry = Registry::with_builtins();
        let _hub = noodle_nodes::register_all(&mut registry);
        let (controller, processor) = engine_with_workers(SETTINGS, workers, true).unwrap();
        let mut rig = Self {
            project: Project::new(),
            registry,
            controller,
            processor,
            mix: NodeId(0),
        };
        rig.mix = rig.add(
            Node::new("noodle.util.mix")
                .with_config(Config::new().with("inputs", Value::Int(CHAINS as i64 + 1))),
        );
        let output = rig.add(Node::new(OUTPUT_ID));
        rig.wire(rig.mix, "out", output, "in");
        for i in 0..CHAINS {
            rig.chain(i, 110.0 * (i + 1) as f32);
        }
        rig.update();
        rig
    }

    fn add(&mut self, node: Node) -> NodeId {
        let id = self.project.new_node_id();
        Command::AddNode { id, node }
            .apply(&mut self.project)
            .unwrap();
        id
    }

    fn wire(&mut self, from: NodeId, from_port: &str, to: NodeId, to_port: &str) {
        Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        })
        .apply(&mut self.project)
        .unwrap();
    }

    /// saw → Analyze → Spectral Shift → Spectral Blur → Synthesize → mix.
    fn chain(&mut self, i: usize, hz: f32) {
        let saw = self.add(Node::new("noodle.osc.saw").with_param("frequency", hz));
        let analyze = self.add(Node::new("noodle.spectral.analyze"));
        let shift =
            self.add(Node::new("noodle.spectral.shift").with_param("shift", 25.0 * i as f32));
        let blur = self.add(Node::new("noodle.spectral.blur").with_param("amount", 0.5));
        let synth = self.add(Node::new("noodle.spectral.synthesize"));
        self.wire(saw, "out", analyze, "in");
        self.wire(analyze, "spectrum", shift, "spectrum");
        self.wire(shift, "out", blur, "spectrum");
        self.wire(blur, "out", synth, "spectrum");
        self.wire(synth, "out", self.mix, &format!("in{}", i + 1));
    }

    fn update(&mut self) {
        let diagnostics = self
            .controller
            .update_project(&self.project, &self.registry);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * SETTINGS.channels];
        self.processor.process(&mut out);
        out
    }
}

fn session(workers: usize) -> Vec<f32> {
    let mut rig = Rig::new(workers);
    let mut all = Vec::new();
    for round in 0..8 {
        all.extend(rig.render(1000));
        if round == 2 {
            // A new chain swaps the plan with the others' state carried over.
            rig.chain(CHAINS, 77.0);
            rig.update();
        }
        rig.controller.maintain();
    }
    all
}

#[test]
fn workers_sound_like_one_thread_through_spectral_chains() {
    let serial = session(0);
    assert!(
        serial.iter().any(|&x| x.abs() > 0.05),
        "the rig makes sound"
    );
    for workers in [1, 3] {
        let parallel = session(workers);
        assert!(
            serial == parallel,
            "{workers} workers differ from one thread"
        );
    }
}
