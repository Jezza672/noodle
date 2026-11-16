//! The drum models through ONNX Runtime. Opening checks each network's shapes
//! against the manifest and says what it found if they differ, since these are
//! other people's exports.
//!
//! The expected signatures are in [`Layout`]. Where a model's inputs or
//! outputs come in another order, they are told apart by rank (a waveform is
//! rank 3, a spectrogram rank 4, the spectrogram branch's output rank 5).

use std::path::Path;
use std::sync::Arc;

use ort::session::Session;
use ort::value::Tensor;

use crate::SeparateError;
use crate::drum::{
    DrumModelManifest, DrumModelStore, HybridNet, HybridOutput, HybridSeparator, Layout, MaskNet,
    MaskSeparator, PartSeparator,
};
use crate::onnx::{OnnxSeparator, failed, fits};

type Environment = Arc<ort::environment::Environment>;

fn open_session(environment: &Environment, file: &Path) -> Result<Session, SeparateError> {
    Session::builder(environment)
        .and_then(|mut builder| builder.commit_from_file(file))
        .map_err(|e| failed(format!("can't open the model {}: {e}", file.display())))
}

fn shape_of(session: &Session, input: bool, index: usize) -> Option<Vec<i64>> {
    let io = if input {
        session.inputs().get(index)?.dtype()
    } else {
        session.outputs().get(index)?.dtype()
    };
    io.tensor_shape().map(|s| s.to_vec())
}

struct OnnxMask {
    session: Session,
    shape: [usize; 4],
}

impl OnnxMask {
    fn open(
        environment: &Environment,
        manifest: &DrumModelManifest,
        file: &Path,
    ) -> Result<Self, SeparateError> {
        let session = open_session(environment, file)?;
        let (bins, frames) = (
            manifest.chunking.bins() as i64,
            manifest.chunking.frames() as i64,
        );
        let want = [1, 2, bins, frames];
        let ok = session.inputs().len() == 1
            && !session.outputs().is_empty()
            && shape_of(&session, true, 0).is_some_and(|s| fits(&s, &want))
            && shape_of(&session, false, 0).is_some_and(|s| fits(&s, &want));
        if !ok {
            return Err(failed(format!(
                "{} is not the kind of {} network Noodle can run. It needs one input and an \
                 output of {want:?} (a stereo magnitude spectrogram); this one has input {:?} \
                 and output {:?}",
                file.display(),
                manifest.name,
                shape_of(&session, true, 0),
                shape_of(&session, false, 0),
            )));
        }
        Ok(Self {
            session,
            shape: [1, 2, bins as usize, frames as usize],
        })
    }
}

impl MaskNet for OnnxMask {
    fn run(&mut self, magnitude: &[f32]) -> Result<Vec<f32>, SeparateError> {
        let tensor = Tensor::from_array((self.shape, magnitude.to_vec())).map_err(failed)?;
        let outputs = self.session.run(ort::inputs![tensor]).map_err(failed)?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>().map_err(failed)?;
        Ok(data.to_vec())
    }
}

struct OnnxHybrid {
    session: Session,
    segment: usize,
    spec_shape: [usize; 4],
    /// Which input is the waveform (the other is the spectrogram).
    wave_first: bool,
    /// Which output is the spectrogram branch.
    spectrum_first: bool,
}

impl OnnxHybrid {
    fn open(
        environment: &Environment,
        manifest: &DrumModelManifest,
        file: &Path,
    ) -> Result<Self, SeparateError> {
        let session = open_session(environment, file)?;
        let c = manifest.chunking;
        let le = c.segment_samples.div_ceil(c.hop);
        let wave = [1, 2, c.segment_samples as i64];
        let spec = [1, 4, c.bins() as i64, le as i64];
        let parts = manifest.parts.len() as i64;
        let spectrum_out = [1, parts, 4, c.bins() as i64, le as i64];
        let time_out = [1, parts, 2, c.segment_samples as i64];
        let session_ref = &session;
        let shapes = |input: bool, n: usize| (0..n).map(move |i| shape_of(session_ref, input, i));
        let inputs: Vec<_> = shapes(true, session.inputs().len()).collect();
        let outputs: Vec<_> = shapes(false, session.outputs().len()).collect();
        let is = |s: &Option<Vec<i64>>, want: &[i64]| s.as_ref().is_some_and(|s| fits(s, want));
        let wave_first = inputs.len() == 2 && is(&inputs[0], &wave) && is(&inputs[1], &spec);
        let spec_first = inputs.len() == 2 && is(&inputs[0], &spec) && is(&inputs[1], &wave);
        let spectrum_first =
            outputs.len() >= 2 && is(&outputs[0], &spectrum_out) && is(&outputs[1], &time_out);
        let time_first =
            outputs.len() >= 2 && is(&outputs[0], &time_out) && is(&outputs[1], &spectrum_out);
        if !(wave_first || spec_first) || !(spectrum_first || time_first) {
            return Err(failed(format!(
                "{} is not the kind of Hybrid Demucs model Noodle can run. It needs two inputs, \
                 a waveform {wave:?} and a spectrogram {spec:?}, and two outputs, {spectrum_out:?} \
                 and {time_out:?}; this one has inputs {inputs:?} and outputs {outputs:?}",
                file.display()
            )));
        }
        Ok(Self {
            session,
            segment: c.segment_samples,
            spec_shape: [1, 4, c.bins(), le],
            wave_first,
            spectrum_first,
        })
    }
}

impl HybridNet for OnnxHybrid {
    fn run(&mut self, wave: &[f32], spectrum: &[f32]) -> Result<HybridOutput, SeparateError> {
        let w = Tensor::from_array(([1usize, 2, self.segment], wave.to_vec())).map_err(failed)?;
        let s = Tensor::from_array((self.spec_shape, spectrum.to_vec())).map_err(failed)?;
        let outputs = if self.wave_first {
            self.session.run(ort::inputs![w, s])
        } else {
            self.session.run(ort::inputs![s, w])
        }
        .map_err(failed)?;
        let (a, b) = (0, 1);
        let (spec_index, time_index) = if self.spectrum_first { (a, b) } else { (b, a) };
        let spectrum = outputs[spec_index]
            .try_extract_tensor::<f32>()
            .map_err(failed)?
            .1
            .to_vec();
        let time = outputs[time_index]
            .try_extract_tensor::<f32>()
            .map_err(failed)?
            .1
            .to_vec();
        Ok(HybridOutput { spectrum, time })
    }
}

/// Opens `manifest`'s model from the weights in `store`. `runtime` is
/// ONNX Runtime's library (see [`OnnxSeparator::load_runtime`]). Fails with a
/// message that says what to do if the weights are not downloaded or a file
/// is not the network the manifest describes.
pub fn open_separator(
    manifest: &'static DrumModelManifest,
    store: &DrumModelStore,
    runtime: Option<&Path>,
) -> Result<Box<dyn PartSeparator>, SeparateError> {
    let files = store.files(manifest).ok_or_else(|| {
        failed(format!(
            "{} has not been downloaded. Download it in Settings, Models",
            manifest.name
        ))
    })?;
    open_files(manifest, &files, runtime)
}

/// As [`open_separator`], from files already on disk in the manifest's order:
/// for trying weights by hand.
pub fn open_files(
    manifest: &'static DrumModelManifest,
    files: &[std::path::PathBuf],
    runtime: Option<&Path>,
) -> Result<Box<dyn PartSeparator>, SeparateError> {
    if files.len() != manifest.files.len() {
        return Err(failed(format!(
            "{} needs {} files, got {}",
            manifest.name,
            manifest.files.len(),
            files.len()
        )));
    }
    let environment: Environment = Arc::new(
        OnnxSeparator::environment_builder(runtime)?
            .with_name("noodle")
            .build()
            .map_err(|e| failed(format!("can't start ONNX Runtime: {e}")))?,
    );
    match manifest.layout {
        Layout::PerPartSpectrogram(_) => {
            let nets = files
                .iter()
                .map(|f| OnnxMask::open(&environment, manifest, f).map(|n| Box::new(n) as _))
                .collect::<Result<Vec<Box<dyn MaskNet>>, _>>()?;
            Ok(Box::new(MaskSeparator::new(manifest, nets)?))
        }
        Layout::HybridDemucs => {
            let net = OnnxHybrid::open(&environment, manifest, &files[0])?;
            Ok(Box::new(HybridSeparator::new(manifest, Box::new(net))?))
        }
    }
}
