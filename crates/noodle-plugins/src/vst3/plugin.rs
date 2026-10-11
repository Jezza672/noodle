//! One VST3 plugin: its component and controller, set up and torn down in
//! the order the standard asks for, and described for the scanner.

use std::ffi::c_void;
use std::sync::Arc;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper, Interface};

use crate::descriptor::{AudioPort, Descriptor, ParamDesc, PluginFormat};
use crate::host::{HostError, MAX_PARAMS};

use super::classes::{ComponentHandler, Edits, HostApp, Stream, raw, wstring};
use super::module::{ClassInfo, Module};

/// What the component's buses are, for the node's ports.
pub(crate) struct Buses {
    pub audio_in: Vec<AudioPort>,
    pub audio_out: Vec<AudioPort>,
    pub events_in: u32,
    pub events_out: u32,
}

pub(crate) struct Plugin {
    // The component goes last of the three: it is the object the others are
    // interfaces of, and some plugins complain if it is released while they
    // are still held.
    pub controller: Option<ComPtr<IEditController>>,
    pub processor: ComPtr<IAudioProcessor>,
    pub component: ComPtr<IComponent>,
    /// The controller is its own object (not the component answering for
    /// both), so it is initialised and terminated on its own.
    separate_controller: bool,
    active: bool,
    processing: bool,
    /// The component and controller, wired to message each other.
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    /// What the controller's editor changes, for the audio side to send on.
    edits: Arc<Edits>,
    _handler: ComWrapper<ComponentHandler>,
    _host: ComWrapper<HostApp>,
    // Last: the module's code must outlive everything made from it.
    _module: Arc<Module>,
}

// SAFETY: a VST3 plugin is used by one thread at a time (set up and torn down
// on the plugin thread, processed on the audio thread), and the node hands it
// from one to the other, never sharing it.
unsafe impl Send for Plugin {}

fn ok(result: tresult) -> bool {
    result == kResultOk
}

/// Makes the class with `cid` as an `I`.
fn create<I: Interface>(module: &Module, cid: &TUID) -> Option<ComPtr<I>> {
    let mut object: *mut c_void = std::ptr::null_mut();
    // SAFETY: the factory is valid for as long as the module; the IID is 16
    // bytes.
    let result = unsafe {
        module.factory().createInstance(
            cid.as_ptr(),
            I::IID.as_ptr() as *const std::ffi::c_char,
            &mut object,
        )
    };
    if !ok(result) {
        return None;
    }
    // SAFETY: on success the factory returns an `I` with a reference for us.
    unsafe { ComPtr::from_raw(object as *mut I) }
}

fn audio_buses(component: &ComPtr<IComponent>, direction: BusDirection) -> Vec<AudioPort> {
    // SAFETY: plain queries of the plugin.
    let count = unsafe { component.getBusCount(MediaTypes_::kAudio as MediaType, direction) };
    (0..count.max(0))
        .filter_map(|index| {
            let mut info: BusInfo = unsafe { std::mem::zeroed() };
            let result = unsafe {
                component.getBusInfo(
                    MediaTypes_::kAudio as MediaType,
                    direction,
                    index,
                    &mut info,
                )
            };
            ok(result).then(|| AudioPort {
                name: wstring(&info.name),
                channels: info.channelCount.max(1) as u32,
            })
        })
        .collect()
}

fn event_bus_count(component: &ComPtr<IComponent>, direction: BusDirection) -> u32 {
    // SAFETY: a plain query of the plugin.
    unsafe { component.getBusCount(MediaTypes_::kEvent as MediaType, direction) }.max(0) as u32
}

impl Plugin {
    /// Creates the plugin of class `cid` and initialises its component and
    /// controller, but doesn't activate it.
    pub fn create(module: &Arc<Module>, cid: &TUID) -> Result<Plugin, HostError> {
        let fail = |what: &str| HostError(format!("{}: {what}", module.path().display()));
        let host = ComWrapper::new(HostApp);
        let edits = Arc::new(Edits::default());
        let handler = ComWrapper::new(ComponentHandler {
            edits: Arc::clone(&edits),
        });
        let host_ptr = raw::<IHostApplication, _>(&host) as *mut FUnknown;

        let component =
            create::<IComponent>(module, cid).ok_or_else(|| fail("can't create the plugin"))?;
        // SAFETY: initialising a freshly made component with a host context.
        if !ok(unsafe { component.initialize(host_ptr) }) {
            return Err(fail("the plugin refused to initialise"));
        }
        let Some(processor) = component.cast::<IAudioProcessor>() else {
            // SAFETY: matching the initialise above.
            unsafe { component.terminate() };
            return Err(fail("not an audio processor"));
        };

        // The controller is the component itself or another class it names.
        let mut separate_controller = false;
        let mut controller = component.cast::<IEditController>();
        if controller.is_none() {
            let mut controller_cid: TUID = [0; 16];
            // SAFETY: a plain query of the plugin.
            if ok(unsafe { component.getControllerClassId(&mut controller_cid) }) {
                controller = create::<IEditController>(module, &controller_cid);
                if let Some(controller) = &controller {
                    // SAFETY: as for the component.
                    if ok(unsafe { controller.initialize(host_ptr) }) {
                        separate_controller = true;
                    }
                }
                if !separate_controller {
                    controller = None;
                }
            }
        }
        let mut connection = None;
        if let Some(controller) = &controller {
            // SAFETY: handing the controller a handler that outlives it (the
            // plugin keeps it in a field that drops after the controller is
            // terminated).
            unsafe { controller.setComponentHandler(raw::<IComponentHandler, _>(&handler)) };
            edits.set_ids(editable_ids(controller));
            // A component and a controller that are separate objects message
            // each other through connection points.
            if separate_controller
                && let (Some(a), Some(b)) = (
                    component.cast::<IConnectionPoint>(),
                    controller.cast::<IConnectionPoint>(),
                )
            {
                // SAFETY: both points are alive, and are disconnected before
                // either is terminated.
                let connected = unsafe { a.connect(b.as_ptr()) == kResultOk }
                    && unsafe { b.connect(a.as_ptr()) == kResultOk };
                if connected {
                    connection = Some((a, b));
                } else {
                    // SAFETY: undoing what may have been connected.
                    unsafe {
                        a.disconnect(b.as_ptr());
                        b.disconnect(a.as_ptr());
                    }
                }
            }
        }
        Ok(Plugin {
            component,
            processor,
            controller,
            separate_controller,
            active: false,
            processing: false,
            connection,
            edits,
            _handler: handler,
            _host: host,
            _module: Arc::clone(module),
        })
    }

    pub fn buses(&self) -> Buses {
        Buses {
            audio_in: audio_buses(&self.component, BusDirections_::kInput as BusDirection),
            audio_out: audio_buses(&self.component, BusDirections_::kOutput as BusDirection),
            events_in: event_bus_count(&self.component, BusDirections_::kInput as BusDirection),
            events_out: event_bus_count(&self.component, BusDirections_::kOutput as BusDirection),
        }
    }

    /// The parameters a user can set (not hidden, read-only or a program
    /// list), up to [`MAX_PARAMS`].
    pub fn params(&self) -> Vec<ParamDesc> {
        let Some(controller) = &self.controller else {
            return Vec::new();
        };
        listed_params(controller)
            .into_iter()
            .map(|info| {
                let steps = info.stepCount.max(0);
                let (max, default) = if steps > 0 {
                    (
                        f64::from(steps),
                        (info.defaultNormalizedValue * f64::from(steps)).round(),
                    )
                } else {
                    (1.0, info.defaultNormalizedValue)
                };
                let title = wstring(&info.title);
                ParamDesc {
                    id: info.id,
                    name: if title.is_empty() {
                        wstring(&info.shortTitle)
                    } else {
                        title
                    },
                    module: String::new(),
                    min: 0.0,
                    max,
                    default,
                    stepped: steps > 0,
                }
            })
            .collect()
    }

    /// What the controller's editor changes, to be sent to the processor.
    pub fn edits(&self) -> &Arc<Edits> {
        &self.edits
    }

    /// The module the plugin's code is from, to keep loaded.
    pub fn module(&self) -> &Arc<Module> {
        &self._module
    }

    /// The part of the plugin the plugin thread uses (state, parameter
    /// values, the editor), apart from the audio side.
    pub fn control(&self) -> Control {
        Control {
            component: self.component.clone(),
            controller: self.controller.clone(),
            separate: self.separate_controller,
        }
    }

    /// Sets the buses up, tells the plugin the sample rate and block size and
    /// activates it.
    pub fn activate(&mut self, sample_rate: f64, max_frames: usize) -> Result<(), HostError> {
        let fail = |what: &str| HostError(what.to_owned());
        // SAFETY: plain calls into the plugin, in the order the standard
        // gives (arrangements, buses, setup, then active).
        unsafe {
            if !ok(self
                .processor
                .canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32))
            {
                return Err(fail("the plugin can't process 32-bit audio"));
            }
            let mut all_known = true;
            let mut arrangements = |direction: BusDirection, count: i32| {
                (0..count)
                    .map(|index| {
                        let mut arrangement: SpeakerArrangement = 0;
                        all_known &= ok(self.processor.getBusArrangement(
                            direction,
                            index,
                            &mut arrangement,
                        ));
                        arrangement
                    })
                    .collect::<Vec<_>>()
            };
            let ins = self.component.getBusCount(
                MediaTypes_::kAudio as MediaType,
                BusDirections_::kInput as i32,
            );
            let outs = self.component.getBusCount(
                MediaTypes_::kAudio as MediaType,
                BusDirections_::kOutput as i32,
            );
            let mut inputs = arrangements(BusDirections_::kInput as i32, ins);
            let mut outputs = arrangements(BusDirections_::kOutput as i32, outs);
            // Declining is fine: the plugin keeps the arrangement it has. If
            // it couldn't say what that is, it isn't asked to change.
            if all_known {
                self.processor.setBusArrangements(
                    inputs.as_mut_ptr(),
                    inputs.len() as i32,
                    outputs.as_mut_ptr(),
                    outputs.len() as i32,
                );
            }
            for (media, direction) in [
                (MediaTypes_::kAudio, BusDirections_::kInput),
                (MediaTypes_::kAudio, BusDirections_::kOutput),
                (MediaTypes_::kEvent, BusDirections_::kInput),
                (MediaTypes_::kEvent, BusDirections_::kOutput),
            ] {
                let count = self
                    .component
                    .getBusCount(media as MediaType, direction as i32);
                for index in 0..count {
                    self.component
                        .activateBus(media as MediaType, direction as i32, index, 1);
                }
            }
            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: max_frames as i32,
                sampleRate: sample_rate,
            };
            if !ok(self.processor.setupProcessing(&mut setup)) {
                return Err(fail("the plugin refused the sample rate or block size"));
            }
            if !ok(self.component.setActive(1)) {
                return Err(fail("the plugin refused to activate"));
            }
        }
        self.active = true;
        Ok(())
    }

    /// Starts processing; belongs on the audio thread, before the first
    /// block.
    pub fn start_processing(&mut self) -> bool {
        if !self.processing {
            // SAFETY: a plain call into the plugin.
            let result = unsafe { self.processor.setProcessing(1) };
            // A plugin that doesn't implement it (kNotImplemented) is still
            // processed.
            self.processing = result == kResultOk || result == kNotImplemented;
        }
        self.processing
    }

    /// Clears the plugin's internal state (delay lines, voices), as a
    /// transport restart should. Stopping and starting processing is the
    /// standard's signal for it, and is allowed on the audio thread
    /// (`setActive` is not).
    pub fn reset(&mut self) {
        if self.processing {
            // SAFETY: plain calls into the plugin.
            unsafe {
                self.processor.setProcessing(0);
                self.processor.setProcessing(1);
            }
        }
    }

    /// The component's and the controller's state, as one byte string.
    pub fn save_state(&self) -> Result<Vec<u8>, HostError> {
        self.control().save_state()
    }

    /// Restores what [`save_state`](Self::save_state) saved.
    pub fn load_state(&self, state: &[u8]) -> Result<(), HostError> {
        let bad = || HostError("the saved plugin state is damaged".into());
        let rest = state.strip_prefix(STATE_MAGIC).ok_or_else(bad)?;
        let (len, rest) = rest.split_first_chunk::<4>().ok_or_else(bad)?;
        let len = u32::from_le_bytes(*len) as usize;
        if rest.len() < len {
            return Err(bad());
        }
        let (component_bytes, controller_bytes) = rest.split_at(len);
        let component = Stream::new(component_bytes.to_vec());
        // SAFETY: the streams outlive the calls.
        if !ok(unsafe { self.component.setState(raw::<IBStream, _>(&component)) }) {
            return Err(HostError("the plugin refused its saved state".into()));
        }
        if let Some(controller) = &self.controller
            && self.separate_controller
        {
            component.rewind();
            unsafe { controller.setComponentState(raw::<IBStream, _>(&component)) };
            if !controller_bytes.is_empty() {
                let own = Stream::new(controller_bytes.to_vec());
                unsafe { controller.setState(raw::<IBStream, _>(&own)) };
            }
        }
        Ok(())
    }

    /// The MIDI controllers the plugin maps to parameters, for channel 0..16
    /// and the controllers a keyboard uses: `(channel, controller, id)`.
    /// Controller 128 is channel pressure and 129 is pitch bend.
    pub fn midi_assignments(&self) -> Vec<(i16, i16, ParamID)> {
        let Some(mapping) = self
            .controller
            .as_ref()
            .and_then(|c| c.cast::<IMidiMapping>())
        else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for channel in 0..16i16 {
            for controller in MIDI_CONTROLLERS {
                let mut id: ParamID = 0;
                // SAFETY: a plain query of the plugin.
                if ok(unsafe {
                    mapping.getMidiControllerAssignment(0, channel, controller, &mut id)
                }) {
                    found.push((channel, controller, id));
                }
            }
        }
        found
    }

    /// Sets a parameter on the controller too, so what it reports (and saves)
    /// agrees with what the audio side was told.
    #[allow(dead_code)]
    pub fn set_normalized(&self, id: ParamID, value: f64) {
        if let Some(controller) = &self.controller {
            // SAFETY: a plain call into the plugin.
            unsafe { controller.setParamNormalized(id, value) };
        }
    }
}

/// The component and controller as the plugin thread uses them: to save the
/// plugin's state, read its parameters and open its editor, while the audio
/// side processes. (The standard has these on the UI thread, with the audio
/// thread free to run.)
#[derive(Clone)]
pub(crate) struct Control {
    pub component: ComPtr<IComponent>,
    pub controller: Option<ComPtr<IEditController>>,
    /// The controller is its own object.
    separate: bool,
}

impl Control {
    /// The component's and the controller's state, as one byte string.
    pub fn save_state(&self) -> Result<Vec<u8>, HostError> {
        let component = Stream::new(Vec::new());
        // SAFETY: the stream outlives the call.
        if !ok(unsafe { self.component.getState(raw::<IBStream, _>(&component)) }) {
            return Err(HostError("this plugin can't save state".into()));
        }
        let controller = Stream::new(Vec::new());
        if self.separate
            && let Some(c) = &self.controller
        {
            // A controller with nothing of its own to keep is fine.
            unsafe { c.getState(raw::<IBStream, _>(&controller)) };
        }
        let (component, controller) = (component.bytes(), controller.bytes());
        let mut bytes = Vec::with_capacity(8 + component.len() + controller.len());
        bytes.extend_from_slice(STATE_MAGIC);
        bytes.extend_from_slice(&(component.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&component);
        bytes.extend_from_slice(&controller);
        Ok(bytes)
    }

    /// Each listed parameter's ID and its value as its port holds it (the
    /// step number for a stepped one).
    pub fn values(&self) -> Vec<(u32, f64)> {
        let Some(controller) = &self.controller else {
            return Vec::new();
        };
        listed_params(controller)
            .into_iter()
            .map(|info| {
                // SAFETY: a plain query of the plugin.
                let normalized = unsafe { controller.getParamNormalized(info.id) };
                let steps = info.stepCount.max(0);
                let value = if steps > 0 {
                    (normalized * f64::from(steps)).round()
                } else {
                    normalized
                };
                (info.id, value)
            })
            .collect()
    }
}

/// The parameters of `controller` a user can set, up to [`MAX_PARAMS`].
fn listed_params(controller: &ComPtr<IEditController>) -> Vec<ParameterInfo> {
    // SAFETY: plain queries of the plugin.
    let count = unsafe { controller.getParameterCount() }.max(0) as usize;
    let mut found = Vec::new();
    for index in 0..count {
        if found.len() == MAX_PARAMS {
            break;
        }
        let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
        if !ok(unsafe { controller.getParameterInfo(index as i32, &mut info) }) {
            continue;
        }
        let hidden = ParameterInfo_::ParameterFlags_::kIsHidden
            | ParameterInfo_::ParameterFlags_::kIsReadOnly
            | ParameterInfo_::ParameterFlags_::kIsProgramChange;
        if info.flags & hidden == 0 {
            found.push(info);
        }
    }
    found
}

/// The most parameters edits are kept for.
const MAX_EDITABLE: usize = 2048;

/// Every parameter an editor may change: any but read-only ones, listed or
/// not (hidden ones such as bypass and program changes included).
fn editable_ids(controller: &ComPtr<IEditController>) -> Vec<ParamID> {
    // SAFETY: plain queries of the plugin.
    let count = unsafe { controller.getParameterCount() }.max(0) as usize;
    let mut ids = Vec::new();
    for index in 0..count.min(MAX_EDITABLE) {
        let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
        if ok(unsafe { controller.getParameterInfo(index as i32, &mut info) })
            && info.flags & ParameterInfo_::ParameterFlags_::kIsReadOnly == 0
        {
            ids.push(info.id);
        }
    }
    ids
}

/// The controllers [`Plugin::midi_assignments`] asks about: modulation wheel,
/// volume, pan, expression, sustain, channel pressure and pitch bend.
const MIDI_CONTROLLERS: [i16; 7] = [1, 7, 10, 11, 64, 128, 129];

const STATE_MAGIC: &[u8] = b"NVS1";

impl Drop for Plugin {
    fn drop(&mut self) {
        // SAFETY: tearing down in the reverse of the order set up in.
        unsafe {
            if self.processing {
                self.processor.setProcessing(0);
            }
            if self.active {
                self.component.setActive(0);
            }
            if let Some((component, controller)) = self.connection.take() {
                component.disconnect(controller.as_ptr());
                controller.disconnect(component.as_ptr());
            }
            if self.separate_controller
                && let Some(controller) = &self.controller
            {
                controller.terminate();
            }
            self.component.terminate();
        }
    }
}

/// Describes every audio plugin in the file at `path`. Runs the plugins'
/// code, so the scanner calls it in a child process.
pub fn describe_file(path: &std::path::Path) -> Result<Vec<Descriptor>, HostError> {
    let module = Module::load(path)?;
    let classes = module.classes();
    if classes.is_empty() {
        return Err(HostError("no audio plugins in this VST3 module".into()));
    }
    let mut found = Vec::new();
    let mut failure = None;
    for class in classes {
        // One plugin that won't start doesn't hide the file's others.
        match describe_class(&module, &class) {
            Ok(descriptor) => found.push(descriptor),
            Err(error) => failure = Some(error),
        }
    }
    match failure {
        Some(error) if found.is_empty() => Err(error),
        _ => Ok(found),
    }
}

fn describe_class(module: &Arc<Module>, class: &ClassInfo) -> Result<Descriptor, HostError> {
    let plugin = Plugin::create(module, &class.cid)?;
    let buses = plugin.buses();
    let params = plugin.params();
    Ok(Descriptor {
        format: PluginFormat::Vst3,
        id: class_id_text(&class.cid),
        name: class.name.clone(),
        vendor: class.vendor.clone(),
        instrument: class.sub_categories.split('|').any(|c| c == "Instrument"),
        audio_inputs: buses.audio_in,
        audio_outputs: buses.audio_out,
        note_inputs: buses.events_in,
        note_outputs: buses.events_out,
        params,
    })
}

/// A class ID as 32 hex digits, the plugin's ID in Noodle.
pub(crate) fn class_id_text(cid: &TUID) -> String {
    let mut bytes = cid.map(|b| b as u8);
    swap_guid_fields(&mut bytes);
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// On Windows a class ID is stored as a COM GUID, whose first three fields are
/// little-endian; elsewhere they are big-endian. Swapping them makes a plugin's
/// ID text the same on every system, so a project opens on any of them.
fn swap_guid_fields(bytes: &mut [u8; 16]) {
    if cfg!(windows) {
        bytes[0..4].reverse();
        bytes[4..6].reverse();
        bytes[6..8].reverse();
    }
}

/// The inverse of [`class_id_text`].
pub(crate) fn class_id_from_text(text: &str) -> Option<TUID> {
    if text.len() != 32 || !text.is_ascii() {
        return None;
    }
    let mut cid: TUID = [0; 16];
    for (i, slot) in cid.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()? as std::ffi::c_char;
    }
    let mut bytes = cid.map(|b| b as u8);
    swap_guid_fields(&mut bytes);
    Some(bytes.map(|b| b as std::ffi::c_char))
}
