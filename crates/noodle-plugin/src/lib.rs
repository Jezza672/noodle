//! Noodle as a CLAP plugin. One binary holds two plugins, **Noodle** (an
//! audio effect: Host In to Host Out) and **Noodle Instrument** (a Lead Synth
//! played by the host's notes). Each instance is a project run by a
//! [`Session`](noodle_app::session::Session), driven by the host's audio
//! callback through `noodle-io`'s host backend.
//!
//! This is the headless plugin: ports, process, state, latency and the 64
//! host parameters. The window comes later; until then a host shows its own
//! generic parameter list.
//!
//! Threads. The main thread owns the session (edits, undo, compiling the
//! graph); the audio thread owns the [`HostBackend`](noodle_io::HostBackend)
//! and calls the engine, reading the host's parameters and playing its notes
//! as they arrive. They share the engine's channels and the
//! [`HostLink`](noodle_io::HostLink) bank, and nothing else.

use std::cell::RefCell;
use std::ffi::CStr;
use std::marker::PhantomData;
use std::sync::Arc;

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    HostParams, ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, ParamRescanFlags,
    PluginAudioProcessorParams, PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::entry::prelude::*;
use clack_plugin::events::event_types::MidiEvent;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::plugin::features::{AUDIO_EFFECT, INSTRUMENT, STEREO, SYNTHESIZER};
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use noodle_app::session::host::{Flavour, HostSettings};
use noodle_app::session::{Nodes, Session};
use noodle_io::{
    HOST_PARAMS, HostBackend, HostEvents, HostLink, HostMidi, HostParamChange, HostTransport,
};
use std::io::{Read, Write};

/// What a state chunk starts with, so a chunk that isn't ours is refused
/// rather than parsed.
const STATE_MAGIC: &[u8] = b"noodle-plugin 1\n";

/// The ID of the effect, as hosts list it.
pub const EFFECT_ID: &str = "org.noodle.noodle";
/// The ID of the instrument.
pub const INSTRUMENT_ID: &str = "org.noodle.noodle-instrument";

/// The largest block of notes or parameter changes taken in one call; more
/// are dropped (and a host that sends that many is sending something else).
const MAX_EVENTS: usize = 4096;

/// The channels a plugin instance has on each audio port.
const CHANNELS: usize = 2;

/// Which of the two plugins an instance is.
pub trait Kind: 'static {
    const FLAVOUR: Flavour;
}

/// The effect: audio in, audio out, notes in.
pub struct Effect;
/// The instrument: notes in, audio out.
pub struct Instrument;

impl Kind for Effect {
    const FLAVOUR: Flavour = Flavour::Effect;
}

impl Kind for Instrument {
    const FLAVOUR: Flavour = Flavour::Instrument;
}

/// A plugin instance of kind `K`.
pub struct NoodlePlugin<K>(PhantomData<fn() -> K>);

/// Nothing is shared between the threads beyond what the session's
/// [`HostLink`] already holds.
pub struct Shared;

impl PluginShared<'_> for Shared {}

impl<K: Kind> Plugin for NoodlePlugin<K> {
    type AudioProcessor<'a> = AudioThread;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginLatency>();
    }
}

/// The main thread's half of an instance.
pub struct MainThread<'a> {
    host: HostMainThreadHandle<'a>,
    flavour: Flavour,
    session: RefCell<Session>,
    /// The names the host has been told, to see when it needs telling again.
    names: RefCell<[Option<String>; HOST_PARAMS]>,
}

impl<'a> MainThread<'a> {
    fn new(host: HostMainThreadHandle<'a>, flavour: Flavour) -> Self {
        let mut session = Session::new(Nodes::all());
        session.new_host_project(flavour);
        let names = RefCell::new(session.host_param_names());
        Self {
            host,
            flavour,
            session: RefCell::new(session),
            names,
        }
    }

    /// Tells the host if the parameters' names or which of them exist have
    /// changed since it last asked.
    fn rescan_params(&self) {
        let now = self.session.borrow().host_param_names();
        if *self.names.borrow() != now {
            *self.names.borrow_mut() = now;
            if let Some(params) = self.host.get_extension::<HostParams>() {
                params.rescan(&self.host, ParamRescanFlags::INFO | ParamRescanFlags::TEXT);
            }
        }
    }

    /// The session, for tests and the window shell.
    pub fn session(&self) -> &RefCell<Session> {
        &self.session
    }
}

impl<'a> PluginMainThread<'a, Shared> for MainThread<'a> {}

impl PluginAudioPortsImpl for MainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        // An instrument has no audio in.
        u32::from(!is_input || self.flavour == Flavour::Effect)
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 && PluginAudioPortsImpl::count(self, is_input) == 1 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(u32::from(!is_input)),
                name: if is_input { b"In" } else { b"Out" },
                channel_count: CHANNELS as u32,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}

impl PluginNotePortsImpl for MainThread<'_> {
    fn count(&self, _is_input: bool) -> u32 {
        1
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if index == 0 {
            writer.set(&NotePortInfo {
                id: ClapId::new(u32::from(!is_input)),
                name: if is_input { b"Notes in" } else { b"Notes out" },
                supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
                preferred_dialect: Some(if is_input {
                    NoteDialect::Clap
                } else {
                    NoteDialect::Midi
                }),
            });
        }
    }
}

impl PluginLatencyImpl for MainThread<'_> {
    fn get(&self) -> u32 {
        self.session.borrow().host_latency()
    }
}

impl PluginMainThreadParams for MainThread<'_> {
    fn count(&self) -> u32 {
        HOST_PARAMS as u32
    }

    fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
        let Some(slot) = usize::try_from(index).ok().filter(|&s| s < HOST_PARAMS) else {
            return;
        };
        let names = self.names.borrow();
        let name = names[slot]
            .clone()
            .unwrap_or_else(|| format!("Param {}", slot + 1));
        // A slot nothing reads is still listed, as "Param n": hosts that
        // read the list once (VST3 ones mostly) would never show a slot that
        // was hidden at load.
        let flags = ParamInfoFlags::IS_AUTOMATABLE;
        info.set(&ParamInfo {
            id: ClapId::new(index),
            flags,
            cookie: Default::default(),
            name: name.as_bytes(),
            module: b"",
            min_value: 0.0,
            max_value: 1.0,
            default_value: 0.0,
        });
    }

    fn get_value(&self, id: ClapId) -> Option<f64> {
        let slot = usize::try_from(id.get())
            .ok()
            .filter(|&s| s < HOST_PARAMS)?;
        Some(f64::from(self.session.borrow().host_link().value(slot)))
    }

    fn value_to_text(
        &self,
        _id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write;
        write!(writer, "{value:.2}")
    }

    fn text_to_value(&self, _id: ClapId, text: &CStr) -> Option<f64> {
        text.to_str().ok()?.trim().parse().ok()
    }

    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        let session = self.session.borrow();
        let link = session.host_link();
        for event in input {
            if let Some(CoreEventSpace::ParamValue(change)) = event.as_core_event()
                && let Some(id) = change.param_id()
            {
                link.set_value(id.get() as usize, change.value() as f32);
            }
        }
    }
}

impl PluginStateImpl for MainThread<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        let text = self.session.borrow().host_state();
        output.write_all(STATE_MAGIC)?;
        output.write_all(text.as_bytes())?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        let Some(body) = bytes.strip_prefix(STATE_MAGIC) else {
            return Err(PluginError::Message("not Noodle's state"));
        };
        let text = std::str::from_utf8(body)?;
        self.session
            .borrow_mut()
            .load_host_state(text)
            .map_err(|reason| PluginError::Error(reason.into()))?;
        self.rescan_params();
        Ok(())
    }
}

/// The audio thread's half of an instance.
pub struct AudioThread {
    backend: HostBackend,
    link: Arc<HostLink>,
    /// The host's blocks, planar, one block of the activation's longest.
    input: [Vec<f32>; CHANNELS],
    output: [Vec<f32>; CHANNELS],
    in_channels: usize,
    midi: Vec<HostMidi>,
    params: Vec<HostParamChange>,
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for AudioThread {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        main_thread: &MainThread<'a>,
        _shared: &'a Shared,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let in_channels = if main_thread.flavour == Flavour::Effect {
            CHANNELS
        } else {
            0
        };
        let capacity = config.max_frames_count as usize;
        let mut session = main_thread.session.borrow_mut();
        let backend = session
            .start_host_engine(HostSettings {
                sample_rate: config.sample_rate as f32,
                out_channels: CHANNELS,
                in_channels,
            })
            .map_err(|reason| PluginError::Error(reason.into()))?;
        let link = Arc::clone(session.host_link());
        Ok(Self {
            backend,
            link,
            input: std::array::from_fn(|_| vec![0.0; capacity]),
            output: std::array::from_fn(|_| vec![0.0; capacity]),
            in_channels,
            midi: Vec::with_capacity(MAX_EVENTS),
            params: Vec::with_capacity(MAX_EVENTS),
        })
    }

    fn process(
        &mut self,
        process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let capacity = self.output[0].len();
        let frames = (audio.frames_count() as usize).min(capacity);

        // The host's events, as the backend takes them.
        self.midi.clear();
        self.params.clear();
        for event in events.input {
            let time = event.header().time();
            match event.as_core_event() {
                Some(CoreEventSpace::NoteOn(note)) => {
                    self.push_note(time, 0x90, note.channel(), note.key(), note.velocity())
                }
                Some(CoreEventSpace::NoteOff(note)) => {
                    self.push_note(time, 0x80, note.channel(), note.key(), note.velocity())
                }
                Some(CoreEventSpace::NoteChoke(note)) => {
                    self.push_note(time, 0x80, note.channel(), note.key(), 0.0)
                }
                Some(CoreEventSpace::Midi(midi)) => {
                    if self.midi.len() < MAX_EVENTS {
                        self.midi.push(HostMidi {
                            offset: time,
                            bytes: midi.data(),
                        });
                    }
                }
                Some(CoreEventSpace::ParamValue(change)) => {
                    if let Some(id) = change.param_id()
                        && self.params.len() < MAX_EVENTS
                    {
                        self.params.push(HostParamChange {
                            offset: time,
                            slot: id.get() as usize,
                            value: change.value() as f32,
                        });
                    }
                }
                _ => {}
            }
        }

        // The audio in, copied so an in-place buffer can be written over.
        let mut channel = 0;
        for mut pair in &mut audio {
            let Some(channels) = pair.channels()?.into_f32() else {
                continue;
            };
            for pair in channels {
                let input = match pair {
                    ChannelPair::InputOnly(i) | ChannelPair::InputOutput(i, _) => Some(i),
                    ChannelPair::InPlace(b) => Some(&*b),
                    ChannelPair::OutputOnly(_) => None,
                };
                if let Some(input) = input
                    && channel < self.in_channels
                {
                    let n = frames.min(input.len());
                    self.input[channel][..n].copy_from_slice(&input[..n]);
                    channel += 1;
                }
            }
        }

        let transport = match process.transport {
            Some(t) => {
                use clack_plugin::events::event_types::TransportFlags;
                HostTransport {
                    playing: t.flags.contains(TransportFlags::IS_PLAYING),
                    tempo: t
                        .flags
                        .contains(TransportFlags::HAS_TEMPO)
                        .then_some(t.tempo),
                    beats: t
                        .flags
                        .contains(TransportFlags::HAS_BEATS_TIMELINE)
                        .then(|| f64::from(t.song_pos_beats)),
                }
            }
            // A host with no transport is free-running: the engine plays.
            None => HostTransport {
                playing: true,
                tempo: None,
                beats: None,
            },
        };

        let [in_left, in_right] = &self.input;
        let [out_left, out_right] = &mut self.output;
        let inputs: [&[f32]; CHANNELS] = [&in_left[..frames], &in_right[..frames]];
        let mut outputs = [&mut out_left[..frames], &mut out_right[..frames]];
        let out = events.output;
        self.backend.process(
            frames,
            &inputs[..self.in_channels],
            &mut outputs,
            HostEvents {
                midi: &self.midi,
                params: &self.params,
                transport,
            },
            &mut |message| {
                let _ = out.try_push(MidiEvent::new(message.offset, 0, message.bytes));
            },
        );

        // The audio out.
        let mut channel = 0;
        for mut pair in &mut audio {
            let Some(channels) = pair.channels()?.into_f32() else {
                continue;
            };
            for pair in channels {
                let output = match pair {
                    ChannelPair::OutputOnly(o) | ChannelPair::InputOutput(_, o) => o,
                    ChannelPair::InPlace(b) => b,
                    ChannelPair::InputOnly(_) => continue,
                };
                let n = frames.min(output.len());
                match self.output.get(channel) {
                    Some(source) => output[..n].copy_from_slice(&source[..n]),
                    None => output.fill(0.0),
                }
                channel += 1;
            }
        }
        Ok(ProcessStatus::Continue)
    }

    fn deactivate(self, main_thread: &MainThread<'a>) {
        main_thread.session.borrow_mut().stop_host_engine();
    }

    fn reset(&mut self) {}
}

impl AudioThread {
    fn push_note(
        &mut self,
        offset: u32,
        status: u8,
        channel: clack_plugin::events::Match<u16>,
        key: clack_plugin::events::Match<u16>,
        velocity: f64,
    ) {
        let (Some(key), true) = (key.into_specific(), self.midi.len() < MAX_EVENTS) else {
            return;
        };
        let channel = channel.into_specific().unwrap_or(0).min(15) as u8;
        let velocity = if status == 0x90 {
            ((velocity * 127.0).round() as u8).clamp(1, 127)
        } else {
            (velocity * 127.0).round() as u8
        };
        self.midi.push(HostMidi {
            offset,
            bytes: [status | channel, key.min(127) as u8, velocity],
        });
    }
}

impl PluginAudioProcessorParams for AudioThread {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            if let Some(CoreEventSpace::ParamValue(change)) = event.as_core_event()
                && let Some(id) = change.param_id()
            {
                self.link
                    .set_value(id.get() as usize, change.value() as f32);
            }
        }
    }
}

struct Factory {
    effect: PluginDescriptor,
    instrument: PluginDescriptor,
}

impl Factory {
    fn new() -> Self {
        Self {
            effect: PluginDescriptor::new(EFFECT_ID, "Noodle")
                .with_vendor("Noodle")
                .with_description("A node-based DAW, as an effect")
                .with_features([AUDIO_EFFECT, STEREO]),
            instrument: PluginDescriptor::new(INSTRUMENT_ID, "Noodle Instrument")
                .with_vendor("Noodle")
                .with_description("A node-based DAW, as an instrument")
                .with_features([INSTRUMENT, SYNTHESIZER, STEREO]),
        }
    }
}

impl PluginFactoryImpl for Factory {
    fn plugin_count(&self) -> u32 {
        2
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        match index {
            0 => Some(&self.effect),
            1 => Some(&self.instrument),
            _ => None,
        }
    }

    fn create_plugin<'a>(
        &'a self,
        host_info: HostInfo<'a>,
        plugin_id: &CStr,
    ) -> Option<PluginInstance<'a>> {
        fn make<'a, K: Kind>(
            host_info: HostInfo<'a>,
            descriptor: &'a PluginDescriptor,
        ) -> PluginInstance<'a> {
            PluginInstance::new::<NoodlePlugin<K>>(
                host_info,
                descriptor,
                |_host| Ok(Shared),
                |host, _shared| Ok(MainThread::new(host, K::FLAVOUR)),
            )
        }
        if plugin_id == self.effect.id()? {
            Some(make::<Effect>(host_info, &self.effect))
        } else if plugin_id == self.instrument.id()? {
            Some(make::<Instrument>(host_info, &self.instrument))
        } else {
            None
        }
    }
}

/// The entry a host loads.
pub struct NoodleEntry {
    factory: PluginFactoryWrapper<Factory>,
}

impl Entry for NoodleEntry {
    fn new(_plugin_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
        Ok(Self {
            factory: PluginFactoryWrapper::new(Factory::new()),
        })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

clack_export_entry!(NoodleEntry);
