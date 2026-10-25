//! A plugin as a node: its audio ports, note ports and parameters become the
//! node's ports.
//!
//! Layout (from the scan and the config alone, so a project's wires never
//! depend on whether the plugin could be loaded; the editor shows the note
//! inputs first):
//!
//! - audio inputs `in`, `in2`, ... then one input (`p<id>`) for each
//!   parameter the `params` setting exposes (see [`exposed_params`]);
//! - audio outputs `out`, `out2`, ...;
//! - note inputs `notes`, `notes2`, ... and note outputs `notes_out`, ...
//!
//! A plugin is one instance with one voice, so a polyphonic signal into it
//! is refused (put a Voice Mix before it). Parameters are sampled every
//! [`PARAM_STEP`] frames while a wire modulates them, and sent to the plugin
//! as timed events, so audio-rate modulation is stepped, not smooth.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

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
    CacheEnv, Config, ConfigInfo, Event, EventKind, Expression, Instance, Layout, NodeError,
    NodeInfo, NodeType, NoteId, ParamInfo, ParamKind, Setup, Shape, Uncacheable, Unit,
};

use crate::bridge::{Bridge, Buffers, Bus, Format, MAX_EVENTS, ParamSlot, Position, Status};
use crate::descriptor::{
    Descriptor, ParamDesc, PluginFormat, exposed_params, file_stamp, input_key, note_input_key,
    note_output_key, output_key, param_key,
};
use crate::host::{Flags, Host};
use crate::{PARAMS_KEY, STATE_KEY};
use crate::{au, editor, sandbox, thread, vst3};

const NONDETERMINISTIC: ConfigInfo = ConfigInfo::int(
    "nondeterministic",
    "Uses randomness (never cached or frozen)",
    0,
);

/// Which parameters get ports; see [`exposed_params`]. Chosen in the
/// properties panel.
const EXPOSED: ConfigInfo = ConfigInfo::text(PARAMS_KEY, "Exposed parameters");

static CONFIG: [ConfigInfo; 2] = [NONDETERMINISTIC, EXPOSED];

/// A node type for one plugin, of either format.
pub struct PluginNodeType {
    info: &'static NodeInfo,
    path: PathBuf,
    descriptor: Descriptor,
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
        Self {
            info,
            path,
            descriptor,
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

fn layout_of(descriptor: &Descriptor, exposed: &[&ParamDesc]) -> Layout {
    let mut layout = Layout::realtime();
    for (index, port) in descriptor.audio_inputs.iter().enumerate() {
        layout = layout.input(input_key(index), port.name.clone());
    }
    for param in exposed {
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

    fn initial_config(&self) -> Config {
        match crate::initial_params(&self.descriptor) {
            Some(text) => Config::new().with(PARAMS_KEY, noodle_engine::Value::Text(text)),
            None => Config::new(),
        }
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let exposed = exposed_params(&self.descriptor, config);
        let mut layout = layout_of(&self.descriptor, &exposed);
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
        self.instantiate_with(setup, false)
    }
}

impl PluginNodeType {
    /// Builds the node in a plugin host process if sandboxing is on (see
    /// [`crate::set_sandbox`]), else in this one. `patient` is for offline
    /// renders, which wait for a slow plugin instead of dropping a block.
    pub(crate) fn instantiate_with(
        &self,
        setup: &Setup<'_>,
        patient: bool,
    ) -> Result<Instance, NodeError> {
        match sandbox::current_command() {
            Some(command) => self.instantiate_sandboxed(setup, command, patient),
            None => self.instantiate_local(setup),
        }
    }

    fn instantiate_sandboxed(
        &self,
        setup: &Setup<'_>,
        command: Arc<sandbox::SandboxCommand>,
        patient: bool,
    ) -> Result<Instance, NodeError> {
        let layout = self.layout(setup.config)?;
        // One shape per port (a caller that gave fewer gets mono ones).
        let shapes = |shapes: &[Shape], ports: usize| -> Vec<(usize, usize)> {
            (0..ports)
                .map(|i| shapes.get(i).copied().unwrap_or(Shape::MONO))
                .map(|s| (s.voices, s.channels))
                .collect()
        };
        let params = sandbox::proxy::Params {
            command,
            plugin_path: self.path.clone(),
            descriptor: self.descriptor.clone(),
            config: setup.config.clone(),
            node: setup.node,
            sample_rate: setup.sample_rate,
            seed: setup.seed,
            position: setup.position,
            spec: sandbox::shm::Spec {
                max_frames: setup.max_frames,
                inputs: shapes(setup.input_shapes, layout.inputs.len()),
                outputs: shapes(setup.output_shapes, layout.outputs.len()),
                event_inputs: layout.event_inputs.len(),
                event_outputs: layout.event_outputs.len(),
            },
            patient,
        };
        let (node, shared) = sandbox::proxy::SandboxedNode::start(params)
            .map_err(|error| NodeError::config(format!("plugin host: {error}")))?;
        let key = sandbox::register(setup.node, &shared);
        Ok(Instance::realtime(node.with_key(key)))
    }

    /// Loads the plugin into this process.
    pub(crate) fn instantiate_local(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let state = match setup.config.get(STATE_KEY) {
            Some(noodle_engine::Value::Text(text)) if !text.is_empty() => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .map_err(|error| NodeError::config(format!("bad plugin state: {error}")))?,
            ),
            _ => None,
        };
        let exposed = exposed_params(&self.descriptor, setup.config);
        if self.descriptor.format == PluginFormat::Au {
            return self.instantiate_au(setup, state, &exposed);
        }
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
            return Ok(Instance::realtime(vst3::vst3_node(
                loaded.plugin,
                &self.descriptor,
                &exposed,
                &loaded.midi_assignments,
                Some(key),
                loaded.latency,
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
        Ok(Instance::realtime(clap_node(
            &self.descriptor,
            &exposed,
            loaded.take_processor().into(),
            loaded.key,
            loaded.latency,
            Arc::clone(&loaded.flags),
            setup.max_frames,
        )))
    }
}

impl PluginNodeType {
    /// Loads an Audio Unit into this process. (Without an editor: the unit's
    /// parameters have ports, and a node's saved state is what it holds.)
    #[cfg(target_os = "macos")]
    fn instantiate_au(
        &self,
        setup: &Setup<'_>,
        state: Option<Vec<u8>>,
        exposed: &[&ParamDesc],
    ) -> Result<Instance, NodeError> {
        let loaded = au::load(&self.descriptor, state, setup.sample_rate, setup.max_frames)
            .map_err(|error| NodeError::config(error.to_string()))?;
        Ok(Instance::realtime(au::au_node(
            loaded,
            &self.descriptor,
            exposed,
            setup.max_frames,
        )))
    }

    #[cfg(not(target_os = "macos"))]
    fn instantiate_au(
        &self,
        _setup: &Setup<'_>,
        _state: Option<Vec<u8>>,
        _exposed: &[&ParamDesc],
    ) -> Result<Instance, NodeError> {
        Err(NodeError::config(au::unsupported().to_string()))
    }
}

/// A node type shared between registries, so a render's registry and the live
/// one build the same thing without leaking a new `NodeInfo` each time.
///
/// `.1` is whether renders using it wait for a slow sandboxed plugin (see
/// `instantiate_with`).
pub(crate) struct Shared(pub Arc<PluginNodeType>, pub bool);

impl NodeType for Shared {
    fn info(&self) -> &NodeInfo {
        self.0.info()
    }
    fn config(&self) -> &[ConfigInfo] {
        self.0.config()
    }
    fn initial_config(&self) -> Config {
        self.0.initial_config()
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
        self.0.instantiate_with(setup, self.1)
    }
}

/// The CLAP half of a plugin node: how events, the transport and the audio
/// buffers are handed to the plugin (see [`crate::bridge`] for the rest).
struct ClapFormat {
    /// Started on the first block, which runs on the audio thread (CLAP's
    /// `start_processing` belongs there).
    processor: Option<PluginAudioProcessor<Host>>,
    /// The shared plugin thread's key for this instance.
    key: u64,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    in_events: EventBuffer,
    out_events: EventBuffer,
    /// The parameter behind each of the bridge's parameter slots.
    param_ids: Vec<ClapId>,
    transport: Option<TransportEvent>,
    steady: u64,
    /// The latency the plugin reported when it was activated, in samples.
    latency: u32,
    /// Set by the plugin's `request_process` and `request_flush`.
    flags: Arc<Flags>,
}

type ClapNode = Bridge<ClapFormat>;

fn clap_node(
    descriptor: &Descriptor,
    exposed: &[&ParamDesc],
    processor: PluginAudioProcessor<Host>,
    key: u64,
    latency: u32,
    flags: Arc<Flags>,
    max_frames: usize,
) -> ClapNode {
    let channels = |ports: &[crate::descriptor::AudioPort]| -> Vec<u32> {
        ports.iter().map(|p| p.channels).collect()
    };
    let buffers = Buffers::new(
        channels(&descriptor.audio_inputs),
        channels(&descriptor.audio_outputs),
        max_frames,
    );
    let count = |buses: &[Bus]| buses.iter().map(|b| b.channels.len()).sum();
    let in_ports = AudioPorts::with_capacity(count(&buffers.inputs), buffers.inputs.len());
    let out_ports = AudioPorts::with_capacity(count(&buffers.outputs), buffers.outputs.len());
    let first_param = descriptor.audio_inputs.len();
    let mut param_ids = Vec::new();
    let params = exposed
        .iter()
        .enumerate()
        .filter_map(|(index, param)| {
            let info = param_info(param.min, param.max, param.default, param.stepped);
            param_ids.push(ClapId::from_raw(param.id)?);
            Some(ParamSlot::new(
                Some(first_param + index),
                info.min,
                info.max,
                param.stepped,
                info.default,
            ))
        })
        .collect();
    let format = ClapFormat {
        processor: Some(processor),
        key,
        in_ports,
        out_ports,
        in_events: EventBuffer::with_capacity(MAX_EVENTS),
        out_events: EventBuffer::with_capacity(MAX_EVENTS),
        param_ids,
        transport: None,
        steady: 0,
        latency,
        flags,
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

impl Format for ClapFormat {
    fn begin(&mut self, _n: usize) -> bool {
        self.in_events.clear();
        self.out_events.clear();
        // The plugin wants a call (an edit in its editor), which wakes it.
        self.flags.process_requested.swap(false, Ordering::AcqRel)
    }

    fn param(&mut self, slot: usize, at: usize, value: f32) {
        self.in_events.push(&ParamValueEvent::new(
            at as u32,
            self.param_ids[slot],
            Pckn::match_all(),
            f64::from(value),
        ));
    }

    fn note(&mut self, port: usize, event: &Event, at: usize, key: Option<(u8, u8)>) {
        let (port, time) = (port as u16, at as u32);
        let (channel, key) = key.unwrap_or((0, 0));
        match event.kind {
            EventKind::NoteOn {
                note,
                channel,
                key,
                velocity,
            } => {
                self.in_events.push(&NoteOnEvent::new(
                    time,
                    pckn(port, note.0, channel, key),
                    f64::from(velocity),
                ));
            }
            EventKind::NoteOff { note, velocity } => {
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
            other @ (EventKind::Midi(_) | EventKind::Channel { .. }) => {
                if let Some(data) = other.midi1() {
                    self.in_events.push(&MidiEvent::new(time, port, data));
                }
            }
        }
    }

    fn transport(&mut self, position: &Position) {
        self.transport = Some(transport_event(position));
        self.steady = position.steady;
    }

    fn process(&mut self, n: usize, buffers: &mut Buffers) -> Status {
        let Some(Ok(processor)) = self
            .processor
            .as_mut()
            .map(PluginAudioProcessor::ensure_processing_started)
        else {
            return Status::Failed;
        };
        let input_events = InputEvents::from_buffer(&self.in_events);
        let mut output_events = OutputEvents::from_buffer(&mut self.out_events);

        let Buffers { inputs, outputs } = buffers;
        let input = self
            .in_ports
            .with_input_buffers(inputs.iter_mut().map(|bus| {
                let silent = bus.silent;
                AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        bus.channels.iter_mut().enumerate().map(move |(i, c)| {
                            // A silent lane is a constant: all zeros.
                            InputChannel::from_buffer(&mut c[..n], i < 64 && silent & (1 << i) != 0)
                        }),
                    ),
                }
            }));
        let mut output = self
            .out_ports
            .with_output_buffers(outputs.iter_mut().map(|bus| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    bus.channels.iter_mut().map(|c| &mut c[..n]),
                ),
            }));
        let result = processor.process(
            &input,
            &mut output,
            &input_events,
            &mut output_events,
            Some(self.steady),
            self.transport.as_ref(),
        );
        match result {
            Ok(ProcessStatus::Sleep) => Status::Sleep,
            Ok(ProcessStatus::ContinueIfNotQuiet) => Status::MaybeQuiet,
            Ok(ProcessStatus::Continue | ProcessStatus::Tail) => Status::Continue,
            Err(_) => Status::Failed,
        }
    }

    fn drain_note_outputs(&mut self, emit: &mut dyn FnMut(usize, u32, EventKind)) {
        for event in self.out_events.iter() {
            let time = event.header().time();
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
            emit(usize::from(port), time, kind);
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    /// Stops the plugin's processing, as CLAP asks of a host whose plugin
    /// sleeps; the next block that has something for it starts it again.
    fn sleep(&mut self) {
        if let Some(processor) = self.processor.as_mut() {
            processor.ensure_processing_stopped();
        }
    }

    fn reset(&mut self) {
        if let Some(processor) = self.processor.as_mut() {
            processor.reset();
        }
    }
}

fn pckn(port: u16, note: u32, channel: u8, key: u8) -> Pckn {
    Pckn::new(port, u16::from(channel), u16::from(key), note)
}

/// What the transport looks like at the start of a chunk.
fn transport_event(position: &Position) -> TransportEvent {
    let mut flags = TransportFlags::HAS_TEMPO
        | TransportFlags::HAS_BEATS_TIMELINE
        | TransportFlags::HAS_SECONDS_TIMELINE
        | TransportFlags::HAS_TIME_SIGNATURE;
    if position.playing {
        flags |= TransportFlags::IS_PLAYING;
    }
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(position.beats),
        song_pos_seconds: SecondsTime::from_float(position.seconds),
        tempo: position.bpm,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_int(0),
        loop_end_beats: BeatTime::from_int(0),
        loop_start_seconds: SecondsTime::from_int(0),
        loop_end_seconds: SecondsTime::from_int(0),
        bar_start: BeatTime::from_float(position.bar_start),
        bar_number: position.bar_number,
        time_signature_numerator: u16::from(position.signature.numerator),
        time_signature_denominator: u16::from(position.signature.denominator),
    }
}

impl Drop for ClapFormat {
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
