//! An Audio Unit instance: finding components, making and setting one up,
//! and reading its buses, parameters, state and latency.

use std::ffi::{c_char, c_void};
use std::mem::{MaybeUninit, size_of};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use objc2_audio_toolbox::{
    AURenderCallbackStruct, AudioComponent, AudioComponentCopyName, AudioComponentDescription,
    AudioComponentFindNext, AudioComponentGetDescription, AudioComponentGetVersion,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioUnit, AudioUnitGetProperty,
    AudioUnitGetPropertyInfo, AudioUnitInitialize, AudioUnitParameterInfo,
    AudioUnitParameterOptions, AudioUnitParameterUnit, AudioUnitReset, AudioUnitSetProperty,
    AudioUnitUninitialize, HostCallbackInfo, kAudioUnitProperty_ClassInfo,
    kAudioUnitProperty_ElementCount, kAudioUnitProperty_HostCallbacks, kAudioUnitProperty_Latency,
    kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitProperty_ParameterInfo,
    kAudioUnitProperty_ParameterList, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global, kAudioUnitScope_Input,
    kAudioUnitScope_Output,
};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, kAudioFormatFlagIsNonInterleaved,
    kAudioFormatFlagsNativeFloatPacked, kAudioFormatLinearPCM,
};
use objc2_core_foundation::{CFData, CFPropertyList, CFPropertyListFormat, CFRetained, CFString};

use crate::descriptor::{AudioPort, Descriptor, ParamDesc, PluginFormat};
use crate::host::{HostError, LOAD_TIMEOUT};
use crate::thread;

use super::{ComponentId, HOSTED_TYPES, code_text, component_of_path, component_path, split_name};

/// `noErr`.
const OK: i32 = 0;

fn check(status: i32, what: &str) -> Result<(), HostError> {
    if status == OK {
        Ok(())
    } else {
        Err(HostError(format!("{what} failed (OSStatus {status})")))
    }
}

/// Every installed component of a type a node can host, with its
/// description.
fn installed() -> Vec<(AudioComponent, ComponentId)> {
    let mut found = Vec::new();
    for kind in HOSTED_TYPES {
        let mut wanted = AudioComponentDescription {
            componentType: kind,
            componentSubType: 0,
            componentManufacturer: 0,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        let mut component: AudioComponent = ptr::null_mut();
        loop {
            // SAFETY: `wanted` is a valid description, and `component` is
            // null or the last one returned.
            component = unsafe { AudioComponentFindNext(component, NonNull::from(&mut wanted)) };
            if component.is_null() {
                break;
            }
            let mut description = MaybeUninit::<AudioComponentDescription>::uninit();
            // SAFETY: a component FindNext just returned.
            let status = unsafe {
                AudioComponentGetDescription(
                    component,
                    NonNull::new_unchecked(description.as_mut_ptr()),
                )
            };
            if status != OK {
                continue;
            }
            // SAFETY: filled by the call that succeeded.
            let description = unsafe { description.assume_init() };
            found.push((
                component,
                ComponentId {
                    kind: description.componentType,
                    subtype: description.componentSubType,
                    manufacturer: description.componentManufacturer,
                },
            ));
        }
    }
    found
}

/// The virtual files for every installed AU, in a stable order.
pub(crate) fn component_files() -> Vec<PathBuf> {
    let mut files: Vec<_> = installed()
        .into_iter()
        .map(|(_, id)| component_path(id))
        .collect();
    files.sort();
    files.dedup();
    files
}

fn find(id: ComponentId) -> Result<AudioComponent, HostError> {
    let mut wanted = AudioComponentDescription {
        componentType: id.kind,
        componentSubType: id.subtype,
        componentManufacturer: id.manufacturer,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    // SAFETY: a valid description; null starts the search.
    let component = unsafe { AudioComponentFindNext(ptr::null_mut(), NonNull::from(&mut wanted)) };
    let missing = || HostError(format!("no Audio Unit {} is installed", id.to_text()));
    if component.is_null() {
        return Err(missing());
    }
    // A zero field is a wildcard to AudioToolbox, so the first match may be
    // some other unit: only the exact one will do.
    let mut found = MaybeUninit::<AudioComponentDescription>::uninit();
    // SAFETY: a component FindNext just returned, and a valid out pointer.
    let status = unsafe {
        AudioComponentGetDescription(component, NonNull::new_unchecked(found.as_mut_ptr()))
    };
    // SAFETY: filled by the call that succeeded.
    let found = unsafe { found.assume_init() };
    if status == OK
        && (
            found.componentType,
            found.componentSubType,
            found.componentManufacturer,
        ) == (id.kind, id.subtype, id.manufacturer)
    {
        Ok(component)
    } else {
        Err(missing())
    }
}

/// An AU's version, as the scan cache's modification time: a plugin that
/// updates has a new one. (There is no file to stamp.)
pub(crate) fn stamp(path: &Path) -> Option<(u64, u64)> {
    let component = find(component_of_path(path)?).ok()?;
    let mut version = 0u32;
    // SAFETY: a live component and a valid out pointer.
    let status = unsafe { AudioComponentGetVersion(component, NonNull::from(&mut version)) };
    (status == OK).then_some((0, u64::from(version)))
}

/// What the transport looks like, for the unit's host callbacks. The node
/// writes it before each render and the callbacks (on the same thread) read it.
#[derive(Default)]
pub(crate) struct Transport {
    pub playing: AtomicBool,
    pub tempo: AtomicU64,
    pub beat: AtomicU64,
    pub sample: AtomicU64,
    pub numerator: AtomicU32,
    pub denominator: AtomicU32,
    pub bar_beat: AtomicU64,
}

impl Transport {
    pub fn set(
        &self,
        playing: bool,
        tempo: f64,
        beat: f64,
        sample: f64,
        signature: (u32, u32),
        bar_beat: f64,
    ) {
        self.playing.store(playing, Ordering::Relaxed);
        self.tempo.store(tempo.to_bits(), Ordering::Relaxed);
        self.beat.store(beat.to_bits(), Ordering::Relaxed);
        self.sample.store(sample.to_bits(), Ordering::Relaxed);
        self.numerator.store(signature.0.max(1), Ordering::Relaxed);
        self.denominator
            .store(signature.1.max(1), Ordering::Relaxed);
        self.bar_beat.store(bar_beat.to_bits(), Ordering::Relaxed);
    }

    fn f64(slot: &AtomicU64) -> f64 {
        f64::from_bits(slot.load(Ordering::Relaxed))
    }
}

/// The transport behind a callback's user data.
///
/// # Safety
/// `data` is the pointer [`Unit::new`] gave the unit: an `Arc<Transport>`'s
/// contents, alive as long as the unit.
unsafe fn transport<'a>(data: *mut c_void) -> &'a Transport {
    unsafe { &*(data as *const Transport) }
}

unsafe extern "C-unwind" fn beat_and_tempo(
    data: *mut c_void,
    beat: *mut f64,
    tempo: *mut f64,
) -> i32 {
    // SAFETY: see `transport`; AudioToolbox passes null for what it doesn't want.
    unsafe {
        let t = transport(data);
        if !beat.is_null() {
            *beat = Transport::f64(&t.beat);
        }
        if !tempo.is_null() {
            *tempo = Transport::f64(&t.tempo);
        }
    }
    OK
}

unsafe extern "C-unwind" fn musical_time_location(
    data: *mut c_void,
    to_next_beat: *mut u32,
    numerator: *mut f32,
    denominator: *mut u32,
    bar_beat: *mut f64,
) -> i32 {
    // SAFETY: as above.
    unsafe {
        let t = transport(data);
        if !to_next_beat.is_null() {
            *to_next_beat = 0;
        }
        if !numerator.is_null() {
            *numerator = t.numerator.load(Ordering::Relaxed) as f32;
        }
        if !denominator.is_null() {
            *denominator = t.denominator.load(Ordering::Relaxed);
        }
        if !bar_beat.is_null() {
            *bar_beat = Transport::f64(&t.bar_beat);
        }
    }
    OK
}

unsafe extern "C-unwind" fn transport_state(
    data: *mut c_void,
    playing: *mut u8,
    changed: *mut u8,
    sample: *mut f64,
    cycling: *mut u8,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> i32 {
    // SAFETY: as above.
    unsafe {
        let t = transport(data);
        if !playing.is_null() {
            *playing = u8::from(t.playing.load(Ordering::Relaxed));
        }
        if !changed.is_null() {
            *changed = 0;
        }
        if !sample.is_null() {
            *sample = Transport::f64(&t.sample);
        }
        if !cycling.is_null() {
            *cycling = 0;
        }
        if !cycle_start.is_null() {
            *cycle_start = 0.0;
        }
        if !cycle_end.is_null() {
            *cycle_end = 0.0;
        }
    }
    OK
}

/// An AU instance, not yet or already initialised.
pub(crate) struct Unit {
    instance: AudioUnit,
    pub id: ComponentId,
    initialized: bool,
    /// Kept alive for the host callbacks, which point into it.
    transport: Arc<Transport>,
}

// SAFETY: an AU instance may be called from any one thread at a time, which
// is how a node uses it (made and set up on the plugin thread, then used on
// the audio thread, one at a time).
unsafe impl Send for Unit {}

/// One bus of a unit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bus {
    pub channels: u32,
}

impl Unit {
    /// Makes an instance of the component `id`.
    pub fn new(id: ComponentId) -> Result<Self, HostError> {
        let component = find(id)?;
        let mut instance: AudioUnit = ptr::null_mut();
        // SAFETY: a live component and a valid out pointer.
        let status = unsafe { AudioComponentInstanceNew(component, NonNull::from(&mut instance)) };
        check(status, "making the Audio Unit")?;
        if instance.is_null() {
            return Err(HostError("the Audio Unit wasn't made".into()));
        }
        Ok(Self {
            instance,
            id,
            initialized: false,
            transport: Arc::default(),
        })
    }

    pub fn raw(&self) -> AudioUnit {
        self.instance
    }

    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }

    fn get<T>(&self, property: u32, scope: u32, element: u32) -> Result<T, HostError> {
        let mut value = MaybeUninit::<T>::uninit();
        let mut size = size_of::<T>() as u32;
        // SAFETY: the buffer is `size` bytes; the unit fills at most that.
        let status = unsafe {
            AudioUnitGetProperty(
                self.instance,
                property,
                scope,
                element,
                NonNull::new_unchecked(value.as_mut_ptr().cast()),
                NonNull::from(&mut size),
            )
        };
        check(status, &format!("reading property {property}"))?;
        // SAFETY: the unit wrote the value (a plain-data struct or number).
        Ok(unsafe { value.assume_init() })
    }

    fn get_list<T: Copy>(&self, property: u32, scope: u32) -> Result<Vec<T>, HostError> {
        let mut size = 0u32;
        let mut writable = 0u8;
        // SAFETY: valid out pointers.
        let status = unsafe {
            AudioUnitGetPropertyInfo(self.instance, property, scope, 0, &mut size, &mut writable)
        };
        check(status, &format!("sizing property {property}"))?;
        let count = size as usize / size_of::<T>();
        let mut list: Vec<MaybeUninit<T>> = Vec::with_capacity(count);
        let mut size = (count * size_of::<T>()) as u32;
        if count == 0 {
            return Ok(Vec::new());
        }
        // SAFETY: the list has room for `size` bytes.
        let status = unsafe {
            AudioUnitGetProperty(
                self.instance,
                property,
                scope,
                0,
                NonNull::new_unchecked(list.as_mut_ptr().cast()),
                NonNull::from(&mut size),
            )
        };
        check(status, &format!("reading property {property}"))?;
        let filled = (size as usize / size_of::<T>()).min(count);
        // SAFETY: the unit wrote `filled` elements.
        unsafe { list.set_len(filled) };
        Ok(list
            .into_iter()
            // SAFETY: each was written.
            .map(|v| unsafe { v.assume_init() })
            .collect())
    }

    fn set<T>(&self, property: u32, scope: u32, element: u32, value: &T) -> Result<(), HostError> {
        // SAFETY: a plain value of the size given.
        let status = unsafe {
            AudioUnitSetProperty(
                self.instance,
                property,
                scope,
                element,
                (value as *const T).cast(),
                size_of::<T>() as u32,
            )
        };
        check(status, &format!("setting property {property}"))
    }

    /// How many buses the unit has in a scope, and the channels each has.
    fn buses(&self, scope: u32) -> Vec<Bus> {
        let count: u32 = self
            .get(kAudioUnitProperty_ElementCount, scope, 0)
            .unwrap_or(0);
        (0..count)
            .map(|element| {
                let format: Result<AudioStreamBasicDescription, _> =
                    self.get(kAudioUnitProperty_StreamFormat, scope, element);
                Bus {
                    channels: format.map_or(2, |f| f.mChannelsPerFrame.max(1)),
                }
            })
            .collect()
    }

    pub fn input_buses(&self) -> Vec<Bus> {
        self.buses(kAudioUnitScope_Input)
    }

    pub fn output_buses(&self) -> Vec<Bus> {
        self.buses(kAudioUnitScope_Output)
    }

    /// Sets every bus to 32-bit float, one buffer per channel, at
    /// `sample_rate`, keeping each bus's channel count.
    fn set_formats(&self, sample_rate: f64) -> Result<(), HostError> {
        for (scope, buses) in [
            (kAudioUnitScope_Input, self.input_buses()),
            (kAudioUnitScope_Output, self.output_buses()),
        ] {
            for (element, bus) in buses.iter().enumerate() {
                let format = AudioStreamBasicDescription {
                    mSampleRate: sample_rate,
                    mFormatID: kAudioFormatLinearPCM,
                    mFormatFlags: kAudioFormatFlagsNativeFloatPacked
                        | kAudioFormatFlagIsNonInterleaved,
                    mBytesPerPacket: 4,
                    mFramesPerPacket: 1,
                    mBytesPerFrame: 4,
                    mChannelsPerFrame: bus.channels,
                    mBitsPerChannel: 32,
                    mReserved: 0,
                };
                self.set(
                    kAudioUnitProperty_StreamFormat,
                    scope,
                    element as u32,
                    &format,
                )?;
            }
        }
        Ok(())
    }

    /// Hands the unit a render callback for each input bus: it pulls from
    /// `pull`, which must outlive the unit's use.
    pub fn set_input_callback(
        &self,
        bus: u32,
        callback: objc2_audio_toolbox::AURenderCallback,
        data: *mut c_void,
    ) -> Result<(), HostError> {
        self.set(
            kAudioUnitProperty_SetRenderCallback,
            kAudioUnitScope_Input,
            bus,
            &AURenderCallbackStruct {
                inputProc: callback,
                inputProcRefCon: data,
            },
        )
    }

    /// Sets the unit up to process blocks of up to `max_frames` at
    /// `sample_rate`. Input callbacks are set by the caller before this.
    pub fn initialize(&mut self, sample_rate: f64, max_frames: usize) -> Result<(), HostError> {
        if self.initialized {
            return Ok(());
        }
        self.set_formats(sample_rate)?;
        self.set(
            kAudioUnitProperty_MaximumFramesPerSlice,
            kAudioUnitScope_Global,
            0,
            &(max_frames as u32),
        )?;
        let callbacks = HostCallbackInfo {
            hostUserData: Arc::as_ptr(&self.transport) as *mut c_void,
            beatAndTempoProc: Some(beat_and_tempo),
            musicalTimeLocationProc: Some(musical_time_location),
            transportStateProc: Some(transport_state),
            transportStateProc2: None,
        };
        // A unit that doesn't ask for the transport can refuse; it just
        // won't know the tempo.
        let _ = self.set(
            kAudioUnitProperty_HostCallbacks,
            kAudioUnitScope_Global,
            0,
            &callbacks,
        );
        // SAFETY: a live, not yet initialised unit.
        check(
            unsafe { AudioUnitInitialize(self.instance) },
            "initialising the Audio Unit",
        )?;
        self.initialized = true;
        Ok(())
    }

    /// The latency the unit reports, in samples.
    pub fn latency(&self, sample_rate: f64) -> u32 {
        let seconds: f64 = self
            .get(kAudioUnitProperty_Latency, kAudioUnitScope_Global, 0)
            .unwrap_or(0.0);
        if seconds.is_finite() && seconds > 0.0 {
            (seconds * sample_rate).round() as u32
        } else {
            0
        }
    }

    pub fn reset(&self) {
        // SAFETY: a live unit.
        unsafe { AudioUnitReset(self.instance, kAudioUnitScope_Global, 0) };
    }

    /// The parameters a user can set, in the unit's order.
    pub fn params(&self) -> Vec<ParamDesc> {
        let ids: Vec<u32> = self
            .get_list(kAudioUnitProperty_ParameterList, kAudioUnitScope_Global)
            .unwrap_or_default();
        ids.into_iter().filter_map(|id| self.param(id)).collect()
    }

    fn param(&self, id: u32) -> Option<ParamDesc> {
        let info: AudioUnitParameterInfo = self
            .get(kAudioUnitProperty_ParameterInfo, kAudioUnitScope_Global, id)
            .ok()?;
        let flags = info.flags;
        // The strings the unit handed over are ours to release when it says so.
        let release = flags.contains(AudioUnitParameterOptions::Flag_CFNameRelease);
        let own = |string: *const CFString| {
            NonNull::new(string.cast_mut()).map(|string| {
                // SAFETY: a CFString the unit gave us a reference to when it
                // set the release flag; otherwise we retain it.
                if release {
                    unsafe { CFRetained::from_raw(string) }
                } else {
                    unsafe { CFRetained::retain(string) }
                }
            })
        };
        let long_name = flags
            .contains(AudioUnitParameterOptions::Flag_HasCFNameString)
            .then(|| own(info.cfNameString))
            .flatten();
        // A unit with a custom unit name hands that over the same way.
        if release && info.unit == AudioUnitParameterUnit::CustomUnit {
            drop(own(info.unitName));
        }
        let name = match long_name {
            Some(name) => name.to_string(),
            None => {
                // SAFETY: a NUL-terminated C string within the array.
                let bytes = info.name.map(|c: c_char| c as u8);
                let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                String::from_utf8_lossy(&bytes[..end]).into_owned()
            }
        };
        let hidden = flags.contains(AudioUnitParameterOptions::Flag_MeterReadOnly)
            || flags.contains(AudioUnitParameterOptions::Flag_NonRealTime)
            || (flags.contains(AudioUnitParameterOptions::Flag_IsReadable)
                && !flags.contains(AudioUnitParameterOptions::Flag_IsWritable));
        if hidden {
            return None;
        }
        let stepped = matches!(
            info.unit,
            AudioUnitParameterUnit::Indexed | AudioUnitParameterUnit::Boolean
        );
        Some(ParamDesc {
            id,
            name: if name.is_empty() {
                format!("Parameter {id}")
            } else {
                name
            },
            module: String::new(),
            min: f64::from(info.minValue),
            max: f64::from(info.maxValue),
            default: f64::from(info.defaultValue),
            stepped,
        })
    }

    /// The unit's state, as a binary property list.
    pub fn save_state(&self) -> Result<Vec<u8>, HostError> {
        let list: *const CFPropertyList =
            self.get(kAudioUnitProperty_ClassInfo, kAudioUnitScope_Global, 0)?;
        let list = NonNull::new(list.cast_mut())
            .ok_or_else(|| HostError("the Audio Unit has no state".into()))?;
        // SAFETY: the property hands over a retained property list.
        let list = unsafe { CFRetained::from_raw(list) };
        // SAFETY: a live property list and no error out pointer.
        let data = unsafe {
            objc2_core_foundation::CFPropertyListCreateData(
                None,
                Some(&list),
                CFPropertyListFormat::BinaryFormat_v1_0,
                0,
                ptr::null_mut(),
            )
        }
        .ok_or_else(|| HostError("couldn't serialise the Audio Unit's state".into()))?;
        Ok(data.to_vec())
    }

    /// Restores state saved by [`save_state`](Self::save_state).
    pub fn load_state(&self, state: &[u8]) -> Result<(), HostError> {
        let data = CFData::from_bytes(state);
        let mut format = CFPropertyListFormat::BinaryFormat_v1_0;
        // SAFETY: a live data object and valid out pointers.
        let list = unsafe {
            objc2_core_foundation::CFPropertyListCreateWithData(
                None,
                Some(&data),
                0,
                &mut format,
                ptr::null_mut(),
            )
        }
        .ok_or_else(|| HostError("unreadable Audio Unit state".into()))?;
        let pointer = CFRetained::as_ptr(&list).as_ptr().cast_const();
        self.set(
            kAudioUnitProperty_ClassInfo,
            kAudioUnitScope_Global,
            0,
            &pointer,
        )
    }
}

impl Drop for Unit {
    fn drop(&mut self) {
        // SAFETY: the instance is ours, made by `new`, and not used again.
        unsafe {
            if self.initialized {
                AudioUnitUninitialize(self.instance);
            }
            AudioComponentInstanceDispose(self.instance);
        }
    }
}

/// The name AudioToolbox gives a component.
fn component_name(component: AudioComponent) -> String {
    let mut name: *const CFString = ptr::null();
    // SAFETY: a live component and a valid out pointer.
    let status = unsafe { AudioComponentCopyName(component, NonNull::from(&mut name)) };
    match (status == OK, NonNull::new(name.cast_mut())) {
        // SAFETY: Copy returns a retained string.
        (true, Some(name)) => unsafe { CFRetained::from_raw(name) }.to_string(),
        _ => String::new(),
    }
}

/// Describes the component a virtual file stands for. Instantiates it, so
/// this runs in the scanner's child process.
pub fn describe_file(path: &Path) -> Result<Vec<Descriptor>, HostError> {
    let id = component_of_path(path)
        .ok_or_else(|| HostError(format!("{} isn't an Audio Unit", path.display())))?;
    let component = find(id)?;
    let unit = Unit::new(id)?;
    let ports = |buses: Vec<Bus>, label: &str| -> Vec<AudioPort> {
        buses
            .into_iter()
            .enumerate()
            .map(|(index, bus)| AudioPort {
                name: format!("{label} {}", index + 1),
                channels: bus.channels,
            })
            .collect()
    };
    let full = component_name(component);
    let (vendor, name) = split_name(&full);
    Ok(vec![Descriptor {
        format: PluginFormat::Au,
        id: id.to_text(),
        name: if name.is_empty() {
            format!("{} {}", code_text(id.subtype), code_text(id.manufacturer))
        } else {
            name
        },
        vendor,
        instrument: id.is_instrument(),
        audio_inputs: ports(unit.input_buses(), "Input"),
        audio_outputs: ports(unit.output_buses(), "Output"),
        note_inputs: u32::from(id.takes_notes()),
        note_outputs: 0,
        params: unit.params(),
    }])
}

/// A unit made and set up, with the latency it reported.
pub(crate) struct Loaded<T> {
    /// First, so it drops before `extra`, which its callbacks may point into.
    pub unit: Unit,
    pub latency: u32,
    /// What `inputs` made for the unit's callbacks.
    pub extra: T,
}

/// Makes the plugin `class_id`, restores `state`, and sets it up on the
/// plugin thread. `inputs` is called with the new unit to set its input
/// callbacks before it is initialised.
pub(crate) fn load<T: Send + 'static>(
    class_id: &str,
    state: Option<Vec<u8>>,
    sample_rate: f32,
    max_frames: usize,
    inputs: impl FnOnce(&Unit) -> Result<T, HostError> + Send + 'static,
) -> Result<Loaded<T>, HostError> {
    let class_id = class_id.to_owned();
    thread::run_job(LOAD_TIMEOUT, move || -> Result<Loaded<T>, HostError> {
        let id = ComponentId::from_text(&class_id)
            .ok_or_else(|| HostError(format!("{class_id} isn't an Audio Unit ID")))?;
        let mut unit = Unit::new(id)?;
        // Formats first: state can depend on the layout.
        unit.set_formats(f64::from(sample_rate))?;
        if let Some(state) = state {
            unit.load_state(&state)?;
        }
        let extra = inputs(&unit)?;
        unit.initialize(f64::from(sample_rate), max_frames)?;
        let latency = unit.latency(f64::from(sample_rate));
        Ok(Loaded {
            unit,
            latency,
            extra,
        })
    })?
}

/// Makes the plugin, optionally restores `state`, and returns the state it
/// then saves.
pub(crate) fn capture_state(
    _path: &Path,
    class_id: &str,
    state: Option<&[u8]>,
) -> Result<Vec<u8>, HostError> {
    let (class_id, state) = (class_id.to_owned(), state.map(<[u8]>::to_vec));
    thread::run_job(LOAD_TIMEOUT, move || -> Result<Vec<u8>, HostError> {
        let id = ComponentId::from_text(&class_id)
            .ok_or_else(|| HostError(format!("{class_id} isn't an Audio Unit ID")))?;
        let unit = Unit::new(id)?;
        if let Some(state) = state {
            unit.load_state(&state)?;
        }
        unit.save_state()
    })?
}

/// The memory for an `AudioBufferList` of `buffers` buffers, which the C type
/// declares as one with a flexible array.
pub(crate) struct BufferList {
    words: Vec<u64>,
    buffers: usize,
}

impl BufferList {
    pub fn new(buffers: usize) -> Self {
        // An 8-byte header (count, then padding to the first buffer's pointer
        // alignment) and 16 bytes a buffer.
        Self {
            words: vec![0; 1 + 2 * buffers.max(1)],
            buffers,
        }
    }

    pub fn as_ptr(&mut self) -> NonNull<AudioBufferList> {
        NonNull::new(self.words.as_mut_ptr().cast()).expect("a vector's pointer isn't null")
    }

    /// Points buffer `index` at `data`, `frames` samples long.
    pub fn set(&mut self, index: usize, data: *mut f32, frames: usize) {
        debug_assert!(index < self.buffers);
        // SAFETY: the list has room for `buffers` buffers, and `index` is one.
        unsafe {
            let list = self.as_ptr().as_ptr();
            (*list).mNumberBuffers = self.buffers as u32;
            let buffer = (ptr::addr_of_mut!((*list).mBuffers)
                as *mut objc2_core_audio_types::AudioBuffer)
                .add(index);
            (*buffer).mNumberChannels = 1;
            (*buffer).mDataByteSize = (frames * size_of::<f32>()) as u32;
            (*buffer).mData = data.cast();
        }
    }

    /// Where buffer `index` points now (a unit may point it at its own
    /// memory).
    pub fn data(&mut self, index: usize) -> *const f32 {
        // SAFETY: as in `set`.
        unsafe {
            let list = self.as_ptr().as_ptr();
            let buffer = (ptr::addr_of_mut!((*list).mBuffers)
                as *mut objc2_core_audio_types::AudioBuffer)
                .add(index);
            (*buffer).mData.cast()
        }
    }
}
