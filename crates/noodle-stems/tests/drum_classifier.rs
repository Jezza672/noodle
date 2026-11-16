//! The learned drum classifier through ONNX Runtime. Needs the model and the
//! runtime, neither of which CI has, so it runs only when
//! `NOODLE_CLASSIFIER_MODEL` names the file (and `ORT_DYLIB_PATH` the runtime):
//!
//! `NOODLE_CLASSIFIER_MODEL=classifier.onnx ORT_DYLIB_PATH=libonnxruntime.dylib cargo test -p noodle-stems --features onnx --test drum_classifier`
#![cfg(feature = "onnx")]

use noodle_io::{DrumKind, HitLabeller, analyse_hits};
use noodle_stems::drum::OnnxLabeller;
use std::path::PathBuf;

fn sine_kick() -> Vec<f32> {
    let rate = 44_100.0f32;
    let mut x = vec![0.0; 44_100];
    let mut phase = 0.0f32;
    for i in 0..22_050 {
        let t = i as f32 / rate;
        let hz = 50.0 + 100.0 * (-t * 40.0).exp();
        phase += std::f32::consts::TAU * hz / rate;
        x[4_410 + i] = phase.sin() * (-t * 12.0).exp();
    }
    x
}

#[test]
fn the_model_opens_and_gives_a_probability_for_a_hit() {
    let Some(path) = std::env::var_os("NOODLE_CLASSIFIER_MODEL").map(PathBuf::from) else {
        eprintln!("skipped: NOODLE_CLASSIFIER_MODEL is not set");
        return;
    };
    let labeller = OnnxLabeller::open(&path, None).expect("the model opens");
    let hits = analyse_hits(&sine_kick(), 44_100, &mut |_| true).unwrap();
    assert!(!hits.is_empty());
    let (kind, confidence) = labeller.label(&hits[0]);
    assert!((0.0..=1.0).contains(&confidence));
    assert_eq!(
        kind,
        DrumKind::Kick,
        "a low sine thump is a kick (got {kind:?})"
    );
}

#[test]
fn a_file_that_is_not_a_classifier_is_refused() {
    let Some(path) = std::env::var_os("NOODLE_CLASSIFIER_MODEL").map(PathBuf::from) else {
        return;
    };
    // Any other file is not a model at all.
    let junk = path.with_file_name("not-a-model.onnx");
    std::fs::write(&junk, b"not a model").unwrap();
    assert!(OnnxLabeller::open(&junk, None).is_err());
    std::fs::remove_file(junk).ok();
}
