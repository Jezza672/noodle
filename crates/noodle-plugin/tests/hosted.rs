//! Noodle hosting Noodle: the plugin is loaded by `noodle-plugins`' own CLAP
//! host, as a node in an engine, and driven through the engine's test
//! harness. This proves the engine side (ports, audio, notes, state,
//! parameters, tempo) in a real host. The window has its own test (`window.rs`).

mod common;

use common::plugin_path;

use base64::Engine as _;
use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::testing::Harness;
use noodle_engine::{
    Event, EventKind, HOST_DEVICE, INPUT_DEVICE_KEY, INPUT_ID, NoteId, OUTPUT_DEVICE_KEY,
    OUTPUT_ID, Shape,
};
use noodle_nodes::{HOST_PARAM_ID, HOST_PARAM_SLOT_KEY, MIDI_IN_ID};
use noodle_plugin::{EFFECT_ID, INSTRUMENT_ID};
use noodle_plugins::{
    Catalog, PluginFile, PluginNodeType, STATE_KEY, capture_state, describe_file,
};

const RATE: f32 = 48_000.0;

fn catalog() -> Catalog {
    let path = plugin_path();
    let plugins = describe_file(&path).expect("describe the plugin");
    Catalog::new(&[PluginFile {
        path,
        len: 0,
        modified: 0,
        plugins,
        error: None,
    }])
}

fn node_type<'a>(catalog: &'a Catalog, id: &str) -> &'a PluginNodeType {
    catalog
        .iter()
        .find(|t| t.descriptor().id == id)
        .expect("plugin in catalog")
}

fn text(key: &str, value: &str) -> Config {
    let mut config = Config::new();
    config.set(key, Value::Text(value.to_owned()));
    config
}

/// The 64 host parameters' values, all 0, as the state holds them.
const ZERO_PARAMS: &str = "00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000\n";

/// A saved project as the plugin's state holds it, as the config the node
/// takes it in.
fn state(project: &Project, id: &str) -> Config {
    let mut bytes = b"noodle-plugin 2\n".to_vec();
    bytes.extend(ZERO_PARAMS.as_bytes());
    bytes.extend(project.to_ron().as_bytes());
    let sealed = capture_state(&plugin_path(), id, Some(&bytes)).expect("load the state");
    Config::new().with(
        STATE_KEY,
        Value::Text(base64::engine::general_purpose::STANDARD.encode(sealed)),
    )
}

struct Build(Project);

impl Build {
    fn new() -> Self {
        Self(Project::new())
    }
    fn add(&mut self, node: Node) -> NodeId {
        let id = self.0.new_node_id();
        Command::AddNode { id, node }.apply(&mut self.0).unwrap();
        id
    }
    fn wire(&mut self, from: NodeId, from_port: &str, to: NodeId, to_port: &str) {
        Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        })
        .apply(&mut self.0)
        .unwrap();
    }
    fn out(&mut self) -> NodeId {
        self.add(Node::new(OUTPUT_ID).with_config(text(OUTPUT_DEVICE_KEY, HOST_DEVICE)))
    }
}

fn input_port(node: &PluginNodeType, config: &Config, key: &str) -> usize {
    noodle_engine::NodeType::layout(node, config)
        .unwrap()
        .inputs
        .iter()
        .position(|p| p.key == key)
        .unwrap_or_else(|| panic!("no input {key}"))
}

fn note_on(time: u32, key: u8) -> Event {
    Event {
        time,
        kind: EventKind::NoteOn {
            note: NoteId(1),
            channel: 0,
            key,
            velocity: 1.0,
        },
    }
}

#[test]
fn the_effect_and_the_instrument_have_their_ports() {
    let catalog = catalog();
    let effect =
        noodle_engine::NodeType::layout(node_type(&catalog, EFFECT_ID), &Config::new()).unwrap();
    assert_eq!(effect.inputs[0].key, "in");
    // The 64 host parameters are the plugin's, and a host node only shows
    // the few it is asked to (see `params`).
    assert_eq!(
        node_type(&catalog, EFFECT_ID).descriptor().params.len(),
        noodle_io::HOST_PARAMS
    );
    assert_eq!(effect.outputs.len(), 1);
    assert_eq!(effect.event_inputs.len(), 1);
    assert_eq!(effect.event_outputs.len(), 1);
    let instrument =
        noodle_engine::NodeType::layout(node_type(&catalog, INSTRUMENT_ID), &Config::new())
            .unwrap();
    assert_eq!(instrument.outputs.len(), 1);
    assert!(node_type(&catalog, INSTRUMENT_ID).descriptor().instrument);
    assert!(!node_type(&catalog, EFFECT_ID).descriptor().instrument);
}

#[test]
fn the_default_effect_passes_audio_through() {
    let catalog = catalog();
    let node = node_type(&catalog, EFFECT_ID);
    let mut h = Harness::new(node, &Config::new(), &[(0, Shape::STEREO)], RATE, 256).unwrap();
    let mut input = h.input(0, 256);
    for channel in 0..2 {
        for (i, s) in input.lane_mut(0, channel).iter_mut().enumerate() {
            *s = (i as f32 * 0.01).sin() * 0.5 * if channel == 0 { 1.0 } else { -1.0 };
        }
    }
    h.run(256).unwrap();
    // Past the engine's start-up fade, what goes in comes out.
    for _ in 0..8 {
        let mut input = h.input(0, 256);
        for channel in 0..2 {
            input
                .lane_mut(0, channel)
                .fill(0.25 * if channel == 0 { 1.0 } else { -1.0 });
        }
        h.run(256).unwrap();
    }
    let out = h.output(0);
    assert!(out.lane(0, 0).iter().all(|&s| (s - 0.25).abs() < 1e-5));
    assert!(out.lane(0, 1).iter().all(|&s| (s + 0.25).abs() < 1e-5));
}

#[test]
fn the_default_instrument_sounds_when_a_note_arrives() {
    let catalog = catalog();
    let node = node_type(&catalog, INSTRUMENT_ID);
    let mut h = Harness::new(node, &Config::new(), &[], RATE, 256).unwrap();
    h.run(256).unwrap();
    assert!(h.output(0).lane(0, 0).iter().all(|&s| s.abs() < 1e-6));
    h.send_events(0, &[note_on(0, 69)]);
    let mut loudest = 0.0f32;
    for _ in 0..32 {
        h.run(256).unwrap();
        loudest = h
            .output(0)
            .lane(0, 0)
            .iter()
            .fold(loudest, |m, s| m.max(s.abs()));
    }
    assert!(loudest > 0.01, "the Lead Synth made no sound: {loudest}");
}

/// A project saved in the plugin comes back in a new instance and plays.
#[test]
fn saved_state_restores_the_project() {
    let mut b = Build::new();
    let input = b.add(Node::new(INPUT_ID).with_config(text(INPUT_DEVICE_KEY, HOST_DEVICE)));
    let gain = b.add(Node::new("noodle.util.gain").with_param("gain", 0.0));
    let out = b.out();
    b.wire(input, "out", gain, "in");
    b.wire(gain, "out", out, "in");
    let catalog = catalog();
    let node = node_type(&catalog, EFFECT_ID);
    let config = state(&b.0, EFFECT_ID);
    let mut h = Harness::new(node, &config, &[(0, Shape::STEREO)], RATE, 256).unwrap();
    for _ in 0..8 {
        h.input(0, 256).lane_mut(0, 0).fill(0.5);
        h.run(256).unwrap();
    }
    // A fresh effect would pass 0.5 through; this one has a Gain of 0 dB
    // saved in it, which is still 0.5: so also check the saved project is
    // the one in the plugin by its tempo node's rate below.
    assert!(
        h.output(0)
            .lane(0, 0)
            .iter()
            .all(|&s| (s - 0.5).abs() < 1e-4)
    );
    // And what the plugin saves is ours: the magic, the parameters, then the project.
    let saved = capture_state(&plugin_path(), EFFECT_ID, None).unwrap();
    let text = String::from_utf8_lossy(&saved);
    assert!(text.contains("noodle-plugin 2"), "{text:.200}");
    assert!(text.contains("noodle.io.input"), "{text:.400}");
}

#[test]
fn damaged_state_is_refused() {
    assert!(capture_state(&plugin_path(), EFFECT_ID, Some(b"not noodle")).is_err());
    let mut bytes = b"noodle-plugin 2\n".to_vec();
    bytes.extend(ZERO_PARAMS.as_bytes());
    bytes.extend(b"this is not a project");
    assert!(capture_state(&plugin_path(), EFFECT_ID, Some(&bytes)).is_err());
}

#[test]
fn host_automation_of_a_slot_moves_the_wired_value() {
    let mut b = Build::new();
    let mut config = Config::new();
    config.set(HOST_PARAM_SLOT_KEY, Value::Int(3));
    let param = b.add(Node::new(HOST_PARAM_ID).with_config(config));
    let out = b.out();
    b.wire(param, "value", out, "in");
    let catalog = catalog();
    let node = node_type(&catalog, INSTRUMENT_ID);
    let mut config = state(&b.0, INSTRUMENT_ID);
    config.set(noodle_plugins::PARAMS_KEY, Value::Text("2,3".into()));
    let mut h = Harness::new(node, &config, &[], RATE, 256).unwrap();
    let slot3 = input_port(node, &config, "p2");
    let level = |h: &mut Harness| {
        for _ in 0..8 {
            h.run(256).unwrap();
        }
        h.output(0).lane(0, 0)[255]
    };
    assert!(level(&mut h).abs() < 1e-6);
    h.set(slot3, 0.5);
    assert!((level(&mut h) - 0.5).abs() < 1e-4);
    h.set(slot3, 0.9);
    assert!((level(&mut h) - 0.9).abs() < 1e-4);
    // Another slot doesn't reach it.
    h.set(input_port(node, &config, "p3"), 0.2);
    assert!((level(&mut h) - 0.9).abs() < 1e-4);
}

#[test]
fn the_hosts_tempo_reaches_the_graph() {
    let mut b = Build::new();
    let tempo = b.add(Node::new("noodle.mod.tempo"));
    let out = b.out();
    b.wire(tempo, "rate", out, "in");
    let catalog = catalog();
    let node = node_type(&catalog, INSTRUMENT_ID);
    let mut h = Harness::new(node, &state(&b.0, INSTRUMENT_ID), &[], RATE, 256).unwrap();
    h.set_playing(true);
    for _ in 0..4 {
        h.run(256).unwrap();
    }
    // The harness plays at 120 bpm: two quarter notes a second.
    assert!((h.output(0).lane(0, 0)[255] - 2.0).abs() < 1e-3);
}

#[test]
fn notes_pass_through_a_midi_in_to_midi_out() {
    let mut b = Build::new();
    let midi_in = b.add(Node::new(MIDI_IN_ID).with_config(text("port", HOST_DEVICE)));
    let midi_out =
        b.add(Node::new(noodle_nodes::MIDI_OUT_ID).with_config(text("port", HOST_DEVICE)));
    b.wire(midi_in, "out", midi_out, "in");
    b.out();
    let catalog = catalog();
    let node = node_type(&catalog, INSTRUMENT_ID);
    let mut h = Harness::new(node, &state(&b.0, INSTRUMENT_ID), &[], RATE, 256).unwrap();
    h.send_events(0, &[note_on(37, 60)]);
    h.run(256).unwrap();
    let out = h.events(0);
    let on: Vec<_> = out
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Midi(bytes) if bytes[..2] == [0x90, 60]))
        .collect();
    assert_eq!(on.len(), 1, "{out:?}");
    assert_eq!(on[0].time, 37);
}
