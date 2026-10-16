//! The audio side of an AU node: the buffers, parameter changes and MIDI of
//! each block, and the calls that make the unit render.
//!
//! Like the CLAP and VST3 nodes, it runs a block in chunks of at most
//! `max_frames`, samples wired parameters every [`PARAM_STEP`] frames, and
//! merges every note input into one time-ordered stream. An AU takes
//! parameter changes and MIDI as calls made just before a render, each with
//! a sample offset into that render.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_audio_toolbox::{
    AudioUnitParameterID, AudioUnitRender, AudioUnitRenderActionFlags, AudioUnitSetParameter,
    MusicDeviceMIDIEvent, kAudioUnitScope_Global,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp, AudioTimeStampFlags};

use noodle_engine::{Context, EventKind, Io, Node};

use crate::descriptor::{Descriptor, ParamDesc};
use crate::node::{PARAM_STEP, param_range};

use super::midi_message;
use super::plugin::{BufferList, Loaded, Unit};

/// Notes remembered so a note-off can say which key it is for.
const MAX_NOTES: usize = 256;

struct Slot {
    id: AudioUnitParameterID,
    /// The input's position in the layout.
    input: usize,
    min: f32,
    max: f32,
    stepped: bool,
    /// The last value sent (the default until one is), so a port nobody
    /// touched leaves the unit's own, or restored, value alone.
    last: f32,
}

struct NoteKey {
    port: u16,
    note: u32,
    channel: u8,
    key: u8,
}

/// What the unit's input callbacks read: one list of channel buffers per
/// input bus, owned by the node and filled before each render.
pub(crate) struct Pull {
    buses: Vec<Vec<Vec<f32>>>,
}

/// The render callback of an input bus: copies the node's input into the
/// buffers the unit asks to have filled.
unsafe extern "C-unwind" fn pull_input(
    data: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    _time: NonNull<AudioTimeStamp>,
    bus: u32,
    frames: u32,
    list: *mut AudioBufferList,
) -> i32 {
    // SAFETY: `data` is the `Pull` the node boxed for this unit, alive as
    // long as the unit, and only read here (the node doesn't touch it during
    // a render). The list is the unit's, with `mNumberBuffers` buffers.
    unsafe {
        let pull = &*(data.as_ptr() as *const Pull);
        let Some(list) = list.as_mut() else { return 0 };
        let channels = pull.buses.get(bus as usize);
        let buffers =
            std::ptr::addr_of_mut!(list.mBuffers) as *mut objc2_core_audio_types::AudioBuffer;
        for index in 0..list.mNumberBuffers as usize {
            let buffer = &mut *buffers.add(index);
            let bytes = frames as usize * size_of::<f32>();
            match channels.and_then(|c| c.get(index)) {
                Some(source)
                    if !buffer.mData.is_null() && buffer.mDataByteSize as usize >= bytes =>
                {
                    std::ptr::copy_nonoverlapping(
                        source.as_ptr(),
                        buffer.mData.cast::<f32>(),
                        (frames as usize).min(source.len()),
                    );
                }
                Some(source) => {
                    // The unit gave no memory: point it at ours.
                    buffer.mData = source.as_ptr().cast_mut().cast();
                    buffer.mDataByteSize = bytes as u32;
                }
                None if !buffer.mData.is_null() => {
                    std::ptr::write_bytes(
                        buffer.mData.cast::<u8>(),
                        0,
                        buffer.mDataByteSize as usize,
                    );
                }
                None => {}
            }
        }
    }
    0
}

pub(crate) struct AuNode {
    unit: Unit,
    max_frames: usize,
    out_buses: Vec<Vec<Vec<f32>>>,
    out_lists: Vec<BufferList>,
    params: Vec<Slot>,
    notes: Vec<NoteKey>,
    cursors: Vec<usize>,
    /// Frames rendered, for the unit's sample times.
    steady: f64,
    latency: u32,
    takes_notes: bool,
    /// The unit's callbacks hold this pointer. Last, so it drops after the unit.
    pull: PullBox,
}

// SAFETY: the raw pointer is a box this node owns; the unit is `Send`.
unsafe impl Send for AuNode {}

/// The input buses' buffers for a descriptor, ready to share with the unit's
/// callbacks.
pub(crate) fn make_pull(descriptor: &Descriptor, max_frames: usize) -> *mut Pull {
    Box::into_raw(Box::new(Pull {
        buses: descriptor
            .audio_inputs
            .iter()
            .map(|port| {
                (0..port.channels.max(1))
                    .map(|_| vec![0.0; max_frames])
                    .collect()
            })
            .collect(),
    }))
}

/// A `*mut Pull` that can move to the plugin thread with the closure that sets
/// the callbacks up.
pub(crate) struct PullPtr(pub *mut Pull);
// SAFETY: only used to set the unit's callback data, on one thread.
unsafe impl Send for PullPtr {}

/// Points each input bus of `unit` at the pull buffers.
pub(crate) fn connect_inputs(
    unit: &Unit,
    pull: &PullPtr,
    buses: usize,
) -> Result<(), crate::host::HostError> {
    for bus in 0..buses as u32 {
        unit.set_input_callback(bus, Some(pull_input), pull.0.cast())?;
    }
    Ok(())
}

impl AuNode {
    pub fn new(
        loaded: Loaded,
        pull: *mut Pull,
        descriptor: &Descriptor,
        exposed: &[&ParamDesc],
        max_frames: usize,
    ) -> Self {
        let out_buses: Vec<Vec<Vec<f32>>> = descriptor
            .audio_outputs
            .iter()
            .map(|port| {
                (0..port.channels.max(1))
                    .map(|_| vec![0.0; max_frames])
                    .collect()
            })
            .collect();
        let out_lists = out_buses
            .iter()
            .map(|channels| BufferList::new(channels.len()))
            .collect();
        let first_param = descriptor.audio_inputs.len();
        let params = exposed
            .iter()
            .enumerate()
            .map(|(index, param)| {
                let (min, max, default) = param_range(param.min, param.max, param.default);
                Slot {
                    id: param.id,
                    input: first_param + index,
                    min,
                    max,
                    stepped: param.stepped,
                    last: if param.stepped {
                        default.round()
                    } else {
                        default
                    },
                }
            })
            .collect();
        let id = loaded.unit.id;
        Self {
            unit: loaded.unit,
            max_frames,
            out_buses,
            out_lists,
            params,
            notes: Vec::with_capacity(MAX_NOTES),
            cursors: vec![0; descriptor.note_inputs as usize],
            steady: 0.0,
            latency: loaded.latency,
            takes_notes: id.takes_notes(),
            pull: PullBox(pull),
        }
    }

    /// Runs frames `start..start + n` of the block.
    fn chunk(&mut self, ctx: &Context, io: &mut Io<'_, '_>, start: usize, n: usize) {
        // SAFETY: the node's own box; nothing else reads it until the render.
        let pull = unsafe { &mut *self.pull.0 };
        for (port, channels) in pull.buses.iter_mut().enumerate() {
            let Some(input) = io.inputs.get(port) else {
                continue;
            };
            for (channel, buffer) in channels.iter_mut().enumerate() {
                buffer[..n].copy_from_slice(&input.lane(0, channel)[start..start + n]);
            }
        }
        self.set_transport(ctx, start);
        self.send_events(io, start, n);
        self.render(io, start, n);
    }

    fn set_transport(&self, ctx: &Context, start: usize) {
        let t = &ctx.transport;
        let advance = if t.playing { start as f64 } else { 0.0 };
        let rate = f64::from(ctx.sample_rate);
        self.unit.transport().set(
            t.playing,
            t.bpm,
            t.tick / 960.0 + advance / rate * t.bpm / 60.0,
            t.position as f64 + advance,
            (
                u32::from(t.signature.numerator),
                u32::from(t.signature.denominator),
            ),
        );
    }

    fn render(&mut self, io: &mut Io<'_, '_>, start: usize, n: usize) {
        let time = AudioTimeStamp {
            mSampleTime: self.steady,
            mFlags: AudioTimeStampFlags::SampleTimeValid,
            // SAFETY: a plain-data struct that is valid all zero.
            ..unsafe { std::mem::zeroed() }
        };
        for (bus, channels) in self.out_buses.iter_mut().enumerate() {
            let list = &mut self.out_lists[bus];
            for (channel, buffer) in channels.iter_mut().enumerate() {
                list.set(channel, buffer.as_mut_ptr(), n);
            }
            let mut flags = AudioUnitRenderActionFlags(0);
            // SAFETY: a live, initialised unit; the list's buffers point at
            // `n` samples each; the time stamp is valid.
            let status = unsafe {
                AudioUnitRender(
                    self.unit.raw(),
                    &mut flags,
                    NonNull::from(&time),
                    bus as u32,
                    n as u32,
                    list.as_ptr(),
                )
            };
            let out = &mut io.outputs[bus];
            for channel in 0..channels.len() {
                let lane = &mut out.lane_mut(0, channel)[start..start + n];
                if status == 0 {
                    // The unit may have pointed the buffer at its own memory.
                    let data = list.data(channel);
                    if data.is_null() {
                        lane.fill(0.0);
                    } else {
                        // SAFETY: a rendered buffer of `n` samples.
                        lane.copy_from_slice(unsafe { std::slice::from_raw_parts(data, n) });
                    }
                } else {
                    lane.fill(0.0);
                }
            }
        }
        self.steady += n as f64;
    }

    /// Sends the chunk's parameter changes and notes to the unit, each with
    /// its offset into the render that follows.
    fn send_events(&mut self, io: &Io<'_, '_>, start: usize, n: usize) {
        self.sample_params(io, start, 0);
        let mut next_step = PARAM_STEP;
        self.cursors.iter_mut().for_each(|c| *c = 0);
        loop {
            let mut pick: Option<(usize, usize)> = None;
            for (port, cursor) in self.cursors.iter_mut().enumerate() {
                let events = io.event_inputs.get(port).copied().unwrap_or(&[]);
                while let Some(event) = events.get(*cursor) {
                    let time = event.time as usize;
                    if time < start {
                        *cursor += 1;
                    } else {
                        if time < start + n && pick.is_none_or(|(_, t)| time < t) {
                            pick = Some((port, time));
                        }
                        break;
                    }
                }
            }
            let Some((port, time)) = pick else { break };
            let event = &io.event_inputs[port][self.cursors[port]];
            self.cursors[port] += 1;
            let at = time - start;
            while next_step <= at {
                self.sample_params(io, start, next_step);
                next_step += PARAM_STEP;
            }
            if self.takes_notes {
                self.send_note(port as u16, &event.kind, at as u32);
            }
        }
        while next_step < n {
            self.sample_params(io, start, next_step);
            next_step += PARAM_STEP;
        }
    }

    /// Sends the parameters whose value at `start + at` differs from what the
    /// unit has. Unmodulated ones are only looked at at `at == 0`.
    fn sample_params(&mut self, io: &Io<'_, '_>, start: usize, at: usize) {
        for slot in &mut self.params {
            let input = &io.inputs[slot.input];
            let value = match input.constant() {
                Some(value) => {
                    if at != 0 {
                        continue;
                    }
                    value
                }
                None => input.lane(0, 0)[start + at],
            };
            let mut value = value.clamp(slot.min, slot.max);
            if slot.stepped {
                value = value.round();
            }
            if value == slot.last || !value.is_finite() {
                continue;
            }
            slot.last = value;
            // SAFETY: a live unit; the parameter is in the global scope.
            unsafe {
                AudioUnitSetParameter(
                    self.unit.raw(),
                    slot.id,
                    kAudioUnitScope_Global,
                    0,
                    value,
                    at as u32,
                );
            }
        }
    }

    fn send_note(&mut self, port: u16, kind: &EventKind, at: u32) {
        let message = match *kind {
            EventKind::NoteOn {
                note, channel, key, ..
            } => {
                self.notes.retain(|n| !(n.port == port && n.note == note.0));
                if self.notes.len() == MAX_NOTES {
                    self.notes.remove(0);
                }
                self.notes.push(NoteKey {
                    port,
                    note: note.0,
                    channel,
                    key,
                });
                midi_message(kind)
            }
            EventKind::NoteOff { note, velocity } => {
                let found = self
                    .notes
                    .iter()
                    .position(|n| n.port == port && n.note == note.0)
                    .map(|i| self.notes.remove(i));
                let (channel, key) = found.map_or((0, 0), |n| (n.channel, n.key));
                Some([
                    0x80 | (channel & 0x0f),
                    key & 0x7f,
                    (velocity.clamp(0.0, 1.0) * 127.0).round() as u8,
                ])
            }
            _ => midi_message(kind),
        };
        let Some([status, data1, data2]) = message else {
            return;
        };
        // SAFETY: a live unit; MIDI is data bytes and a frame offset.
        unsafe {
            MusicDeviceMIDIEvent(
                self.unit.raw(),
                u32::from(status),
                u32::from(data1),
                u32::from(data2),
                at,
            );
        }
    }
}

impl Node for AuNode {
    fn latency(&self) -> u32 {
        self.latency
    }

    fn process(&mut self, ctx: &Context, mut io: Io<'_, '_>) {
        let mut start = 0;
        while start < ctx.frames {
            let n = (ctx.frames - start).min(self.max_frames);
            self.chunk(ctx, &mut io, start, n);
            start += n;
        }
    }

    fn reset(&mut self) {
        self.unit.reset();
        self.notes.clear();
    }
}

/// The pull buffers, freed when the node is. A field declared after the unit,
/// so the unit (which holds the pointer in its callbacks) is gone first.
struct PullBox(*mut Pull);

impl Drop for PullBox {
    fn drop(&mut self) {
        // SAFETY: from `Box::into_raw`, freed once, after the unit's gone.
        unsafe { drop(Box::from_raw(self.0)) };
    }
}
