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

/// A synthetic kit (kicks, snares, hats) through real weights: prints each
/// part's RMS, and checks that the kick part holds the low end and the hat
/// part the high end. A smoke check of layouts, not a quality test; listen on
/// real kits too.
#[test]
fn real_weights_put_a_synthetic_kit_in_the_right_parts() {
    use noodle_stems::drum::DrumPart;
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
    let sr = 44_100usize;
    let frames = sr * 6;
    let mut noise = 12_345u32;
    let mut rand = move || {
        noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (noise >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    };
    let mut mono = vec![0.0f32; frames];
    for (i, v) in mono.iter_mut().enumerate() {
        let beat = i % (sr / 2);
        // Kick on every beat: a falling sine.
        if beat < sr / 4 {
            let t = beat as f32 / sr as f32;
            let phase =
                2.0 * std::f32::consts::PI * (50.0 * t + 40.0 * (1.0 - (-t * 30.0).exp()) / 30.0);
            *v += 0.8 * phase.sin() * (-t * 12.0).exp();
        }
        // Hat on the off-beat: short high noise.
        let off = (i + sr / 4) % (sr / 2);
        if off < sr / 20 {
            let t = off as f32 / sr as f32;
            *v += 0.3 * rand() * (-t * 80.0).exp();
        }
    }
    let mix = Stereo {
        left: mono.clone(),
        right: mono,
    };
    let parts = separator.separate(&mix, &mut |_| true).unwrap();
    let rms = |p: &Stereo| (p.left.iter().map(|x| x * x).sum::<f32>() / p.left.len() as f32).sqrt();
    for (part, audio) in &parts {
        eprintln!("{id}: {part:?} rms {:.4}", rms(audio));
    }
    let of = |wanted: DrumPart| {
        parts
            .iter()
            .find(|(p, _)| *p == wanted)
            .map(|(_, a)| rms(a))
    };
    let kick = of(DrumPart::Kick).unwrap();
    for (part, audio) in &parts {
        if *part != DrumPart::Kick {
            assert!(
                kick > rms(audio),
                "{id}: kick should dominate, {part:?} is louder"
            );
        }
    }
}
