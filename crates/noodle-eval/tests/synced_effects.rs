//! The library's tempo-synced effects follow the project's tempo, and the
//! Drum Rack template is a working group (#142, #143).

use std::path::Path;

use noodle_core::library::Library;
use noodle_core::{
    Command, Connection, Endpoint, History, Node, NodeId, Position, Project, TempoMap,
    TimeSignature,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 256,
    channels: 1,
};

/// `source` through the system template `slug`, with `set` put on the Tempo
/// inside it, to the output, at `bpm`.
fn render(slug: &str, bpm: f64, source: Node, set: &[(&str, f32)], frames: usize) -> Vec<f32> {
    let library = Library::in_memory();
    let (mut project, mut history) = (Project::new(), History::new());
    let apply = |project: &mut Project, history: &mut History, command| {
        history.apply(project, command).unwrap();
    };
    apply(
        &mut project,
        &mut history,
        Command::SetTempoMap(TempoMap::constant(bpm, TimeSignature::COMMON).unwrap()),
    );
    let template = library
        .template(&format!("system:template:{slug}"))
        .expect("the template is in the library");
    let (group, command) =
        template.instantiate(slug, None, Position::default(), || project.new_node_id());
    apply(&mut project, &mut history, command);
    let click = project.new_node_id();
    apply(
        &mut project,
        &mut history,
        Command::AddNode {
            id: click,
            node: source,
        },
    );
    let out = project.new_node_id();
    apply(
        &mut project,
        &mut history,
        Command::AddNode {
            id: out,
            node: Node::new(OUTPUT_ID),
        },
    );
    let mut wire = |from: NodeId, from_port: &str, to: NodeId, to_port: &str| {
        apply(
            &mut project,
            &mut history,
            Command::Connect(Connection {
                from: Endpoint::new(from, from_port),
                to: Endpoint::new(to, to_port),
            }),
        );
    };
    wire(click, "out", group, "in");
    wire(group, "out", out, "in");
    // An unwired group input leaves the value inside as it is, so the
    // settings go on the Tempo node in the group.
    let tempo = project
        .graph()
        .children(Some(group))
        .find(|(_, n)| n.type_id == "noodle.mod.tempo")
        .expect("the group has a Tempo")
        .0;
    for &(key, value) in set {
        history
            .apply(
                &mut project,
                Command::SetParam {
                    node: tempo,
                    key: key.into(),
                    value: Some(value),
                },
            )
            .unwrap();
    }
    let mut registry = Registry::with_builtins();
    let out = noodle_nodes::render_project_with_clips(
        &project,
        &mut registry,
        Path::new("."),
        SETTINGS,
        frames,
    )
    .unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    out.render.samples
}

/// A short click on every beat.
fn click() -> Node {
    Node::new("noodle.generator.clock")
        .with_param("rate", 2.0)
        .with_param("width", 0.01)
}

fn peak(samples: &[f32], from: usize, len: usize) -> f32 {
    samples[from..from + len]
        .iter()
        .fold(0.0, |m, x| m.max(x.abs()))
}

#[test]
fn the_echo_lands_an_eighth_after_the_click_at_any_tempo() {
    // The first click is at 0 and the next at one beat; the echo, on the
    // eighth, sits halfway between them.
    for (bpm, beat) in [(120.0, 24_000), (60.0, 48_000), (90.0, 32_000)] {
        let samples = render("tempo-echo", bpm, click(), &[("division", 3.0)], beat * 2);
        let half = beat / 2;
        assert!(peak(&samples, 0, 100) > 0.5, "{bpm}: the dry click");
        assert!(peak(&samples, half + 200, 100) > 0.1, "{bpm}: the echo");
        assert!(
            peak(&samples, half / 3, 100) < 1e-3,
            "{bpm}: silence before the echo"
        );
    }
}

#[test]
fn the_tremolo_repeats_every_division_at_any_tempo() {
    // Eighth notes: the level pattern repeats every half beat, and it moves.
    for (bpm, period) in [(120.0, 12_000), (60.0, 24_000), (90.0, 16_000)] {
        let tone = Node::new("noodle.osc.sine").with_param("frequency", 1_000.0);
        let samples = render("tempo-tremolo", bpm, tone, &[("division", 3.0)], period * 3);
        let level = |at: usize| peak(&samples, at, 240);
        let levels: Vec<f32> = (0..period).step_by(500).map(level).collect();
        for (i, at) in (0..period).step_by(500).enumerate() {
            assert!(
                (levels[i] - level(at + period)).abs() < 0.05,
                "{bpm}: the same level a period later at {at}"
            );
        }
        let (low, high) = levels
            .iter()
            .fold((f32::MAX, 0.0f32), |(l, h), &x| (l.min(x), h.max(x)));
        assert!(high - low > 0.3, "{bpm}: the level moves, {low} to {high}");
    }
}

#[test]
fn the_drum_rack_has_a_slot_per_general_midi_key_and_no_samples() {
    let library = Library::in_memory();
    let mut project = Project::new();
    let template = library.template("system:template:drum-rack").unwrap();
    let (group, command) =
        template.instantiate("rack", None, Position::default(), || project.new_node_id());
    History::new().apply(&mut project, command).unwrap();
    let graph = project.graph();
    let inside = |kind: &str| -> Vec<&Node> {
        graph
            .children(Some(group))
            .filter(|(_, n)| n.type_id == kind)
            .map(|(_, n)| n)
            .collect()
    };
    let samplers = inside("noodle.instrument.sampler");
    assert_eq!(samplers.len(), 8);
    assert!(samplers.iter().all(|s| {
        s.config
            .get("sample")
            .is_none_or(|v| matches!(v, noodle_core::Value::Text(t) if t.is_empty()))
    }));
    let split = inside("noodle.event.note_split");
    assert_eq!(split.len(), 1);
    let keys: Vec<f32> = (1..=8)
        .map(|i| split[0].params[&format!("low{i}")])
        .collect();
    assert_eq!(keys, [36.0, 38.0, 42.0, 46.0, 39.0, 45.0, 50.0, 49.0]);
    let ports = graph.group_ports(group);
    assert!(ports.input("notes").is_some() && ports.output("out").is_some());
}
