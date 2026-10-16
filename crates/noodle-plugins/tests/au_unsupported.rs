//! Off macOS an Audio Unit can't be hosted, and says so without crashing.
//! The twin of `au.rs`.

#![cfg(not(target_os = "macos"))]

mod common;

use common::scanner;
use noodle_engine::testing::Harness;
use noodle_engine::{Config, NodeType, Shape};
use noodle_plugins::{
    Catalog, Descriptor, PluginFile, PluginFormat, audio_unit_files, capture_state, describe_file,
    scan_file,
};

const ID: &str = "61756678-64656c79-6170706c";

fn path() -> std::path::PathBuf {
    format!("audiounit:{ID}").into()
}

#[test]
fn no_audio_units_are_listed() {
    assert!(audio_unit_files().is_empty());
}

#[test]
fn scanning_an_audio_unit_reports_an_error() {
    let file = scan_file(&scanner(), &path());
    assert!(file.plugins.is_empty());
    assert!(file.error.as_deref().is_some_and(|e| e.contains("macOS")));
    assert!(describe_file(&path()).is_err());
    assert!(capture_state(&path(), ID, None).is_err());
}

#[test]
fn a_saved_audio_unit_node_fails_to_build_with_a_reason() {
    // A scan cache saved on a Mac, opened elsewhere.
    let file = PluginFile {
        path: path(),
        len: 0,
        modified: 1,
        plugins: vec![Descriptor {
            format: PluginFormat::Au,
            id: ID.into(),
            name: "AUDelay".into(),
            vendor: "Apple".into(),
            instrument: false,
            audio_inputs: vec![noodle_plugins::AudioPort {
                name: "Input 1".into(),
                channels: 2,
            }],
            audio_outputs: vec![noodle_plugins::AudioPort {
                name: "Output 1".into(),
                channels: 2,
            }],
            note_inputs: 0,
            note_outputs: 0,
            params: Vec::new(),
        }],
        error: None,
    };
    // Not current: there is nothing to stamp it against here.
    assert!(!file.is_current());
    let catalog = Catalog::new(&[file]);
    let node = catalog.iter().next().unwrap();
    assert_eq!(node.info().id, format!("au.{ID}"));
    let error = Harness::new(node, &Config::new(), &[(0, Shape::STEREO)], 48_000.0, 256)
        .err()
        .expect("can't host it here");
    assert!(error.to_string().contains("macOS"), "{error}");
}
