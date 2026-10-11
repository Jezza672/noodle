//! The COM objects a VST3 host hands to a plugin: the host application, the
//! component handler, memory streams for state, and the parameter and event
//! lists that carry a block's changes.
//!
//! The lists are used from one thread at a time (the audio thread, around one
//! `process` call), so they keep their data in `UnsafeCell`s and never
//! allocate after they are made.

use std::cell::{Cell, UnsafeCell};
use std::ffi::c_void;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper};

/// Copies `text` into a plugin's UTF-16 buffer, cut to fit.
pub(crate) fn copy_wstring(text: &str, out: &mut [TChar]) {
    let mut len = 0;
    for (unit, slot) in text.encode_utf16().zip(out.iter_mut()) {
        *slot = unit as TChar;
        len += 1;
    }
    if len < out.len() {
        out[len] = 0;
    } else if let Some(last) = out.last_mut() {
        *last = 0;
    }
}

/// A plugin's UTF-16 string, up to its terminator.
pub(crate) fn wstring(text: &[TChar]) -> String {
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..end])
}

/// A plugin's C string field, up to its terminator.
pub(crate) fn cstring(text: &[std::ffi::c_char]) -> String {
    let bytes: Vec<u8> = text
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `IHostApplication`: the host's name. Plugins ask it for messages and
/// attribute lists too; it has none, which plugins that use them for more
/// than an editor tolerate.
pub(crate) struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        copy_wstring("Noodle", unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        _cid: *mut TUID,
        _iid: *mut TUID,
        _obj: *mut *mut c_void,
    ) -> tresult {
        kNotImplemented
    }
}

/// `IComponentHandler`: the controller's way of telling the host about edits.
/// Noodle has no VST3 editor yet, so a controller has nothing to report that
/// matters; the plugin is told its calls worked.
pub(crate) struct ComponentHandler;

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn performEdit(&self, _id: ParamID, _value: ParamValue) -> tresult {
        kResultOk
    }
    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        kResultOk
    }
}

/// `IBStream` over memory: how plugins read and write their state.
pub(crate) struct Stream {
    data: UnsafeCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl Class for Stream {
    type Interfaces = (IBStream,);
}

impl Stream {
    pub fn new(bytes: Vec<u8>) -> ComWrapper<Stream> {
        ComWrapper::new(Stream {
            data: UnsafeCell::new(bytes),
            pos: Cell::new(0),
        })
    }

    /// Everything written so far.
    pub fn bytes(&self) -> Vec<u8> {
        // SAFETY: streams are used from one thread, and never re-entrantly.
        unsafe { (*self.data.get()).clone() }
    }

    pub fn rewind(&self) {
        self.pos.set(0);
    }
}

impl IBStreamTrait for Stream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: int32, num_read: *mut int32) -> tresult {
        let data = unsafe { &*self.data.get() };
        let pos = self.pos.get().min(data.len());
        let n = (num_bytes.max(0) as usize).min(data.len() - pos);
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().add(pos), buffer as *mut u8, n) };
        self.pos.set(pos + n);
        if !num_read.is_null() {
            unsafe { *num_read = n as int32 };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_written: *mut int32,
    ) -> tresult {
        let data = unsafe { &mut *self.data.get() };
        let n = num_bytes.max(0) as usize;
        let pos = self.pos.get();
        if data.len() < pos + n {
            data.resize(pos + n, 0);
        }
        let bytes = unsafe { std::slice::from_raw_parts(buffer as *const u8, n) };
        data[pos..pos + n].copy_from_slice(bytes);
        self.pos.set(pos + n);
        if !num_written.is_null() {
            unsafe { *num_written = n as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = unsafe { (*self.data.get()).len() } as i64;
        let target = match mode as IBStream_::IStreamSeekMode {
            IBStream_::IStreamSeekMode_::kIBSeekSet => pos,
            IBStream_::IStreamSeekMode_::kIBSeekCur => self.pos.get() as i64 + pos,
            IBStream_::IStreamSeekMode_::kIBSeekEnd => len + pos,
            _ => return kInvalidArgument,
        };
        if target < 0 {
            return kInvalidArgument;
        }
        self.pos.set(target as usize);
        if !result.is_null() {
            unsafe { *result = target };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if !pos.is_null() {
            unsafe { *pos = self.pos.get() as i64 };
        }
        kResultOk
    }
}

/// One parameter's changes within a block: `(sample offset, normalized
/// value)`, in time order.
pub(crate) struct Queue {
    id: Cell<ParamID>,
    /// Accepts points and forgets them (the plugin's output queue).
    discard: bool,
    points: UnsafeCell<Vec<(i32, f64)>>,
}

impl Class for Queue {
    type Interfaces = (IParamValueQueue,);
}

impl Queue {
    fn new(id: ParamID, capacity: usize, discard: bool) -> ComWrapper<Queue> {
        ComWrapper::new(Queue {
            id: Cell::new(id),
            discard,
            points: UnsafeCell::new(Vec::with_capacity(capacity)),
        })
    }

    #[allow(clippy::mut_from_ref)]
    fn points(&self) -> &mut Vec<(i32, f64)> {
        // SAFETY: only the audio thread touches a queue, and only outside the
        // plugin's `process` call or from within it, never both.
        unsafe { &mut *self.points.get() }
    }
}

impl IParamValueQueueTrait for Queue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.points().len() as int32
    }

    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        match self.points().get(index as usize) {
            Some(&(at, v)) => {
                unsafe {
                    *offset = at;
                    *value = v;
                }
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        if self.discard {
            if !index.is_null() {
                unsafe { *index = 0 };
            }
            return kResultOk;
        }
        let points = self.points();
        // A host never grows a list on the audio thread; a plugin that
        // writes more points than the room made has the rest dropped.
        if points.len() == points.capacity() {
            return kResultFalse;
        }
        points.push((offset, value));
        if !index.is_null() {
            unsafe { *index = (points.len() - 1) as int32 };
        }
        kResultOk
    }
}

/// `IParameterChanges` for the plugin's input: a queue per parameter that has
/// changes in the block.
pub(crate) struct Changes {
    queues: Vec<ComWrapper<Queue>>,
    /// Indices into `queues` of the ones with points, in the order added.
    active: UnsafeCell<Vec<usize>>,
}

impl Class for Changes {
    type Interfaces = (IParameterChanges,);
}

impl Changes {
    /// Room for `ids`' parameters, each able to hold `points` points a block.
    pub fn new(ids: &[ParamID], points: usize) -> ComWrapper<Changes> {
        ComWrapper::new(Changes {
            queues: ids
                .iter()
                .map(|&id| Queue::new(id, points, false))
                .collect(),
            active: UnsafeCell::new(Vec::with_capacity(ids.len())),
        })
    }

    #[allow(clippy::mut_from_ref)]
    fn active(&self) -> &mut Vec<usize> {
        // SAFETY: as for `Queue::points`.
        unsafe { &mut *self.active.get() }
    }

    pub fn clear(&self) {
        for &index in self.active().iter() {
            self.queues[index].points().clear();
        }
        self.active().clear();
    }

    /// Records a change to the `slot`th parameter. Dropped if its queue is
    /// full.
    pub fn push(&self, slot: usize, offset: i32, value: f64) {
        let queue = &self.queues[slot];
        if queue.points().is_empty() {
            self.active().push(slot);
        }
        let points = queue.points();
        // Points must not go back in time, whoever added them.
        let offset = points.last().map_or(offset, |&(last, _)| offset.max(last));
        if points.len() < points.capacity() {
            points.push((offset, value));
        }
    }
}

impl IParameterChangesTrait for Changes {
    unsafe fn getParameterCount(&self) -> int32 {
        self.active().len() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        match self.active().get(index as usize) {
            Some(&slot) => self.queues[slot]
                .as_com_ref::<IParamValueQueue>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            None => std::ptr::null_mut(),
        }
    }

    unsafe fn addParameterData(
        &self,
        _id: *const ParamID,
        _index: *mut int32,
    ) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

/// `IParameterChanges` for the plugin's output. What a plugin reports there
/// (meters, mostly) is thrown away.
pub(crate) struct DiscardChanges {
    queue: ComWrapper<Queue>,
}

impl Class for DiscardChanges {
    type Interfaces = (IParameterChanges,);
}

impl DiscardChanges {
    pub fn new() -> ComWrapper<DiscardChanges> {
        ComWrapper::new(DiscardChanges {
            queue: Queue::new(0, 0, true),
        })
    }
}

impl IParameterChangesTrait for DiscardChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        0
    }

    unsafe fn getParameterData(&self, _index: int32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        self.queue.id.set(unsafe { *id });
        if !index.is_null() {
            unsafe { *index = 0 };
        }
        self.queue
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

/// `IEventList`, with room for a fixed number of events.
pub(crate) struct Events {
    list: UnsafeCell<Vec<Event>>,
}

impl Class for Events {
    type Interfaces = (IEventList,);
}

impl Events {
    pub fn new(capacity: usize) -> ComWrapper<Events> {
        ComWrapper::new(Events {
            list: UnsafeCell::new(Vec::with_capacity(capacity)),
        })
    }

    #[allow(clippy::mut_from_ref)]
    fn list(&self) -> &mut Vec<Event> {
        // SAFETY: as for `Queue::points`.
        unsafe { &mut *self.list.get() }
    }

    pub fn clear(&self) {
        self.list().clear();
    }

    pub fn len(&self) -> usize {
        self.list().len()
    }

    /// Adds an event if there is room.
    pub fn push(&self, event: Event) {
        let list = self.list();
        if list.len() < list.capacity() {
            list.push(event);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Event> {
        self.list().iter()
    }
}

impl IEventListTrait for Events {
    unsafe fn getEventCount(&self) -> int32 {
        self.list().len() as int32
    }

    unsafe fn getEvent(&self, index: int32, event: *mut Event) -> tresult {
        match self.list().get(index as usize) {
            Some(found) => {
                unsafe { *event = *found };
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, event: *mut Event) -> tresult {
        let list = self.list();
        if list.len() == list.capacity() {
            return kOutOfMemory;
        }
        list.push(unsafe { *event });
        kResultOk
    }
}

/// A raw interface pointer from a wrapper, without touching its count.
pub(crate) fn raw<I: vst3::Interface, C: Class>(wrapper: &ComWrapper<C>) -> *mut I {
    wrapper
        .as_com_ref::<I>()
        .map_or(std::ptr::null_mut(), |r: ComRef<'_, I>| r.as_ptr())
}
