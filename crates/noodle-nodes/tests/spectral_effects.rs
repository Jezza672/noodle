//! The spectral effects added after M11's first batch, end to end: the frame
//! size, Pitch Shift, Time Stretch, Spectral Compress and Spectral Denoise.

use noodle_core::{Config, Connection, Endpoint, Graph, Node, NodeId, Value};
use noodle_engine::{Problem, Registry, Settings, compile, render};
use noodle_nodes::register_all;

const RATE: f32 = 48_000.0;

fn settings(max_frames: usize) -> Settings {
    Settings {
        sample_rate: RATE,
        max_frames,
        channels: 1,
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

fn analyze(size: i64) -> Node {
    let mut config = Config::new();
    config.set("size", Value::Int(size));
    Node::new("noodle.spectral.analyze").with_config(config)
}

/// `source` into an Analyze of `size`, through `nodes` (each with a
/// `spectrum` input and an `out` output), into Synthesize and the output.
fn through(source: Node, size: i64, nodes: &[Node]) -> Graph {
    let mut list = vec![(NodeId(1), source), (NodeId(2), analyze(size))];
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

fn samples(graph: &Graph, frames: usize) -> Vec<f32> {
    let result = render(graph, &registry(), settings(256), frames).unwrap();
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    result.samples
}

/// The frequency of a clean tone, from its rising zero crossings.
fn frequency(samples: &[f32]) -> f32 {
    let crossings: Vec<f32> = (1..samples.len())
        .filter(|&i| samples[i - 1] < 0.0 && samples[i] >= 0.0)
        .map(|i| i as f32 - samples[i] / (samples[i] - samples[i - 1]))
        .collect();
    assert!(
        crossings.len() > 8,
        "no tone: {} crossings",
        crossings.len()
    );
    (crossings.len() - 1) as f32 * RATE / (crossings[crossings.len() - 1] - crossings[0])
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
}

/// The level of `hz` in `samples`, by correlating with a sine and cosine.
fn level_at(samples: &[f32], hz: f32) -> f32 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, &x) in samples.iter().enumerate() {
        let phase = std::f64::consts::TAU * f64::from(hz) * n as f64 / f64::from(RATE);
        re += f64::from(x) * phase.cos();
        im += f64::from(x) * phase.sin();
    }
    (2.0 * (re * re + im * im).sqrt() / samples.len() as f64) as f32
}

#[test]
fn every_frame_size_gives_the_dry_signal_back() {
    for size in [256, 512, 1024, 2048, 4096, 8192] {
        let wet = samples(&through(sine(440.0), size, &[]), 20_000);
        let dry = samples(
            &Graph::from_parts(
                vec![
                    (NodeId(1), sine(440.0)),
                    (NodeId(2), Node::new("noodle.io.output")),
                ],
                vec![wire((1, "out"), (2, "in"))],
            )
            .unwrap(),
            20_000,
        );
        let worst = wet
            .iter()
            .zip(&dry)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-3, "size {size}: worst {worst}");
    }
}

#[test]
fn a_size_that_is_not_a_power_of_two_is_rounded() {
    // 3000 rounds to 2048 or 4096; either way it renders as a clean tone.
    let wet = samples(&through(sine(440.0), 3000, &[]), 20_000);
    assert!((frequency(&wet[8000..]) - 440.0).abs() < 2.0);
}

#[test]
fn spectral_nodes_adopt_the_size_of_the_wire_and_mismatches_are_reported() {
    let registry = registry();
    // Two analyzers of different sizes into a Cross.
    let graph = Graph::from_parts(
        vec![
            (NodeId(1), sine(220.0)),
            (NodeId(2), sine(330.0)),
            (NodeId(3), analyze(512)),
            (NodeId(4), analyze(2048)),
            (NodeId(5), Node::new("noodle.spectral.cross")),
            (NodeId(6), Node::new("noodle.spectral.synthesize")),
            (NodeId(7), Node::new("noodle.io.output")),
        ],
        vec![
            wire((1, "out"), (3, "in")),
            wire((2, "out"), (4, "in")),
            wire((3, "spectrum"), (5, "carrier")),
            wire((4, "spectrum"), (5, "modulator")),
            wire((5, "out"), (6, "spectrum")),
            wire((6, "out"), (7, "in")),
        ],
    )
    .unwrap();
    let (_, diagnostics) = compile(&graph, &registry);
    assert!(
        diagnostics
            .iter()
            .any(|d| matches!(&d.problem, Problem::Node(e)
            if e.to_string().contains("frame sizes"))),
        "{diagnostics:?}"
    );
    // The same two at one size are fine.
    let graph = Graph::from_parts(
        vec![
            (NodeId(1), sine(220.0)),
            (NodeId(2), sine(330.0)),
            (NodeId(3), analyze(512)),
            (NodeId(4), analyze(512)),
            (NodeId(5), Node::new("noodle.spectral.cross")),
            (NodeId(6), Node::new("noodle.spectral.synthesize")),
            (NodeId(7), Node::new("noodle.io.output")),
        ],
        vec![
            wire((1, "out"), (3, "in")),
            wire((2, "out"), (4, "in")),
            wire((3, "spectrum"), (5, "carrier")),
            wire((4, "spectrum"), (5, "modulator")),
            wire((5, "out"), (6, "spectrum")),
            wire((6, "out"), (7, "in")),
        ],
    )
    .unwrap();
    let (_, diagnostics) = compile(&graph, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn pitch_shift_moves_a_tone_by_the_semitones_asked() {
    for (semitones, size) in [(12.0f32, 2048), (7.0, 2048), (-5.0, 2048), (12.0, 1024)] {
        let shift = Node::new("noodle.spectral.pitch").with_param("shift", semitones);
        let out = samples(&through(sine(300.0), size, &[shift]), 40_000);
        let want = 300.0 * (semitones / 12.0).exp2();
        let got = frequency(&out[12_000..]);
        assert!(
            (got / want - 1.0).abs() < 0.01,
            "{semitones} semitones at {size}: {got} Hz, wanted {want}"
        );
        // It is still a tone of about the same loudness.
        let level = level_at(&out[12_000..], got);
        assert!((level - 1.0).abs() < 0.15, "level {level}");
    }
}

#[test]
fn pitch_shift_of_nothing_leaves_the_tone_alone() {
    let shift = Node::new("noodle.spectral.pitch");
    let out = samples(&through(sine(300.0), 2048, &[shift]), 30_000);
    assert!((frequency(&out[8_000..]) - 300.0).abs() < 0.5);
}

#[test]
fn pitch_shift_of_a_chord_keeps_its_intervals() {
    // A fifth (200 and 300 Hz) up an octave is 400 and 600.
    let mix = Node::new("noodle.util.mix");
    let graph = {
        let shift = Node::new("noodle.spectral.pitch").with_param("shift", 12.0);
        Graph::from_parts(
            vec![
                (NodeId(1), sine(200.0)),
                (NodeId(2), sine(300.0)),
                (NodeId(3), mix.clone()),
                (NodeId(4), analyze(2048)),
                (NodeId(5), shift),
                (NodeId(6), Node::new("noodle.spectral.synthesize")),
                (NodeId(7), Node::new("noodle.io.output")),
            ],
            vec![
                wire((1, "out"), (3, "in1")),
                wire((2, "out"), (3, "in2")),
                wire((3, "out"), (4, "in")),
                wire((4, "spectrum"), (5, "spectrum")),
                wire((5, "out"), (6, "spectrum")),
                wire((6, "out"), (7, "in")),
            ],
        )
        .unwrap()
    };
    let out = samples(&graph, 40_000);
    let tail = &out[16_000..];
    let (low, high) = (level_at(tail, 400.0), level_at(tail, 600.0));
    let (old_low, old_high) = (level_at(tail, 200.0), level_at(tail, 300.0));
    assert!(low > 0.5 && high > 0.5, "{low} {high}");
    assert!(old_low < 0.1 && old_high < 0.1, "{old_low} {old_high}");
}

#[test]
fn time_stretch_at_one_passes_a_tone_and_slower_keeps_the_pitch() {
    for rate in [1.0f32, 0.5, 0.25] {
        let stretch = Node::new("noodle.spectral.stretch")
            .with_param("rate", rate)
            .with_param("length", 1.0);
        let out = samples(&through(sine(300.0), 2048, &[stretch]), 60_000);
        let got = frequency(&out[12_000..]);
        assert!((got - 300.0).abs() < 1.5, "rate {rate}: {got} Hz");
        let level = rms(&out[12_000..]);
        assert!(level > 0.5, "rate {rate}: rms {level}");
    }
}

/// The times, in seconds, at which the loudness of `samples` rises through
/// half its peak: where each burst of a gated tone starts.
fn burst_starts(samples: &[f32]) -> Vec<f32> {
    let window = 1_200;
    let loud: Vec<f32> = samples.chunks(window).map(rms).collect();
    let peak = loud.iter().copied().fold(0.0, f32::max);
    (1..loud.len())
        .filter(|&i| loud[i - 1] < peak * 0.5 && loud[i] >= peak * 0.5)
        .map(|i| (i * window) as f32 / RATE)
        .collect()
}

#[test]
fn time_stretch_slows_a_run_of_notes_and_keeps_their_pitch() {
    // A 300 Hz tone gated on and off twice a second.
    let gated = |stretch: Option<f32>| {
        let mut nodes = vec![
            (NodeId(1), sine(300.0)),
            (
                NodeId(2),
                Node::new("noodle.mod.lfo")
                    .with_param("rate", 2.0)
                    .with_param("shape", 3.0)
                    .with_param("depth", 0.5)
                    .with_param("offset", 0.5),
            ),
            (NodeId(3), Node::new("noodle.util.vca")),
            (NodeId(4), analyze(2048)),
            (NodeId(6), Node::new("noodle.spectral.synthesize")),
            (NodeId(7), Node::new("noodle.io.output")),
        ];
        let mut wires = vec![
            wire((1, "out"), (3, "in")),
            wire((2, "out"), (3, "level")),
            wire((3, "out"), (4, "in")),
            wire((6, "out"), (7, "in")),
        ];
        match stretch {
            Some(rate) => {
                nodes.push((
                    NodeId(5),
                    Node::new("noodle.spectral.stretch")
                        .with_param("rate", rate)
                        .with_param("length", 4.0),
                ));
                wires.push(wire((4, "spectrum"), (5, "spectrum")));
                wires.push(wire((5, "out"), (6, "spectrum")));
            }
            None => wires.push(wire((4, "spectrum"), (6, "spectrum"))),
        }
        Graph::from_parts(nodes, wires).unwrap()
    };
    let plain = burst_starts(&samples(&gated(None), 192_000));
    let slow = burst_starts(&samples(&gated(Some(0.5)), 192_000));
    assert!(plain.len() >= 3 && slow.len() >= 2, "{plain:?} {slow:?}");
    let period =
        |starts: &[f32]| (starts[starts.len() - 1] - starts[0]) / (starts.len() - 1) as f32;
    assert!((period(&plain) - 0.5).abs() < 0.05, "{plain:?}");
    // Half speed: bursts twice as far apart.
    assert!((period(&slow) - 1.0).abs() < 0.1, "{slow:?}");
    // And the tone inside is still 300 Hz.
    let out = samples(&gated(Some(0.5)), 192_000);
    let level = |hz| level_at(&out[24_000..60_000], hz);
    assert!(
        level(300.0) > 5.0 * level(150.0),
        "{} {}",
        level(300.0),
        level(150.0)
    );
}

#[test]
fn spectral_compress_evens_out_loud_and_quiet_tones() {
    // A loud tone at 500 Hz and a quiet one at 3 kHz. A compressor over each
    // band turns the loud one down and leaves the quiet one.
    let loud = Node::new("noodle.util.gain").with_param("gain", -6.0);
    let quiet = Node::new("noodle.util.gain").with_param("gain", -70.0);
    let build = |compress: Option<Node>| {
        let mut nodes = vec![
            (NodeId(1), sine(500.0)),
            (NodeId(2), sine(3000.0)),
            (NodeId(3), loud.clone()),
            (NodeId(4), quiet.clone()),
            (NodeId(5), Node::new("noodle.util.mix")),
            (NodeId(6), analyze(2048)),
            (NodeId(8), Node::new("noodle.spectral.synthesize")),
            (NodeId(9), Node::new("noodle.io.output")),
        ];
        let mut wires = vec![
            wire((1, "out"), (3, "in")),
            wire((2, "out"), (4, "in")),
            wire((3, "out"), (5, "in1")),
            wire((4, "out"), (5, "in2")),
            wire((5, "out"), (6, "in")),
            wire((8, "out"), (9, "in")),
        ];
        match compress {
            Some(node) => {
                nodes.push((NodeId(7), node));
                wires.push(wire((6, "spectrum"), (7, "spectrum")));
                wires.push(wire((7, "out"), (8, "spectrum")));
            }
            None => wires.push(wire((6, "spectrum"), (8, "spectrum"))),
        }
        Graph::from_parts(nodes, wires).unwrap()
    };
    let compress = Node::new("noodle.spectral.compress")
        .with_param("threshold", -60.0)
        .with_param("ratio", 8.0)
        .with_param("attack", 0.001)
        .with_param("release", 0.01)
        .with_param("knee", 0.0)
        .with_param("bands", 32.0);
    let plain = samples(&build(None), 40_000);
    let squashed = samples(&build(Some(compress)), 40_000);
    let tail = |x: &[f32], hz| level_at(&x[16_000..], hz);
    let (loud_before, loud_after) = (tail(&plain, 500.0), tail(&squashed, 500.0));
    let (quiet_before, quiet_after) = (tail(&plain, 3000.0), tail(&squashed, 3000.0));
    assert!(
        loud_after < loud_before * 0.5,
        "loud tone {loud_before} -> {loud_after}"
    );
    // The quiet tone is under the threshold, apart from its band's knee.
    assert!(
        (quiet_after / quiet_before - 1.0).abs() < 0.15,
        "quiet tone {quiet_before} -> {quiet_after}"
    );
}

#[test]
fn spectral_compress_upward_lifts_a_quiet_tone() {
    let quiet = Node::new("noodle.util.gain").with_param("gain", -50.0);
    let graph = |compress: Node| {
        Graph::from_parts(
            vec![
                (NodeId(1), sine(1000.0)),
                (NodeId(2), quiet.clone()),
                (NodeId(3), analyze(2048)),
                (NodeId(4), compress),
                (NodeId(5), Node::new("noodle.spectral.synthesize")),
                (NodeId(6), Node::new("noodle.io.output")),
            ],
            vec![
                wire((1, "out"), (2, "in")),
                wire((2, "out"), (3, "in")),
                wire((3, "spectrum"), (4, "spectrum")),
                wire((4, "out"), (5, "spectrum")),
                wire((5, "out"), (6, "in")),
            ],
        )
        .unwrap()
    };
    let off = Node::new("noodle.spectral.compress")
        .with_param("ratio", 1.0)
        .with_param("makeup", 0.0);
    let up = Node::new("noodle.spectral.compress")
        .with_param("mode", 1.0)
        .with_param("threshold", -30.0)
        .with_param("ratio", 2.0)
        .with_param("knee", 0.0)
        .with_param("attack", 0.001)
        .with_param("release", 0.01);
    let flat = samples(&graph(off), 40_000);
    let lifted = samples(&graph(up), 40_000);
    let (a, b) = (
        level_at(&flat[16_000..], 1000.0),
        level_at(&lifted[16_000..], 1000.0),
    );
    // 50 dB below 1.0, and 20 dB under a threshold of -30 with ratio 2: it
    // comes up by about 10 dB.
    assert!(b > a * 2.0, "{a} -> {b}");
    assert!(b < a * 6.0, "{a} -> {b}");
}

/// Noise at about -26 dB, optionally with a 1 kHz tone at -20 dB under it,
/// through a Spectral Denoise whose `noise` input hears a second, separate
/// noise source of the same level.
fn denoised(tone: bool, denoise: Option<Node>) -> Graph {
    let gain = |db| Node::new("noodle.util.gain").with_param("gain", db);
    let mut nodes = vec![
        (NodeId(1), Node::new("noodle.noise.white")),
        (NodeId(2), gain(-26.0)),
        (NodeId(4), Node::new("noodle.util.mix")),
        (NodeId(6), analyze(2048)),
        (NodeId(8), Node::new("noodle.spectral.synthesize")),
        (NodeId(9), Node::new("noodle.io.output")),
        (NodeId(10), Node::new("noodle.noise.white")),
        (NodeId(11), gain(-26.0)),
        (NodeId(12), analyze(2048)),
    ];
    let mut wires = vec![
        wire((1, "out"), (2, "in")),
        wire((2, "out"), (4, "in1")),
        wire((4, "out"), (6, "in")),
        wire((8, "out"), (9, "in")),
        wire((10, "out"), (11, "in")),
        wire((11, "out"), (12, "in")),
    ];
    if tone {
        nodes.push((NodeId(3), sine(1000.0)));
        nodes.push((NodeId(5), gain(-20.0)));
        wires.push(wire((3, "out"), (5, "in")));
        wires.push(wire((5, "out"), (4, "in2")));
    }
    match denoise {
        Some(node) => {
            nodes.push((NodeId(7), node));
            wires.push(wire((6, "spectrum"), (7, "spectrum")));
            wires.push(wire((12, "spectrum"), (7, "noise")));
            wires.push(wire((7, "out"), (8, "spectrum")));
        }
        None => wires.push(wire((6, "spectrum"), (8, "spectrum"))),
    }
    Graph::from_parts(nodes, wires).unwrap()
}

#[test]
fn denoise_with_a_noise_wire_removes_the_noise_and_keeps_the_tone() {
    let denoise = || Node::new("noodle.spectral.denoise");
    let tail = |x: &[f32]| x[24_000..].to_vec();
    // Noise alone: much quieter.
    let before = rms(&tail(&samples(&denoised(false, None), 72_000)));
    let after = rms(&tail(&samples(&denoised(false, Some(denoise())), 72_000)));
    assert!(after < before * 0.25, "noise {before} -> {after}");
    // With a tone: the tone stays.
    let with_tone = samples(&denoised(true, Some(denoise())), 72_000);
    let tone = level_at(&tail(&with_tone), 1000.0);
    assert!((tone - 0.1).abs() < 0.015, "tone {tone}");
}

#[test]
fn denoise_with_nothing_learned_passes_the_sound() {
    let nodes = vec![
        (NodeId(1), Node::new("noodle.noise.white")),
        (
            NodeId(2),
            Node::new("noodle.util.gain").with_param("gain", -26.0),
        ),
        (NodeId(3), analyze(2048)),
        (NodeId(4), Node::new("noodle.spectral.denoise")),
        (NodeId(5), Node::new("noodle.spectral.synthesize")),
        (NodeId(6), Node::new("noodle.io.output")),
    ];
    let wires = vec![
        wire((1, "out"), (2, "in")),
        wire((2, "out"), (3, "in")),
        wire((3, "spectrum"), (4, "spectrum")),
        wire((4, "out"), (5, "spectrum")),
        wire((5, "out"), (6, "in")),
    ];
    let through_it = samples(&Graph::from_parts(nodes.clone(), wires).unwrap(), 30_000);
    // The same without the Denoise in the chain.
    let direct_nodes: Vec<_> = nodes.into_iter().filter(|(id, _)| id.0 != 4).collect();
    let direct_wires = vec![
        wire((1, "out"), (2, "in")),
        wire((2, "out"), (3, "in")),
        wire((3, "spectrum"), (5, "spectrum")),
        wire((5, "out"), (6, "in")),
    ];
    let direct = samples(
        &Graph::from_parts(direct_nodes, direct_wires).unwrap(),
        30_000,
    );
    let worst = through_it
        .iter()
        .zip(&direct)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(worst < 1e-6, "worst {worst}");
}
