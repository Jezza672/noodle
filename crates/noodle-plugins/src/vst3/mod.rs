//! Hosting VST3 plugins, through the `vst3` crate's bindings.
//!
//! A VST3 module is loaded from its bundle, its factory lists the plugin
//! classes, and each is made as a component (the audio side) and a controller
//! (parameters and state). Plugins are made, set up and torn down on the
//! shared plugin thread (see `thread.rs`), and processed on the audio thread.
//!
//! Not supported yet: plugin editors (`IPlugView`), `IMessage` between a
//! component and its controller, units and program lists, and 64-bit audio.

mod classes;
mod module;
mod node;
mod plugin;

use std::path::Path;

use crate::host::{HostError, LOAD_TIMEOUT};
use crate::thread;

pub(crate) use node::Vst3Node;
pub use plugin::describe_file;
pub(crate) use plugin::{Plugin, class_id_from_text};

/// A plugin made and activated, with what the node needs from its controller.
pub(crate) struct Loaded {
    pub plugin: Plugin,
    pub midi_assignments: Vec<(i16, i16, vst3::Steinberg::Vst::ParamID)>,
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
        Ok(Loaded {
            plugin,
            midi_assignments,
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
