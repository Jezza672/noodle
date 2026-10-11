//! A plugin as a node: its audio ports, note ports and parameters become the
//! node's ports.
//!
//! Layout (the same for every config, so a project's wires never depend on
//! whether the plugin could be loaded):
//!
//! - audio inputs `in`, `in2`, ... then one input per parameter (`p<id>`);
//! - audio outputs `out`, `out2`, ...;
//! - note inputs `notes`, `notes2`, ... and note outputs `notes_out`, ...
//!
//! A plugin is one instance with one voice, so a polyphonic signal into it
//! is refused (put a Voice Mix before it). Parameters are sampled every
//! [`PARAM_STEP`] frames while a wire modulates them, and sent to the plugin
//! as timed events, so audio-rate modulation is stepped, not smooth.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use clack_host::events::event_types::{
    MidiEvent, NoteExpressionEvent, NoteExpressionType, NoteOffEvent, NoteOnEvent, ParamValueEvent,
    TransportEvent, TransportFlags,
};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{EventFlags, EventHeader, Pckn};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, ClapId, SecondsTime};
use noodle_core::{KeyBuilder, NodeId};
use noodle_engine::{
    CacheEnv, Config, ConfigInfo, Context, Event, EventKind, Expression, Instance, Io, Layout,
    Node, NodeError, NodeInfo, NodeType, NoteId, ParamInfo, ParamKind, Setup, Shape, Uncacheable,
    Unit,
};

use crate::STATE_KEY;
use crate::descriptor::{
    Descriptor, PluginFormat, file_stamp, input_key, note_input_key, note_output_key, output_key,
    param_key,
};
use crate::host::{Flags, Host};
use crate::{editor, thread, vst3};

/// While a wire drives a parameter, it's sampled this often (in frames).
pub const PARAM_STEP: usize = 32;

/// The most events (notes and parameter changes) handed to a plugin per
/// block. The rest are dropped, so the audio thread never allocates. (The
/// events a plugin writes go to a buffer of the same size; one that writes
/// more in a block makes that buffer grow, which a plugin that busy would
/// need to be told not to do.)
const MAX_EVENTS: usize = 4096;

/// Room kept for note events: parameter changes stop this far short of
/// [`MAX_EVENTS`], so a flood of modulation can't push out a note-off and
/// leave a note stuck.
const NOTE_RESERVE: usize = 512;

/// Notes remembered so a note-off can say which key it is for.
const MAX_NOTES: usize = 256;

const NONDETERMINISTIC: ConfigInfo = ConfigInfo::int(
    "nondeterministic",
    "Uses randomness (never cached or frozen)",
    0,
);

static CONFIG: [ConfigInfo; 1] = [NONDETERMINISTIC];

/// A node type for one plugin, of either format.
pub struct PluginNodeType {
    info: &'static NodeInfo,
    path: PathBuf,
    descriptor: Descriptor,
    layout: Layout,
}

impl PluginNodeType {
    /// Leaks the names the node info needs; make one per plugin and share it.
    pub fn new(path: PathBuf, descriptor: Descriptor) -> Self {
        let leak = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
        let info = Box::leak(Box::new(NodeInfo {
            id: leak(descriptor.node_id()),
            version: 1,
            name: leak(descriptor.name.clone()),
            category: "Plugins",
        }));
        let layout = layout_of(&descriptor);
        Self {
            info,
            path,
            descriptor,
            layout,
        }
    }

    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// A parameter's range and default as the port has them: a plugin's own
/// when it makes sense, else 0 to 1.
pub(crate) fn param_range(min: f64, max: f64, default: f64) -> (f32, f32, f32) {
    let (min, max) = if min.is_finite() && max.is_finite() && max > min {
        (min as f32, max as f32)
    } else {
        (0.0, 1.0)
    };
    (min, max, (default as f32).clamp(min, max))
}

fn param_info(min: f64, max: f64, default: f64, stepped: bool) -> ParamInfo {
    let (min, max, default) = param_range(min, max, default);
    let info = ParamInfo::new(min, max, default).unit(Unit::None);
    if stepped {
        ParamInfo {
            kind: ParamKind::Stepped { labels: Vec::new() },
            ..info
        }
    } else {
        info.offset()
    }
}

fn layout_of(descriptor: &Descriptor) -> Layout {
    let mut layout = Layout::realtime();
    for (index, port) in descriptor.audio_inputs.iter().enumerate() {
        layout = layout.input(input_key(index), port.name.clone());
    }
    for param in &descriptor.params {
        let name = if param.module.is_empty() {
            param.name.clone()
        } else {
            format!("{} / {}", param.module, param.name)
        };
        layout = layout.param(
            param_key(param.id),
            name,
            param_info(param.min, param.max, param.default, param.stepped),
        );
    }
    for (index, port) in descriptor.audio_outputs.iter().enumerate() {
        layout = layout.output(output_key(index), port.name.clone());
    }
    for index in 0..descriptor.note_inputs as usize {
        layout = layout.event_input(note_input_key(index), "Notes");
    }
    for index in 0..descriptor.note_outputs as usize {
        layout = layout.event_output(note_output_key(index), "Notes out");
    }
    layout
}

impl NodeType for PluginNodeType {
    fn info(&self) -> &NodeInfo {
        self.info
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let mut layout = self.layout.clone();
        // Plugins are taken to be deterministic, so freezing and export can
        // cache them; a plugin that uses randomness opts out.
        layout.deterministic = NONDETERMINISTIC.get_int(config) == 0;
        Ok(layout)
    }

    fn cache_inputs(
        &self,
        _node: NodeId,
        _config: &Config,
        _env: &CacheEnv<'_>,
        key: &mut KeyBuilder,
    ) -> Result<(), Uncacheable> {
        // The plugin's code is part of what the output depends on. Its size
        // and modification time stand for its contents, as hashing a plugin
        // on every edit would be slow. (The saved state is in the config,
        // which is keyed already.)
        let (len, modified) = file_stamp(&self.path).ok_or_else(|| {
            Uncacheable::Because(format!(
                "can't read the plugin file {}",
                self.path.display()
            ))
        })?;
        key.str(&self.descriptor.id).u64(len).u64(modified);
        Ok(())
    }

    fn output_shapes(
        &self,
        _config: &Config,
        layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        for (index, shape) in inputs.iter().enumerate() {
            if shape.voices != 1 {
                return Err(NodeError::config(
                    "a plugin isn't polyphonic: wire a Voice Mix before it",
                ));
            }
            if let Some(port) = self.descriptor.audio_inputs.get(index)
                && shape.channels != 1
                && shape.channels != port.channels as usize
            {
                return Err(NodeError::config(format!(
                    "input {} takes {} channels, but {} are wired",
                    port.name, port.channels, shape.channels
                )));
            }
        }
        Ok(layout
            .outputs
            .iter()
            .zip(&self.descriptor.audio_outputs)
            .map(|(_, port)| Shape::new(1, port.channels.max(1) as usize))
            .collect())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let state = match setup.config.get(STATE_KEY) {
            Some(noodle_engine::Value::Text(text)) if !text.is_empty() => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .map_err(|error| NodeError::config(format!("bad plugin state: {error}")))?,
            ),
            _ => None,
        };
        if self.descriptor.format == PluginFormat::Vst3 {
            let loaded = vst3::load(
                &self.path,
                &self.descriptor.id,
                state,
                setup.sample_rate,
                setup.max_frames,
            )
            .map_err(|error| NodeError::config(error.to_string()))?;
            // The editor opens by the node, like a CLAP plugin's.
            let flags = Arc::new(Flags::default());
            let key = vst3::gui::register(&loaded.plugin, &flags);
            editor::register(setup.node, key, flags);
            return Ok(Instance::realtime(vst3::Vst3Node::new(
                loaded.plugin,
                &self.descriptor,
                &loaded.midi_assignments,
                Some(key),
                setup.max_frames,
            )));
        }
        let mut loaded = thread::start(
            &self.path,
            &self.descriptor.id,
            state,
            setup.sample_rate,
            setup.max_frames,
        )
        .map_err(|error| NodeError::config(error.to_string()))?;
        editor::register(setup.node, loaded.key, Arc::clone(&loaded.flags));
        let mut node = ClapNode::new(
            &self.descriptor,
            loaded.take_processor().into(),
            loaded.key,
            setup.max_frames,
        );
        node.latency = loaded.latency;
        Ok(Instance::realtime(node))
    }
}

/// A node type shared between registries, so a render's registry and the live
/// one build the same thing without leaking a new `NodeInfo` each time.
pub(crate) struct Shared(pub Arc<PluginNodeType>);

impl NodeType for Shared {
    fn info(&self) -> &NodeInfo {
        self.0.info()
    }
    fn config(&self) -> &[ConfigInfo] {
        self.0.config()
    }
    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        self.0.layout(config)
    }
    fn cache_inputs(
        &self,
        node: NodeId,
        config: &Config,
        env: &CacheEnv<'_>,
        key: &mut KeyBuilder,
    ) -> Result<(), Uncacheable> {
        self.0.cache_inputs(node, config, env, key)
    }
    fn output_shapes(
        &self,
        config: &Config,
        layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        self.0.output_shapes(config, layout, inputs)
    }
    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        self.0.instantiate(setup)
    }
}

struct ParamSlot {
    id: ClapId,
    /// The input's position in the layout.
    input: usize,
    min: f32,
    max: f32,
    stepped: bool,
    /// The last value sent to the plugin. NaN until the first block.
    last: f32,
}

struct PortBuffers {
    channels: Vec<Vec<f32>>,
}

impl PortBuffers {
    fn new(channels: u32, frames: usize) -> Self {
        Self {
            channels: (0..channels.max(1)).map(|_| vec![0.0; frames]).collect(),
        }
    }
}

struct NoteKey {
    port: u16,
    note: u32,
    channel: u8,
    key: u8,
}

struct ClapNode {
    /// Started on the first block, which runs on the audio thread (CLAP's
    /// `start_processing` belongs there).
    processor: Option<PluginAudioProcessor<Host>>,
    /// The shared plugin thread's key for this instance.
    key: u64,
    max_frames: usize,
    audio_in: Vec<PortBuffers>,
    audio_out: Vec<PortBuffers>,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    in_events: EventBuffer,
    out_events: EventBuffer,
    params: Vec<ParamSlot>,
    notes: Vec<NoteKey>,
    /// Where each note input's merge has got to, in `build_events`.
    cursors: Vec<usize>,
    steady: u64,
    /// The latency the plugin reported when it was activated, in samples.
    latency: u32,
}

impl ClapNode {
    fn new(
        descriptor: &Descriptor,
        processor: PluginAudioProcessor<Host>,
        key: u64,
        max_frames: usize,
    ) -> Self {
        let audio_in: Vec<_> = descriptor
            .audio_inputs
            .iter()
            .map(|p| PortBuffers::new(p.channels, max_frames))
            .collect();
        let audio_out: Vec<_> = descriptor
            .audio_outputs
            .iter()
            .map(|p| PortBuffers::new(p.channels, max_frames))
            .collect();
        let channels = |ports: &[PortBuffers]| ports.iter().map(|p| p.channels.len()).sum();
        let in_ports = AudioPorts::with_capacity(channels(&audio_in), audio_in.len());
        let out_ports = AudioPorts::with_capacity(channels(&audio_out), audio_out.len());
        let first_param = descriptor.audio_inputs.len();
        let params = descriptor
            .params
            .iter()
            .enumerate()
            .filter_map(|(index, param)| {
                let info = param_info(param.min, param.max, param.default, param.stepped);
                Some(ParamSlot {
                    id: ClapId::from_raw(param.id)?,
                    input: first_param + index,
                    min: info.min,
                    max: info.max,
                    stepped: param.stepped,
                    last: f32::NAN,
                })
            })
            .collect();
        Self {
            processor: Some(processor),
            key,
            max_frames,
            audio_in,
            audio_out,
            in_ports,
            out_ports,
            in_events: EventBuffer::with_capacity(MAX_EVENTS),
            out_events: EventBuffer::with_capacity(MAX_EVENTS),
            params,
            notes: Vec::with_capacity(MAX_NOTES),
            cursors: vec![0; descriptor.note_inputs as usize],
            steady: 0,
            latency: 0,
        }
    }

    /// Runs frames `start..start + n` of the block.
    fn chunk(&mut self, ctx: &Context, io: &mut Io<'_, '_>, start: usize, n: usize) {
        for (port, buffers) in self.audio_in.iter_mut().enumerate() {
            let input = &io.inputs[port];
            for (channel, buffer) in buffers.channels.iter_mut().enumerate() {
                buffer[..n].copy_from_slice(&input.lane(0, channel)[start..start + n]);
            }
        }
        self.build_events(io, start, n);
        let transport = transport_event(ctx, start);

        let Some(Ok(processor)) = self
            .processor
            .as_mut()
            .map(PluginAudioProcessor::ensure_processing_started)
        else {
            return self.silence(io, start, n);
        };
        let input_events = InputEvents::from_buffer(&self.in_events);
        self.out_events.clear();
        let mut output_events = OutputEvents::from_buffer(&mut self.out_events);

        let audio_in = &mut self.audio_in;
        let audio_out = &mut self.audio_out;
        let input = self
            .in_ports
            .with_input_buffers(audio_in.iter_mut().map(|port| {
                AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        port.channels
                            .iter_mut()
                            .map(|c| InputChannel::variable(&mut c[..n])),
                    ),
                }
            }));
        let mut output = self
            .out_ports
            .with_output_buffers(audio_out.iter_mut().map(|port| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    port.channels.iter_mut().map(|c| &mut c[..n]),
                ),
            }));
        let result = processor.process(
            &input,
            &mut output,
            &input_events,
            &mut output_events,
            Some(self.steady),
            Some(&transport),
        );
        self.steady += n as u64;
        if result.is_err() {
            return self.silence(io, start, n);
        }

        for (port, buffers) in self.audio_out.iter().enumerate() {
            let out = &mut io.outputs[port];
            for (channel, buffer) in buffers.channels.iter().enumerate() {
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

    /// Fills `in_events` for the chunk: parameter changes, and the chunk's
    /// share of the note events, in time order.
    fn build_events(&mut self, io: &Io<'_, '_>, start: usize, n: usize) {
        self.in_events.clear();
        self.sample_params(io, start, 0);
        let mut next_step = PARAM_STEP;
        // Every note input is read, merged into one time-ordered stream (the
        // plugin wants its events in order whatever port they are for).
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
            if self.in_events.len() as usize >= MAX_EVENTS {
                break;
            }
            self.push_note(port as u16, event, at as u32);
        }
        while next_step < n {
            self.sample_params(io, start, next_step);
            next_step += PARAM_STEP;
        }
    }

    /// Sends the parameters whose value at `start + at` differs from what the
    /// plugin has. Unmodulated ones are only looked at at `at == 0`.
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
            if self.in_events.len() as usize >= MAX_EVENTS - NOTE_RESERVE {
                return;
            }
            slot.last = value;
            self.in_events.push(&ParamValueEvent::new(
                at as u32,
                slot.id,
                Pckn::match_all(),
                f64::from(value),
            ));
        }
    }

    fn push_note(&mut self, port: u16, event: &Event, time: u32) {
        match event.kind {
            EventKind::NoteOn {
                note,
                channel,
                key,
                velocity,
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
                self.in_events.push(&NoteOnEvent::new(
                    time,
                    pckn(port, note.0, channel, key),
                    f64::from(velocity),
                ));
            }
            EventKind::NoteOff { note, velocity } => {
                let (channel, key) = self.note_key(port, note.0);
                self.notes.retain(|n| !(n.port == port && n.note == note.0));
                self.in_events.push(&NoteOffEvent::new(
                    time,
                    pckn(port, note.0, channel, key),
                    f64::from(velocity),
                ));
            }
            EventKind::Expression {
                note,
                expression,
                value,
            } => {
                let (channel, key) = self.note_key(port, note.0);
                let (kind, value) = match expression {
                    Expression::Pitch => (NoteExpressionType::Tuning, value),
                    Expression::Pressure => (NoteExpressionType::Pressure, value),
                    Expression::Timbre => (NoteExpressionType::Brightness, value),
                    Expression::Volume => (NoteExpressionType::Volume, value),
                    // Noodle's pan runs -1 to 1; CLAP's 0 to 1.
                    Expression::Pan => (NoteExpressionType::Pan, (value + 1.0) / 2.0),
                };
                self.in_events.push(&NoteExpressionEvent::new(
                    time,
                    pckn(port, note.0, channel, key),
                    kind,
                    f64::from(value),
                ));
            }
            EventKind::Midi(data) => {
                self.in_events.push(&MidiEvent::new(time, port, data));
            }
        }
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
            let time = event.header().time().saturating_add(start as u32);
            let (port, kind) = match event.as_core_event() {
                Some(CoreEventSpace::NoteOn(on)) => (
                    on.port_index().into_specific().unwrap_or(0),
                    EventKind::NoteOn {
                        note: NoteId(on.note_id().into_specific().unwrap_or(0)),
                        channel: on.channel().into_specific().unwrap_or(0) as u8,
                        key: on.key().into_specific().unwrap_or(0) as u8,
                        velocity: on.velocity() as f32,
                    },
                ),
                Some(CoreEventSpace::NoteOff(off)) => (
                    off.port_index().into_specific().unwrap_or(0),
                    EventKind::NoteOff {
                        note: NoteId(off.note_id().into_specific().unwrap_or(0)),
                        velocity: off.velocity() as f32,
                    },
                ),
                Some(CoreEventSpace::Midi(midi)) => {
                    (midi.port_index(), EventKind::Midi(midi.data()))
                }
                _ => continue,
            };
            // A port the plugin didn't declare goes to the first.
            let port = usize::from(port).min(io.event_outputs.len() - 1);
            let _ = io.event_outputs[port].push(Event { time, kind });
        }
    }
}

fn pckn(port: u16, note: u32, channel: u8, key: u8) -> Pckn {
    Pckn::new(port, u16::from(channel), u16::from(key), note)
}

/// What the transport looks like `start` frames into the block.
fn transport_event(ctx: &Context, start: usize) -> TransportEvent {
    let t = &ctx.transport;
    let mut flags = TransportFlags::HAS_TEMPO
        | TransportFlags::HAS_BEATS_TIMELINE
        | TransportFlags::HAS_SECONDS_TIMELINE
        | TransportFlags::HAS_TIME_SIGNATURE;
    if t.playing {
        flags |= TransportFlags::IS_PLAYING;
    }
    // The block's start, moved on by `start` frames while playing.
    let advance = if t.playing { start as f64 } else { 0.0 };
    let seconds = (t.position as f64 + advance) / f64::from(ctx.sample_rate);
    let beats = t.tick / 960.0 + advance / f64::from(ctx.sample_rate) * t.bpm / 60.0;
    let numerator = u16::from(t.signature.numerator);
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(beats),
        song_pos_seconds: SecondsTime::from_float(seconds),
        tempo: t.bpm,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_int(0),
        loop_end_beats: BeatTime::from_int(0),
        loop_start_seconds: SecondsTime::from_int(0),
        loop_end_seconds: SecondsTime::from_int(0),
        bar_start: BeatTime::from_float(
            (beats / f64::from(numerator.max(1))).floor() * f64::from(numerator.max(1)),
        ),
        bar_number: (beats / f64::from(numerator.max(1))).floor() as i32,
        time_signature_numerator: numerator,
        time_signature_denominator: u16::from(t.signature.denominator),
    }
}

impl Node for ClapNode {
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
        if let Some(processor) = self.processor.as_mut() {
            processor.reset();
        }
        self.notes.clear();
    }
}

impl Drop for ClapNode {
    fn drop(&mut self) {
        editor::unregister(self.key);
        if let Some(processor) = self.processor.take() {
            // Stopping belongs on the audio thread too, but a node is dropped
            // on the controller's thread; the plugin is deactivated right
            // after, which is what matters.
            thread::close(self.key, processor.into_stopped());
        }
    }
}
