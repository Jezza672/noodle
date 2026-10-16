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

impl OnnxSeparator {
    /// Opens the model at `weights`. `runtime` is the ONNX Runtime library
    /// (`libonnxruntime.so`, `.dylib` or `onnxruntime.dll`); `None` leaves it to
    /// the `ORT_DYLIB_PATH` variable and the system's search.
    pub fn open(
        weights: &Path,
        runtime: Option<&Path>,
        model: ModelId,
    ) -> Result<Self, SeparateError> {
        let builder = match runtime {
            Some(path) => ort::init_from(path).map_err(|e| {
                failed(format!(
                    "can't load the ONNX Runtime library at {}: {e}",
                    path.display()
                ))
            })?,
            None => ort::init(),
        };
        let environment = builder
            .with_name("noodle")
            .build()
            .map_err(|e| failed(format!("can't start ONNX Runtime: {e}")))?;
        let session = Session::builder(&environment)
            .and_then(|mut builder| builder.commit_from_file(weights))
            .map_err(|e| failed(format!("can't open the model {}: {e}", weights.display())))?;
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
    let starts: Vec<usize> = (0..frames.max(1)).step_by(stride).collect();
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
        if start + SEGMENT >= frames {
            break;
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
        // Longer than two segments, so there are overlaps and a short last one.
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
