//! Spectral wires, end to end: audio goes through Analyze, spectral nodes and
//! Synthesize inside a real graph, and comes out as the dry signal when
//! nothing changes the frames. Covers the compiler (kinds, shapes, buffer
//! reuse) and the plan (views, delay compensation across the pair).

use noodle_core::{Connection, Endpoint, Graph, Node, NodeId};
use noodle_engine::{Problem, Registry, Settings, compile, render};
use noodle_nodes::register_all;

fn settings(channels: usize, max_frames: usize) -> Settings {
    Settings {
        sample_rate: 48_000.0,
        max_frames,
        channels,
    }
}

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    let _telemetry = register_all(&mut registry);
    registry
}

fn wire(from: (u64, &str), to: (u64, &str)) -> Connection {
    Connection {
        from: Endpoint::new(NodeId(from.0), from.1),
        to: Endpoint::new(NodeId(to.0), to.1),
    }
}

fn sine(hz: f32) -> Node {
    Node::new("noodle.osc.sine").with_param("frequency", hz)
}

/// A sine through `nodes` (spectral nodes with one `spectrum` input and an
/// `out` output, in order) between Analyze and Synthesize, to the output.
fn through(nodes: &[Node]) -> Graph {
    let mut list = vec![
        (NodeId(1), sine(440.0)),
        (NodeId(2), Node::new("noodle.spectral.analyze")),
    ];
    let mut wires = vec![wire((1, "out"), (2, "in"))];
    let mut previous = (2, "spectrum");
    let mut id = 3;
    for node in nodes {
        list.push((NodeId(id), node.clone()));
        wires.push(wire(previous, (id, "spectrum")));
        previous = (id, "out");
        id += 1;
    }
    list.push((NodeId(id), Node::new("noodle.spectral.synthesize")));
    list.push((NodeId(id + 1), Node::new("noodle.io.output")));
    wires.push(wire(previous, (id, "spectrum")));
    wires.push(wire((id, "out"), (id + 1, "in")));
    Graph::from_parts(list, wires).unwrap()
}

fn dry() -> Graph {
    Graph::from_parts(
        vec![
            (NodeId(1), sine(440.0)),
            (NodeId(2), Node::new("noodle.io.output")),
        ],
        vec![wire((1, "out"), (2, "in"))],
    )
    .unwrap()
}

fn worst_difference(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max)
}

#[test]
fn analyze_into_synthesize_gives_the_dry_signal_back_on_the_timeline() {
    // Blocks shorter than a hop, and not a divisor of it.
    let s = settings(1, 37);
    let wet = render(&through(&[]), &registry(), s, 6_000).unwrap();
    assert!(wet.diagnostics.is_empty(), "{:?}", wet.diagnostics);
    let dry = render(&dry(), &registry(), s, 6_000).unwrap();
    assert!(dry.samples.iter().any(|x| x.abs() > 0.5));
    // The render drops the pair's latency, so the two line up from the
    // first sample.
    assert!(
        worst_difference(&wet.samples, &dry.samples) < 1e-3,
        "worst {}",
        worst_difference(&wet.samples, &dry.samples)
    );
}

#[test]
fn the_block_size_does_not_change_the_render() {
    let small = render(&through(&[]), &registry(), settings(1, 64), 4_000).unwrap();
    let large = render(&through(&[]), &registry(), settings(1, 1024), 4_000).unwrap();
    assert!(worst_difference(&small.samples, &large.samples) < 1e-5);
}

#[test]
fn a_long_chain_reuses_spectral_buffers_without_mixing_them_up() {
    // Pass-through settings for every node: a gate that cuts nothing, a
    // filter that stops only a band no sine reaches, a shift of 0, a blur
    // of 0 and a freeze that is off.
    let nodes = [
        Node::new("noodle.spectral.gate")
            .with_param("threshold", -120.0)
            .with_param("reduction", 0.0),
        // A band stop far above anything the sine puts out.
        Node::new("noodle.spectral.filter")
            .with_param("low", 23_000.0)
            .with_param("high", 23_500.0)
            .with_param("mode", 1.0),
        Node::new("noodle.spectral.shift"),
        Node::new("noodle.spectral.blur").with_param("amount", 0.0),
        Node::new("noodle.spectral.freeze"),
        Node::new("noodle.spectral.gate")
            .with_param("threshold", -120.0)
            .with_param("reduction", 0.0),
    ];
    let s = settings(1, 128);
    let wet = render(&through(&nodes), &registry(), s, 6_000).unwrap();
    assert!(wet.diagnostics.is_empty(), "{:?}", wet.diagnostics);
    let dry = render(&dry(), &registry(), s, 6_000).unwrap();
    let worst = worst_difference(&wet.samples, &dry.samples);
    assert!(worst < 1e-3, "worst {worst}");
}

#[test]
fn a_spectrum_can_feed_several_nodes_and_cross_takes_two() {
    // Analyze 440 Hz once, and feed Cross as both carrier and modulator at
    // mix 0.5: the geometric blend of a level with itself is itself.
    let graph = Graph::from_parts(
        vec![
            (NodeId(1), sine(440.0)),
            (NodeId(2), Node::new("noodle.spectral.analyze")),
            (
                NodeId(3),
                Node::new("noodle.spectral.cross").with_param("mix", 0.5),
            ),
            (NodeId(4), Node::new("noodle.spectral.synthesize")),
            (NodeId(5), Node::new("noodle.io.output")),
        ],
        vec![
            wire((1, "out"), (2, "in")),
            wire((2, "spectrum"), (3, "carrier")),
            wire((2, "spectrum"), (3, "modulator")),
            wire((3, "out"), (4, "spectrum")),
            wire((4, "out"), (5, "in")),
        ],
    )
    .unwrap();
    let s = settings(1, 256);
    let wet = render(&graph, &registry(), s, 6_000).unwrap();
    assert!(wet.diagnostics.is_empty(), "{:?}", wet.diagnostics);
    let dry = render(&dry(), &registry(), s, 6_000).unwrap();
    assert!(worst_difference(&wet.samples, &dry.samples) < 2e-3);
}

#[test]
fn stereo_lanes_stay_apart() {
    // A saw panned hard left: the right channel stays silent through the
    // spectral path.
    let graph = Graph::from_parts(
        vec![
            (
                NodeId(1),
                Node::new("noodle.osc.saw").with_param("frequency", 220.0),
            ),
            (
                NodeId(2),
                Node::new("noodle.util.pan").with_param("pan", -1.0),
            ),
            (NodeId(3), Node::new("noodle.spectral.analyze")),
            (NodeId(4), Node::new("noodle.spectral.synthesize")),
            (NodeId(5), Node::new("noodle.io.output")),
        ],
        vec![
            wire((1, "out"), (2, "in")),
            wire((2, "out"), (3, "in")),
            wire((3, "spectrum"), (4, "spectrum")),
            wire((4, "out"), (5, "in")),
        ],
    )
    .unwrap();
    let rendered = render(&graph, &registry(), settings(2, 100), 4_000).unwrap();
    assert!(
        rendered.diagnostics.is_empty(),
        "{:?}",
        rendered.diagnostics
    );
    let left: Vec<f32> = rendered.samples.iter().step_by(2).copied().collect();
    let right: Vec<f32> = rendered
        .samples
        .iter()
        .skip(1)
        .step_by(2)
        .copied()
        .collect();
    assert!(left.iter().any(|x| x.abs() > 0.3), "left should sound");
    assert!(
        right.iter().all(|x| x.abs() < 1e-6),
        "right should be silent"
    );
}

#[test]
fn spectral_and_audio_ports_do_not_connect() {
    let graph = Graph::from_parts(
        vec![
            (NodeId(1), sine(440.0)),
            (NodeId(2), Node::new("noodle.spectral.analyze")),
            (NodeId(3), Node::new("noodle.spectral.synthesize")),
            (NodeId(4), Node::new("noodle.io.output")),
        ],
        vec![
            // A spectrum into an audio input, and audio into a spectral one.
            wire((2, "spectrum"), (4, "in")),
            wire((1, "out"), (3, "spectrum")),
        ],
    )
    .unwrap();
    let (_, diagnostics) = compile(&graph, &registry());
    let mismatches = diagnostics
        .iter()
        .filter(|d| d.problem == Problem::KindMismatch)
        .count();
    assert_eq!(mismatches, 2, "{diagnostics:?}");
}
