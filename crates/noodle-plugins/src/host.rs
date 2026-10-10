//! Loading CLAP plugins, with clack.
//!
//! A plugin instance belongs to the thread that made it ("the main thread" in
//! CLAP terms), but a node has to be `Send`. So each instance gets its own
//! thread, which makes the plugin, activates it, and then waits until the
//! node hands the audio processor back to be deactivated. The node itself
//! holds only the processor, which CLAP lets the audio thread use.

use std::ffi::CString;
use std::io::Cursor;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::note_ports::PluginNotePorts;
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::state::PluginState;
use clack_host::prelude::*;

use crate::descriptor::{AudioPort, Descriptor, ParamDesc};

/// The host side of a plugin. Plugins may ask for things (a restart, a
/// callback); nothing here needs them yet.
pub(crate) struct Host;

pub(crate) struct HostShared;

impl SharedHandler<'_> for HostShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}

impl HostHandlers for Host {
    type Shared<'a> = HostShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

#[derive(Debug)]
pub struct HostError(pub String);

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

/// How long loading a plugin (create, restore state, activate) may take
/// before the node gives up, so one that hangs can't freeze the app for good.
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Most parameters a plugin may have as ports. More would make the node
/// unusable, and a plugin that has them is exposing internals.
pub const MAX_PARAMS: usize = 1024;

fn host_info() -> Result<HostInfo, HostError> {
    HostInfo::new(
        "Noodle",
        "Noodle",
        "https://github.com/Jezza672/noodle",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|e| HostError(e.to_string()))
}

fn load_entry(path: &Path) -> Result<PluginEntry, HostError> {
    // SAFETY: loading a plugin runs its code. That's what the user asked for
    // by choosing it; scanning does it in a child process for that reason.
    unsafe { PluginEntry::load(path) }
        .map_err(|error| HostError(format!("can't load {}: {error}", path.display())))
}

fn text(bytes: Option<&std::ffi::CStr>) -> String {
    bytes
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Loads the file and describes every plugin in it. This runs the plugins'
/// code (it instantiates each one to read its ports), so call it in the
/// scanner's child process.
pub fn describe_file(path: &Path) -> Result<Vec<Descriptor>, HostError> {
    let entry = load_entry(path)?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| HostError("not a CLAP plugin: no plugin factory".into()))?;
    let info = host_info()?;
    let mut found = Vec::new();
    let mut failure = None;
    for descriptor in factory.plugin_descriptors() {
        let Some(id) = descriptor.id() else { continue };
        // One plugin that won't start doesn't hide the file's others.
        let mut instance =
            match PluginInstance::<Host>::new(|_| HostShared, |_| (), &entry, id, &info) {
                Ok(instance) => instance,
                Err(error) => {
                    failure = Some(HostError(format!(
                        "can't create {}: {error}",
                        id.to_string_lossy()
                    )));
                    continue;
                }
            };
        let mut described = describe_instance(&mut instance);
        described.id = id.to_string_lossy().into_owned();
        described.name = text(descriptor.name());
        if described.name.is_empty() {
            described.name = described.id.clone();
        }
        described.vendor = text(descriptor.vendor());
        described.instrument = descriptor
            .features()
            .any(|feature| feature.to_bytes() == b"instrument");
        found.push(described);
    }
    match failure {
        Some(error) if found.is_empty() => Err(error),
        _ => Ok(found),
    }
}

fn describe_instance(instance: &mut PluginInstance<Host>) -> Descriptor {
    let mut handle = instance.plugin_handle();
    let audio = |handle: &mut PluginMainThreadHandle, is_input: bool| {
        let Some(ports) = handle.get_extension::<PluginAudioPorts>() else {
            return Vec::new();
        };
        let mut buffer = AudioPortInfoBuffer::new();
        (0..ports.count(handle, is_input))
            .filter_map(|index| {
                let port = ports.get(handle, index, is_input, &mut buffer)?;
                Some(AudioPort {
                    name: String::from_utf8_lossy(port.name).into_owned(),
                    channels: port.channel_count,
                })
            })
            .collect::<Vec<_>>()
    };
    let audio_inputs = audio(&mut handle, true);
    let audio_outputs = audio(&mut handle, false);
    let notes = |handle: &mut PluginMainThreadHandle, is_input: bool| {
        handle
            .get_extension::<PluginNotePorts>()
            .map_or(0, |ports| ports.count(handle, is_input))
    };
    let note_inputs = notes(&mut handle, true);
    let note_outputs = notes(&mut handle, false);
    let mut params = Vec::new();
    if let Some(ext) = handle.get_extension::<PluginParams>() {
        let mut buffer = ParamInfoBuffer::new();
        for index in 0..ext.count(&handle).min(MAX_PARAMS as u32) {
            let Some(info) = ext.get_info(&handle, index, &mut buffer) else {
                continue;
            };
            if info
                .flags
                .intersects(ParamInfoFlags::IS_HIDDEN | ParamInfoFlags::IS_READONLY)
            {
                continue;
            }
            params.push(ParamDesc {
                id: info.id.get(),
                name: String::from_utf8_lossy(info.name).into_owned(),
                module: String::from_utf8_lossy(info.module).into_owned(),
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
            });
        }
    }
    Descriptor {
        id: String::new(),
        name: String::new(),
        vendor: String::new(),
        instrument: false,
        audio_inputs,
        audio_outputs,
        note_inputs,
        note_outputs,
        params,
    }
}

/// What a running instance reports once it's active.
pub(crate) struct Running {
    pub processor: StoppedPluginAudioProcessor<Host>,
    pub control: Control,
}

/// The handle to an instance's thread. The node closes it when it's dropped.
pub(crate) struct Control {
    close: mpsc::Sender<StoppedPluginAudioProcessor<Host>>,
}

impl Control {
    /// Gives the processor back so the plugin is deactivated and destroyed on
    /// its own thread.
    pub fn close(self, processor: StoppedPluginAudioProcessor<Host>) {
        let _ = self.close.send(processor);
    }
}

/// Loads the plugin, restores `state` into it if given, and activates it.
pub(crate) fn start(
    path: &Path,
    plugin_id: &str,
    state: Option<Vec<u8>>,
    sample_rate: f32,
    max_frames: usize,
) -> Result<Running, HostError> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (close, incoming) = mpsc::channel();
    let path = path.to_owned();
    let plugin_id = CString::new(plugin_id).map_err(|e| HostError(e.to_string()))?;
    thread::Builder::new()
        .name("noodle-plugin".into())
        .spawn(move || {
            let created = (|| {
                let entry = load_entry(&path)?;
                let info = host_info()?;
                let mut instance =
                    PluginInstance::<Host>::new(|_| HostShared, |_| (), &entry, &plugin_id, &info)
                        .map_err(|e| HostError(e.to_string()))?;
                if let Some(state) = &state {
                    load_state(&mut instance, state)?;
                }
                let processor = instance
                    .activate(
                        |_, _| (),
                        PluginAudioConfiguration {
                            sample_rate: f64::from(sample_rate),
                            min_frames_count: 1,
                            max_frames_count: max_frames as u32,
                        },
                    )
                    .map_err(|e| HostError(e.to_string()))?;
                Ok::<_, HostError>((entry, instance, processor))
            })();
            let (_entry, mut instance, processor) = match created {
                Ok(parts) => parts,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            if let Err(mpsc::SendError(Ok(processor))) = ready_tx.send(Ok(processor)) {
                // The node gave up waiting (see `LOAD_TIMEOUT`).
                instance.deactivate(processor);
                return;
            }
            // Until the node gives the processor back (or is leaked, and the
            // sender with it).
            if let Ok(processor) = incoming.recv() {
                instance.deactivate(processor);
            }
        })
        .map_err(|e| HostError(e.to_string()))?;
    let processor = ready_rx
        .recv_timeout(LOAD_TIMEOUT)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                HostError(format!("the plugin didn't load within {LOAD_TIMEOUT:?}"))
            }
            mpsc::RecvTimeoutError::Disconnected => {
                HostError("the plugin's thread ended while loading".into())
            }
        })??;
    Ok(Running {
        processor,
        control: Control { close },
    })
}

fn load_state(instance: &mut PluginInstance<Host>, state: &[u8]) -> Result<(), HostError> {
    let handle = instance.plugin_handle();
    let ext = handle
        .get_extension::<PluginState>()
        .ok_or_else(|| HostError("this plugin can't load state".into()))?;
    ext.load(&handle, &mut Cursor::new(state))
        .map_err(|error| HostError(error.to_string()))
}

fn save_state_of(instance: &mut PluginInstance<Host>) -> Result<Vec<u8>, HostError> {
    let handle = instance.plugin_handle();
    let ext = handle
        .get_extension::<PluginState>()
        .ok_or_else(|| HostError("this plugin can't save state".into()))?;
    let mut bytes = Vec::new();
    ext.save(&handle, &mut bytes)
        .map_err(|error| HostError(error.to_string()))?;
    Ok(bytes)
}

/// Loads a plugin, optionally restores `state` into it, and returns the state
/// it then saves. This is how state gets into a project: the result goes in
/// the node's `state` setting (see [`crate::STATE_KEY`]). Runs the plugin's
/// code, on the calling thread.
pub fn capture_state(
    path: &Path,
    plugin_id: &str,
    state: Option<&[u8]>,
) -> Result<Vec<u8>, HostError> {
    let entry = load_entry(path)?;
    let info = host_info()?;
    let id = CString::new(plugin_id).map_err(|e| HostError(e.to_string()))?;
    let mut instance = PluginInstance::<Host>::new(|_| HostShared, |_| (), &entry, &id, &info)
        .map_err(|e| HostError(e.to_string()))?;
    if let Some(state) = state {
        load_state(&mut instance, state)?;
    }
    save_state_of(&mut instance)
}
