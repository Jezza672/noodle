//! Feedback loops through a Delay, end to end: the loop compiles, echoes
//! decay as the loop's gain says, and a loop that doesn't pass through a
//! Delay's signal input is still refused.

use noodle_core::{Connection, Endpoint, Graph, Node, NodeId};
use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node as EngineNode, NodeError, NodeInfo, NodeType,
    Problem, Registry, Settings, Setup, render,
};
use noodle_nodes::{DELAY_ID, FEEDBACK_ID, register_all};

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

/// `440 + 200 * in`, a frequency for an oscillator to follow.
struct Scale;

static SCALE_INFO: NodeInfo = NodeInfo {
    id: "test.scale",
    version: 1,
    name: "Scale",
    category: "Test",
};

impl NodeType for Scale {
    fn info(&self) -> &NodeInfo {
        &SCALE_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In").output("out", "Out"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ScaleNode))
    }
}

struct ScaleNode;

impl EngineNode for ScaleNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let input = io.inputs[0].lane(0, 0);
        for (out, x) in io.outputs[0].lane_mut(0, 0).iter_mut().zip(input) {
            *out = 440.0 + 200.0 * x;
        }
    }
}

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    let _telemetry = register_all(&mut registry);
    registry.register(Scale);
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

/// Renders `echo(time)` with gain `gain` (dB) at an engine block of
/// `max_frames`.
fn render_echo(time: f32, gain: f32, max_frames: usize, frames: usize) -> Vec<f32> {
    let mut patch = echo(time);
    patch.add(4, Node::new("noodle.util.gain").with_param("gain", gain));
    let settings = Settings {
        max_frames,
        ..SETTINGS
    };
    let rendered = render(&patch.graph(), &registry(), settings, frames).unwrap();
    assert!(
        rendered.diagnostics.is_empty(),
        "{:?}",
        rendered.diagnostics
    );
    rendered.samples
}

#[test]
fn a_loop_can_be_shorter_than_a_block() {
    // 100 samples asked for in a block of 256: the loop's region runs in
    // chunks of 100, so the echo is where it was asked for.
    let s = render_echo(100.0 / 48_000.0, -6.0206, 256, 1_200);
    for (n, expected) in [(0, 1.0), (100, 0.5), (200, 0.25), (300, 0.125)] {
        assert!((s[n] - expected).abs() < 1e-3, "sample {n} is {}", s[n]);
    }
    let others = s
        .iter()
        .enumerate()
        .filter(|(n, x)| n % 100 != 0 && x.abs() > 1e-3)
        .count();
    assert_eq!(others, 0, "only the echoes sound");
}

#[test]
fn a_loop_as_short_as_a_sample_runs_sample_by_sample() {
    let s = render_echo(1.0 / 48_000.0, -6.0206, 256, 600);
    for n in 0..8 {
        let expected = 0.5f32.powi(n);
        assert!((s[n as usize] - expected).abs() < 1e-3, "sample {n}");
    }
}

#[test]
fn a_loop_rings_at_the_pitch_of_its_delay() {
    // A delay of 1/880 s (54.5 samples) is far inside one block. The echoes
    // of the impulse sit at multiples of it: the centre of the fifth is
    // within a hundredth of a sample, about 0.3 cents.
    let period = 48_000.0 / 880.0;
    let s = render_echo(1.0 / 880.0, -0.4, 256, 2_000);
    let around = 5.0 * period;
    let (lo, hi) = ((around - 20.0) as usize, (around + 20.0) as usize);
    let (mut weight, mut moment) = (0.0, 0.0);
    for (n, x) in s.iter().enumerate().take(hi).skip(lo) {
        weight += x;
        moment += x * n as f32;
    }
    assert!(weight > 0.5, "the echo is there: {weight}");
    assert!(
        (moment / weight - around).abs() < 0.05,
        "centre {} wanted {around}",
        moment / weight
    );
}

#[test]
fn the_sound_of_a_loop_does_not_depend_on_the_engine_block() {
    let reference = render_echo(1.0 / 880.0, -1.0, 128, 4_000);
    for block in [16, 32, 64, 512] {
        let s = render_echo(1.0 / 880.0, -1.0, block, 4_000);
        for (n, (a, b)) in reference.iter().zip(&s).enumerate() {
            assert!(
                (a - b).abs() < 1e-5,
                "block {block}, sample {n}: {a} vs {b}"
            );
        }
    }
}

#[test]
fn a_time_swept_across_a_block_does_not_click() {
    // A loop whose time starts above the block and falls below it.
    for time in [300.0, 90.0, 33.0, 7.0] {
        let s = render_echo(time / 48_000.0, -6.0206, 128, 2_000);
        assert!(s.iter().all(|x| x.is_finite()), "{time}");
    }
}

#[test]
fn a_loop_with_no_delay_is_still_refused() {
    let mut graph = echo(0.01);
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

/// A sine whose output goes through `node` (a Feedback, say) and a scale
/// back into its own frequency: feedback FM, which only a delay of one sample
/// can do.
fn fm_patch(node: &str) -> Patch {
    let mut graph = Patch::default();
    graph.add(1, Node::new("noodle.osc.sine"));
    graph.add(2, Node::new(node));
    graph.add(3, Node::new("test.scale"));
    graph.add(4, Node::new("noodle.io.output"));
    graph.wire((1, "out"), (2, "in"));
    graph.wire((2, "out"), (3, "in"));
    graph.wire((3, "out"), (1, "frequency"));
    graph.wire((1, "out"), (4, "in"));
    graph
}

/// The same, computed a sample at a time.
fn fm_reference(frames: usize) -> Vec<f32> {
    let mut phase = 0.0f32;
    let mut previous = 0.0f32;
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        let frequency = 440.0 + 200.0 * previous;
        let x = (phase * std::f32::consts::TAU).sin();
        phase = (phase + frequency / 48_000.0).rem_euclid(1.0);
        out.push(x);
        previous = x;
    }
    out
}

#[test]
fn a_feedback_node_makes_a_delay_free_loop_legal() {
    for block in [16, 64, 256] {
        let settings = Settings {
            max_frames: block,
            ..SETTINGS
        };
        let rendered =
            render(&fm_patch(FEEDBACK_ID).graph(), &registry(), settings, 3_000).unwrap();
        assert!(
            rendered.diagnostics.is_empty(),
            "{:?}",
            rendered.diagnostics
        );
        let expected = fm_reference(3_000);
        for (n, (a, b)) in rendered.samples.iter().zip(&expected).enumerate() {
            assert!(
                (a - b).abs() < 1e-4,
                "block {block}, sample {n}: {a} vs {b}"
            );
        }
    }
}

#[test]
fn without_a_feedback_node_the_same_loop_is_refused_with_a_hint() {
    let mut graph = fm_patch("noodle.util.gain");
    graph.add(2, Node::new("noodle.util.gain"));
    let rendered = render(&graph.graph(), &registry(), SETTINGS, 512).unwrap();
    let problem = rendered
        .diagnostics
        .iter()
        .find(|d| d.problem == Problem::Loop)
        .expect("refused");
    assert!(problem.problem.to_string().contains("Feedback"));
}

#[test]
fn a_feedback_node_outside_a_loop_delays_by_one_sample() {
    let mut graph = Patch::default();
    graph.add(1, Node::new("test.impulse"));
    graph.add(2, Node::new(FEEDBACK_ID));
    graph.add(3, Node::new("noodle.io.output"));
    graph.wire((1, "out"), (2, "in"));
    graph.wire((2, "out"), (3, "in"));
    let rendered = render(&graph.graph(), &registry(), SETTINGS, 64).unwrap();
    assert!(rendered.samples[0].abs() < 1e-6);
    assert!((rendered.samples[1] - 1.0).abs() < 1e-6);
    assert!(rendered.samples[2..].iter().all(|x| x.abs() < 1e-6));
}

/// A clock gates a Key; Voices plays its notes into a saw through an ADSR
/// and a VCA, into a mix with a short Delay loop round it, and the mix also
/// goes to Voices' `busy`: one region with Voices (its note events), the saw's
/// frequency and the delay loop in it.
fn played_loop() -> Patch {
    let mut graph = Patch::default();
    graph.add(
        1,
        Node::new("noodle.generator.clock").with_param("rate", 2.0),
    );
    graph.add(2, Node::new("noodle.event.key").with_param("note", 57.0));
    graph.add(
        3,
        Node::new("noodle.poly.voices")
            .with_config(Config::new().with("voices", noodle_core::Value::Int(1))),
    );
    graph.add(4, Node::new("noodle.osc.saw"));
    graph.add(5, Node::new("noodle.mod.adsr").with_param("sustain", 1.0));
    graph.add(6, Node::new("noodle.util.vca"));
    graph.add(7, Node::new("noodle.util.mix"));
    graph.add(8, Node::new(DELAY_ID).with_param("time", 32.0 / 48_000.0));
    graph.add(9, Node::new("noodle.util.gain").with_param("gain", -6.0));
    graph.add(10, Node::new("noodle.io.output"));
    graph.wire((1, "out"), (2, "gate"));
    graph.wire((2, "out"), (3, "in"));
    graph.wire((3, "pitch"), (4, "frequency"));
    graph.wire((3, "gate"), (5, "gate"));
    graph.wire((4, "out"), (6, "in"));
    graph.wire((5, "out"), (6, "level"));
    graph.wire((6, "out"), (7, "in1"));
    graph.wire((7, "out"), (8, "in"));
    graph.wire((8, "out"), (9, "in"));
    graph.wire((9, "out"), (7, "in2"));
    graph.wire((7, "out"), (3, "busy"));
    graph.wire((7, "out"), (10, "in"));
    graph
}

#[test]
fn a_note_inside_a_loop_region_lands_on_its_sample() {
    // At a block of 128 the region runs in chunks of 32; at 32 it runs whole
    // blocks, so events reach the nodes in it unchanged and Voices sees the
    // same stretches of audio. The notes begin and end partway into a block
    // of 128: it sounds the same either way.
    let render_at = |max_frames: usize| {
        let settings = Settings {
            max_frames,
            ..SETTINGS
        };
        let rendered = render(&played_loop().graph(), &registry(), settings, 40_000).unwrap();
        assert!(
            rendered.diagnostics.is_empty(),
            "{:?}",
            rendered.diagnostics
        );
        rendered.samples
    };
    let chunked = render_at(128);
    let whole = render_at(32);
    assert!(
        chunked[24_000..26_000].iter().any(|x| x.abs() > 0.05),
        "the second note sounds"
    );
    for (n, (a, b)) in chunked.iter().zip(&whole).enumerate() {
        assert!((a - b).abs() < 1e-4, "sample {n}: {a} vs {b}");
    }
}
