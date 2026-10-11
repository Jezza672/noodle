//! A small CLAP plugin for the tests to load.
//!
//! - Audio: stereo in, stereo out. The output is the input times `Gain`
//!   (inverted if `Invert` is on), plus a sine for each held note, plus
//!   `bias`.
//! - Notes: one note input port; a note-on starts a sine at the note's pitch.
//! - Parameters: `Gain` (continuous) and `Invert` (stepped).
//! - Editor: a floating "window" that draws nothing. While it is shown, a
//!   timer (host timer support) plays the user: the first tick turns `Gain`
//!   down to 0.25, as if a knob had been dragged. That makes the editor, the
//!   host's timers and capturing parameters from a live plugin testable.
//! - State: `bias`, a number that is not a parameter and so can only come back
//!   through saved state.

use std::cell::Cell;
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicU32, Ordering};

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, PluginAudioPorts, PluginAudioPortsImpl,
};
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, PluginGui, PluginGuiImpl, Window,
};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_extensions::timer::{HostTimer, PluginTimer, PluginTimerImpl, TimerId};
use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use std::io::{Read, Write};

const GAIN: ClapId = ClapId::new(1);
const INVERT: ClapId = ClapId::new(2);

pub struct TestPlugin;

/// Shared between the main thread and the audio thread: the parameter values
/// and `bias`, as f32 bits.
pub struct Shared {
    gain: AtomicU32,
    invert: AtomicU32,
    bias: AtomicU32,
}

impl Plugin for TestPlugin {
    type AudioProcessor<'a> = Processor<'a>;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginGui>()
            .register::<PluginTimer>();
    }
}

impl DefaultPluginFactory for TestPlugin {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::*;
        PluginDescriptor::new("noodle.test.gain-sine", "Noodle Test Plugin")
            .with_vendor("Noodle")
            .with_features([INSTRUMENT, AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
        Ok(Shared {
            gain: AtomicU32::new(1.0f32.to_bits()),
            invert: AtomicU32::new(0.0f32.to_bits()),
            bias: AtomicU32::new(0.0f32.to_bits()),
        })
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a Shared,
    ) -> Result<MainThread<'a>, PluginError> {
        Ok(MainThread {
            host,
            shared,
            gui: Cell::new(Gui::None),
            timer: Cell::new(None),
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Gui {
    None,
    Created,
    Shown,
}

pub struct MainThread<'a> {
    host: HostMainThreadHandle<'a>,
    shared: &'a Shared,
    gui: Cell<Gui>,
    timer: Cell<Option<TimerId>>,
}

impl PluginGuiImpl for MainThread<'_> {
    fn is_api_supported(&self, configuration: GuiConfiguration) -> bool {
        configuration.is_floating && configuration.api_type == native_api()
    }

    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        Some(GuiConfiguration {
            api_type: native_api(),
            is_floating: true,
        })
    }

    fn create(&self, _configuration: GuiConfiguration) -> Result<(), PluginError> {
        self.gui.set(Gui::Created);
        Ok(())
    }

    fn destroy(&self) {
        self.hide_editor();
        self.gui.set(Gui::None);
    }

    fn set_scale(&self, _scale: f64) -> Result<(), PluginError> {
        Ok(())
    }

    fn get_size(&self) -> Option<GuiSize> {
        Some(GuiSize {
            width: 200,
            height: 100,
        })
    }

    fn set_size(&self, _size: GuiSize) -> Result<(), PluginError> {
        Ok(())
    }

    fn set_parent(&self, _window: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("floating only"))
    }

    fn set_transient(&self, _window: Window) -> Result<(), PluginError> {
        Ok(())
    }

    fn show(&self) -> Result<(), PluginError> {
        if self.gui.get() == Gui::None {
            return Err(PluginError::Message("not created"));
        }
        self.gui.set(Gui::Shown);
        if self.timer.get().is_none()
            && let Some(timer) = self.host.get_extension::<HostTimer>()
        {
            self.timer.set(timer.register_timer(&self.host, 20).ok());
        }
        Ok(())
    }

    fn hide(&self) -> Result<(), PluginError> {
        self.hide_editor();
        Ok(())
    }
}

impl MainThread<'_> {
    fn hide_editor(&self) {
        if let (Some(id), Some(timer)) = (self.timer.take(), self.host.get_extension::<HostTimer>())
        {
            let _ = timer.unregister_timer(&self.host, id);
        }
        if self.gui.get() == Gui::Shown {
            self.gui.set(Gui::Created);
        }
    }
}

impl PluginTimerImpl for MainThread<'_> {
    fn on_timer(&self, _timer_id: TimerId) {
        if self.gui.get() == Gui::Shown {
            // The user drags the Gain knob.
            self.shared.gain.store(0.25f32.to_bits(), Ordering::Relaxed);
        }
    }
}

fn native_api() -> GuiApiType<'static> {
    if cfg!(windows) {
        GuiApiType::WIN32
    } else if cfg!(target_os = "macos") {
        GuiApiType::COCOA
    } else {
        GuiApiType::X11
    }
}

impl PluginShared<'_> for Shared {}

impl<'a> PluginMainThread<'a, Shared> for MainThread<'a> {}

impl PluginAudioPortsImpl for MainThread<'_> {
    fn count(&self, _is_input: bool) -> u32 {
        1
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(if is_input { 0 } else { 1 }),
                name: b"main",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(clack_extensions::audio_ports::AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}

impl PluginNotePortsImpl for MainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        u32::from(is_input)
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if index == 0 && is_input {
            writer.set(&NotePortInfo {
                id: ClapId::new(0),
                name: b"notes",
                supported_dialects: NoteDialects::CLAP,
                preferred_dialect: Some(NoteDialect::Clap),
            });
        }
    }
}

impl PluginMainThreadParams for MainThread<'_> {
    fn count(&self) -> u32 {
        2
    }

    fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
        match index {
            0 => info.set(&ParamInfo {
                id: GAIN,
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Default::default(),
                name: b"Gain",
                module: b"",
                min_value: 0.0,
                max_value: 2.0,
                default_value: 1.0,
            }),
            1 => info.set(&ParamInfo {
                id: INVERT,
                flags: ParamInfoFlags::IS_AUTOMATABLE | ParamInfoFlags::IS_STEPPED,
                cookie: Default::default(),
                name: b"Invert",
                module: b"",
                min_value: 0.0,
                max_value: 1.0,
                default_value: 0.0,
            }),
            _ => {}
        }
    }

    fn get_value(&self, id: ClapId) -> Option<f64> {
        let cell = match id {
            GAIN => &self.shared.gain,
            INVERT => &self.shared.invert,
            _ => return None,
        };
        Some(f64::from(f32::from_bits(cell.load(Ordering::Relaxed))))
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

    fn text_to_value(&self, _id: ClapId, text: &std::ffi::CStr) -> Option<f64> {
        text.to_str().ok()?.parse().ok()
    }

    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            apply(self.shared, event);
        }
    }
}

fn apply(shared: &Shared, event: &UnknownEvent) {
    if let Some(CoreEventSpace::ParamValue(event)) = event.as_core_event() {
        let cell = match event.param_id() {
            Some(GAIN) => &shared.gain,
            Some(INVERT) => &shared.invert,
            _ => return,
        };
        cell.store((event.value() as f32).to_bits(), Ordering::Relaxed);
    }
}

impl PluginStateImpl for MainThread<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        let bias = f32::from_bits(self.shared.bias.load(Ordering::Relaxed));
        output.write_all(&bias.to_le_bytes())?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut bytes = [0u8; 4];
        input.read_exact(&mut bytes)?;
        self.shared
            .bias
            .store(f32::from_le_bytes(bytes).to_bits(), Ordering::Relaxed);
        Ok(())
    }
}

struct Voice {
    key: u16,
    phase: f32,
    step: f32,
    level: f32,
}

const VOICES: usize = 8;

pub struct Processor<'a> {
    shared: &'a Shared,
    sample_rate: f32,
    voices: [Option<Voice>; VOICES],
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for Processor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &MainThread<'a>,
        shared: &'a Shared,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            shared,
            sample_rate: config.sample_rate as f32,
            voices: Default::default(),
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        const MAX: usize = 512;
        let frames = (audio.frames_count() as usize).min(MAX);
        let bias = f32::from_bits(self.shared.bias.load(Ordering::Relaxed));

        // One pass over the block: events take effect at their frame, and
        // each frame gets its gain, sign and synth sample.
        let mut gains = [0.0f32; MAX];
        let mut signs = [0.0f32; MAX];
        let mut synth = [0.0f32; MAX];
        let mut pending = events.input.into_iter().peekable();
        for i in 0..frames {
            while let Some(event) = pending.next_if(|e| e.header().time() as usize <= i) {
                apply(self.shared, event);
                self.note_event(event);
            }
            let gain = f32::from_bits(self.shared.gain.load(Ordering::Relaxed));
            // Exactly 1: a host must round a stepped value before sending it.
            let invert = f32::from_bits(self.shared.invert.load(Ordering::Relaxed)) == 1.0;
            gains[i] = gain;
            signs[i] = if invert { -1.0 } else { 1.0 };
            let mut sum = 0.0;
            for voice in self.voices.iter_mut().flatten() {
                sum += (voice.phase * TAU).sin() * 0.2 * voice.level;
                voice.phase = (voice.phase + voice.step).fract();
            }
            synth[i] = sum;
        }
        // Events past the last frame still count.
        for event in pending {
            apply(self.shared, event);
            self.note_event(event);
        }

        for mut pair in &mut audio {
            let Some(channels) = pair.channels()?.into_f32() else {
                continue;
            };
            for channel in channels {
                match channel {
                    ChannelPair::InputOnly(_) => {}
                    ChannelPair::OutputOnly(out) => {
                        for (i, o) in out.iter_mut().enumerate().take(frames) {
                            *o = synth[i] * gains[i] + bias;
                        }
                    }
                    ChannelPair::InputOutput(input, out) => {
                        for (i, (o, x)) in out.iter_mut().zip(input).enumerate().take(frames) {
                            *o = (x * signs[i] + synth[i]) * gains[i] + bias;
                        }
                    }
                    ChannelPair::InPlace(buffer) => {
                        for (i, b) in buffer.iter_mut().enumerate().take(frames) {
                            *b = (*b * signs[i] + synth[i]) * gains[i] + bias;
                        }
                    }
                }
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

impl Processor<'_> {
    fn note_event(&mut self, event: &UnknownEvent) {
        if let Some(on) = event.as_event::<NoteOnEvent>() {
            let key = on.key().into_specific().unwrap_or(0);
            let freq = 440.0 * 2f32.powf((f32::from(key) - 69.0) / 12.0);
            let voice = Voice {
                key,
                phase: 0.0,
                step: freq / self.sample_rate,
                level: on.velocity() as f32,
            };
            if let Some(slot) = self.voices.iter_mut().find(|v| v.is_none()) {
                *slot = Some(voice);
            }
        } else if let Some(off) = event.as_event::<NoteOffEvent>() {
            let key = off.key().into_specific();
            for slot in &mut self.voices {
                if slot.as_ref().is_some_and(|v| Some(v.key) == key) {
                    *slot = None;
                }
            }
        }
    }
}

impl PluginAudioProcessorParams for Processor<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for event in input {
            apply(self.shared, event);
        }
    }
}

clack_export_entry!(SinglePluginEntry<TestPlugin>);
