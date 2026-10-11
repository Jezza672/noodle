//! The audio side of a VST3 plugin node: the buffers, parameter changes and
//! events of each block, and the call to the plugin's `process`.
//!
//! Like the CLAP node, it runs a block in chunks of at most `max_frames`,
//! samples wired parameters every [`PARAM_STEP`] frames and hands them over
//! as timed points, and merges every note input into one time-ordered stream.

use vst3::ComWrapper;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;

use noodle_engine::{Context, Event as NoodleEvent, EventKind, Expression, Io, Node, NoteId};

use crate::descriptor::Descriptor;
use crate::node::{PARAM_STEP, param_range};

use super::classes::{Changes, DiscardChanges, Events, raw};
use super::plugin::Plugin;

/// The most events handed to a plugin per block; see the CLAP node.
const MAX_EVENTS: usize = 4096;

/// Notes remembered so a note-off can say which key it is for.
const MAX_NOTES: usize = 256;

/// A parameter the node sends: one of the plugin's (with a port) or one a MIDI
/// controller drives (without).
struct Slot {
    id: ParamID,
    /// The input's position in the layout, for a parameter with a port.
    input: Option<usize>,
    min: f32,
    max: f32,
    /// Steps of a stepped parameter (the port then holds 0 to `steps`), or 0.
    steps: i32,
    /// The last value sent, as the port has it. Starts at the default, so a
    /// parameter nobody touched leaves the plugin's own (or restored) value.
    last: f32,
}

/// A MIDI controller a parameter answers to.
struct MidiParam {
    channel: u8,
    controller: u8,
    slot: usize,
}

struct NoteKey {
    port: u16,
    note: u32,
    channel: u8,
    key: u8,
}

struct Bus {
    channels: Vec<Vec<f32>>,
    /// Pointers to `channels`' buffers, which `AudioBusBuffers` points to.
    pointers: Vec<*mut f32>,
}

impl Bus {
    fn new(channels: u32, frames: usize) -> Self {
        let mut channels: Vec<Vec<f32>> = (0..channels.max(1)).map(|_| vec![0.0; frames]).collect();
        let pointers = channels.iter_mut().map(|c| c.as_mut_ptr()).collect();
        Self { channels, pointers }
    }
}

pub(crate) struct Vst3Node {
    plugin: Option<Plugin>,
    max_frames: usize,
    audio_in: Vec<Bus>,
    audio_out: Vec<Bus>,
    /// The `AudioBusBuffers` for the buses, pointing into the buses above.
    in_buffers: Vec<AudioBusBuffers>,
    out_buffers: Vec<AudioBusBuffers>,
    changes: ComWrapper<Changes>,
    discard: ComWrapper<DiscardChanges>,
    in_events: ComWrapper<Events>,
    out_events: ComWrapper<Events>,
    slots: Vec<Slot>,
    midi_params: Vec<MidiParam>,
    notes: Vec<NoteKey>,
    cursors: Vec<usize>,
    started: bool,
    steady: i64,
}

// SAFETY: the raw pointers point into buffers the node owns, and the plugin
// is `Send`; the COM lists are only used from the thread running `process`.
unsafe impl Send for Vst3Node {}

fn buffers_for(buses: &mut [Bus]) -> Vec<AudioBusBuffers> {
    buses
        .iter_mut()
        .map(|bus| AudioBusBuffers {
            numChannels: bus.channels.len() as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: bus.pointers.as_mut_ptr(),
            },
        })
        .collect()
}

impl Vst3Node {
    pub fn new(
        plugin: Plugin,
        descriptor: &Descriptor,
        midi_assignments: &[(i16, i16, ParamID)],
        max_frames: usize,
    ) -> Self {
        let mut audio_in: Vec<Bus> = descriptor
            .audio_inputs
            .iter()
            .map(|p| Bus::new(p.channels, max_frames))
            .collect();
        let mut audio_out: Vec<Bus> = descriptor
            .audio_outputs
            .iter()
            .map(|p| Bus::new(p.channels, max_frames))
            .collect();
        let in_buffers = buffers_for(&mut audio_in);
        let out_buffers = buffers_for(&mut audio_out);

        let first_param = descriptor.audio_inputs.len();
        let mut slots: Vec<Slot> = descriptor
            .params
            .iter()
            .enumerate()
            .map(|(index, param)| {
                let (min, max, _) = param_range(param.min, param.max, param.default);
                Slot {
                    id: param.id,
                    input: Some(first_param + index),
                    min,
                    max,
                    steps: if param.stepped { max as i32 } else { 0 },
                    last: (param.default as f32).clamp(min, max),
                }
            })
            .collect();
        let mut midi_params = Vec::new();
        for &(channel, controller, id) in midi_assignments {
            // A parameter that already has a port is driven through it too.
            let slot = match slots.iter().position(|s| s.id == id) {
                Some(slot) => slot,
                None => {
                    slots.push(Slot {
                        id,
                        input: None,
                        min: 0.0,
                        max: 1.0,
                        steps: 0,
                        last: f32::NAN,
                    });
                    slots.len() - 1
                }
            };
            midi_params.push(MidiParam {
                channel: channel as u8,
                controller: controller as u8,
                slot,
            });
        }

        let ids: Vec<ParamID> = slots.iter().map(|s| s.id).collect();
        // A point at each parameter step, and the block's start and end.
        let points = max_frames / PARAM_STEP + 3;
        Self {
            plugin: Some(plugin),
            max_frames,
            audio_in,
            audio_out,
            in_buffers,
            out_buffers,
            changes: Changes::new(&ids, points + 16),
            discard: DiscardChanges::new(),
            in_events: Events::new(MAX_EVENTS),
            out_events: Events::new(MAX_EVENTS),
            slots,
            midi_params,
            notes: Vec::with_capacity(MAX_NOTES),
            cursors: vec![0; descriptor.note_inputs as usize],
            started: false,
            steady: 0,
        }
    }

    fn chunk(&mut self, ctx: &Context, io: &mut Io<'_, '_>, start: usize, n: usize) {
        for (port, bus) in self.audio_in.iter_mut().enumerate() {
            let input = &io.inputs[port];
            for (channel, buffer) in bus.channels.iter_mut().enumerate() {
                buffer[..n].copy_from_slice(&input.lane(0, channel)[start..start + n]);
            }
        }
        self.changes.clear();
        self.in_events.clear();
        self.out_events.clear();
        self.build_events(io, start, n);

        let Some(plugin) = self.plugin.as_mut() else {
            return self.silence(io, start, n);
        };
        if !self.started {
            self.started = plugin.start_processing();
        }
        if !self.started {
            return self.silence(io, start, n);
        }

        let mut context = process_context(ctx, start, self.steady);
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
            processContext: &mut context,
        };
        // SAFETY: `data` points at buffers and lists that live as long as the
        // call, which the plugin may use for its duration only.
        let result = unsafe { plugin.processor.process(&mut data) };
        self.steady += n as i64;
        if result != kResultOk {
            return self.silence(io, start, n);
        }

        for (port, bus) in self.audio_out.iter().enumerate() {
            let out = &mut io.outputs[port];
            for (channel, buffer) in bus.channels.iter().enumerate() {
                out.lane_mut(0, channel)[start..start + n].copy_from_slice(&buffer[..n]);
            }
        }
        self.emit_notes(io, start);
    }

    fn silence(&mut self, io: &mut Io<'_, '_>, start: usize, n: usize) {
        for out in io.outputs.iter_mut() {
            for channel in 0..out.shape().channels {
                out.lane_mut(0, channel)[start..start + n].fill(0.0);
            }
        }
    }

    /// Fills the chunk's parameter changes and note events, in time order.
    fn build_events(&mut self, io: &Io<'_, '_>, start: usize, n: usize) {
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
            let Some((port, time)) = pick else {
                break;
            };
            let event = &io.event_inputs[port][self.cursors[port]];
            self.cursors[port] += 1;
            let at = time - start;
            while next_step <= at {
                self.sample_params(io, start, next_step);
                next_step += PARAM_STEP;
            }
            if self.in_events.len() >= MAX_EVENTS {
                break;
            }
            self.push_event(port as i32, event, at as i32);
        }
        while next_step < n {
            self.sample_params(io, start, next_step);
            next_step += PARAM_STEP;
        }
    }

    /// Sends the parameters whose value at `start + at` differs from what the
    /// plugin has. Unmodulated ones are only looked at at `at == 0`.
    fn sample_params(&mut self, io: &Io<'_, '_>, start: usize, at: usize) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let Some(input) = slot.input else { continue };
            let input = &io.inputs[input];
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
            if slot.steps > 0 {
                value = value.round();
            }
            if value == slot.last || !value.is_finite() {
                continue;
            }
            slot.last = value;
            let normalized = if slot.steps > 0 {
                f64::from(value) / f64::from(slot.steps)
            } else {
                f64::from(value)
            };
            self.changes.push(index, at as i32, normalized);
        }
    }

    fn push_event(&mut self, port: i32, event: &NoodleEvent, time: i32) {
        match event.kind {
            EventKind::NoteOn {
                note,
                channel,
                key,
                velocity,
            } => {
                self.notes
                    .retain(|n| !(n.port == port as u16 && n.note == note.0));
                if self.notes.len() == MAX_NOTES {
                    self.notes.remove(0);
                }
                self.notes.push(NoteKey {
                    port: port as u16,
                    note: note.0,
                    channel,
                    key,
                });
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
                let (channel, key) = self.note_key(port as u16, note.0);
                self.notes
                    .retain(|n| !(n.port == port as u16 && n.note == note.0));
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
                let (channel, key) = self.note_key(port as u16, note.0);
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
            EventKind::Midi(data) => self.push_midi(port, time, data),
        }
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

    fn note_key(&self, port: u16, note: u32) -> (u8, u8) {
        self.notes
            .iter()
            .find(|n| n.port == port && n.note == note)
            .map_or((0, 0), |n| (n.channel, n.key))
    }

    /// Passes the plugin's note output on.
    fn emit_notes(&mut self, io: &mut Io<'_, '_>, start: usize) {
        if io.event_outputs.is_empty() {
            return;
        }
        for event in self.out_events.iter() {
            let time = (event.sampleOffset.max(0) as u32).saturating_add(start as u32);
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
            let port = (event.busIndex.max(0) as usize).min(io.event_outputs.len() - 1);
            let _ = io.event_outputs[port].push(NoodleEvent { time, kind });
        }
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

/// What the transport looks like `start` frames into the block.
fn process_context(ctx: &Context, start: usize, steady: i64) -> ProcessContext {
    use ProcessContext_::StatesAndFlags_::*;
    let t = &ctx.transport;
    // SAFETY: the context is plain data; zero is "not valid" for the fields
    // not set.
    let mut context: ProcessContext = unsafe { std::mem::zeroed() };
    let advance = if t.playing { start as f64 } else { 0.0 };
    let rate = f64::from(ctx.sample_rate);
    let beats = t.tick / 960.0 + advance / rate * t.bpm / 60.0;
    let numerator = i32::from(t.signature.numerator).max(1);
    context.state =
        (kTempoValid | kTimeSigValid | kProjectTimeMusicValid | kBarPositionValid | kContTimeValid)
            as u32;
    if t.playing {
        context.state |= kPlaying as u32;
    }
    context.sampleRate = rate;
    context.projectTimeSamples = t.position as i64 + advance as i64;
    context.continousTimeSamples = steady;
    context.projectTimeMusic = beats;
    context.barPositionMusic = (beats / f64::from(numerator)).floor() * f64::from(numerator);
    context.tempo = t.bpm;
    context.timeSigNumerator = numerator;
    context.timeSigDenominator = i32::from(t.signature.denominator).max(1);
    context
}

impl Node for Vst3Node {
    fn process(&mut self, ctx: &Context, mut io: Io<'_, '_>) {
        let mut start = 0;
        while start < ctx.frames {
            let n = (ctx.frames - start).min(self.max_frames);
            self.chunk(ctx, &mut io, start, n);
            start += n;
        }
    }

    fn reset(&mut self) {
        if let Some(plugin) = self.plugin.as_mut() {
            plugin.reset();
        }
        self.notes.clear();
    }
}

impl Drop for Vst3Node {
    fn drop(&mut self) {
        // The plugin is torn down on the plugin thread, like it was made.
        if let Some(plugin) = self.plugin.take() {
            crate::thread::run_job_detached(move || drop(plugin));
        }
    }
}
