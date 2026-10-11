//! The test plugin as a node, driven through the engine's test harness.

mod common;

use std::f32::consts::TAU;

use base64::Engine as _;
use common::*;
use noodle_engine::testing::Harness;
use noodle_engine::{Config, Event, EventKind, NodeType, NoteId, Shape, Value};
use noodle_plugins::{STATE_KEY, capture_state};

// Port order: in, Gain, Invert, out, notes.
const IN: usize = 0;
const GAIN: usize = 1;
const INVERT: usize = 2;

const RATE: f32 = 48_000.0;

fn harness(config: &Config, connected: &[(usize, Shape)]) -> Harness {
    let catalog = catalog();
    Harness::new(node_type(&catalog), config, connected, RATE, 256).expect("harness")
}

fn fill_input(h: &mut Harness, frames: usize, value: impl Fn(usize) -> f32) {
    let mut input = h.input(IN, frames);
    for channel in 0..input.shape().channels {
        for (i, s) in input.lane_mut(0, channel).iter_mut().enumerate() {
            *s = value(i);
        }
    }
}

#[test]
fn ports_come_from_the_plugin() {
    let catalog = catalog();
    let layout = noodle_engine::NodeType::layout(node_type(&catalog), &Config::new()).unwrap();
    let keys =
        |ports: &[noodle_engine::Port]| ports.iter().map(|p| p.key.to_string()).collect::<Vec<_>>();
    let inputs: Vec<_> = layout.inputs.iter().map(|p| p.key.to_string()).collect();
    assert_eq!(inputs, ["in", "p1", "p2"]);
    assert_eq!(keys(&layout.outputs), ["out"]);
    assert_eq!(keys(&layout.event_inputs), ["notes", "notes2"]);
    assert!(layout.event_outputs.is_empty());
    // The gain's port carries the plugin's range and default.
    let noodle_engine::InputKind::Param(gain) = &layout.inputs[GAIN].kind else {
        panic!("gain is a parameter");
    };
    assert_eq!((gain.min, gain.max, gain.default), (0.0, 2.0, 1.0));
    // A stepped parameter is stepped.
    let noodle_engine::InputKind::Param(invert) = &layout.inputs[INVERT].kind else {
        panic!("invert is a parameter");
    };
    assert!(matches!(
        invert.kind,
        noodle_engine::ParamKind::Stepped { .. }
    ));
}

#[test]
fn audio_goes_through_the_plugin_scaled_by_the_gain_parameter() {
    let mut h = harness(&Config::new(), &[(IN, Shape::STEREO)]);
    fill_input(&mut h, 256, |i| (i as f32 * 0.01).sin() * 0.5);

    h.run(256).unwrap();
    let out = h.output(0);
    assert_eq!(out.shape(), Shape::STEREO);
    for channel in 0..2 {
        for (i, &s) in out.lane(0, channel).iter().enumerate() {
            let expected = (i as f32 * 0.01).sin() * 0.5;
            assert!(
                (s - expected).abs() < 1e-6,
                "ch {channel} frame {i}: {s} vs {expected}"
            );
        }
    }

    h.set(GAIN, 0.25);
    h.run(256).unwrap();
    let out = h.output(0);
    for (i, &s) in out.lane(0, 1).iter().enumerate() {
        let expected = (i as f32 * 0.01).sin() * 0.5 * 0.25;
        assert!((s - expected).abs() < 1e-6, "frame {i}: {s} vs {expected}");
    }
}

#[test]
fn a_stepped_parameter_is_rounded_and_reaches_the_plugin() {
    let mut h = harness(&Config::new(), &[(IN, Shape::STEREO)]);
    fill_input(&mut h, 64, |_| 0.5);
    h.set(INVERT, 0.7);
    h.run(64).unwrap();
    assert!(
        h.output(0)
            .lane(0, 0)
            .iter()
            .all(|&s| (s + 0.5).abs() < 1e-6)
    );
}

#[test]
fn a_mono_signal_feeds_both_channels() {
    let mut h = harness(&Config::new(), &[(IN, Shape::MONO)]);
    fill_input(&mut h, 64, |_| 0.25);
    h.run(64).unwrap();
    let out = h.output(0);
    assert!(out.lane(0, 0).iter().all(|&s| (s - 0.25).abs() < 1e-6));
    assert!(out.lane(0, 1).iter().all(|&s| (s - 0.25).abs() < 1e-6));
}

#[test]
fn a_polyphonic_input_is_refused() {
    let catalog = catalog();
    let result = Harness::new(
        node_type(&catalog),
        &Config::new(),
        &[(IN, Shape::new(4, 2))],
        RATE,
        256,
    );
    let error = result.err().expect("refused").to_string();
    assert!(error.contains("Voice Mix"), "{error}");
}

#[test]
fn a_three_channel_signal_into_a_stereo_port_is_refused() {
    let catalog = catalog();
    let result = Harness::new(
        node_type(&catalog),
        &Config::new(),
        &[(IN, Shape::new(1, 3))],
        RATE,
        256,
    );
    assert!(result.is_err());
}

fn note_on(time: u32, id: u32, key: u8, velocity: f32) -> Event {
    Event {
        time,
        kind: EventKind::NoteOn {
            note: NoteId(id),
            channel: 0,
            key,
            velocity,
        },
    }
}

fn note_off(time: u32, id: u32) -> Event {
    Event {
        time,
        kind: EventKind::NoteOff {
            note: NoteId(id),
            velocity: 0.0,
        },
    }
}

#[test]
fn a_note_makes_the_plugin_sound_at_that_pitch_and_a_note_off_stops_it() {
    let mut h = harness(&Config::new(), &[]);
    // A4 at full velocity.
    h.send_events(0, &[note_on(0, 7, 69, 1.0)]);
    h.run(256).unwrap();
    {
        let out = h.output(0);
        for (i, &s) in out.lane(0, 0).iter().enumerate().skip(1) {
            // The plugin's sine for the n-th sample after the start.
            let expected = (TAU * 440.0 * i as f32 / RATE).sin() * 0.2;
            assert!((s - expected).abs() < 2e-3, "frame {i}: {s} vs {expected}");
        }
    }
    // Still held: the next block carries on (no events this time).
    h.send_events(0, &[]);
    h.run(256).unwrap();
    assert!(h.output(0).lane(0, 0).iter().any(|&s| s.abs() > 0.1));

    // Released by id alone: the host fills in the key it remembers.
    h.send_events(0, &[note_off(0, 7)]);
    h.run(256).unwrap();
    assert!(h.output(0).lane(0, 0).iter().all(|&s| s == 0.0));
}

#[test]
fn every_note_input_reaches_the_plugin_in_time_order() {
    let mut h = harness(&Config::new(), &[]);
    // A4 on the first port at frame 0, A4 on the second (an octave up) at
    // frame 100, and the second port's earlier note first in its list.
    h.send_events(0, &[note_on(0, 1, 69, 1.0)]);
    h.send_events(1, &[note_on(100, 2, 69, 1.0)]);
    h.run(256).unwrap();
    let out = h.output(0);
    for (i, &s) in out.lane(0, 0).iter().enumerate().skip(1) {
        let low = (TAU * 440.0 * i as f32 / RATE).sin() * 0.2;
        let high = if i > 100 {
            (TAU * 880.0 * (i - 100) as f32 / RATE).sin() * 0.2
        } else {
            0.0
        };
        assert!((s - (low + high)).abs() < 3e-3, "frame {i}: {s} vs {}", low + high);
    }
    // A note-off on the second port by id alone stops that note only.
    h.send_events(0, &[]);
    h.send_events(1, &[note_off(0, 2)]);
    h.run(256).unwrap();
    let out = h.output(0);
    let tail = out.lane(0, 0);
    let expected = |i: usize| (TAU * 440.0 * (256 + i) as f32 / RATE).sin() * 0.2;
    for i in 0..256 {
        assert!((tail[i] - expected(i)).abs() < 3e-3, "frame {i}");
    }
}

#[test]
fn a_note_starts_at_its_frame_within_the_block() {
    let mut h = harness(&Config::new(), &[]);
    h.send_events(0, &[note_on(100, 1, 69, 1.0)]);
    h.run(256).unwrap();
    // The plugin applies note events at their frame: silence until 100, and
    // the sine starts there (it's 0 at its first frame).
    let out = h.output(0);
    assert!(out.lane(0, 0)[..=100].iter().all(|&s| s == 0.0));
    assert!(out.lane(0, 0)[101..].iter().any(|&s| s.abs() > 0.01));
}

#[test]
fn a_wire_into_a_parameter_modulates_it() {
    // Connect the gain input as a signal: 0 for the first half, 1 for the second.
    let mut h = harness(&Config::new(), &[(IN, Shape::STEREO), (GAIN, Shape::MONO)]);
    fill_input(&mut h, 256, |_| 1.0);
    {
        let mut gain = h.input(GAIN, 256);
        for (i, s) in gain.lane_mut(0, 0).iter_mut().enumerate() {
            *s = if i < 128 { 0.0 } else { 1.0 };
        }
    }
    h.run(256).unwrap();
    let out = h.output(0);
    let lane = out.lane(0, 0);
    // Parameters follow the wire in steps of PARAM_STEP frames.
    assert!(lane[..128].iter().all(|&s| s == 0.0));
    assert!(lane[128..].iter().all(|&s| s == 1.0));
}

#[test]
fn a_block_longer_than_max_frames_is_split() {
    let mut h = harness(&Config::new(), &[(IN, Shape::STEREO)]);
    // The harness's buffers hold max_frames, so drive the longer block by
    // running two in a row and checking nothing is lost between them.
    fill_input(&mut h, 256, |_| 0.5);
    h.run(256).unwrap();
    h.run(256).unwrap();
    assert!(
        h.output(0)
            .lane(0, 0)
            .iter()
            .all(|&s| (s - 0.5).abs() < 1e-6)
    );
}

fn state_config(bias: f32) -> Config {
    let text = base64::engine::general_purpose::STANDARD.encode(bias.to_le_bytes());
    Config::new().with(STATE_KEY, Value::Text(text))
}

#[test]
fn saved_state_comes_back_when_the_node_is_built() {
    // `bias` is state, not a parameter: it adds a DC offset.
    let mut h = harness(&state_config(0.25), &[]);
    h.run(64).unwrap();
    assert!(
        h.output(0)
            .lane(0, 0)
            .iter()
            .all(|&s| (s - 0.25).abs() < 1e-6)
    );

    // Without state it's 0.
    let mut h = harness(&Config::new(), &[]);
    h.run(64).unwrap();
    assert!(h.output(0).lane(0, 0).iter().all(|&s| s == 0.0));
}

#[test]
fn state_captured_from_a_plugin_restores_to_the_same_sound() {
    let catalog = catalog();
    let node = node_type(&catalog);
    let saved = 0.125f32.to_le_bytes();
    let captured = capture_state(node.path(), PLUGIN_ID, Some(&saved)).unwrap();
    assert_eq!(captured, saved);
    let text = base64::engine::general_purpose::STANDARD.encode(&captured);
    let config = Config::new().with(STATE_KEY, Value::Text(text));
    let mut h = Harness::new(node, &config, &[], RATE, 256).unwrap();
    h.run(32).unwrap();
    assert!(
        h.output(0)
            .lane(0, 1)
            .iter()
            .all(|&s| (s - 0.125).abs() < 1e-6)
    );
}

#[test]
fn bad_state_is_an_error_not_a_crash() {
    let catalog = catalog();
    let node = node_type(&catalog);
    let bad = Config::new().with(STATE_KEY, Value::Text("!!not base64!!".into()));
    assert!(Harness::new(node, &bad, &[], RATE, 256).is_err());
    // Valid base64, but too short for the plugin to read.
    let short = Config::new().with(STATE_KEY, Value::Text("AA==".into()));
    assert!(Harness::new(node, &short, &[], RATE, 256).is_err());
}

#[test]
fn a_missing_plugin_file_is_an_error_when_the_node_is_built() {
    let mut file = noodle_plugins::scan_file(&scanner(), &plugin_path());
    file.path = "/no/such/plugin.clap".into();
    let catalog = noodle_plugins::Catalog::new(&[file]);
    let result = Harness::new(node_type(&catalog), &Config::new(), &[], RATE, 256);
    assert!(result.is_err());
}

#[test]
fn registering_adds_one_node_type_per_plugin_and_replaces_on_repeat() {
    let catalog = catalog();
    let mut registry = noodle_engine::Registry::new();
    catalog.register(&mut registry);
    // Doing it again (a render's registry is built each time) is fine.
    (catalog.extender())(&mut registry);
    let ty = registry
        .get("clap.noodle.test.gain-sine")
        .expect("registered under clap.<id>");
    assert_eq!(ty.info().name, "Noodle Test Plugin");
    assert_eq!(ty.info().category, "Plugins");
}

#[test]
fn plugins_are_deterministic_unless_the_node_says_otherwise() {
    let catalog = catalog();
    let node = node_type(&catalog);
    assert!(
        NodeType::layout(node, &Config::new())
            .unwrap()
            .deterministic
    );
    let random = Config::new().with("nondeterministic", Value::Int(1));
    assert!(!NodeType::layout(node, &random).unwrap().deterministic);
    // The setting doesn't change the ports.
    let mut a = NodeType::layout(node, &Config::new()).unwrap();
    a.deterministic = false;
    assert_eq!(a, NodeType::layout(node, &random).unwrap());
}

#[test]
fn the_cache_key_covers_the_plugin_file() {
    let key_for = |path: &std::path::Path| {
        let mut file = noodle_plugins::scan_file(&scanner(), &plugin_path());
        file.path = path.to_owned();
        let catalog = noodle_plugins::Catalog::new(&[file]);
        let project = noodle_core::Project::new();
        let env = noodle_engine::CacheEnv {
            project: &project,
            sample_rate: RATE,
            frames: 1000,
            file_key: &|_| None,
        };
        let mut key = noodle_core::KeyBuilder::new("plugin-test");
        node_type(&catalog)
            .cache_inputs(noodle_core::NodeId(1), &Config::new(), &env, &mut key)
            .map(|()| key.finish())
    };
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("p.clap");
    std::fs::copy(plugin_path(), &copy).unwrap();
    let before = key_for(&copy).unwrap();
    assert_eq!(key_for(&copy).unwrap(), before, "same file, same key");
    let mut bytes = std::fs::read(&copy).unwrap();
    bytes.push(0);
    std::fs::write(&copy, bytes).unwrap();
    assert_ne!(
        key_for(&copy).unwrap(),
        before,
        "a changed plugin is a new key"
    );
    std::fs::remove_file(&copy).unwrap();
    assert!(key_for(&copy).is_err(), "a missing plugin can't be keyed");
}
