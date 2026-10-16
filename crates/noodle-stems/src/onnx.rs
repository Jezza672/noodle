//! HT-Demucs through ONNX Runtime.
//!
//! The runtime is loaded when a separator is opened (`ort`'s `load-dynamic`),
//! from the path given or the system's library search, so building this needs
//! no native library and a machine without one gets an error that says so.
//!
//! The model is taken to be an HT-Demucs export whose single input is a
//! `[1, 2, 343980]` waveform and whose first output is `[1, 4, 2, 343980]`, the
//! stems in the order HT-Demucs trains them (drums, bass, other, vocals), as
//! the Mixxx export has it. Audio is cut into overlapping segments of that
//! length, each run through the model, and blended with triangular weights, as
//! Demucs's own `apply_model` does.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;

use crate::{MODEL_RATE, ModelId, SeparateError, Separator, Stem, Stereo};

/// Frames the model takes at once: 7.8 s at 44.1 kHz.
const SEGMENT: usize = 343_980;
/// How much neighbouring segments overlap, as a fraction of a segment.
const OVERLAP: f32 = 0.25;
/// Where each of the model's outputs goes in [`Stem::ALL`] order.
const OUTPUT_ORDER: [Stem; 4] = [Stem::Drums, Stem::Bass, Stem::Other, Stem::Vocals];

pub struct OnnxSeparator {
    session: Session,
    model: ModelId,
    // Keeps the runtime loaded for as long as the session is.
    _environment: ort::environment::Environment,
}

fn failed(error: impl std::fmt::Display) -> SeparateError {
    SeparateError::Failed(error.to_string())
}

/// What `ort` refused the runtime for, once it has.
static LOAD_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The name ONNX Runtime's library has on this platform.
pub fn runtime_file_name() -> String {
    format!(
        "{}onnxruntime{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}

/// Whether `shape` (where -1 is any size) is `want`.
fn fits(shape: &[i64], want: &[i64]) -> bool {
    shape.len() == want.len() && shape.iter().zip(want).all(|(&s, &w)| s == w || s < 0)
}

const SIGNATURE: &str = "one input [1, 2, 343980] (a stereo waveform) and an output [1, 4, 2, 343980] \
     (four stereo stems), with the STFT and its inverse inside the graph";

impl OnnxSeparator {
    /// Loads the ONNX Runtime library, so that a machine without it finds out
    /// before anything is downloaded. `runtime` is the library's path; `None`
    /// uses the `ORT_DYLIB_PATH` variable, then the system's search for
    /// `libonnxruntime.so`, `.dylib` or `onnxruntime.dll`. Loading again is
    /// harmless.
    pub fn load_runtime(runtime: Option<&Path>) -> Result<(), SeparateError> {
        Self::environment_builder(runtime).map(|_| ())
    }

    fn environment_builder(
        runtime: Option<&Path>,
    ) -> Result<ort::environment::EnvironmentBuilder, SeparateError> {
        let path = match runtime {
            Some(path) => path.to_path_buf(),
            None => match std::env::var("ORT_DYLIB_PATH") {
                Ok(path) if !path.is_empty() => path.into(),
                _ => runtime_file_name().into(),
            },
        };
        let explain = |why: &dyn std::fmt::Display| {
            failed(format!(
                "can't load ONNX Runtime ({}): {why}. Install ONNX Runtime and put {} in the \
                 model folder or on the library path",
                path.display(),
                runtime_file_name()
            ))
        };
        // `ort` keeps a failed load, and after one it says every later load
        // worked and then panics when the library is first used. So a library
        // that is not there is found out here first, where trying again after
        // installing it works, and a failure `ort` itself reports is kept.
        let mut failure = LOAD_FAILURE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(why) = failure.as_ref() {
            return Err(failed(format!(
                "{why}. ONNX Runtime has to be fixed before Noodle is started again"
            )));
        }
        // SAFETY: loading a library runs its initialisers, which is what the
        // user asked for by installing it.
        let library = unsafe { libloading::Library::new(&path) }.map_err(|e| explain(&e))?;
        // SAFETY: only looks the symbol up.
        unsafe { library.get::<unsafe extern "C" fn()>(b"OrtGetApiBase") }
            .map_err(|_| explain(&"it does not look like ONNX Runtime"))?;
        drop(library);
        match ort::init_from(&path) {
            Ok(builder) => Ok(builder),
            Err(e) => {
                let error = explain(&e);
                *failure = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// Opens the model at `weights`. See [`load_runtime`](Self::load_runtime)
    /// for `runtime`. A model that doesn't have the signature this backend
    /// expects is refused here, with the signature spelled out.
    pub fn open(
        weights: &Path,
        runtime: Option<&Path>,
        model: ModelId,
    ) -> Result<Self, SeparateError> {
        let builder = Self::environment_builder(runtime)?;
        let environment = builder
            .with_name("noodle")
            .build()
            .map_err(|e| failed(format!("can't start ONNX Runtime: {e}")))?;
        let session = Session::builder(&environment)
            .and_then(|mut builder| builder.commit_from_file(weights))
            .map_err(|e| failed(format!("can't open the model {}: {e}", weights.display())))?;
        let seg = SEGMENT as i64;
        let input_ok = session.inputs().len() == 1
            && session.inputs()[0]
                .dtype()
                .tensor_shape()
                .is_some_and(|s| fits(s, &[1, 2, seg]));
        let output_ok = !session.outputs().is_empty()
            && session.outputs()[0]
                .dtype()
                .tensor_shape()
                .is_some_and(|s| fits(s, &[1, 4, 2, seg]));
        if !input_ok || !output_ok {
            return Err(failed(format!(
                "{} is not the kind of HT-Demucs model Noodle can run. It needs {SIGNATURE}; this one has {} input(s) and {} output(s)",
                weights.display(),
                session.inputs().len(),
                session.outputs().len(),
            )));
        }
        Ok(Self {
            session,
            model,
            _environment: environment,
        })
    }

    /// One segment through the model: `[2][SEGMENT]` in, `[4][2][SEGMENT]` out.
    fn run_segment(&mut self, input: Vec<f32>) -> Result<Vec<f32>, SeparateError> {
        let tensor = Tensor::from_array(([1usize, 2, SEGMENT], input)).map_err(failed)?;
        let outputs = self.session.run(ort::inputs![tensor]).map_err(failed)?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(failed)?;
        if data.len() != 4 * 2 * SEGMENT {
            return Err(failed(format!(
                "the model gave {shape:?} where [1, 4, 2, {SEGMENT}] was expected"
            )));
        }
        Ok(data.to_vec())
    }
}

/// The weight of frame `i` of a segment: a triangle, so a segment counts least
/// at its ends, where the model is least sure.
fn weight(i: usize) -> f32 {
    let half = SEGMENT / 2;
    if i < half {
        (i + 1) as f32
    } else {
        (SEGMENT - i) as f32
    }
}

impl Separator for OnnxSeparator {
    fn model(&self) -> ModelId {
        self.model.clone()
    }

    fn sample_rate(&self) -> u32 {
        MODEL_RATE
    }

    fn separate(
        &mut self,
        mix: &Stereo,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<[Stereo; 4], SeparateError> {
        blend(mix, progress, |input| self.run_segment(input))
    }
}

/// Runs `model` over `mix` a segment at a time and blends the overlapping
/// results. `model` takes `[2][SEGMENT]` samples and gives `[4][2][SEGMENT]`.
fn blend(
    mix: &Stereo,
    progress: &mut dyn FnMut(f32) -> bool,
    mut model: impl FnMut(Vec<f32>) -> Result<Vec<f32>, SeparateError>,
) -> Result<[Stereo; 4], SeparateError> {
    let frames = mix.len();
    // Demucs works on a mix scaled to zero mean and unit variance.
    let mono: Vec<f32> = mix
        .left
        .iter()
        .zip(&mix.right)
        .map(|(l, r)| 0.5 * (l + r))
        .collect();
    let mean = mono.iter().sum::<f32>() / frames.max(1) as f32;
    let variance = mono.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / frames.max(1) as f32;
    let std = variance.sqrt().max(1e-8);

    let mut sum = Stem::ALL.map(|_| Stereo::silent(frames));
    let mut total_weight = vec![0.0f32; frames];
    let stride = ((SEGMENT as f32 * (1.0 - OVERLAP)) as usize).max(1);
    // Segments every stride, the last one ending where the audio does, so the
    // model sees real audio through to the end instead of padding.
    let mut starts = Vec::new();
    let mut next = 0;
    loop {
        if next + SEGMENT >= frames {
            starts.push(frames.saturating_sub(SEGMENT));
            break;
        }
        starts.push(next);
        next += stride;
    }
    for (n, &start) in starts.iter().enumerate() {
        if !progress(n as f32 / starts.len() as f32) {
            return Err(SeparateError::Cancelled);
        }
        let len = (frames - start).min(SEGMENT);
        let mut input = vec![0.0f32; 2 * SEGMENT];
        for i in 0..len {
            input[i] = (mix.left[start + i] - mean) / std;
            input[SEGMENT + i] = (mix.right[start + i] - mean) / std;
        }
        let output = model(input)?;
        for (slot, stem) in OUTPUT_ORDER.iter().enumerate() {
            let target = &mut sum[Stem::ALL.iter().position(|s| s == stem).expect("a stem")];
            for channel in 0..2 {
                let from = &output[(slot * 2 + channel) * SEGMENT..][..len];
                let to = if channel == 0 {
                    &mut target.left
                } else {
                    &mut target.right
                };
                for i in 0..len {
                    to[start + i] += from[i] * weight(i);
                }
            }
        }
        for i in 0..len {
            total_weight[start + i] += weight(i);
        }
    }
    for stem in &mut sum {
        for channel in [&mut stem.left, &mut stem.right] {
            for (i, x) in channel.iter_mut().enumerate() {
                *x = *x / total_weight[i].max(1e-12) * std + mean;
            }
        }
    }
    if !progress(1.0) {
        return Err(SeparateError::Cancelled);
    }
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_library_anywhere_is_an_error_not_a_panic() {
        // `None` searches the system for the library, which isn't there.
        if std::env::var("ORT_DYLIB_PATH").is_ok_and(|p| !p.is_empty()) {
            return;
        }
        let error = OnnxSeparator::load_runtime(None).unwrap_err().to_string();
        assert!(
            error.contains("onnxruntime") && error.contains("Install"),
            "{error}"
        );
    }

    #[test]
    fn a_second_try_after_a_failed_load_fails_too() {
        let bad = Path::new("/nonexistent/libonnxruntime.so");
        assert!(OnnxSeparator::load_runtime(Some(bad)).is_err());
        assert!(OnnxSeparator::load_runtime(Some(bad)).is_err());
    }

    #[test]
    fn shapes_match_with_dynamic_sizes_as_wildcards() {
        assert!(fits(&[1, 2, 343_980], &[1, 2, 343_980]));
        assert!(fits(&[-1, 2, -1], &[1, 2, 343_980]));
        assert!(!fits(&[1, 2, 100], &[1, 2, 343_980]));
        assert!(!fits(&[1, 2], &[1, 2, 343_980]));
    }

    #[test]
    fn a_missing_library_is_an_error_that_names_it() {
        let error = OnnxSeparator::open(
            Path::new("/nonexistent/model.onnx"),
            Some(Path::new("/nonexistent/libonnxruntime.so")),
            ModelId {
                id: "x".into(),
                version: "1".into(),
            },
        )
        .err()
        .expect("there is no such library");
        assert!(error.to_string().contains("libonnxruntime"), "{error}");
    }

    /// A model that gives each output slot a different multiple of its input.
    fn scaling(input: Vec<f32>) -> Result<Vec<f32>, SeparateError> {
        let mut out = Vec::with_capacity(4 * 2 * SEGMENT);
        for slot in 0..4 {
            out.extend(input.iter().map(|x| x * (slot + 1) as f32));
        }
        Ok(out)
    }

    #[test]
    fn overlapping_segments_blend_back_into_one_signal_in_the_stems_order() {
        // Longer than two segments, so there are overlaps and a last segment
        // that starts early to end with the audio.
        let frames = SEGMENT * 2 + 12_345;
        let mix = Stereo {
            left: (0..frames)
                .map(|i| (i as f32 * 0.01).sin() * 0.5 + 0.1)
                .collect(),
            right: (0..frames)
                .map(|i| (i as f32 * 0.013).cos() * 0.25)
                .collect(),
        };
        let mean = mix
            .left
            .iter()
            .zip(&mix.right)
            .map(|(l, r)| 0.5 * (l + r))
            .sum::<f32>()
            / frames as f32;
        let stems = blend(&mix, &mut |_| true, scaling).unwrap();
        // Slot k holds (k + 1) times the mix about its mean, and slots 2 and 3
        // are other and vocals, which come out in the order vocals, other.
        for (stem, scale) in [(0, 1.0), (1, 2.0), (2, 4.0), (3, 3.0)] {
            assert_eq!(stems[stem].len(), frames);
            for i in (0..frames).step_by(997) {
                let want = scale * (mix.left[i] - mean) + mean;
                assert!(
                    (stems[stem].left[i] - want).abs() < 1e-3,
                    "stem {stem} frame {i}: {} against {want}",
                    stems[stem].left[i]
                );
            }
        }
    }

    #[test]
    fn a_segment_that_fails_or_is_cancelled_stops_the_job() {
        let mix = Stereo::silent(1000);
        let failing = |_: Vec<f32>| Err(SeparateError::Failed("boom".into()));
        assert_eq!(
            blend(&mix, &mut |_| true, failing).unwrap_err(),
            SeparateError::Failed("boom".into())
        );
        assert_eq!(
            blend(&mix, &mut |_| false, scaling).unwrap_err(),
            SeparateError::Cancelled
        );
    }

    #[test]
    fn segments_blend_with_a_triangle_that_peaks_in_the_middle() {
        assert_eq!(weight(0), 1.0);
        assert_eq!(weight(SEGMENT - 1), 1.0);
        assert!(weight(SEGMENT / 2) >= weight(SEGMENT / 2 - 1));
        assert!(weight(SEGMENT / 2) > weight(10));
    }
}
