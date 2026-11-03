//! A sample wire from a Sample node chooses what a Sampler plays: directly,
//! across a group's boundary, and in what comes out of the engine.

use noodle_core::group::group_nodes;
use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{OUTPUT_ID, Registry, Settings, compile, engine};
use noodle_io::Audio;
use noodle_nodes::{
    RANDOM_SELECT_ID, SAMPLE_INPUT_ID, SAMPLE_KEY, SAMPLER_ID, SampleStore, Sampler,
    register_library,
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

/// Samples wired into a Random Select of samples, and it into a Sampler.
fn picking(project: &mut Project, texts: &[&str]) -> (NodeId, NodeId) {
    let select = add(
        project,
        Node::new(RANDOM_SELECT_ID)
            .with_config(
                Config::new()
                    .with("kind", Value::Int(2))
                    .with("inputs", Value::Int(texts.len() as i64)),
            )
            .with_param("avoid_repeat", 1.0),
    );
    for (i, t) in texts.iter().enumerate() {
        let source = add(project, Node::new(SAMPLE_INPUT_ID).with_config(text(t)));
        wire(
            project,
            (source, "sample"),
            (select, &format!("in{}", i + 1)),
        );
    }
    let sampler = add(project, Node::new(SAMPLER_ID));
    wire(project, (select, "out"), (sampler, "sample"));
    (select, sampler)
}

#[test]
fn a_pick_between_samples_loads_every_candidate_into_the_sampler() {
    let store = SampleStore::new();
    let registry = registry(&store);
    let mut project = Project::new();
    let (_, sampler) = picking(&mut project, &["aaa", "bbb:0.25:0.5"]);
    assert_eq!(
        compiled_sample(&project, &registry, sampler),
        "aaa|bbb:0.25:0.5"
    );
}

#[test]
fn a_pick_between_the_same_sample_is_resolved_at_compile_time() {
    let store = SampleStore::new();
    let registry = registry(&store);
    let mut project = Project::new();
    let (select, sampler) = picking(&mut project, &["aaa", "aaa"]);
    assert_eq!(compiled_sample(&project, &registry, sampler), "aaa");
    // And the picker's signal is not wired in.
    let (schedule, _) = compile(project.graph(), &registry);
    assert!(schedule.nodes.iter().any(|n| n.id == select));
}

#[test]
fn a_pick_that_feeds_a_pick_is_reported() {
    let store = SampleStore::new();
    let registry = registry(&store);
    let mut project = Project::new();
    let (inner, _) = picking(&mut project, &["aaa", "bbb"]);
    let (outer, sampler) = picking(&mut project, &["ccc", "ddd"]);
    wire(&mut project, (inner, "out"), (outer, "in1"));
    let (_, diagnostics) = compile(project.graph(), &registry);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let _ = sampler;
}

#[test]
fn the_sampler_plays_the_sample_the_trigger_picked() {
    let store = SampleStore::new();
    let registry = registry(&store);
    let constant = |value: f32| {
        store.insert(&Audio {
            samples: vec![value; 48_000],
            channels: 1,
            sample_rate: 48_000,
        })
    };
    let (quiet, loud) = (constant(0.1), constant(0.6));
    let peak = |trigger: bool| {
        let mut project = Project::new();
        let (select, sampler) = picking(&mut project, &[&quiet, &loud]);
        // Avoiding a repeat from the first, the first trigger picks the second.
        let button = add(
            &mut project,
            Node::new("noodle.input.button")
                .with_param("state", f64::from(u8::from(trigger)) as f32),
        );
        wire(&mut project, (button, "out"), (select, "trigger"));
        let notes = add(
            &mut project,
            Node::new("noodle.input.button").with_param("state", 1.0),
        );
        let key = add(&mut project, Node::new("noodle.event.key"));
        let out = add(&mut project, Node::new(OUTPUT_ID));
        wire(&mut project, (notes, "out"), (key, "gate"));
        wire(&mut project, (key, "out"), (sampler, "in"));
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
    assert!(
        (0.05..0.3).contains(&peak(false)),
        "the first, until a trigger"
    );
    assert!(peak(true) > 0.4, "the second, once triggered");
}
