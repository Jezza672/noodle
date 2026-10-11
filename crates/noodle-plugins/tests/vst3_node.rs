//! The VST3 test plugin as a node, driven through the engine's test harness.
//! The twin of `plugin_node.rs`.

mod common;

use std::f32::consts::TAU;

use base64::Engine as _;
use common::*;
use noodle_engine::testing::Harness;
use noodle_engine::{Config, Event, EventKind, NodeType, NoteId, Shape, Value};
use noodle_plugins::{PluginFormat, STATE_KEY, capture_state, scan_file};

// Port order: in, Gain, Invert, out, notes, notes2. (Boost is hidden.)
const IN: usize = 0;
const GAIN: usize = 1;
const INVERT: usize = 2;

const RATE: f32 = 48_000.0;

fn harness(config: &Config, connected: &[(usize, Shape)]) -> Harness {
    let catalog = vst3_catalog();
    Harness::new(vst3_node_type(&catalog), config, connected, RATE, 256).expect("harness")
}

fn fill_input(h: &mut Harness, frames: usize, value: impl Fn(usize) -> f32) {
    let mut input = h.input(IN, frames);
    for channel in 0..input.shape().channels {
        for (i, s) in input.lane_mut(0, channel).iter_mut().enumerate() {
            *s = value(i);
        }
    }
}

fn note_on(time: u32, id: u32, key: u8) -> Event {
    Event {
        time,
        kind: EventKind::NoteOn {
            note: NoteId(id),
            channel: 0,
            key,
            velocity: 1.0,
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

/// The plugin's state as the node saves it: a header, the component's bytes
/// (gain, invert, bias as f32) and the controller's (none).
fn state_of(gain: f32, invert: f32, bias: f32) -> Vec<u8> {
    let mut component = Vec::new();
    for v in [gain, invert, bias] {
        component.extend_from_slice(&v.to_le_bytes());
    }
    let mut bytes = b"NVS1".to_vec();
    bytes.extend_from_slice(&(component.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&component);
    bytes
}

fn state_config(bytes: &[u8]) -> Config {
    let text = base64::engine::general_purpose::STANDARD.encode(bytes);
    Config::new().with(STATE_KEY, Value::Text(text))
}

#[test]
fn ports_come_from_the_plugin() {
    let catalog = vst3_catalog();
    let node = vst3_node_type(&catalog);
    assert_eq!(node.descriptor().format, PluginFormat::Vst3);
    assert!(node.info().id.starts_with("vst3."));
    let layout = NodeType::layout(node, &Config::new()).unwrap();
    let keys =
        |ports: &[noodle_engine::Port]| ports.iter().map(|p| p.key.to_string()).collect::<Vec<_>>();
    let inputs: Vec<_> = layout.inputs.iter().map(|p| p.key.to_string()).collect();
    // The hidden parameter has no port.
    assert_eq!(inputs, ["in", "p1", "p2"]);
    assert_eq!(keys(&layout.outputs), ["out"]);
    assert_eq!(keys(&layout.event_inputs), ["notes", "notes2"]);
    assert!(layout.event_outputs.is_empty());
    // Parameters are normalized, 0 to 1, with the plugin's default.
    let noodle_engine::InputKind::Param(gain) = &layout.inputs[GAIN].kind else {
        panic!("gain is a parameter");
    };
    assert_eq!((gain.min, gain.max, gain.default), (0.0, 1.0, 0.5));
    // A stepped parameter holds its steps.
    let noodle_engine::InputKind::Param(invert) = &layout.inputs[INVERT].kind else {
        panic!("invert is a parameter");
    };
    assert!(matches!(
        invert.kind,
        noodle_engine::ParamKind::Stepped { .. }
    ));
    assert_eq!((invert.min, invert.max), (0.0, 1.0));
}

#[test]
fn audio_goes_through_the_plugin_scaled_by_the_gain_parameter() {
    let mut h = harness(&Config::new(), &[(IN, Shape::STEREO)]);
    fill_input(&mut h, 256, |i| (i as f32 * 0.01).sin() * 0.5);

    // The port's default (0.5 normalized) is the plugin's gain of 1.
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

    h.set(GAIN, 0.125);
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
fn polyphonic_and_three_channel_inputs_are_refused() {
    let catalog = vst3_catalog();
    let node = vst3_node_type(&catalog);
    let poly = Harness::new(node, &Config::new(), &[(IN, Shape::new(4, 2))], RATE, 256);
    let error = poly.err().expect("refused").to_string();
    assert!(error.contains("Voice Mix"), "{error}");
    let wide = Harness::new(node, &Config::new(), &[(IN, Shape::new(1, 3))], RATE, 256);
    assert!(wide.is_err());
}

#[test]
fn a_note_makes_the_plugin_sound_at_that_pitch_and_a_note_off_stops_it() {
    let mut h = harness(&Config::new(), &[]);
    h.send_events(0, &[note_on(0, 7, 69)]);
    h.run(256).unwrap();
    for (i, &s) in h.output(0).lane(0, 0).iter().enumerate().skip(1) {
        let expected = (TAU * 440.0 * i as f32 / RATE).sin() * 0.2;
        assert!((s - expected).abs() < 2e-3, "frame {i}: {s} vs {expected}");
    }
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
    h.send_events(0, &[note_on(100, 1, 69)]);
    h.send_events(1, &[note_on(0, 1, 69)]);
    h.run(256).unwrap();
    let out = h.output(0);
    for (i, &s) in out.lane(0, 0).iter().enumerate().skip(1) {
        let high = (TAU * 880.0 * i as f32 / RATE).sin() * 0.2;
        let low = if i > 100 {
            (TAU * 440.0 * (i - 100) as f32 / RATE).sin() * 0.2
        } else {
            0.0
        };
        assert!((s - (low + high)).abs() < 3e-3, "frame {i}");
    }
}

#[test]
fn a_note_starts_at_its_frame_within_the_block() {
    let mut h = harness(&Config::new(), &[]);
    h.send_events(0, &[note_on(100, 1, 69)]);
    h.run(256).unwrap();
    let out = h.output(0);
    assert!(out.lane(0, 0)[..=100].iter().all(|&s| s == 0.0));
    assert!(out.lane(0, 0)[101..].iter().any(|&s| s.abs() > 0.01));
}

#[test]
fn the_modulation_wheel_reaches_a_parameter_that_has_no_port() {
    let mut h = harness(&Config::new(), &[]);
    h.send_events(
        0,
        &[Event {
            time: 10,
            // Controller 1 at its maximum, on channel 0.
            kind: EventKind::Midi([0xB0, 1, 127]),
        }],
    );
    h.run(64).unwrap();
    let lane = h.output(0).lane(0, 0).to_vec();
    assert!(lane[..10].iter().all(|&s| s == 0.0));
    // `boost` adds 0.1 at its maximum.
    assert!(
        lane[10..].iter().all(|&s| (s - 0.1).abs() < 1e-6),
        "{lane:?}"
    );
}

#[test]
fn a_wire_into_a_parameter_modulates_it() {
    let mut h = harness(&Config::new(), &[(IN, Shape::STEREO), (GAIN, Shape::MONO)]);
    fill_input(&mut h, 256, |_| 1.0);
    {
        let mut gain = h.input(GAIN, 256);
        for (i, s) in gain.lane_mut(0, 0).iter_mut().enumerate() {
            *s = if i < 128 { 0.0 } else { 0.5 };
        }
    }
    h.run(256).unwrap();
    let out = h.output(0);
    let lane = out.lane(0, 0);
    assert!(lane[..128].iter().all(|&s| s == 0.0));
    assert!(lane[128..].iter().all(|&s| s == 1.0));
}

#[test]
fn saved_state_comes_back_when_the_node_is_built() {
    // `bias` is state, not a parameter: it adds a DC offset.
    let mut h = harness(&state_config(&state_of(1.0, 0.0, 0.25)), &[]);
    h.run(64).unwrap();
    assert!(
        h.output(0)
            .lane(0, 0)
            .iter()
            .all(|&s| (s - 0.25).abs() < 1e-6)
    );
    let mut h = harness(&Config::new(), &[]);
    h.run(64).unwrap();
    assert!(h.output(0).lane(0, 0).iter().all(|&s| s == 0.0));
}

#[test]
fn restored_state_is_not_overridden_by_untouched_ports() {
    // The state's gain is 0.5 (a quarter of the port's range): the port is
    // left at its default, so the plugin keeps the restored value.
    let mut h = harness(
        &state_config(&state_of(0.5, 0.0, 0.0)),
        &[(IN, Shape::STEREO)],
    );
    fill_input(&mut h, 64, |_| 1.0);
    h.run(64).unwrap();
    assert!(
        h.output(0)
            .lane(0, 0)
            .iter()
            .all(|&s| (s - 0.5).abs() < 1e-6)
    );
}

#[test]
fn state_captured_from_a_plugin_restores_to_the_same_sound() {
    let catalog = vst3_catalog();
    let node = vst3_node_type(&catalog);
    let saved = state_of(1.0, 0.0, 0.125);
    let captured = capture_state(node.path(), &node.descriptor().id, Some(&saved)).unwrap();
    // The same bytes, with the checksummed header.
    assert_eq!(captured[..4], *b"NVS2");
    assert_eq!(captured[12..], saved[8..]);
    let mut h = Harness::new(node, &state_config(&captured), &[], RATE, 256).unwrap();
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
    let catalog = vst3_catalog();
    let node = vst3_node_type(&catalog);
    let bad = Config::new().with(STATE_KEY, Value::Text("!!not base64!!".into()));
    assert!(Harness::new(node, &bad, &[], RATE, 256).is_err());
    // Valid base64, but not state this host saved.
    let short = Config::new().with(STATE_KEY, Value::Text("AA==".into()));
    assert!(Harness::new(node, &short, &[], RATE, 256).is_err());
    // The right header, but too little for the plugin to read.
    let cut = state_of(1.0, 0.0, 0.0)[..10].to_vec();
    assert!(Harness::new(node, &state_config(&cut), &[], RATE, 256).is_err());
}

#[test]
fn saved_state_has_a_checksum_that_refuses_cut_or_edited_bytes() {
    let catalog = vst3_catalog();
    let node = vst3_node_type(&catalog);
    let saved = state_of(1.0, 0.0, 0.25);
    let captured = capture_state(node.path(), &node.descriptor().id, Some(&saved)).unwrap();
    assert!(captured.starts_with(b"NVS2"));
    assert!(Harness::new(node, &state_config(&captured), &[], RATE, 256).is_ok());
    let cut = &captured[..captured.len() - 1];
    assert!(Harness::new(node, &state_config(cut), &[], RATE, 256).is_err());
    let mut edited = captured.clone();
    *edited.last_mut().unwrap() ^= 1;
    assert!(Harness::new(node, &state_config(&edited), &[], RATE, 256).is_err());
}

#[test]
fn a_missing_plugin_file_is_an_error_when_the_node_is_built() {
    let mut file = noodle_plugins::scan_file(&scanner(), &vst3_path());
    file.path = "/no/such/plugin.vst3".into();
    let catalog = noodle_plugins::Catalog::new(&[file]);
    let result = Harness::new(vst3_node_type(&catalog), &Config::new(), &[], RATE, 256);
    assert!(result.is_err());
}

#[test]
fn registering_adds_the_plugin_under_its_vst3_id() {
    let catalog = vst3_catalog();
    let mut registry = noodle_engine::Registry::new();
    catalog.register(&mut registry);
    (catalog.extender())(&mut registry);
    let node = vst3_node_type(&catalog);
    let ty = registry.get(node.info().id).expect("registered");
    assert_eq!(ty.info().name, "Noodle Test VST3");
    assert_eq!(ty.info().category, "Plugins");
}

#[test]
fn a_clap_and_a_vst3_plugin_can_share_a_catalog() {
    let clap = scan_file(&scanner(), &plugin_path());
    let vst3 = scan_file(&scanner(), &vst3_path());
    let catalog = noodle_plugins::Catalog::new(&[clap, vst3]);
    assert_eq!(catalog.len(), 2);
    assert!(catalog.get("clap.noodle.test.gain-sine").is_some());
    assert!(catalog.iter().any(|t| t.info().id.starts_with("vst3.")));
}

#[test]
fn the_cache_key_covers_the_plugin_bundle() {
    let key_for = |path: &std::path::Path| {
        let mut file = noodle_plugins::scan_file(&scanner(), &vst3_path());
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
        vst3_node_type(&catalog)
            .cache_inputs(noodle_core::NodeId(1), &Config::new(), &env, &mut key)
            .map(|()| key.finish())
    };
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("p.vst3");
    copy_dir(&vst3_path(), &copy);
    let before = key_for(&copy).unwrap();
    assert_eq!(key_for(&copy).unwrap(), before, "same bundle, same key");
    // Change the binary inside, not the folder itself.
    let binary = walk_files(&copy).pop().unwrap();
    let mut bytes = std::fs::read(&binary).unwrap();
    bytes.push(0);
    std::fs::write(&binary, bytes).unwrap();
    assert_ne!(
        key_for(&copy).unwrap(),
        before,
        "a changed plugin is a new key"
    );
    std::fs::remove_dir_all(&copy).unwrap();
    assert!(key_for(&copy).is_err(), "a missing plugin can't be keyed");
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn walk_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.path().is_dir() {
            found.extend(walk_files(&entry.path()));
        } else {
            found.push(entry.path());
        }
    }
    found.sort();
    found
}
