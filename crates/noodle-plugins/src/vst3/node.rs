//! The VST3 half of a plugin node: how parameter changes, events, the
//! transport and the audio buses are handed to the plugin's `process` (see
//! [`crate::bridge`] for the rest).

use std::sync::Arc;

use vst3::ComWrapper;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;

use noodle_engine::{Event as NoodleEvent, EventKind, Expression, NoteId};

use crate::bridge::{Bridge, Buffers, Format, MAX_EVENTS, PARAM_STEP, ParamSlot, Position, Status};
use crate::descriptor::{Descriptor, ParamDesc};
use crate::node::param_range;

use super::classes::{Changes, DiscardChanges, Edits, Events, raw};
use super::plugin::Plugin;

/// A parameter the node sends: one of the plugin's (with a port) or one a MIDI
/// controller drives or the editor changes (without). The bridge's parameter
/// slots are in the same order.
struct Slot {
    id: ParamID,
    /// Steps of a stepped parameter (the port then holds 0 to `steps`), or 0.
    steps: i32,
    /// This parameter's place in the edits table, if it has one.
    edit_index: Option<usize>,
}

/// A MIDI controller a parameter answers to.
struct MidiParam {
    channel: u8,
    controller: u8,
    slot: usize,
}

/// A parameter the plugin's editor changes: where its slot is.
struct EditSlot {
    slot: usize,
}

pub(crate) struct Vst3Format {
    plugin: Option<Plugin>,
    /// Pointers to each bus's channel buffers, which `AudioBusBuffers` points to.
    _in_pointers: Vec<Vec<*mut f32>>,
    _out_pointers: Vec<Vec<*mut f32>>,
    /// The `AudioBusBuffers` for the buses, pointing into the buffers above.
    in_buffers: Vec<AudioBusBuffers>,
    out_buffers: Vec<AudioBusBuffers>,
    changes: ComWrapper<Changes>,
    discard: ComWrapper<DiscardChanges>,
    in_events: ComWrapper<Events>,
    out_events: ComWrapper<Events>,
    slots: Vec<Slot>,
    midi_params: Vec<MidiParam>,
    /// What the controller's editor changed, and the slot of each parameter
    /// it may change (in the order of the edits' IDs).
    edits: Arc<Edits>,
    edit_slots: Vec<EditSlot>,
    /// The editor this node's plugin can open, to forget on drop.
    gui_key: Option<u64>,
    context: ProcessContext,
    /// The latency the plugin reported when it was activated, in samples.
    latency: u32,
    started: bool,
}

// SAFETY: the raw pointers point into buffers the bridge owns, and the plugin
// is `Send`; the COM lists are only used from the thread running `process`.
unsafe impl Send for Vst3Format {}

/// The bridge for a loaded VST3 plugin.
pub(crate) fn vst3_node(
    plugin: Plugin,
    descriptor: &Descriptor,
    exposed: &[&ParamDesc],
    midi_assignments: &[(i16, i16, ParamID)],
    gui_key: Option<u64>,
    latency: u32,
    max_frames: usize,
) -> Bridge<Vst3Format> {
    let channels = |ports: &[crate::descriptor::AudioPort]| -> Vec<u32> {
        ports.iter().map(|p| p.channels).collect()
    };
    let mut buffers = Buffers::new(
        channels(&descriptor.audio_inputs),
        channels(&descriptor.audio_outputs),
        max_frames,
    );
    let mut in_pointers = pointers_of(&mut buffers.inputs);
    let mut out_pointers = pointers_of(&mut buffers.outputs);
    let in_buffers = bus_buffers(&mut in_pointers);
    let out_buffers = bus_buffers(&mut out_pointers);

    let first_param = descriptor.audio_inputs.len();
    let mut slots: Vec<Slot> = Vec::new();
    let mut params: Vec<ParamSlot> = Vec::new();
    for (index, param) in exposed.iter().enumerate() {
        let (min, max, default) = param_range(param.min, param.max, param.default);
        slots.push(Slot {
            id: param.id,
            steps: if param.stepped { max as i32 } else { 0 },
            edit_index: None,
        });
        params.push(ParamSlot::new(
            Some(first_param + index),
            min,
            max,
            param.stepped,
            default,
        ));
    }
    // A slot with no port, which nothing the bridge samples drives.
    let mut portless = |slots: &mut Vec<Slot>, id: ParamID| -> usize {
        slots.push(Slot {
            id,
            steps: 0,
            edit_index: None,
        });
        params.push(ParamSlot::new(None, 0.0, 1.0, false, 0.0));
        slots.len() - 1
    };
    let mut midi_params = Vec::new();
    for &(channel, controller, id) in midi_assignments {
        // A parameter that already has a port is driven through it too.
        let slot = match slots.iter().position(|s| s.id == id) {
            Some(slot) => slot,
            None => portless(&mut slots, id),
        };
        midi_params.push(MidiParam {
            channel: channel as u8,
            controller: controller as u8,
            slot,
        });
    }

    // A point at each parameter step, and the block's start and end.
    let points = max_frames / PARAM_STEP + 3;
    // Parameters the editor changes but nothing else drives get a slot
    // with room for a couple of points a chunk.
    let plugin_edits = Arc::clone(plugin.edits());
    let mut capacity: Vec<usize> = slots.iter().map(|_| points + 16).collect();
    let edit_slots = plugin_edits
        .ids()
        .iter()
        .map(|&id| {
            let slot = match slots.iter().position(|s| s.id == id) {
                Some(slot) => slot,
                None => {
                    capacity.push(2);
                    portless(&mut slots, id)
                }
            };
            EditSlot { slot }
        })
        .collect();
    for slot in &mut slots {
        slot.edit_index = plugin_edits.index_of(slot.id);
    }
    let ids: Vec<(ParamID, usize)> = slots.iter().map(|s| s.id).zip(capacity).collect();
    let format = Vst3Format {
        plugin: Some(plugin),
        _in_pointers: in_pointers,
        _out_pointers: out_pointers,
        in_buffers,
        out_buffers,
        changes: Changes::new(&ids),
        discard: DiscardChanges::new(),
        in_events: Events::new(MAX_EVENTS),
        out_events: Events::new(MAX_EVENTS),
        slots,
        midi_params,
        edits: plugin_edits,
        edit_slots,
        gui_key,
        // SAFETY: plain data; zero is "not valid" for every field.
        context: unsafe { std::mem::zeroed() },
        latency,
        started: false,
    };
    Bridge::new(
        format,
        buffers,
        params,
        descriptor.note_inputs as usize,
        descriptor.note_outputs > 0,
        max_frames,
    )
}

fn pointers_of(buses: &mut [crate::bridge::Bus]) -> Vec<Vec<*mut f32>> {
    buses
        .iter_mut()
        .map(|bus| bus.channels.iter_mut().map(|c| c.as_mut_ptr()).collect())
        .collect()
}

fn bus_buffers(pointers: &mut [Vec<*mut f32>]) -> Vec<AudioBusBuffers> {
    pointers
        .iter_mut()
        .map(|bus| AudioBusBuffers {
            numChannels: bus.len() as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: bus.as_mut_ptr(),
            },
        })
        .collect()
}

impl Format for Vst3Format {
    fn begin(&mut self, _n: usize) -> bool {
        self.changes.clear();
        self.in_events.clear();
        self.out_events.clear();
        self.apply_edits()
    }

    fn param(&mut self, slot: usize, at: usize, value: f32) {
        let steps = self.slots[slot].steps;
        let normalized = if steps > 0 {
            f64::from(value) / f64::from(steps)
        } else {
            f64::from(value)
        };
        self.changes.push(slot, at as i32, normalized);
        if let Some(i) = self.slots[slot].edit_index {
            self.edits.note_sent(i, normalized);
        }
    }

    fn note(&mut self, port: usize, event: &NoodleEvent, at: usize, key: Option<(u8, u8)>) {
        let (port, time) = (port as i32, at as i32);
        let (channel, key) = key.unwrap_or((0, 0));
        match event.kind {
            EventKind::NoteOn {
                note,
                channel,
                key,
                velocity,
            } => {
                self.in_events.push(note_on(
                    port,
                    time,
                    note.0 as i32,
                    i16::from(channel),
                    i16::from(key),
                    velocity,
                ));
            }
            EventKind::NoteOff { note, velocity } => {
                self.in_events.push(note_off(
                    port,
                    time,
                    note.0 as i32,
                    i16::from(channel),
                    i16::from(key),
                    velocity,
                ));
            }
            EventKind::Expression {
                note,
                expression,
                value,
            } => {
                if expression == Expression::Pressure {
                    self.in_events.push(poly_pressure(
                        port,
                        time,
                        note.0 as i32,
                        i16::from(channel),
                        i16::from(key),
                        value,
                    ));
                    return;
                }
                // VST3 note expressions are normalized 0 to 1. Pitch is in
                // semitones (the standard's tuning range is ±120); pan runs
                // -1 to 1 here.
                let (kind, value) = match expression {
                    Expression::Pitch => (
                        NoteExpressionTypeIDs_::kTuningTypeID as u32,
                        (value + 120.0) / 240.0,
                    ),
                    Expression::Timbre => (NoteExpressionTypeIDs_::kBrightnessTypeID as u32, value),
                    // Noodle's volume is linear gain; VST3's normalized 0 to 1 is
                    // 0 to 4 in gain (0.25 is unity).
                    Expression::Volume => {
                        (NoteExpressionTypeIDs_::kVolumeTypeID as u32, value / 4.0)
                    }
                    Expression::Pan => (
                        NoteExpressionTypeIDs_::kPanTypeID as u32,
                        (value + 1.0) / 2.0,
                    ),
                    Expression::Pressure => unreachable!("handled above"),
                };
                self.in_events.push(note_expression(
                    port,
                    time,
                    note.0 as i32,
                    kind,
                    f64::from(value.clamp(0.0, 1.0)),
                ));
            }
            other @ (EventKind::Midi(_) | EventKind::Channel { .. }) => {
                if let Some(data) = other.midi1() {
                    self.push_midi(port, time, data);
                }
            }
        }
    }

    fn transport(&mut self, position: &Position) {
        self.context = process_context(position);
    }

    fn process(&mut self, n: usize, buffers: &mut Buffers) -> Status {
        let Some(plugin) = self.plugin.as_mut() else {
            return Status::Failed;
        };
        if !self.started {
            self.started = plugin.start_processing();
        }
        if !self.started {
            return Status::Failed;
        }
        for (bus, buffer) in buffers.inputs.iter().zip(&mut self.in_buffers) {
            buffer.silenceFlags = bus.silent;
        }
        for buffer in &mut self.out_buffers {
            buffer.silenceFlags = 0;
        }

        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: n as i32,
            numInputs: self.in_buffers.len() as i32,
            numOutputs: self.out_buffers.len() as i32,
            inputs: self.in_buffers.as_mut_ptr(),
            outputs: self.out_buffers.as_mut_ptr(),
            inputParameterChanges: raw::<IParameterChanges, _>(&self.changes),
            outputParameterChanges: raw::<IParameterChanges, _>(&self.discard),
            inputEvents: raw::<IEventList, _>(&self.in_events),
            outputEvents: raw::<IEventList, _>(&self.out_events),
            processContext: &mut self.context,
        };
        // SAFETY: `data` points at buffers and lists that live as long as the
        // call, which the plugin may use for its duration only.
        let result = unsafe { plugin.processor.process(&mut data) };
        if result != kResultOk {
            return Status::Failed;
        }
        // The buses the plugin says it left silent. All of them means it has
        // nothing to say until something comes in.
        let mut all = !buffers.outputs.is_empty();
        for (bus, buffer) in buffers.outputs.iter_mut().zip(&self.out_buffers) {
            bus.silent = buffer.silenceFlags;
            let wanted = if bus.channels.len() >= 64 {
                u64::MAX
            } else {
                (1u64 << bus.channels.len()) - 1
            };
            all &= buffer.silenceFlags & wanted == wanted;
        }
        if all { Status::Sleep } else { Status::Unknown }
    }

    fn drain_note_outputs(&mut self, emit: &mut dyn FnMut(usize, u32, EventKind)) {
        for event in self.out_events.iter() {
            let time = event.sampleOffset.max(0) as u32;
            // SAFETY: the union member read is the one the type says.
            let kind = unsafe {
                match u32::from(event.r#type) {
                    x if x == Event_::EventTypes_::kNoteOnEvent as u32 => {
                        let on = event.__field0.noteOn;
                        EventKind::NoteOn {
                            note: output_note_id(on.noteId, on.channel, on.pitch),
                            channel: on.channel.clamp(0, 15) as u8,
                            key: on.pitch.clamp(0, 127) as u8,
                            velocity: on.velocity,
                        }
                    }
                    x if x == Event_::EventTypes_::kNoteOffEvent as u32 => {
                        let off = event.__field0.noteOff;
                        EventKind::NoteOff {
                            note: output_note_id(off.noteId, off.channel, off.pitch),
                            velocity: off.velocity,
                        }
                    }
                    _ => continue,
                }
            };
            emit(event.busIndex.max(0) as usize, time, kind);
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        if let Some(plugin) = self.plugin.as_mut() {
            plugin.reset();
        }
    }
}

impl Vst3Format {
    /// Passes on what the plugin's editor changed since the last chunk, at the
    /// chunk's start. A port's last-sent value is left alone, so a port that
    /// isn't touched doesn't undo the edit (capturing the editor stores the
    /// edit in the port, which then agrees with it). Returns whether there was
    /// any.
    fn apply_edits(&mut self) -> bool {
        let (changes, edit_slots, edits_ref) = (&self.changes, &self.edit_slots, &self.edits);
        let mut any = false;
        self.edits.take(|index, value| {
            if let Some(edit) = edit_slots.get(index) {
                changes.push(edit.slot, 0, value);
                edits_ref.note_sent(index, value);
                any = true;
            }
        });
        any
    }

    /// A raw MIDI message: a mapped controller becomes a parameter change,
    /// notes become note events, and the rest goes as a legacy MIDI event.
    fn push_midi(&mut self, port: i32, time: i32, data: [u8; 3]) {
        let channel = data[0] & 0x0F;
        let (controller, value) = match data[0] & 0xF0 {
            0x90 | 0x80 => {
                let on = data[0] & 0xF0 == 0x90 && data[2] > 0;
                let velocity = f32::from(data[2]) / 127.0;
                let (channel, key) = (i16::from(channel), i16::from(data[1]));
                self.in_events.push(if on {
                    note_on(port, time, -1, channel, key, velocity)
                } else {
                    note_off(port, time, -1, channel, key, velocity)
                });
                return;
            }
            0xB0 => (data[1], f64::from(data[2]) / 127.0),
            0xD0 => (128, f64::from(data[1]) / 127.0),
            0xE0 => (
                129,
                (f64::from(data[1]) + 128.0 * f64::from(data[2])) / 16383.0,
            ),
            _ => return,
        };
        if let Some(mapped) = self
            .midi_params
            .iter()
            .find(|m| m.channel == channel && m.controller == controller)
        {
            self.changes.push(mapped.slot, time, value);
            if let Some(i) = self.slots[mapped.slot].edit_index {
                self.edits.note_sent(i, value);
            }
            return;
        }
        // Not mapped to a parameter: sent as the legacy event, which few
        // plugins read as input; without a mapping the message is mostly lost.
        let (control, value, value2) = match controller {
            129 => (129, data[1], data[2]),
            128 => (128, data[1], 0),
            c => (c, data[2], 0),
        };
        self.in_events.push(legacy_midi(
            port,
            time,
            channel,
            control,
            value as i8,
            value2 as i8,
        ));
    }
}

/// The ID of a note a plugin plays. Plugins that don't track notes by ID send
/// -1; those are told apart by channel and key, in a range IDs from real
/// counters don't reach.
fn output_note_id(id: i32, channel: i16, pitch: i16) -> NoteId {
    if id >= 0 {
        NoteId(id as u32)
    } else {
        NoteId(0x8000_0000 | (channel.clamp(0, 15) as u32) << 8 | pitch.clamp(0, 127) as u32)
    }
}

fn blank_event(bus: i32, time: i32, kind: u32) -> Event {
    Event {
        busIndex: bus,
        sampleOffset: time,
        ppqPosition: 0.0,
        flags: Event_::EventFlags_::kIsLive as u16,
        r#type: kind as u16,
        // SAFETY: every member is plain data, and the type says which one is
        // read.
        __field0: unsafe { std::mem::zeroed() },
    }
}

fn note_on(bus: i32, time: i32, id: i32, channel: i16, pitch: i16, velocity: f32) -> Event {
    let mut event = blank_event(bus, time, Event_::EventTypes_::kNoteOnEvent as u32);
    event.__field0.noteOn = NoteOnEvent {
        channel,
        pitch,
        tuning: 0.0,
        velocity,
        length: 0,
        noteId: id,
    };
    event
}

fn note_off(bus: i32, time: i32, id: i32, channel: i16, pitch: i16, velocity: f32) -> Event {
    let mut event = blank_event(bus, time, Event_::EventTypes_::kNoteOffEvent as u32);
    event.__field0.noteOff = NoteOffEvent {
        channel,
        pitch,
        velocity,
        noteId: id,
        tuning: 0.0,
    };
    event
}

fn poly_pressure(bus: i32, time: i32, id: i32, channel: i16, pitch: i16, pressure: f32) -> Event {
    let mut event = blank_event(bus, time, Event_::EventTypes_::kPolyPressureEvent as u32);
    event.__field0.polyPressure = PolyPressureEvent {
        channel,
        pitch,
        pressure,
        noteId: id,
    };
    event
}

fn note_expression(bus: i32, time: i32, id: i32, kind: u32, value: f64) -> Event {
    let mut event = blank_event(
        bus,
        time,
        Event_::EventTypes_::kNoteExpressionValueEvent as u32,
    );
    event.__field0.noteExpressionValue = NoteExpressionValueEvent {
        typeId: kind,
        noteId: id,
        value,
    };
    event
}

fn legacy_midi(bus: i32, time: i32, channel: u8, control: u8, value: i8, value2: i8) -> Event {
    let mut event = blank_event(bus, time, Event_::EventTypes_::kLegacyMIDICCOutEvent as u32);
    event.__field0.midiCCOut = LegacyMIDICCOutEvent {
        controlNumber: control,
        channel: channel as i8,
        value,
        value2,
    };
    event
}

/// What the transport looks like at the start of a chunk.
fn process_context(position: &Position) -> ProcessContext {
    use ProcessContext_::StatesAndFlags_::*;
    // SAFETY: the context is plain data; zero is "not valid" for the fields
    // not set.
    let mut context: ProcessContext = unsafe { std::mem::zeroed() };
    context.state =
        (kTempoValid | kTimeSigValid | kProjectTimeMusicValid | kBarPositionValid | kContTimeValid)
            as u32;
    if position.playing {
        context.state |= kPlaying as u32;
    }
    context.sampleRate = position.sample_rate;
    context.projectTimeSamples = position.samples as i64;
    context.continousTimeSamples = position.steady as i64;
    context.projectTimeMusic = position.beats;
    context.barPositionMusic = position.bar_start;
    context.tempo = position.bpm;
    context.timeSigNumerator = i32::from(position.signature.numerator).max(1);
    context.timeSigDenominator = i32::from(position.signature.denominator).max(1);
    context
}

impl Drop for Vst3Format {
    fn drop(&mut self) {
        // The plugin is torn down on the plugin thread, like it was made,
        // and its editor goes first.
        if let Some(key) = self.gui_key {
            crate::editor::unregister(key);
        }
        if let Some(plugin) = self.plugin.take() {
            let key = self.gui_key;
            crate::thread::run_job_detached(move || {
                if let Some(key) = key {
                    super::gui::unregister(key);
                }
                drop(plugin);
            });
        }
    }
}
