//! The COM objects a VST3 host hands to a plugin: the host application, the
//! component handler, memory streams for state, and the parameter and event
//! lists that carry a block's changes.
//!
//! The lists are used from one thread at a time (the audio thread, around one
//! `process` call), so they keep their data in `UnsafeCell`s and never
//! allocate after they are made.

use std::cell::{Cell, RefCell, UnsafeCell};
use std::ffi::{CStr, CString, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper, Interface};

use crate::host::Flags;

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

/// `IHostApplication`: the host's name, and the messages and attribute lists
/// plugins use to talk to each other (see [`HostMessage`]).
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
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        let bytes = |id: *mut TUID| unsafe { *id }.map(|b| b as u8);
        let (cid, iid) = (bytes(cid), bytes(iid));
        // SAFETY: `obj` is the plugin's out pointer; the object handed over
        // carries one reference for it.
        if cid == IMessage::IID && iid == IMessage::IID {
            let Some(ptr) = ComWrapper::new(HostMessage::new()).to_com_ptr::<IMessage>() else {
                return kOutOfMemory;
            };
            unsafe { *obj = ptr.into_raw() as *mut c_void };
            kResultOk
        } else if cid == IAttributeList::IID && iid == IAttributeList::IID {
            let Some(ptr) = ComWrapper::new(Attributes::default()).to_com_ptr::<IAttributeList>()
            else {
                return kOutOfMemory;
            };
            unsafe { *obj = ptr.into_raw() as *mut c_void };
            kResultOk
        } else {
            kNotImplemented
        }
    }
}

/// A value in an [`Attributes`] list.
enum Attr {
    Int(i64),
    Float(f64),
    /// UTF-16, with its terminator.
    Text(Vec<TChar>),
    Binary(Vec<u8>),
}

/// `IAttributeList`: named values a message carries.
#[derive(Default)]
pub(crate) struct Attributes {
    map: RefCell<Vec<(CString, Attr)>>,
}

impl Class for Attributes {
    type Interfaces = (IAttributeList,);
}

impl Attributes {
    fn set(&self, id: *const std::ffi::c_char, value: Attr) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes a C string.
        let key = unsafe { CStr::from_ptr(id) }.to_owned();
        let mut map = self.map.borrow_mut();
        match map.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => map.push((key, value)),
        }
        kResultOk
    }

    fn get<T>(
        &self,
        id: *const std::ffi::c_char,
        read: impl FnOnce(&Attr) -> Option<T>,
    ) -> Option<T> {
        if id.is_null() {
            return None;
        }
        // SAFETY: as for `set`.
        let key = unsafe { CStr::from_ptr(id) };
        let map = self.map.borrow();
        map.iter()
            .find(|(k, _)| k.as_c_str() == key)
            .and_then(|(_, v)| read(v))
    }
}

impl IAttributeListTrait for Attributes {
    unsafe fn setInt(&self, id: *const std::ffi::c_char, value: int64) -> tresult {
        self.set(id, Attr::Int(value))
    }

    unsafe fn getInt(&self, id: *const std::ffi::c_char, value: *mut int64) -> tresult {
        match self.get(id, |a| if let Attr::Int(v) = a { Some(*v) } else { None }) {
            Some(v) if !value.is_null() => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: *const std::ffi::c_char, value: f64) -> tresult {
        self.set(id, Attr::Float(value))
    }

    unsafe fn getFloat(&self, id: *const std::ffi::c_char, value: *mut f64) -> tresult {
        match self.get(id, |a| {
            if let Attr::Float(v) = a {
                Some(*v)
            } else {
                None
            }
        }) {
            Some(v) if !value.is_null() => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: *const std::ffi::c_char, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut text = Vec::new();
        let mut at = string;
        // SAFETY: the plugin passes a zero-terminated UTF-16 string.
        unsafe {
            while *at != 0 {
                text.push(*at);
                at = at.add(1);
            }
        }
        text.push(0);
        self.set(id, Attr::Text(text))
    }

    unsafe fn getString(
        &self,
        id: *const std::ffi::c_char,
        string: *mut TChar,
        size_in_bytes: uint32,
    ) -> tresult {
        let room = size_in_bytes as usize / std::mem::size_of::<TChar>();
        if string.is_null() || room == 0 {
            return kInvalidArgument;
        }
        let found = self.get(id, |a| {
            if let Attr::Text(t) = a {
                Some(t.clone())
            } else {
                None
            }
        });
        let Some(text) = found else {
            return kResultFalse;
        };
        // SAFETY: the plugin gave `size_in_bytes` of room.
        let out = unsafe { std::slice::from_raw_parts_mut(string, room) };
        let n = text.len().min(room);
        out[..n].copy_from_slice(&text[..n]);
        out[room - 1] = 0;
        kResultOk
    }

    unsafe fn setBinary(
        &self,
        id: *const std::ffi::c_char,
        data: *const c_void,
        size_in_bytes: uint32,
    ) -> tresult {
        if data.is_null() && size_in_bytes != 0 {
            return kInvalidArgument;
        }
        let bytes = if size_in_bytes == 0 {
            Vec::new()
        } else {
            // SAFETY: the plugin passes `size_in_bytes` of data.
            unsafe { std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize) }
                .to_vec()
        };
        self.set(id, Attr::Binary(bytes))
    }

    unsafe fn getBinary(
        &self,
        id: *const std::ffi::c_char,
        data: *mut *const c_void,
        size_in_bytes: *mut uint32,
    ) -> tresult {
        if id.is_null() || data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: as for `set`.
        let key = unsafe { CStr::from_ptr(id) };
        // The pointer stays valid until the attribute is set again or the
        // list is gone, which is how the standard has it.
        let map = self.map.borrow();
        match map.iter().find(|(k, _)| k.as_c_str() == key) {
            Some((_, Attr::Binary(bytes))) => {
                unsafe {
                    *data = bytes.as_ptr() as *const c_void;
                    *size_in_bytes = bytes.len() as uint32;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

/// `IMessage`: what the component and controller of one plugin send each
/// other through `IConnectionPoint`. Delivered on the sender's thread.
pub(crate) struct HostMessage {
    id: RefCell<CString>,
    attributes: ComWrapper<Attributes>,
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl HostMessage {
    fn new() -> Self {
        Self {
            id: RefCell::new(CString::default()),
            attributes: ComWrapper::new(Attributes::default()),
        }
    }
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        // The string lives until the ID is set again.
        self.id.borrow().as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        if !id.is_null() {
            // SAFETY: the plugin passes a C string.
            *self.id.borrow_mut() = unsafe { CStr::from_ptr(id) }.to_owned();
        }
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        raw::<IAttributeList, _>(&self.attributes)
    }
}

/// Parameter edits made by a controller (from its editor), waiting for the
/// audio thread to send them to the processor.
///
/// A controller reports an edit through the component handler, and the host
/// is meant to pass it to the processor as a parameter change; without that a
/// plugin whose controller is separate from its processor changes its
/// display and not its sound. The table is made once the controller's
/// parameters are known and never grows, so the audio thread reads it
/// without a lock.
#[derive(Default)]
pub(crate) struct Edits {
    table: OnceLock<EditTable>,
    dirty: AtomicBool,
}

struct EditTable {
    /// Ascending.
    ids: Vec<ParamID>,
    /// A value's `f64` bits, or [`NONE`] if there is no edit waiting.
    values: Vec<AtomicU64>,
    /// The last value the processor was given for each parameter (from a
    /// port, a MIDI controller or an edit), or [`NONE`].
    sent: Vec<AtomicU64>,
}

/// An `f64` NaN, which no edit is.
const NONE: u64 = 0x7FF8_0000_0000_0000;

impl Edits {
    /// Sets which parameters edits are kept for (the first call wins).
    pub fn set_ids(&self, mut ids: Vec<ParamID>) {
        ids.sort_unstable();
        ids.dedup();
        let values = ids.iter().map(|_| AtomicU64::new(NONE)).collect();
        let sent = ids.iter().map(|_| AtomicU64::new(NONE)).collect();
        let _ = self.table.set(EditTable { ids, values, sent });
    }

    /// The parameters edits are kept for, ascending.
    pub fn ids(&self) -> &[ParamID] {
        self.table.get().map_or(&[], |t| &t.ids)
    }

    /// The parameter's place in [`ids`](Self::ids).
    pub fn index_of(&self, id: ParamID) -> Option<usize> {
        self.table.get()?.ids.binary_search(&id).ok()
    }

    /// Notes what the processor was last given for the parameter at `index`.
    /// Doesn't allocate or lock.
    pub fn note_sent(&self, index: usize, value: f64) {
        if let Some(cell) = self.table.get().and_then(|t| t.sent.get(index)) {
            cell.store(value.to_bits(), Ordering::Release);
        }
    }

    /// Calls `f(id, value)` for each parameter the processor was given a
    /// value for and that has no edit still waiting, for the controller to be
    /// told: it only hears of edits made in its own editor.
    pub fn for_each_sent(&self, mut f: impl FnMut(ParamID, f64)) {
        let Some(table) = self.table.get() else {
            return;
        };
        for (index, id) in table.ids.iter().enumerate() {
            let bits = table.sent[index].load(Ordering::Acquire);
            if bits != NONE && table.values[index].load(Ordering::Acquire) == NONE {
                f(*id, f64::from_bits(bits));
            }
        }
    }

    fn record(&self, id: ParamID, value: f64) {
        let Some(table) = self.table.get() else {
            return;
        };
        if let Ok(index) = table.ids.binary_search(&id)
            && value.is_finite()
        {
            table.values[index].store(value.clamp(0.0, 1.0).to_bits(), Ordering::Release);
            self.dirty.store(true, Ordering::Release);
        }
    }

    /// Calls `send(index, value)` for each edit waiting, `index` being the
    /// parameter's place in [`ids`](Self::ids). Doesn't allocate or lock.
    pub fn take(&self, mut send: impl FnMut(usize, f64)) {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let Some(table) = self.table.get() else {
            return;
        };
        for (index, cell) in table.values.iter().enumerate() {
            let bits = cell.swap(NONE, Ordering::AcqRel);
            if bits != NONE {
                send(index, f64::from_bits(bits));
            }
        }
    }
}

/// `IComponentHandler`: the controller's way of telling the host about edits.
pub(crate) struct ComponentHandler {
    pub edits: Arc<Edits>,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.edits.record(id, value);
        kResultOk
    }
    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        kResultOk
    }
}

/// `IPlugFrame`: how an embedded editor asks the host for another size. The
/// plugin thread carries it out the next time it looks (the plugin may ask
/// from inside a call the host made).
pub(crate) struct Frame {
    pub flags: Arc<Flags>,
}

impl Class for Frame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, _view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kInvalidArgument;
        }
        let rect = unsafe { &*new_size };
        let width = (rect.right - rect.left).max(1) as u64;
        let height = (rect.bottom - rect.top).max(1) as u64;
        self.flags
            .resize
            .store((width << 32) | height, Ordering::Release);
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
    /// Room for each of `params`' parameters, each able to hold the given
    /// number of points a block.
    pub fn new(params: &[(ParamID, usize)]) -> ComWrapper<Changes> {
        ComWrapper::new(Changes {
            queues: params
                .iter()
                .map(|&(id, points)| Queue::new(id, points, false))
                .collect(),
            active: UnsafeCell::new(Vec::with_capacity(params.len())),
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
