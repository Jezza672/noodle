//! Hosting VST3 plugins, through the `vst3` crate's bindings.
//!
//! A VST3 module is loaded from its bundle, its factory lists the plugin
//! classes, and each is made as a component (the audio side) and a controller
//! (parameters and state). Plugins are made, set up and torn down on the
//! shared plugin thread (see `thread.rs`), and processed on the audio thread.
//!
//! Editors (`IPlugView`) are embedded in a window the app makes (`gui.rs`).
//! A component and controller that are separate objects are connected, so
//! their messages reach each other, and the controller's edits reach the
//! processor as parameter changes.
//!
//! Not supported yet: units and program lists, and 64-bit audio.

// The VST3 enum constants are `u32` on most systems and `i32` on Windows, so
// casts to the field types are needed on some and redundant on others.
#![allow(clippy::unnecessary_cast)]

mod classes;
pub(crate) mod gui;
mod module;
mod node;
mod plugin;

use std::path::Path;

use crate::host::{HostError, LOAD_TIMEOUT};
use crate::thread;

pub(crate) use node::vst3_node;
pub use plugin::describe_file;
pub(crate) use plugin::{Plugin, class_id_from_text};

/// A plugin made and activated, with what the node needs from its controller.
pub(crate) struct Loaded {
    pub plugin: Plugin,
    pub midi_assignments: Vec<(i16, i16, vst3::Steinberg::Vst::ParamID)>,
    /// The latency the plugin reported once activated, in samples.
    pub latency: u32,
}

/// Makes, restores and activates the plugin with class ID `class_id` in the
/// module at `path`, on the plugin thread.
pub(crate) fn load(
    path: &Path,
    class_id: &str,
    state: Option<Vec<u8>>,
    sample_rate: f32,
    max_frames: usize,
) -> Result<Loaded, HostError> {
    let (path, class_id) = (path.to_owned(), class_id.to_owned());
    thread::run_job(LOAD_TIMEOUT, move || -> Result<Loaded, HostError> {
        let cid = class_id_from_text(&class_id)
            .ok_or_else(|| HostError(format!("{class_id} isn't a VST3 class ID")))?;
        let module = module::Module::load(&path)?;
        let mut plugin = Plugin::create(&module, &cid)?;
        if let Some(state) = state {
            plugin.load_state(&state)?;
        }
        plugin.activate(f64::from(sample_rate), max_frames)?;
        let midi_assignments = plugin.midi_assignments();
        // Asked here, on the plugin thread like everything else, and only
        // once activated.
        // SAFETY: a plain query on a live, set-up processor.
        let latency = unsafe {
            use vst3::Steinberg::Vst::IAudioProcessorTrait;
            plugin.processor.getLatencySamples()
        };
        Ok(Loaded {
            plugin,
            midi_assignments,
            latency,
        })
    })?
}

/// Makes the plugin (not activated), optionally restores `state`, and returns
/// the state it then saves.
pub(crate) fn capture_state(
    path: &Path,
    class_id: &str,
    state: Option<&[u8]>,
) -> Result<Vec<u8>, HostError> {
    let (path, class_id, state) = (
        path.to_owned(),
        class_id.to_owned(),
        state.map(<[u8]>::to_vec),
    );
    thread::run_job(LOAD_TIMEOUT, move || -> Result<Vec<u8>, HostError> {
        let cid = class_id_from_text(&class_id)
            .ok_or_else(|| HostError(format!("{class_id} isn't a VST3 class ID")))?;
        let module = module::Module::load(&path)?;
        let plugin = Plugin::create(&module, &cid)?;
        if let Some(state) = state {
            plugin.load_state(&state)?;
        }
        plugin.save_state()
    })?
}
