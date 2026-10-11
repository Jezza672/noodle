//! A small VST3 plugin for the tests to load: the CLAP test plugin's twin.
//!
//! - Audio: stereo in, stereo out. The output is the input times `Gain`
//!   (inverted if `Invert` is on), plus a sine for each held note, plus
//!   `bias`, plus `boost` times 0.1.
//! - Notes: two event input buses; a note-on starts a sine at the note's
//!   pitch, an octave higher if it arrives on the second bus.
//! - Parameters: `Gain` (continuous, 0 to 2 plain) and `Invert` (stepped); a
//!   hidden `Boost` driven by the modulation wheel (controller 1) through
//!   `IMidiMapping`.
//! - State: `bias`, a number that is not a parameter, so it can only come back
//!   through saved state.
//!
//! The component and the controller are separate classes, as in most VST3
//! plugins; the controller learns the component's values from its state.
//!
//! - Editor: an `IPlugView` that draws nothing but plays the user when it is
//!   attached: it turns `Gain` down to 0.25 through the component handler,
//!   asks the host for a 300 x 150 window, and sends the component a message
//!   (`set-bias`, bias 0.75). The component answers with `bias-set`, which
//!   makes the controller turn `Invert` on. So the editor, the edits that
//!   reach the processor, the frame and the messages between the two halves
//!   are all observable.
#![allow(non_snake_case, non_upper_case_globals, non_camel_case_types)]
#![allow(clippy::unnecessary_cast)]

use std::cell::{Cell, RefCell, UnsafeCell};
use std::f32::consts::TAU;
use std::ffi::{CStr, CString, c_char, c_void};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface, uid};

const GAIN: u32 = 1;
const INVERT: u32 = 2;
const BOOST: u32 = 3;

const PROCESSOR_ID: TUID = uid(0x4E6F6F64, 0x6C655465, 0x73745031, 0x00000001);
const CONTROLLER_ID: TUID = uid(0x4E6F6F64, 0x6C655465, 0x73745031, 0x00000002);

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c_string = CString::new(src).unwrap_or_default();
    for (src, dst) in c_string.as_bytes_with_nul().iter().zip(dst.iter_mut()) {
        *dst = *src as c_char;
    }
    if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (src, dst) in src.encode_utf16().zip(dst.iter_mut()) {
        *dst = src as TChar;
        len += 1;
    }
    if len < dst.len() {
        dst[len] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    id: i32,
    pitch: i16,
    phase: f32,
    step: f32,
}

struct State {
    gain: f32,
    invert: bool,
    boost: f32,
    bias: f32,
    voices: [Voice; 16],
}

/// No bias is waiting (a bias is never this NaN).
const NO_BIAS: u32 = u32::MAX;

struct Processor {
    state: UnsafeCell<State>,
    /// A bias a message set, for the audio side to pick up.
    pending_bias: AtomicU32,
    host: RefCell<Option<ComPtr<IHostApplication>>>,
    peer: RefCell<Option<ComPtr<IConnectionPoint>>>,
}

impl Class for Processor {
    type Interfaces = (IComponent, IAudioProcessor, IConnectionPoint);
}

/// A new message from the host, with `id` and a float `name` attribute.
fn message_with(
    host: &ComPtr<IHostApplication>,
    id: &CStr,
    attribute: &CStr,
    value: f64,
) -> Option<ComPtr<IMessage>> {
    let mut cid: TUID = IMessage::IID.map(|b| b as c_char);
    let mut iid = cid;
    let mut object: *mut c_void = std::ptr::null_mut();
    if unsafe { host.createInstance(&mut cid, &mut iid, &mut object) } != kResultOk {
        return None;
    }
    let message = unsafe { ComPtr::from_raw(object as *mut IMessage) }?;
    unsafe {
        message.setMessageID(id.as_ptr());
        let attributes = ComRef::from_raw(message.getAttributes())?;
        attributes.setFloat(attribute.as_ptr(), value);
    }
    Some(message)
}

fn message_id(message: &ComRef<'_, IMessage>) -> String {
    unsafe { CStr::from_ptr(message.getMessageID()) }
        .to_string_lossy()
        .into_owned()
}

impl Processor {
    fn new() -> Self {
        Self {
            pending_bias: AtomicU32::new(NO_BIAS),
            host: RefCell::new(None),
            peer: RefCell::new(None),
            state: UnsafeCell::new(State {
                gain: 1.0,
                invert: false,
                boost: 0.0,
                bias: 0.0,
                voices: [Voice {
                    active: false,
                    id: 0,
                    pitch: 0,
                    phase: 0.0,
                    step: 0.0,
                }; 16],
            }),
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn state(&self) -> &mut State {
        // SAFETY: a host calls a plugin from one thread at a time (a message
        // only leaves a bias for the next call to pick up).
        let state = unsafe { &mut *self.state.get() };
        let pending = self.pending_bias.swap(NO_BIAS, Ordering::AcqRel);
        if pending != NO_BIAS {
            state.bias = f32::from_bits(pending);
        }
        state
    }
}

impl IConnectionPointTrait for Processor {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        *self.peer.borrow_mut() = unsafe { ComRef::from_raw(other) }.map(|o| o.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        *self.peer.borrow_mut() = None;
        kResultOk
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        let Some(message) = (unsafe { ComRef::from_raw(message) }) else {
            return kInvalidArgument;
        };
        if message_id(&message) != "set-bias" {
            return kResultFalse;
        }
        let mut bias = 0.0;
        let attributes = unsafe { ComRef::from_raw(message.getAttributes()) };
        if !attributes
            .is_some_and(|a| unsafe { a.getFloat(c"bias".as_ptr(), &mut bias) } == kResultOk)
        {
            return kResultFalse;
        }
        self.pending_bias
            .store((bias as f32).to_bits(), Ordering::Release);
        // Tell the controller it arrived.
        if let (Some(host), Some(peer)) = (&*self.host.borrow(), &*self.peer.borrow())
            && let Some(reply) = message_with(host, c"bias-set", c"bias", bias)
        {
            unsafe { peer.notify(reply.as_ptr()) };
        }
        kResultOk
    }
}

impl IPluginBaseTrait for Processor {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        *self.host.borrow_mut() = unsafe { ComRef::from_raw(context) }.and_then(|c| c.cast());
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        *self.host.borrow_mut() = None;
        *self.peer.borrow_mut() = None;
        kResultOk
    }
}

impl IComponentTrait for Processor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = CONTROLLER_ID };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media: MediaType, dir: BusDirection) -> i32 {
        match (media as MediaTypes, dir as BusDirections) {
            (MediaTypes_::kAudio, _) => 1,
            (MediaTypes_::kEvent, BusDirections_::kInput) => 2,
            _ => 0,
        }
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        let bus = unsafe { &mut *bus };
        bus.mediaType = media;
        bus.direction = dir;
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        match (media as MediaTypes, dir as BusDirections, index) {
            (MediaTypes_::kAudio, BusDirections_::kInput, 0) => {
                bus.channelCount = 2;
                copy_wstring("Input", &mut bus.name);
            }
            (MediaTypes_::kAudio, BusDirections_::kOutput, 0) => {
                bus.channelCount = 2;
                copy_wstring("Output", &mut bus.name);
            }
            (MediaTypes_::kEvent, BusDirections_::kInput, 0 | 1) => {
                bus.channelCount = 16;
                copy_wstring("Notes", &mut bus.name);
            }
            _ => return kInvalidArgument,
        }
        kResultOk
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, _: MediaType, _: BusDirection, _: i32, _: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        let mut bytes = [0u8; 12];
        let mut read = 0;
        let stream = unsafe { ComRef::from_raw(stream) }.expect("a stream");
        let result = unsafe { stream.read(bytes.as_mut_ptr() as *mut c_void, 12, &mut read) };
        if result != kResultOk || read != 12 {
            return kResultFalse;
        }
        let f = |i: usize| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        let state = self.state();
        state.gain = f(0);
        state.invert = f(1) >= 0.5;
        state.bias = f(2);
        kResultOk
    }

    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        let state = self.state();
        let mut bytes = Vec::new();
        for value in [state.gain, f32::from(state.invert), state.bias] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let stream = unsafe { ComRef::from_raw(stream) }.expect("a stream");
        let mut written = 0;
        unsafe { stream.write(bytes.as_mut_ptr() as *mut c_void, 12, &mut written) }
    }
}

impl IAudioProcessorTrait for Processor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        let stereo = SpeakerArr::kStereo;
        let ok = num_ins == 1
            && num_outs == 1
            && unsafe { *inputs } == stereo
            && unsafe { *outputs } == stereo;
        if ok { kResultTrue } else { kResultFalse }
    }

    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }

    /// The latency in `NOODLE_TEST_PLUGIN_LATENCY` (samples), so the latency
    /// tests can ask for any; the audio isn't actually delayed.
    unsafe fn getLatencySamples(&self) -> u32 {
        std::env::var("NOODLE_TEST_PLUGIN_LATENCY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        RATE.set(unsafe { (*setup).sampleRate } as f32);
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = unsafe { &*data };
        let state = self.state();
        let frames = data.numSamples as usize;
        let rate = RATE.get();

        // Parameter points, as (offset, id, normalized value), in time order.
        let mut points: Vec<(i32, u32, f64)> = Vec::new();
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for q in 0..unsafe { changes.getParameterCount() } {
                let Some(queue) = (unsafe { ComRef::from_raw(changes.getParameterData(q)) }) else {
                    continue;
                };
                let id = unsafe { queue.getParameterId() };
                for p in 0..unsafe { queue.getPointCount() } {
                    let (mut offset, mut value) = (0, 0.0);
                    if unsafe { queue.getPoint(p, &mut offset, &mut value) } == kResultOk {
                        points.push((offset, id, value));
                    }
                }
            }
        }
        points.sort_by_key(|p| p.0);

        // Note events likewise.
        let mut events: Vec<Event> = Vec::new();
        if let Some(list) = unsafe { ComRef::from_raw(data.inputEvents) } {
            for e in 0..unsafe { list.getEventCount() } {
                let mut event: Event = unsafe { std::mem::zeroed() };
                if unsafe { list.getEvent(e, &mut event) } == kResultOk {
                    events.push(event);
                }
            }
        }
        events.sort_by_key(|e| e.sampleOffset);

        let input = if data.numInputs > 0 {
            let bus = unsafe { &*data.inputs };
            Some(unsafe {
                std::slice::from_raw_parts(bus.__field0.channelBuffers32, bus.numChannels as usize)
            })
        } else {
            None
        };
        let output = unsafe {
            let bus = &*data.outputs;
            std::slice::from_raw_parts(bus.__field0.channelBuffers32, bus.numChannels as usize)
        };

        let (mut next_point, mut next_event) = (0, 0);
        for i in 0..frames {
            while next_point < points.len() && points[next_point].0 as usize <= i {
                let (_, id, value) = points[next_point];
                match id {
                    GAIN => state.gain = value as f32 * 2.0,
                    INVERT => state.invert = value >= 0.5,
                    BOOST => state.boost = value as f32,
                    _ => {}
                }
                next_point += 1;
            }
            while next_event < events.len() && events[next_event].sampleOffset as usize <= i {
                let event = events[next_event];
                next_event += 1;
                let kind = u32::from(event.r#type);
                match kind {
                    x if x == Event_::EventTypes_::kNoteOnEvent as u32 => {
                        let on = unsafe { event.__field0.noteOn };
                        let octave = if event.busIndex == 1 { 2.0 } else { 1.0 };
                        let hz = 440.0 * 2f32.powf((f32::from(on.pitch) - 69.0) / 12.0) * octave;
                        if let Some(v) = state.voices.iter_mut().find(|v| !v.active) {
                            *v = Voice {
                                active: true,
                                id: on.noteId,
                                pitch: on.pitch,
                                phase: 0.0,
                                step: TAU * hz / rate,
                            };
                        }
                    }
                    x if x == Event_::EventTypes_::kNoteOffEvent as u32 => {
                        let off = unsafe { event.__field0.noteOff };
                        for v in state.voices.iter_mut().filter(|v| v.active) {
                            if v.id == off.noteId || (off.noteId < 0 && v.pitch == off.pitch) {
                                v.active = false;
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut sine = 0.0;
            for v in state.voices.iter_mut().filter(|v| v.active) {
                sine += v.phase.sin() * 0.2;
                v.phase = (v.phase + v.step) % TAU;
            }
            let sign = if state.invert { -1.0 } else { 1.0 };
            for (c, channel) in output.iter().enumerate() {
                let dry = input.map_or(0.0, |input| unsafe { *input[c].add(i) });
                unsafe {
                    *channel.add(i) =
                        dry * state.gain * sign + sine + state.bias + state.boost * 0.1;
                }
            }
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

thread_local! {
    static RATE: Cell<f32> = const { Cell::new(48_000.0) };
}

struct Shared {
    gain: Cell<f64>,
    invert: Cell<f64>,
    boost: Cell<f64>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
    host: RefCell<Option<ComPtr<IHostApplication>>>,
    peer: RefCell<Option<ComPtr<IConnectionPoint>>>,
}

struct Controller {
    shared: Rc<Shared>,
}

impl Class for Controller {
    type Interfaces = (IEditController, IMidiMapping, IConnectionPoint);
}

impl IConnectionPointTrait for Controller {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        *self.shared.peer.borrow_mut() = unsafe { ComRef::from_raw(other) }.map(|o| o.to_com_ptr());
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        *self.shared.peer.borrow_mut() = None;
        kResultOk
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        let Some(message) = (unsafe { ComRef::from_raw(message) }) else {
            return kInvalidArgument;
        };
        if message_id(&message) == "bias-set" {
            self.shared.invert.set(1.0);
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        *self.shared.host.borrow_mut() =
            unsafe { ComRef::from_raw(context) }.and_then(|c| c.cast());
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        *self.shared.host.borrow_mut() = None;
        *self.shared.handler.borrow_mut() = None;
        *self.shared.peer.borrow_mut() = None;
        kResultOk
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, stream: *mut IBStream) -> tresult {
        let mut bytes = [0u8; 12];
        let mut read = 0;
        let stream = unsafe { ComRef::from_raw(stream) }.expect("a stream");
        if unsafe { stream.read(bytes.as_mut_ptr() as *mut c_void, 12, &mut read) } != kResultOk
            || read != 12
        {
            return kResultFalse;
        }
        let f = |i: usize| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        self.shared.gain.set(f64::from(f(0)) / 2.0);
        self.shared.invert.set(f64::from(f(1)));
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        3
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let info = unsafe { &mut *info };
        let (id, title, steps, default, flags) = match index {
            0 => (
                GAIN,
                "Gain",
                0,
                0.5,
                ParameterInfo_::ParameterFlags_::kCanAutomate,
            ),
            1 => (
                INVERT,
                "Invert",
                1,
                0.0,
                ParameterInfo_::ParameterFlags_::kCanAutomate,
            ),
            2 => (
                BOOST,
                "Boost",
                0,
                0.0,
                ParameterInfo_::ParameterFlags_::kIsHidden,
            ),
            _ => return kInvalidArgument,
        };
        info.id = id;
        copy_wstring(title, &mut info.title);
        copy_wstring(title, &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = steps;
        info.defaultNormalizedValue = default;
        info.unitId = 0;
        info.flags = flags;
        kResultOk
    }

    unsafe fn getParamStringByValue(&self, _: u32, value: f64, string: *mut String128) -> tresult {
        copy_wstring(&value.to_string(), unsafe { &mut *string });
        kResultOk
    }

    unsafe fn getParamValueByString(&self, _: u32, _: *mut TChar, _: *mut f64) -> tresult {
        kNotImplemented
    }

    unsafe fn normalizedParamToPlain(&self, id: u32, value: f64) -> f64 {
        if id == GAIN { value * 2.0 } else { value }
    }

    unsafe fn plainParamToNormalized(&self, id: u32, plain: f64) -> f64 {
        if id == GAIN { plain / 2.0 } else { plain }
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        match id {
            GAIN => self.shared.gain.get(),
            INVERT => self.shared.invert.get(),
            BOOST => self.shared.boost.get(),
            _ => 0.0,
        }
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        match id {
            GAIN => self.shared.gain.set(value),
            INVERT => self.shared.invert.set(value),
            BOOST => self.shared.boost.set(value),
            _ => return kInvalidArgument,
        }
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.shared.handler.borrow_mut() =
            unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, name: *const c_char) -> *mut IPlugView {
        if name.is_null() || unsafe { CStr::from_ptr(name) } != c"editor" {
            return std::ptr::null_mut();
        }
        ComWrapper::new(View {
            shared: Rc::clone(&self.shared),
            frame: RefCell::new(None),
            size: Cell::new((200, 100)),
        })
        .to_com_ptr::<IPlugView>()
        .map_or(std::ptr::null_mut(), |view| view.into_raw())
    }
}

impl IMidiMappingTrait for Controller {
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: i32,
        _channel: i16,
        controller: i16,
        id: *mut ParamID,
    ) -> tresult {
        if bus == 0 && controller == 1 {
            unsafe { *id = BOOST };
            kResultOk
        } else {
            kResultFalse
        }
    }
}

/// The editor. It draws nothing; attaching it plays the user (see the top).
struct View {
    shared: Rc<Shared>,
    frame: RefCell<Option<ComPtr<IPlugFrame>>>,
    size: Cell<(i32, i32)>,
}

impl Class for View {
    type Interfaces = (IPlugView,);
}

impl IPlugViewTrait for View {
    unsafe fn isPlatformTypeSupported(&self, _type: FIDString) -> tresult {
        kResultTrue
    }

    unsafe fn attached(&self, parent: *mut c_void, _type: FIDString) -> tresult {
        if parent.is_null() {
            return kInvalidArgument;
        }
        let shared = &self.shared;
        // The user drags Gain to a quarter.
        if let Some(handler) = &*shared.handler.borrow() {
            unsafe {
                handler.beginEdit(GAIN);
                handler.performEdit(GAIN, 0.25);
                handler.endEdit(GAIN);
            }
        }
        shared.gain.set(0.25);
        // The editor wants a bigger window.
        if let Some(frame) = &*self.frame.borrow() {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: 300,
                bottom: 150,
            };
            unsafe { frame.resizeView(std::ptr::null_mut(), &mut rect) };
        }
        // And tells the processor something only a message carries.
        if let (Some(host), Some(peer)) = (&*shared.host.borrow(), &*shared.peer.borrow())
            && let Some(message) = message_with(host, c"set-bias", c"bias", 0.75)
        {
            unsafe { peer.notify(message.as_ptr()) };
        }
        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        let (width, height) = self.size.get();
        unsafe {
            *size = ViewRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
        }
        kResultOk
    }

    unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
        let rect = unsafe { &*new_size };
        self.size
            .set((rect.right - rect.left, rect.bottom - rect.top));
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        *self.frame.borrow_mut() = unsafe { ComRef::from_raw(frame) }.map(|f| f.to_com_ptr());
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultTrue
    }

    /// Sizes snap to multiples of ten.
    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        let rect = unsafe { &mut *rect };
        rect.right = rect.left + (rect.right - rect.left) / 10 * 10;
        rect.bottom = rect.top + (rect.bottom - rect.top) / 10 * 10;
        kResultOk
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy_cstring("Noodle", &mut info.vendor);
        copy_cstring("https://github.com/Jezza672/noodle", &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        2
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let info = unsafe { &mut *info };
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring("Noodle Test VST3", &mut info.name);
        match index {
            0 => {
                info.cid = PROCESSOR_ID;
                copy_cstring("Audio Module Class", &mut info.category);
            }
            1 => {
                info.cid = CONTROLLER_ID;
                copy_cstring("Component Controller Class", &mut info.category);
            }
            _ => return kInvalidArgument,
        }
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let instance = match unsafe { *(cid as *const TUID) } {
            PROCESSOR_ID => ComWrapper::new(Processor::new()).to_com_ptr::<FUnknown>(),
            CONTROLLER_ID => ComWrapper::new(Controller {
                shared: Rc::new(Shared {
                    gain: Cell::new(0.5),
                    invert: Cell::new(0.0),
                    boost: Cell::new(0.0),
                    handler: RefCell::new(None),
                    host: RefCell::new(None),
                    peer: RefCell::new(None),
                }),
            })
            .to_com_ptr::<FUnknown>(),
            _ => None,
        };
        match instance {
            Some(instance) => {
                let ptr = instance.as_ptr();
                unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid as *mut TUID, obj) }
            }
            None => kInvalidArgument,
        }
    }
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn InitDll() -> bool {
    true
}

/// Writes the file named by `NOODLE_TEST_DEINIT_MARKER` when the host exits
/// the module, so a test can see whether the host unloaded it.
fn record_exit() {
    if let Some(path) = std::env::var_os("NOODLE_TEST_DEINIT_MARKER") {
        let _ = std::fs::write(path, b"exit");
    }
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn ExitDll() -> bool {
    record_exit();
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleExit() -> bool {
    record_exit();
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
    record_exit();
    true
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .expect("the factory has this interface")
        .into_raw()
}
