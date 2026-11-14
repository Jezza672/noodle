//! The drum model backends against a real ONNX Runtime. Needs the runtime
//! library, so it runs only where `NOODLE_TEST_ORT_DIR` names a folder holding
//! it and `wrong.onnx` (any model whose shapes are not a drum network's); CI
//! has neither, so it skips.
//!
//! To try real weights by hand, put the files of a model (in the order of its
//! manifest's `files`) in a folder and run, with `NOODLE_DRUM_MODEL` set to
//! the model id and `NOODLE_DRUM_WEIGHTS` to the folder (files sorted by name
//! in manifest order are NOT assumed: they are looked up by manifest name):
//! `NOODLE_TEST_ORT_DIR=... NOODLE_DRUM_MODEL=timbrica NOODLE_DRUM_WEIGHTS=... \
//!  cargo test -p noodle-stems --features onnx --test drum_runtime -- --nocapture`.
#![cfg(feature = "onnx")]

use std::path::PathBuf;

use noodle_stems::drum::{manifest_by_id, open_files};
use noodle_stems::{Stereo, runtime_file_name};

fn runtime() -> Option<(PathBuf, PathBuf)> {
    let dir = PathBuf::from(std::env::var_os("NOODLE_TEST_ORT_DIR")?);
    let named = dir.join(runtime_file_name());
    let lib = if named.exists() {
        named
    } else {
        dir.join("libonnxruntime.so")
    };
    Some((dir, lib))
}

#[test]
fn a_file_with_the_wrong_shapes_is_refused_with_what_was_expected() {
    let Some((dir, lib)) = runtime() else { return };
    for id in ["larsnet", "timbrica", "drumsep"] {
        let manifest = manifest_by_id(id).unwrap();
        let files = vec![dir.join("wrong.onnx"); manifest.files.len()];
        let error = open_files(manifest, &files, Some(&lib))
            .err()
            .expect("refused")
            .to_string();
        assert!(error.contains("Noodle can run"), "{id}: {error}");
    }
}

#[test]
fn real_weights_separate_a_click_track() {
    let Some((_, lib)) = runtime() else { return };
    let (Ok(id), Some(folder)) = (
        std::env::var("NOODLE_DRUM_MODEL"),
        std::env::var_os("NOODLE_DRUM_WEIGHTS"),
    ) else {
        return;
    };
    let manifest = manifest_by_id(&id).expect("a known model id");
    let files: Vec<_> = manifest
        .files
        .iter()
        .map(|f| PathBuf::from(&folder).join(f.name))
        .collect();
    let mut separator = open_files(manifest, &files, Some(&lib)).unwrap();
    let frames = 44_100 * 4;
    let mix = Stereo {
        left: (0..frames)
            .map(|i| {
                if i % 22_050 < 400 {
                    (i as f32 * 0.3).sin() * 0.5
                } else {
                    0.0
                }
            })
            .collect(),
        right: vec![0.0; frames],
    };
    let parts = separator.separate(&mix, &mut |_| true).unwrap();
    assert_eq!(parts.len(), manifest.parts.len());
    assert!(parts.iter().all(|(_, p)| p.len() == frames));
}
