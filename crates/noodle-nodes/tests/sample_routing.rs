//! A sample wire from a Sample node chooses what a Sampler plays: directly,
//! across a group's boundary, and in what comes out of the engine.

use noodle_core::group::group_nodes;
use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{OUTPUT_ID, Registry, Settings, compile, engine};
use noodle_io::Audio;
use noodle_nodes::{
    SAMPLE_INPUT_ID, SAMPLE_KEY, SAMPLER_ID, SampleStore, Sampler, register_library,
};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    channels: 2,
    max_frames: 128,
};

fn registry(store: &SampleStore) -> Registry {
    let mut registry = Registry::with_builtins();
    let _ = register_library(&mut registry);
    registry.replace(Sampler::new(store, &noodle_engine::Telemetry::new()));
    registry
}

fn add(project: &mut Project, node: Node) -> NodeId {
    let id = project.new_node_id();
    Command::AddNode { id, node }.apply(project).unwrap();
    id
}

fn wire(project: &mut Project, from: (NodeId, &str), to: (NodeId, &str)) {
    Command::Connect(Connection {
        from: Endpoint::new(from.0, from.1),
        to: Endpoint::new(to.0, to.1),
    })
    .apply(project)
    .unwrap();
}

fn text(value: &str) -> Config {
    Config::new().with(SAMPLE_KEY, Value::Text(value.into()))
}

/// The sample config the compiled Sampler was built with.
fn compiled_sample(project: &Project, registry: &Registry, sampler: NodeId) -> String {
    let (schedule, diagnostics) = compile(project.graph(), registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let node = schedule.nodes.iter().find(|n| n.id == sampler).unwrap();
    match node.config.get(SAMPLE_KEY) {
        Some(Value::Text(t)) => t.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_wired_sample_overrides_the_samplers_own() {
    let store = SampleStore::new();
    let registry = registry(&store);
    let mut project = Project::new();
    let source = add(
        &mut project,
        Node::new(SAMPLE_INPUT_ID).with_config(text("beef:0.25:0.5")),
    );
    let sampler = add(&mut project, Node::new(SAMPLER_ID).with_config(text("own")));
    assert_eq!(compiled_sample(&project, &registry, sampler), "own");
    wire(&mut project, (source, "sample"), (sampler, "sample"));
    assert_eq!(
        compiled_sample(&project, &registry, sampler),
        "beef:0.25:0.5"
    );
}

#[test]
fn a_sample_node_with_nothing_chosen_silences_the_sampler() {
    let store = SampleStore::new();
    let registry = registry(&store);
    let mut project = Project::new();
    let source = add(&mut project, Node::new(SAMPLE_INPUT_ID));
    let sampler = add(&mut project, Node::new(SAMPLER_ID).with_config(text("own")));
    wire(&mut project, (source, "sample"), (sampler, "sample"));
    assert_eq!(compiled_sample(&project, &registry, sampler), "");
}

#[test]
fn a_sample_wire_crosses_a_group_boundary() {
    let store = SampleStore::new();
    let registry = registry(&store);
    let mut project = Project::new();
    let source = add(
        &mut project,
        Node::new(SAMPLE_INPUT_ID).with_config(text("beef")),
    );
    let sampler = add(&mut project, Node::new(SAMPLER_ID));
    wire(&mut project, (source, "sample"), (sampler, "sample"));
    let snapshot = project.clone();
    let (_, command) = group_nodes(&snapshot, &[sampler], || project.new_node_id()).unwrap();
    command.apply(&mut project).unwrap();
    assert_eq!(compiled_sample(&project, &registry, sampler), "beef");
}

#[test]
fn a_sample_wire_to_an_audio_port_is_reported_not_played() {
    let store = SampleStore::new();
    let registry = registry(&store);
    let mut project = Project::new();
    let source = add(
        &mut project,
        Node::new(SAMPLE_INPUT_ID).with_config(text("beef")),
    );
    let out = add(&mut project, Node::new(OUTPUT_ID));
    wire(&mut project, (source, "sample"), (out, "in"));
    let (_, diagnostics) = compile(project.graph(), &registry);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
}

#[test]
fn the_sampler_plays_the_window_of_the_wired_sample() {
    // A sample that is silent for its first half and a tone in its second.
    let store = SampleStore::new();
    let registry = registry(&store);
    let hash = store.insert(&Audio {
        samples: (0..48_000)
            .map(|i| {
                if i < 24_000 {
                    0.0
                } else {
                    0.5 * (i as f32 * 0.05).sin()
                }
            })
            .collect(),
        channels: 1,
        sample_rate: 48_000,
    });
    let peak = |reference: String| {
        let mut project = Project::new();
        let source = add(
            &mut project,
            Node::new(SAMPLE_INPUT_ID).with_config(text(&reference)),
        );
        let button = add(
            &mut project,
            Node::new("noodle.input.button").with_param("state", 1.0),
        );
        let key = add(&mut project, Node::new("noodle.event.key"));
        let sampler = add(&mut project, Node::new(SAMPLER_ID).with_param("end", 0.5));
        let out = add(&mut project, Node::new(OUTPUT_ID));
        wire(&mut project, (button, "out"), (key, "gate"));
        wire(&mut project, (key, "out"), (sampler, "in"));
        wire(&mut project, (source, "sample"), (sampler, "sample"));
        wire(&mut project, (sampler, "out"), (out, "in"));
        let (mut controller, mut processor) = engine(SETTINGS).unwrap();
        let diagnostics = controller.update_project(&project, &registry);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let mut block = vec![0.0; SETTINGS.max_frames * SETTINGS.channels];
        let mut peak = 0.0f32;
        for _ in 0..40 {
            processor.process(&mut block);
            peak = block.iter().fold(peak, |p, x| p.max(x.abs()));
        }
        peak
    };
    // The sampler's end is half of what it is given. Of the whole sample that
    // is the silent half; of the second half it is the first half of the tone.
    assert!(peak(hash.clone()) < 1e-6);
    assert!(peak(format!("{hash}:0.5:1")) > 0.1);
}
