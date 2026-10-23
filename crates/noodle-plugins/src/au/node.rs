//! The AU half of a plugin node: how parameter changes, MIDI, the transport
//! and the audio are handed to the unit, and the calls that make it render
//! (see [`crate::bridge`] for the rest). An AU takes parameter changes and MIDI
//! as calls made just before a render, each with a sample offset into that
//! render.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_audio_toolbox::{
    AudioUnitParameterID, AudioUnitRender, AudioUnitRenderActionFlags, AudioUnitSetParameter,
    MusicDeviceMIDIEvent, kAudioUnitScope_Global,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp, AudioTimeStampFlags};

use noodle_engine::{Event, EventKind};

use crate::bridge::{Bridge, Buffers, Format, ParamSlot, Position, Status};
use crate::descriptor::{Descriptor, ParamDesc};
use crate::node::param_range;

use super::midi_message;
use super::plugin::{BufferList, Loaded, Unit};

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

/// The input buses' buffers for a descriptor, boxed for the unit's
/// callbacks.
fn make_pull(descriptor: &Descriptor, max_frames: usize) -> PullBox {
    PullBox(Box::into_raw(Box::new(Pull {
        buses: descriptor
            .audio_inputs
            .iter()
            .map(|port| {
                (0..port.channels.max(1))
                    .map(|_| vec![0.0; max_frames])
                    .collect()
            })
            .collect(),
    })))
}

/// Makes and sets up the unit for `descriptor` on the plugin thread. The job
/// makes the pull buffers and the loaded unit owns them, so a load that
/// times out (the job keeps running, unwaited for) frees them with the unit
/// instead of from under it.
pub(crate) fn load(
    descriptor: &Descriptor,
    state: Option<Vec<u8>>,
    sample_rate: f32,
    max_frames: usize,
) -> Result<Loaded<PullBox>, crate::host::HostError> {
    let descriptor = descriptor.clone();
    super::plugin::load(
        &descriptor.id.clone(),
        state,
        sample_rate,
        max_frames,
        move |unit| {
            let pull = make_pull(&descriptor, max_frames);
            for bus in 0..descriptor.audio_inputs.len() as u32 {
                unit.set_input_callback(bus, Some(pull_input), pull.0.cast())?;
            }
            Ok(pull)
        },
    )
}

pub(crate) struct AuFormat {
    unit: Unit,
    out_lists: Vec<BufferList>,
    /// The parameter behind each of the bridge's parameter slots.
    param_ids: Vec<AudioUnitParameterID>,
    /// Frames the node has been given, for the unit's sample times.
    steady: f64,
    latency: u32,
    takes_notes: bool,
    /// The unit's callbacks hold this pointer. Last, so it drops after the unit.
    pull: PullBox,
}

// SAFETY: the raw pointer is a box this node owns; the unit is `Send`.
unsafe impl Send for AuFormat {}

/// The bridge for a loaded Audio Unit.
pub(crate) fn au_node(
    loaded: Loaded<PullBox>,
    descriptor: &Descriptor,
    exposed: &[&ParamDesc],
    max_frames: usize,
) -> Bridge<AuFormat> {
    let channels = |ports: &[crate::descriptor::AudioPort]| -> Vec<u32> {
        ports.iter().map(|p| p.channels).collect()
    };
    let buffers = Buffers::new(
        channels(&descriptor.audio_inputs),
        channels(&descriptor.audio_outputs),
        max_frames,
    );
    let out_lists = buffers
        .outputs
        .iter()
        .map(|bus| BufferList::new(bus.channels.len()))
        .collect();
    let first_param = descriptor.audio_inputs.len();
    let mut param_ids = Vec::new();
    let params = exposed
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let (min, max, default) = param_range(param.min, param.max, param.default);
            param_ids.push(param.id);
            ParamSlot::new(Some(first_param + index), min, max, param.stepped, default)
        })
        .collect();
    let id = loaded.unit.id;
    let format = AuFormat {
        unit: loaded.unit,
        out_lists,
        param_ids,
        steady: 0.0,
        latency: loaded.latency,
        takes_notes: id.takes_notes(),
        pull: loaded.extra,
    };
    Bridge::new(
        format,
        buffers,
        params,
        descriptor.note_inputs as usize,
        false,
        max_frames,
    )
}

impl Format for AuFormat {
    fn begin(&mut self, _n: usize) -> bool {
        false
    }

    fn param(&mut self, slot: usize, at: usize, value: f32) {
        // SAFETY: a live unit; the parameter is in the global scope.
        unsafe {
            AudioUnitSetParameter(
                self.unit.raw(),
                self.param_ids[slot],
                kAudioUnitScope_Global,
                0,
                value,
                at as u32,
            );
        }
    }

    fn note(&mut self, _port: usize, event: &Event, at: usize, key: Option<(u8, u8)>) {
        if !self.takes_notes {
            return;
        }
        let message = match event.kind {
            EventKind::NoteOff { velocity, .. } => {
                // A note we don't remember (never seen, or evicted) has no
                // key to release: sending key 0 could cut a held one.
                key.map(|(channel, key)| {
                    [
                        0x80 | (channel & 0x0f),
                        key & 0x7f,
                        (velocity.clamp(0.0, 1.0) * 127.0).round() as u8,
                    ]
                })
            }
            ref kind => midi_message(kind),
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
                at as u32,
            );
        }
    }

    fn transport(&mut self, position: &Position) {
        self.steady = position.steady as f64;
        self.unit.transport().set(
            position.playing,
            position.bpm,
            position.beats,
            position.samples,
            (
                u32::from(position.signature.numerator),
                u32::from(position.signature.denominator),
            ),
            position.bar_start,
        );
    }

    fn process(&mut self, n: usize, buffers: &mut Buffers) -> Status {
        // SAFETY: the node's own box; nothing else reads it until the render.
        let pull = unsafe { &mut *self.pull.0 };
        for (channels, bus) in pull.buses.iter_mut().zip(&buffers.inputs) {
            for (buffer, source) in channels.iter_mut().zip(&bus.channels) {
                buffer[..n].copy_from_slice(&source[..n]);
            }
        }
        let time = AudioTimeStamp {
            mSampleTime: self.steady,
            mFlags: AudioTimeStampFlags::SampleTimeValid,
            // SAFETY: a plain-data struct that is valid all zero.
            ..unsafe { std::mem::zeroed() }
        };
        let mut all_silent = !buffers.outputs.is_empty();
        for (index, bus) in buffers.outputs.iter_mut().enumerate() {
            let list = &mut self.out_lists[index];
            for (channel, buffer) in bus.channels.iter_mut().enumerate() {
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
                    index as u32,
                    n as u32,
                    list.as_ptr(),
                )
            };
            let silent = status == 0
                && flags.0 & AudioUnitRenderActionFlags::UnitRenderAction_OutputIsSilence.0 != 0;
            all_silent &= silent;
            bus.silent = if silent { u64::MAX } else { 0 };
            for (channel, buffer) in bus.channels.iter_mut().enumerate() {
                if status != 0 {
                    buffer[..n].fill(0.0);
                    continue;
                }
                // The unit may have pointed the buffer at its own memory.
                let data = list.data(channel);
                if data.is_null() {
                    buffer[..n].fill(0.0);
                } else if data != buffer.as_ptr() {
                    // SAFETY: a rendered buffer of `n` samples.
                    buffer[..n].copy_from_slice(unsafe { std::slice::from_raw_parts(data, n) });
                }
            }
        }
        if all_silent {
            Status::Sleep
        } else {
            Status::Unknown
        }
    }

    fn drain_note_outputs(&mut self, _emit: &mut dyn FnMut(usize, u32, EventKind)) {}

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        self.unit.reset();
    }
}

/// The pull buffers, freed when the node is. A field declared after the unit,
/// so the unit (which holds the pointer in its callbacks) is gone first.
pub(crate) struct PullBox(*mut Pull);

// SAFETY: a box of plain buffers; the unit only reads it while rendering on
// whichever thread renders.
unsafe impl Send for PullBox {}

impl Drop for PullBox {
    fn drop(&mut self) {
        // SAFETY: from `Box::into_raw`, freed once, after the unit's gone.
        unsafe { drop(Box::from_raw(self.0)) };
    }
}
