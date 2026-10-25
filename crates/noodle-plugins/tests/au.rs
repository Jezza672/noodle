//! Audio Units, through Apple's own units (present on every Mac): scanning,
//! ports, parameters, audio, notes, state and the catalog. macOS only.
//! Run with the real engine harness, like the CLAP and VST3 node tests.

#![cfg(target_os = "macos")]

mod common;

use base64::Engine as _;
use common::scanner;
use noodle_engine::testing::Harness;
use noodle_engine::{Config, Event, EventKind, NodeType, NoteId, Shape, Value};
use noodle_plugins::{
    Catalog, Descriptor, PluginFile, PluginFormat, STATE_KEY, audio_unit_files, capture_state,
    describe_file, scan_file,
};

const RATE: f32 = 48_000.0;

/// `aufx` `dely` `appl`: Apple's delay.
const DELAY: &str = "61756678-64656c79-6170706c";
/// `aumu` `dls ` `appl`: Apple's General MIDI synthesizer.
const DLS: &str = "61756d75-646c7320-6170706c";

fn file_of(id: &str) -> std::path::PathBuf {
    format!("audiounit:{id}").into()
}

fn scanned(id: &str) -> PluginFile {
    let file = scan_file(&scanner(), &file_of(id));
    assert_eq!(file.error, None, "scanning {id}");
    file
}

fn descriptor(id: &str) -> Descriptor {
    scanned(id).plugins.remove(0)
}

fn harness(id: &str, config: &Config, connected: &[(usize, Shape)]) -> Harness {
    let catalog = Catalog::new(&[scanned(id)]);
    let node = catalog.iter().next().expect("the unit is in the catalog");
    Harness::new(node, config, connected, RATE, 256).expect("harness")
}

fn param_port(d: &Descriptor, name: &str) -> usize {
    d.audio_inputs.len() + d.params.iter().position(|p| p.name == name).expect(name)
}

#[test]
fn installed_units_are_listed_as_virtual_files() {
    let files = audio_unit_files();
    for id in [DELAY, DLS] {
        assert!(files.contains(&file_of(id)), "{id} in {files:?}");
    }
}

#[test]
fn the_delay_is_described_from_the_unit() {
    let d = descriptor(DELAY);
    assert_eq!(d.format, PluginFormat::Au);
    assert_eq!(d.id, DELAY);
    assert_eq!(d.node_id(), format!("au.{DELAY}"));
    assert_eq!(d.name, "AUDelay");
    assert_eq!(d.vendor, "Apple");
    assert!(!d.instrument);
    assert_eq!(d.note_inputs, 0);
    assert_eq!(d.audio_inputs.len(), 1);
    assert_eq!(d.audio_outputs.len(), 1);
    assert_eq!(d.audio_outputs[0].channels, 2);
    let names: Vec<_> = d.params.iter().map(|p| p.name.as_str()).collect();
    for wanted in ["Dry/Wet Mix", "Feedback"] {
        assert!(names.contains(&wanted), "{wanted} in {names:?}");
    }
    let mix = &d.params[names.iter().position(|n| *n == "Dry/Wet Mix").unwrap()];
    assert_eq!((mix.min, mix.max), (0.0, 100.0));
}

#[test]
fn the_synth_is_an_instrument_with_a_note_port_and_no_audio_input() {
    let d = descriptor(DLS);
    assert!(d.instrument);
    assert_eq!(d.note_inputs, 1);
    assert!(d.audio_inputs.is_empty());
    // The synth has a main output bus and may have more.
    assert!(!d.audio_outputs.is_empty());
}

#[test]
fn a_scan_is_current_until_the_unit_changes() {
    assert!(scanned(DELAY).is_current());
}

#[test]
fn a_missing_unit_fails_to_scan_instead_of_crashing() {
    let file = scan_file(&scanner(), &file_of("61756678-00000000-00000000"));
    assert!(file.error.is_some());
    assert!(describe_file(&file_of("not an id")).is_err());
}

#[test]
fn audio_goes_through_the_unit_and_its_parameters_have_ports() {
    let d = descriptor(DELAY);
    let mix = param_port(&d, "Dry/Wet Mix");
    let mut h = harness(DELAY, &Config::new(), &[(0, Shape::STEREO)]);
    let signal = |i: usize| (i as f32 * 0.05).sin() * 0.5;
    let fill = |h: &mut Harness| {
        let mut input = h.input(0, 256);
        for channel in 0..input.shape().channels {
            for (i, s) in input.lane_mut(0, channel).iter_mut().enumerate() {
                *s = signal(i);
            }
        }
    };

    // All dry: the input comes straight out.
    h.set(mix, 0.0);
    fill(&mut h);
    h.run(256).unwrap();
    let out = h.output(0);
    assert_eq!(out.shape(), Shape::STEREO);
    let error = (0..256)
        .map(|i| (out.lane(0, 0)[i] - signal(i)).abs())
        .fold(0.0f32, f32::max);
    assert!(error < 1e-3, "dry output off by {error}");

    // All wet at the default one second of delay: nothing yet.
    h.set(mix, 100.0);
    fill(&mut h);
    h.run(256).unwrap();
    let peak = h
        .output(0)
        .lane(0, 0)
        .iter()
        .fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak < 1e-3, "the wet signal is a second late, got {peak}");
}

#[test]
fn the_default_second_of_delay_echoes_the_input() {
    // The delay time isn't a real-time parameter, so it has no port: the unit
    // keeps its default of one second (48000 frames).
    let d = descriptor(DELAY);
    let mix = param_port(&d, "Dry/Wet Mix");
    let mut h = harness(DELAY, &Config::new(), &[(0, Shape::STEREO)]);
    h.set(mix, 100.0);
    // A burst, then silence: the echo shows up about a second later.
    let mut echoed_at = None;
    for block in 0..260 {
        let mut input = h.input(0, 256);
        for channel in 0..input.shape().channels {
            for (i, s) in input.lane_mut(0, channel).iter_mut().enumerate() {
                *s = if block == 0 && i < 64 { 0.8 } else { 0.0 };
            }
        }
        h.run(256).unwrap();
        let peak = h
            .output(0)
            .lane(0, 0)
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        if peak > 0.05 {
            echoed_at = Some(block);
            break;
        }
    }
    let block = echoed_at.expect("the burst never came back");
    assert!(
        (180..=196).contains(&block),
        "a second is about 187 blocks, the echo came at {block}"
    );
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

fn peak_of_block(h: &mut Harness) -> f32 {
    h.run(256).unwrap();
    let out = h.output(0);
    (0..out.shape().channels)
        .flat_map(|c| out.lane(0, c).iter().copied())
        .fold(0.0f32, |m, s| m.max(s.abs()))
}

#[test]
fn notes_sound_the_synth_and_stop_it() {
    let mut h = harness(DLS, &Config::new(), &[]);
    assert!(peak_of_block(&mut h) < 1e-4, "silent before any note");
    h.send_events(0, &[note_on(0, 1, 69)]);
    let mut loudest = peak_of_block(&mut h);
    h.send_events(0, &[]);
    for _ in 0..40 {
        loudest = loudest.max(peak_of_block(&mut h));
    }
    assert!(loudest > 0.01, "the note sounded: peak {loudest}");

    h.send_events(0, &[note_off(0, 1)]);
    // The release dies away.
    let mut last = 1.0;
    for _ in 0..400 {
        last = peak_of_block(&mut h);
    }
    assert!(last < loudest * 0.1, "released: {last} against {loudest}");
}

#[test]
fn state_round_trips_and_a_node_restores_it() {
    let d = descriptor(DELAY);
    let id = d.id.clone();
    let path = file_of(DELAY);
    let state = capture_state(&path, &id, None).expect("state of a fresh unit");
    assert!(!state.is_empty());
    // Restoring and capturing again is stable enough to feed back in.
    let again = capture_state(&path, &id, Some(&state)).expect("restores");
    assert!(!again.is_empty());

    let text = base64::engine::general_purpose::STANDARD.encode(&again);
    let config = Config::new().with(STATE_KEY, Value::Text(text));
    let mut h = harness(DELAY, &config, &[(0, Shape::STEREO)]);
    h.run(256).unwrap();

    let junk = Config::new().with(STATE_KEY, Value::Text("AAAA".into()));
    let catalog = Catalog::new(&[scanned(DELAY)]);
    let node = catalog.iter().next().unwrap();
    assert!(Harness::new(node, &junk, &[(0, Shape::STEREO)], RATE, 256).is_err());
}

#[test]
fn the_node_type_takes_its_ports_from_the_unit() {
    let catalog = Catalog::new(&[scanned(DLS)]);
    let node = catalog.iter().next().unwrap();
    assert!(node.info().id.starts_with("au."));
    let layout = NodeType::layout(node, &Config::new()).unwrap();
    assert_eq!(layout.event_inputs.len(), 1);
    assert!(!layout.outputs.is_empty());
    assert_eq!(
        layout.outputs.len(),
        noodle_plugins::describe_file(&file_of(DLS)).unwrap()[0]
            .audio_outputs
            .len()
    );
}
