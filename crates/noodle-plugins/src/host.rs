//! Loading CLAP plugins (VST3 is in `vst3/`), with clack: the host's callbacks, and describing a
//! plugin file for the scanner. Live instances belong to the shared plugin
//! thread (see `thread.rs`).

use std::io::Cursor;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::note_ports::PluginNotePorts;
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamInfoBuffer,
    ParamInfoFlags, ParamRescanFlags, PluginParams,
};
use clack_extensions::state::PluginState;
use clack_extensions::timer::{HostTimer, HostTimerImpl, TimerId};
use clack_host::prelude::*;

use crate::descriptor::{AudioPort, Descriptor, ParamDesc, PluginFormat};

/// The host side of a plugin.
pub(crate) struct Host;

/// What the plugin thread and the rest of the program share about one
/// instance, outside the plugin's own handlers.
#[derive(Default)]
pub(crate) struct Flags {
    /// The editor window is up.
    pub editor_open: AtomicBool,
    /// The plugin closed its window itself (or lost it): the thread still has
    /// to destroy the editor.
    pub editor_closed: AtomicBool,
    /// The plugin asked for a call on the main thread.
    pub callback: AtomicBool,
    /// The editor is embedded in a window of the host's, so the plugin's
    /// resize requests are the host's to carry out.
    pub embedded: AtomicBool,
    /// A size the plugin asked for (width in the high half, height in the
    /// low), or 0.
    pub resize: AtomicU64,
    timers: Mutex<Timers>,
}

struct Timer {
    id: TimerId,
    period: Duration,
    due: Instant,
}

#[derive(Default)]
struct Timers {
    next: u32,
    list: Vec<Timer>,
}

impl Flags {
    /// When the next timer is due, if any are registered.
    pub fn next_due(&self) -> Option<Instant> {
        let timers = self.timers.lock().unwrap_or_else(PoisonError::into_inner);
        timers.list.iter().map(|t| t.due).min()
    }

    /// The timers that are due at `now`, rescheduled.
    pub fn take_due(&self, now: Instant) -> Vec<TimerId> {
        let mut timers = self.timers.lock().unwrap_or_else(PoisonError::into_inner);
        timers
            .list
            .iter_mut()
            .filter(|t| t.due <= now)
            .map(|t| {
                t.due = now + t.period;
                t.id
            })
            .collect()
    }
}

pub(crate) struct HostShared {
    flags: Arc<Flags>,
}

impl HostShared {
    pub fn new(flags: Arc<Flags>) -> Self {
        Self { flags }
    }
}

impl SharedHandler<'_> for HostShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {
        self.flags.callback.store(true, Ordering::Release);
    }
}

impl HostGuiImpl for HostShared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> Result<(), clack_host::host::HostError> {
        // Floating windows size themselves; an embedded one is resized by the
        // plugin thread when it next looks.
        if self.flags.embedded.load(Ordering::Acquire) {
            let packed = (u64::from(new_size.width) << 32) | u64::from(new_size.height);
            self.flags.resize.store(packed, Ordering::Release);
        }
        Ok(())
    }

    fn request_show(&self) -> Result<(), clack_host::host::HostError> {
        Err(clack_host::host::HostError::Message(
            "the editor is opened from the node",
        ))
    }

    fn request_hide(&self) -> Result<(), clack_host::host::HostError> {
        Err(clack_host::host::HostError::Message(
            "the editor is closed from the node",
        ))
    }

    fn closed(&self, _was_destroyed: bool) {
        self.flags.editor_open.store(false, Ordering::Release);
        self.flags.editor_closed.store(true, Ordering::Release);
    }
}

impl HostParamsImplShared for HostShared {
    // Parameter changes made in the editor reach the audio thread through the
    // plugin itself; the host has nothing to flush.
    fn request_flush(&self) {}
}

pub(crate) struct HostMain<'a> {
    shared: &'a HostShared,
}

impl<'a> MainThreadHandler<'a> for HostMain<'a> {}

impl HostParamsImplMainThread for HostMain<'_> {
    fn rescan(&self, _flags: ParamRescanFlags) {}
    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

/// The shortest period a plugin's timer is given, whatever it asks for.
const MIN_TIMER_PERIOD: Duration = Duration::from_millis(10);

impl HostTimerImpl for HostMain<'_> {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, clack_host::host::HostError> {
        let mut timers = self
            .shared
            .flags
            .timers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let id = TimerId(timers.next);
        timers.next += 1;
        let period = Duration::from_millis(u64::from(period_ms)).max(MIN_TIMER_PERIOD);
        timers.list.push(Timer {
            id,
            period,
            due: Instant::now() + period,
        });
        Ok(id)
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), clack_host::host::HostError> {
        self.shared
            .flags
            .timers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .list
            .retain(|t| t.id != timer_id);
        Ok(())
    }
}

impl HostHandlers for Host {
    type Shared<'a> = HostShared;
    type MainThread<'a> = HostMain<'a>;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &HostShared) {
        builder
            .register::<HostGui>()
            .register::<HostParams>()
            .register::<HostTimer>();
    }
}

pub(crate) fn new_instance(
    entry: &PluginEntry,
    id: &std::ffi::CStr,
    info: &HostInfo,
    flags: Arc<Flags>,
) -> Result<PluginInstance<Host>, HostError> {
    PluginInstance::<Host>::new(
        |_| HostShared::new(flags),
        |shared| HostMain { shared },
        entry,
        id,
        info,
    )
    .map_err(|e| HostError(e.to_string()))
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

pub(crate) fn host_info() -> Result<HostInfo, HostError> {
    HostInfo::new(
        "Noodle",
        "Noodle",
        "https://github.com/Jezza672/noodle",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|e| HostError(e.to_string()))
}

pub(crate) fn load_entry(path: &Path) -> Result<PluginEntry, HostError> {
    // SAFETY: loading a plugin runs its code. That's what the user asked for
    // by choosing it; scanning does it in a child process for that reason.
    let entry = unsafe { PluginEntry::load(path) }
        .map_err(|error| HostError(format!("can't load {}: {error}", path.display())))?;
    // On macOS plugins stay loaded for the life of the process (CLAP here,
    // VST3 in `vst3/module.rs`). A plugin's Objective-C objects can outlive
    // its instance, still waiting in an autorelease pool the main thread
    // drains later (Surge's editor views do). Unloading or deinitialising
    // the plugin tears down its classes, and the drain then crashes.
    #[cfg(target_os = "macos")]
    std::mem::forget(entry.clone());
    Ok(entry)
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
    if PluginFormat::of_path(path) == PluginFormat::Vst3 {
        return crate::vst3::describe_file(path);
    }
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
        let mut instance = match new_instance(&entry, id, &info, Arc::default()) {
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
    let params = visible_params(&mut handle)
        .into_iter()
        .map(|(_, info)| info)
        .collect();
    Descriptor {
        format: PluginFormat::Clap,
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

/// The parameters a user can set (not hidden, not read-only), up to
/// [`MAX_PARAMS`], with the plugin's own IDs.
pub(crate) fn visible_params(handle: &mut PluginMainThreadHandle) -> Vec<(ClapId, ParamDesc)> {
    let mut params = Vec::new();
    let Some(ext) = handle.get_extension::<PluginParams>() else {
        return params;
    };
    let mut buffer = ParamInfoBuffer::new();
    for index in 0..ext.count(handle).min(MAX_PARAMS as u32) {
        let Some(info) = ext.get_info(handle, index, &mut buffer) else {
            continue;
        };
        if info
            .flags
            .intersects(ParamInfoFlags::IS_HIDDEN | ParamInfoFlags::IS_READONLY)
        {
            continue;
        }
        params.push((
            info.id,
            ParamDesc {
                id: info.id.get(),
                name: String::from_utf8_lossy(info.name).into_owned(),
                module: String::from_utf8_lossy(info.module).into_owned(),
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
            },
        ));
    }
    params
}

/// Marks state wrapped by [`seal_state`].
const STATE_MAGIC: &[u8; 4] = b"NCS1";

pub(crate) fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5_u32, |hash, &byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

/// Wraps a plugin's state in a length and checksum. Some plugins crash the
/// host on state that is cut short, and the plugin runs in our process, so
/// damaged state has to be refused before it gets to them.
pub(crate) fn seal_state(state: &[u8]) -> Vec<u8> {
    let mut sealed = Vec::with_capacity(state.len() + 12);
    sealed.extend_from_slice(STATE_MAGIC);
    sealed.extend_from_slice(&(state.len() as u32).to_le_bytes());
    sealed.extend_from_slice(&checksum(state).to_le_bytes());
    sealed.extend_from_slice(state);
    sealed
}

/// The plugin's own bytes from state [`seal_state`] wrapped; or, for state
/// saved before the wrapper existed (which can't be checked), the bytes as
/// they are.
pub(crate) fn unseal_state(state: &[u8]) -> Result<&[u8], HostError> {
    let Some(rest) = state.strip_prefix(STATE_MAGIC) else {
        return Ok(state);
    };
    let damaged = || HostError("the saved plugin state is damaged (cut short or edited)".into());
    let (len, rest) = rest.split_first_chunk::<4>().ok_or_else(damaged)?;
    let (sum, payload) = rest.split_first_chunk::<4>().ok_or_else(damaged)?;
    if payload.len() != u32::from_le_bytes(*len) as usize
        || checksum(payload) != u32::from_le_bytes(*sum)
    {
        return Err(damaged());
    }
    Ok(payload)
}

pub(crate) fn load_state(
    instance: &mut PluginInstance<Host>,
    state: &[u8],
) -> Result<(), HostError> {
    let state = unseal_state(state)?;
    let handle = instance.plugin_handle();
    let ext = handle
        .get_extension::<PluginState>()
        .ok_or_else(|| HostError("this plugin can't load state".into()))?;
    ext.load(&handle, &mut Cursor::new(state))
        .map_err(|error| HostError(error.to_string()))
}

pub(crate) fn save_state_of(instance: &mut PluginInstance<Host>) -> Result<Vec<u8>, HostError> {
    let handle = instance.plugin_handle();
    let ext = handle
        .get_extension::<PluginState>()
        .ok_or_else(|| HostError("this plugin can't save state".into()))?;
    let mut bytes = Vec::new();
    ext.save(&handle, &mut bytes)
        .map_err(|error| HostError(error.to_string()))?;
    Ok(seal_state(&bytes))
}

/// Loads a plugin, optionally restores `state` into it, and returns the state
/// it then saves. Runs the plugin's code, on the shared plugin thread.
pub fn capture_state(
    path: &Path,
    plugin_id: &str,
    state: Option<&[u8]>,
) -> Result<Vec<u8>, HostError> {
    if PluginFormat::of_path(path) == PluginFormat::Vst3 {
        return crate::vst3::capture_state(path, plugin_id, state);
    }
    crate::thread::capture_fresh(path, plugin_id, state)
}
