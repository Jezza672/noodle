//! A wire into a parameter that offsets (`Modulation::Offset`) moves the
//! parameter along its slider's travel, and a wired parameter reports its
//! live value through a tap.

use noodle_core::{Command, Connection, Endpoint, Node, NodeId, Project};
use noodle_engine::{OUTPUT_ID, Registry, Settings, Telemetry, engine, render_project};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 256,
    channels: 1,
};

struct Patch {
    project: Project,
    registry: Registry,
    telemetry: Telemetry,
}

impl Patch {
    fn new() -> Self {
        let mut registry = Registry::with_builtins();
        let telemetry = noodle_nodes::register_all(&mut registry);
        Self {
            project: Project::new(),
            registry,
            telemetry,
        }
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

    /// A constant `value` from an LFO (depth 0, so only its offset shows).
    fn constant(&mut self, value: f32) -> NodeId {
        self.add(
            Node::new("noodle.mod.lfo")
                .with_param("depth", 0.0)
                .with_param("offset", value),
        )
    }
}

fn db_to_amplitude(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[test]
fn a_wire_offsets_a_linear_parameter_along_its_range() {
    let mut p = Patch::new();
    // A constant 1 into a Gain's `in`, with its gain modulated by 0.25 of the
    // -60..24 dB range: 0 dB plus 21 dB.
    let one = p.add(Node::new("noodle.input.button").with_param("state", 1.0));
    let offset = p.constant(0.25);
    let gain = p.add(Node::new("noodle.util.gain").with_param("gain", 0.0));
    let out = p.add(Node::new(OUTPUT_ID));
    p.wire(one, "out", gain, "in");
    p.wire(offset, "out", gain, "gain");
    p.wire(gain, "out", out, "in");

    let render = render_project(&p.project, &p.registry, SETTINGS, 1_000).unwrap();
    assert!(render.diagnostics.is_empty(), "{:?}", render.diagnostics);
    let expected = db_to_amplitude(21.0);
    let got = render.samples[999];
    assert!(
        (got - expected).abs() < 1e-3 * expected,
        "{got} vs {expected}"
    );
}

#[test]
fn a_modulated_value_stays_inside_the_range() {
    let mut p = Patch::new();
    let one = p.add(Node::new("noodle.input.button").with_param("state", 1.0));
    let too_much = p.constant(1.0);
    let gain = p.add(Node::new("noodle.util.gain").with_param("gain", 20.0));
    let out = p.add(Node::new(OUTPUT_ID));
    p.wire(one, "out", gain, "in");
    p.wire(too_much, "out", gain, "gain");
    p.wire(gain, "out", out, "in");
    let render = render_project(&p.project, &p.registry, SETTINGS, 100).unwrap();
    // The top of the range is 24 dB.
    let expected = db_to_amplitude(24.0);
    assert!((render.samples[99] - expected).abs() < 1e-3 * expected);
}

#[test]
fn the_base_value_is_set_with_the_wire_in_place() {
    let mut p = Patch::new();
    let one = p.add(Node::new("noodle.input.button").with_param("state", 1.0));
    let offset = p.constant(0.0);
    let gain = p.add(Node::new("noodle.util.gain").with_param("gain", -6.0));
    let out = p.add(Node::new(OUTPUT_ID));
    p.wire(one, "out", gain, "in");
    p.wire(offset, "out", gain, "gain");
    p.wire(gain, "out", out, "in");
    let render = render_project(&p.project, &p.registry, SETTINGS, 100).unwrap();
    let expected = db_to_amplitude(-6.0);
    assert!((render.samples[99] - expected).abs() < 1e-3 * expected);
}

#[test]
fn a_parameter_that_replaces_still_takes_the_wire_as_its_value() {
    let mut p = Patch::new();
    // A VCA's level is an envelope's whole value, not an offset from its slider.
    let one = p.add(Node::new("noodle.input.button").with_param("state", 1.0));
    let level = p.constant(0.5);
    let vca = p.add(Node::new("noodle.util.vca").with_param("level", 1.0));
    let out = p.add(Node::new(OUTPUT_ID));
    p.wire(one, "out", vca, "in");
    p.wire(level, "out", vca, "level");
    p.wire(vca, "out", out, "in");
    let render = render_project(&p.project, &p.registry, SETTINGS, 100).unwrap();
    assert!((render.samples[99] - 0.5).abs() < 1e-6);
}

/// Runs `blocks` blocks of the project through a live engine.
fn run(p: &Patch, blocks: usize) {
    let (mut controller, mut processor) = engine(SETTINGS).unwrap();
    let diagnostics = controller.update_project(&p.project, &p.registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let mut out = vec![0.0; SETTINGS.max_frames];
    for _ in 0..blocks {
        processor.process(&mut out);
    }
    drop((controller, processor));
}

#[test]
fn a_tap_shows_the_effective_value_of_a_log_parameter() {
    let mut p = Patch::new();
    // Cutoff 100 Hz, offset by a third of the travel: 20 Hz to 20 kHz is
    // three decades, so a third is one decade, which gives 1 kHz.
    let source = p.add(Node::new("noodle.osc.sine").with_param("frequency", 440.0));
    let offset = p.constant(1.0 / 3.0);
    let svf = p.add(Node::new("noodle.filter.svf").with_param("cutoff", 100.0));
    let out = p.add(Node::new(OUTPUT_ID));
    p.wire(source, "out", svf, "in");
    p.wire(offset, "out", svf, "cutoff");
    p.wire(svf, "low", out, "in");

    let (mut controller, mut processor) = engine(SETTINGS).unwrap();
    controller.set_telemetry(&p.telemetry);
    assert!(
        controller
            .update_project(&p.project, &p.registry)
            .is_empty()
    );
    let mut buffer = vec![0.0; SETTINGS.max_frames];
    processor.process(&mut buffer);
    let reading = p.telemetry.read_param(svf, "cutoff").expect("it reports");
    assert!((reading.value - 1_000.0).abs() < 1.0, "{reading:?}");
    assert!((reading.min - 1_000.0).abs() < 1.0 && (reading.max - 1_000.0).abs() < 1.0);
    // Unwired ports have no tap.
    assert_eq!(p.telemetry.read_param(svf, "resonance"), None);
    drop((controller, processor));
}

#[test]
fn a_tap_reports_the_swing_of_a_modulated_parameter() {
    let mut p = Patch::new();
    let source = p.add(Node::new("noodle.osc.sine").with_param("frequency", 440.0));
    // A square LFO swinging +-0.1 around 0, on a linear 0..1 resonance whose
    // base is 0.5: 0.4 to 0.6.
    let lfo = p.add(
        Node::new("noodle.mod.lfo")
            .with_param("depth", 0.1)
            .with_param("shape", 3.0)
            .with_param("rate", 100.0),
    );
    let svf = p.add(Node::new("noodle.filter.svf").with_param("resonance", 0.5));
    let out = p.add(Node::new(OUTPUT_ID));
    p.wire(source, "out", svf, "in");
    p.wire(lfo, "out", svf, "resonance");
    p.wire(svf, "low", out, "in");

    let (mut controller, mut processor) = engine(SETTINGS).unwrap();
    controller.set_telemetry(&p.telemetry);
    assert!(
        controller
            .update_project(&p.project, &p.registry)
            .is_empty()
    );
    let mut buffer = vec![0.0; SETTINGS.max_frames];
    for _ in 0..8 {
        processor.process(&mut buffer);
    }
    let reading = p.telemetry.read_param(svf, "resonance").unwrap();
    assert!((reading.min - 0.4).abs() < 1e-3, "{reading:?}");
    assert!((reading.max - 0.6).abs() < 1e-3, "{reading:?}");
    drop((controller, processor));
}

#[test]
fn nothing_runs_without_a_reader() {
    // Running with no reader at all must be fine, and costs nothing: the tap
    // stays off.
    let mut p = Patch::new();
    let source = p.add(Node::new("noodle.osc.sine"));
    let offset = p.constant(0.1);
    let svf = p.add(Node::new("noodle.filter.svf"));
    let out = p.add(Node::new(OUTPUT_ID));
    p.wire(source, "out", svf, "in");
    p.wire(offset, "out", svf, "cutoff");
    p.wire(svf, "low", out, "in");
    run(&p, 4);
}

#[test]
fn an_automation_lane_sets_an_offsetting_parameter_instead_of_offsetting_it() {
    use noodle_core::{AutomationLane, AutomationPoint, Tick};
    let mut p = Patch::new();
    // A constant 1 through a Gain whose gain parameter (which offsets for
    // wires) is automated to -6.02 dB: the lane's value is the gain itself.
    let one = p.add(Node::new("noodle.input.button").with_param("state", 1.0));
    let gain = p.add(Node::new("noodle.util.gain"));
    let out = p.add(Node::new(OUTPUT_ID));
    p.wire(one, "out", gain, "in");
    p.wire(gain, "out", out, "in");
    let lane = AutomationLane::new(
        Endpoint::new(gain, "gain"),
        vec![AutomationPoint {
            tick: Tick(0),
            value: -6.0206,
            curve: Default::default(),
        }],
    );
    let id = p.project.new_lane_id();
    Command::AddLane { id, lane }.apply(&mut p.project).unwrap();
    let render = render_project(&p.project, &p.registry, SETTINGS, 1_000).unwrap();
    assert!(render.diagnostics.is_empty(), "{:?}", render.diagnostics);
    assert!(
        (render.samples[999] - 0.5).abs() < 1e-3,
        "{}",
        render.samples[999]
    );
}
