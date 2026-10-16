//! The ONNX backend against a real ONNX Runtime, with tiny models that have
//! HT-Demucs's signature. It needs the runtime library, so it runs only where
//! `NOODLE_TEST_ORT_DIR` names a folder holding the library (as
//! `libonnxruntime.so` or the platform's name), `fake.onnx` (input
//! `[1, 2, 343980]`, output slot k = (k + 1) times the input, `[1, 4, 2, 343980]`)
//! and `wrong.onnx` (any other signature). CI has none of these, so it skips.
#![cfg(feature = "onnx")]

use std::path::PathBuf;

use noodle_stems::{ModelId, OnnxSeparator, Separator, Stereo};

fn folder() -> Option<PathBuf> {
    std::env::var_os("NOODLE_TEST_ORT_DIR").map(PathBuf::from)
}

fn id() -> ModelId {
    ModelId {
        id: "fake".into(),
        version: "1".into(),
    }
}

fn runtime(dir: &std::path::Path) -> PathBuf {
    let named = dir.join(noodle_stems::runtime_file_name());
    if named.exists() {
        named
    } else {
        dir.join("libonnxruntime.so")
    }
}

#[test]
fn a_model_with_the_wrong_signature_is_refused_with_the_signature_spelled_out() {
    let Some(dir) = folder() else { return };
    let error = OnnxSeparator::open(&dir.join("wrong.onnx"), Some(&runtime(&dir)), id())
        .err()
        .expect("refused")
        .to_string();
    assert!(error.contains("[1, 2, 343980]"), "{error}");
}

#[test]
fn a_model_with_the_signature_separates_through_the_real_runtime() {
    let Some(dir) = folder() else { return };
    let mut separator =
        OnnxSeparator::open(&dir.join("fake.onnx"), Some(&runtime(&dir)), id()).unwrap();
    // Longer than a segment, so there are two.
    let frames = 400_000;
    let mix = Stereo {
        left: (0..frames)
            .map(|i| (i as f32 * 0.02).sin() * 0.4 + 0.05)
            .collect(),
        right: (0..frames)
            .map(|i| (i as f32 * 0.031).sin() * 0.2)
            .collect(),
    };
    let mean = mix
        .left
        .iter()
        .zip(&mix.right)
        .map(|(l, r)| 0.5 * (l + r))
        .sum::<f32>()
        / frames as f32;
    let stems = separator.separate(&mix, &mut |_| true).unwrap();
    // Drums, bass, vocals, other are slots 0, 1, 3, 2.
    for (stem, scale) in [(0, 1.0), (1, 2.0), (2, 4.0), (3, 3.0)] {
        assert_eq!(stems[stem].len(), frames);
        for i in (0..frames).step_by(1_009) {
            let want = scale * (mix.left[i] - mean) + mean;
            assert!(
                (stems[stem].left[i] - want).abs() < 1e-3,
                "stem {stem} frame {i}: {} against {want}",
                stems[stem].left[i]
            );
        }
    }
}
