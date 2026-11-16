//! The learned drum hit classifier through ONNX Runtime: a small network over
//! the detector's features (see `noodle_io::feature_vector`) that gives a
//! probability for each of `noodle_io::DRUM_CLASSES`. Opening checks its
//! shapes, because these are other people's exports.

use std::path::Path;
use std::sync::Mutex;

use noodle_io::{DRUM_CLASSES, DRUM_FEATURES, DrumKind, Hit, HitLabeller};
use ort::session::Session;
use ort::value::Tensor;

use crate::SeparateError;
use crate::onnx::{OnnxSeparator, failed};

/// Labels hits with the classifier model. Falls back to `Unknown` for a hit
/// the model fails on, so a bad run cannot take the dialog down.
pub struct OnnxLabeller {
    session: Mutex<Session>,
    _environment: ort::environment::Environment,
}

impl std::fmt::Debug for OnnxLabeller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnnxLabeller")
    }
}

impl OnnxLabeller {
    /// Opens the model at `weights`. See `OnnxSeparator::load_runtime` for
    /// `runtime`. It must take `features` `[n, DRUM_FEATURES]` and give
    /// probabilities `[n, DRUM_CLASSES.len()]` as its first output.
    pub fn open(weights: &Path, runtime: Option<&Path>) -> Result<Self, SeparateError> {
        let environment = OnnxSeparator::environment_builder(runtime)?
            .with_name("noodle")
            .build()
            .map_err(|e| failed(format!("can't start ONNX Runtime: {e}")))?;
        let session = Session::builder(&environment)
            .and_then(|mut builder| builder.commit_from_file(weights))
            .map_err(|e| failed(format!("can't open the model {}: {e}", weights.display())))?;
        let shape = |input: bool| {
            let io = if input {
                session.inputs().first().map(|i| i.dtype())
            } else {
                session.outputs().first().map(|o| o.dtype())
            };
            io.and_then(|d| d.tensor_shape().map(|s| s.to_vec()))
        };
        let (want_in, want_out) = (DRUM_FEATURES as i64, DRUM_CLASSES.len() as i64);
        let fits = |s: Option<Vec<i64>>, last: i64| {
            s.is_some_and(|s| s.len() == 2 && (s[0] == -1 || s[0] == 1) && s[1] == last)
        };
        if session.inputs().len() != 1
            || !fits(shape(true), want_in)
            || !fits(shape(false), want_out)
        {
            return Err(failed(format!(
                "{} is not a drum classifier Noodle can run. It needs one input [n, {want_in}] \
                 and a first output [n, {want_out}] of class probabilities; this one has input \
                 {:?} and output {:?}",
                weights.display(),
                shape(true),
                shape(false),
            )));
        }
        Ok(Self {
            session: Mutex::new(session),
            _environment: environment,
        })
    }

    fn probabilities(&self, hit: &Hit) -> Option<Vec<f32>> {
        let v = noodle_io::feature_vector(hit);
        let tensor = Tensor::from_array(([1usize, DRUM_FEATURES], v.to_vec())).ok()?;
        let mut session = self.session.lock().ok()?;
        let outputs = session.run(ort::inputs![tensor]).ok()?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>().ok()?;
        Some(data.to_vec())
    }
}

impl HitLabeller for OnnxLabeller {
    fn label(&self, hit: &Hit) -> (DrumKind, f32) {
        match self.probabilities(hit) {
            Some(p) => noodle_io::kind_from_probabilities(&p, hit),
            None => (DrumKind::Unknown, 0.0),
        }
    }
}
