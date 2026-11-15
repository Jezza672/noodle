//! The host backend: Noodle running inside another program's audio callback.
//!
//! It stands beside the cpal stream and the virtual output as a way to drive
//! the engine, with no stream and no thread of its own. A plugin's `process`
//! calls [`HostBackend::process`] with the host's planar buffers, and the
//! backend cuts the call into engine blocks, runs the [`Processor`] on them
//! and writes the result back. It also carries what the host says besides
//! audio: notes at their sample offsets, the transport and tempo, and
//! parameter changes (see [`HostLink`]).
//!
//! Nothing here allocates, and nothing blocks except one lock of the MIDI
//! bus's subscriber list, which only the UI and plan changes also take, for
//! a moment. The engine block stays 128 whatever the host's buffer is: the
//! largest block a host will call with is only the size of the scratch the
//! backend keeps, not of the engine.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use noodle_core::{TICKS_PER_QUARTER, Tick};
use noodle_engine::{
    Bus, Controller, HOST_DEVICE, Processor, Settings, SettingsError, TransportControl,
    engine_with_workers,
};

use crate::midi::{AudioClock, MidiBus, MidiMessage, now_micros};
use crate::midi_out::MidiOutHub;
use crate::ump::Packet;

/// The MIDI port name of the host, in a MIDI In or MIDI Out node's `port`
/// setting: Host MIDI In and Host MIDI Out.
pub const HOST_MIDI_PORT: &str = HOST_DEVICE;

/// How many parameters a plugin instance gives its host: the bank a Host
/// Parameter node reads one slot of.
pub const HOST_PARAMS: usize = 64;

/// The most channels the backend takes from or gives to a host per direction.
pub const HOST_MAX_CHANNELS: usize = 8;

/// The most messages a block can hand back to the host. A MIDI Out node that
/// sends more in one block loses the rest.
const OUT_CAPACITY: usize = 1024;

/// What the engine and the host share beyond audio: the parameter bank, which
/// the host's automation writes and Host Parameter nodes read, and the host's
/// tempo and play state, which the backend writes and the UI reads. All are
/// atomics, so neither side waits for the other.
pub struct HostLink {
    values: [AtomicU32; HOST_PARAMS],
    tempo: AtomicU64,
    playing: AtomicBool,
}

impl Default for HostLink {
    fn default() -> Self {
        Self {
            values: std::array::from_fn(|_| AtomicU32::new(0.0f32.to_bits())),
            tempo: AtomicU64::new(f64::NAN.to_bits()),
            playing: AtomicBool::new(false),
        }
    }
}

impl std::fmt::Debug for HostLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostLink")
            .field("tempo", &self.tempo())
            .field("playing", &self.playing())
            .finish_non_exhaustive()
    }
}

impl HostLink {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// The value of parameter `slot` (from 0), 0 to 1. A slot past the bank
    /// is 0.
    pub fn value(&self, slot: usize) -> f32 {
        self.values
            .get(slot)
            .map_or(0.0, |v| f32::from_bits(v.load(Ordering::Relaxed)))
    }

    /// Sets parameter `slot`, clamped to 0 to 1; a value that isn't a number
    /// is ignored.
    pub fn set_value(&self, slot: usize, value: f32) {
        if let Some(cell) = self.values.get(slot)
            && value.is_finite()
        {
            cell.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }

    /// The tempo the host last gave, in quarter notes per minute.
    pub fn tempo(&self) -> Option<f64> {
        let bpm = f64::from_bits(self.tempo.load(Ordering::Relaxed));
        bpm.is_finite().then_some(bpm)
    }

    /// Whether the host's transport is playing.
    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
}

/// A MIDI message for the host or from it, at a frame of the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostMidi {
    /// The frame of the call it belongs to.
    pub offset: u32,
    pub bytes: MidiMessage,
}

/// A change of a host parameter at a frame of the block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostParamChange {
    pub offset: u32,
    /// The slot, from 0.
    pub slot: usize,
    /// The new value, 0 to 1.
    pub value: f32,
}

/// What the host says about its transport at the start of a block.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HostTransport {
    pub playing: bool,
    /// Quarter notes per minute, if the host gives it.
    pub tempo: Option<f64>,
    /// The song position at the block's first frame, in quarter notes, if the
    /// host gives it.
    pub beats: Option<f64>,
}

/// How far from where it would be (in samples) a host's song position can
/// be and still count as the same stretch of playing: hosts round their
/// positions, and a jump is anything bigger.
const JUMP_SAMPLES: f64 = 2.0;

/// What a host-driven engine is built from.
pub struct HostConfig {
    /// The host's sample rate, from its activation.
    pub sample_rate: f32,
    /// Channels the host takes from the engine. At least 1.
    pub out_channels: usize,
    /// Channels the host gives it: 0 for an instrument with no audio input.
    pub in_channels: usize,
    /// The engine's block, in frames (the processor runs this many at a
    /// time whatever the host's buffer is).
    pub block: usize,
    pub midi: MidiBus,
    pub midi_out: MidiOutHub,
    pub link: Arc<HostLink>,
    /// Worker threads that help render wide plans; 0 for none.
    pub workers: usize,
}

/// Makes an engine driven by a host: the controller compiles graphs for it,
/// the backend is called from the host's audio callback. The main bus's
/// device is [`HOST_DEVICE`], so an Output node with that device (Host Out)
/// and one with none are the same.
pub fn host_engine(config: HostConfig) -> Result<(Controller, HostBackend), SettingsError> {
    let settings = Settings {
        sample_rate: config.sample_rate,
        max_frames: config.block,
        channels: config.out_channels,
    };
    if config.in_channels > HOST_MAX_CHANNELS || config.out_channels > HOST_MAX_CHANNELS {
        return Err(SettingsError::Channels);
    }
    let (mut controller, processor) = engine_with_workers(settings, config.workers, false)?;
    controller.set_buses(vec![Bus {
        device: HOST_DEVICE.to_owned(),
        channels: config.out_channels,
    }])?;
    let clock = Arc::clone(config.midi.clock());
    let transport = controller.transport();
    // The host starts and stops the transport; until it says, nothing plays.
    transport.stop();
    let backend = HostBackend {
        processor,
        settings,
        input: vec![0.0; config.block * config.in_channels].into_boxed_slice(),
        output: vec![0.0; config.block * config.out_channels].into_boxed_slice(),
        in_channels: config.in_channels,
        midi: config.midi,
        hub: config.midi_out,
        clock,
        transport,
        link: config.link,
        frames_done: 0,
        expected_beats: None,
        was_playing: None,
        outgoing: Vec::with_capacity(OUT_CAPACITY),
    };
    Ok((controller, backend))
}

/// The audio thread's end of a host-driven engine. See the module's
/// documentation.
pub struct HostBackend {
    processor: Processor,
    settings: Settings,
    /// One engine block of interleaved host input.
    input: Box<[f32]>,
    /// One engine block of interleaved output.
    output: Box<[f32]>,
    in_channels: usize,
    midi: MidiBus,
    hub: MidiOutHub,
    clock: Arc<AudioClock>,
    transport: Arc<TransportControl>,
    link: Arc<HostLink>,
    /// Frames run since the backend was made, the time MIDI out is placed on.
    frames_done: u64,
    /// Where the host's song position should be at the next call if it
    /// carries on, in quarter notes.
    expected_beats: Option<f64>,
    was_playing: Option<bool>,
    /// MIDI out messages waiting for their frame: `(frame, message)`.
    outgoing: Vec<(u64, HostMidi)>,
}

/// What the host gives a block of audio besides the audio: events sorted by
/// their offsets, and the transport.
#[derive(Clone, Copy)]
pub struct HostEvents<'a> {
    pub midi: &'a [HostMidi],
    pub params: &'a [HostParamChange],
    pub transport: HostTransport,
}

impl HostBackend {
    /// What the engine was made with.
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// The engine's transport, the same one the controller holds.
    pub fn transport(&self) -> &Arc<TransportControl> {
        &self.transport
    }

    /// Runs `frames` frames. `inputs` holds the host's input channels and
    /// `outputs` the channels to fill, each at least `frames` long; output
    /// channels the engine doesn't render are silent, and engine channels
    /// the host has no room for are dropped. Anything that isn't a number
    /// becomes silence.
    ///
    /// `midi_out` is called, in order, for every message a MIDI Out node on
    /// the [`HOST_MIDI_PORT`] sends, with the frame it falls on.
    pub fn process(
        &mut self,
        frames: usize,
        inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        events: HostEvents<'_>,
        midi_out: &mut dyn FnMut(HostMidi),
    ) {
        let rate = f64::from(self.settings.sample_rate);
        let engine_channels = self.settings.channels;
        let block = self.settings.max_frames;
        let in_channels = self.in_channels.min(inputs.len());
        let called = now_micros();
        self.follow(&events.transport, frames);
        self.clock.publish_anchor(called, self.transport.position());

        let mut done = 0usize;
        let mut next_midi = 0usize;
        let mut next_param = 0usize;
        while done < frames {
            // A chunk ends at the engine's block, the end of the call, or the
            // next parameter change, so a change lands on its frame.
            let mut end = (done + block).min(frames);
            while let Some(change) = events.params.get(next_param) {
                let at = change.offset as usize;
                if at <= done {
                    self.link.set_value(change.slot, change.value);
                    next_param += 1;
                } else {
                    end = end.min(at);
                    break;
                }
            }
            let n = end - done;
            let start_micros = called + (done as f64 * 1e6 / rate) as u64;
            // The chunk plays exactly the arrivals stamped inside it.
            self.clock.set_window(start_micros);
            self.clock.set_block(start_micros);
            while let Some(message) = events.midi.get(next_midi) {
                if message.offset as usize >= end {
                    break;
                }
                let local = (message.offset as usize).saturating_sub(done);
                // The middle of the frame, so reading the frame back out of
                // the time can't round down to the one before.
                let arrival = start_micros + ((local as f64 + 0.5) * 1e6 / rate) as u64;
                self.midi
                    .send_host(Packet::midi1(0, message.bytes), arrival);
                next_midi += 1;
            }

            let out = &mut self.output[..n * engine_channels];
            if in_channels > 0 {
                let input = &mut self.input[..n * in_channels];
                for (c, channel) in inputs.iter().take(in_channels).enumerate() {
                    for (frame, &x) in channel[done..end].iter().enumerate() {
                        input[frame * in_channels + c] = if x.is_finite() { x } else { 0.0 };
                    }
                }
                self.processor.process_with_input(input, in_channels, out);
            } else {
                self.processor.process(out);
            }
            for (c, channel) in outputs.iter_mut().enumerate() {
                let target = &mut channel[done..end];
                if c < engine_channels {
                    for (frame, slot) in target.iter_mut().enumerate() {
                        let x = out[frame * engine_channels + c];
                        *slot = if x.is_finite() { x } else { 0.0 };
                    }
                } else {
                    target.fill(0.0);
                }
            }

            // What the MIDI Out nodes sent, held until its frame.
            let outgoing = &mut self.outgoing;
            let at = self.frames_done;
            self.hub.drain_host(&mut |message| {
                let into = message.due.saturating_sub(start_micros) as f64 * rate / 1e6;
                if outgoing.len() < OUT_CAPACITY {
                    // Microseconds are coarser than a frame's fraction, so round.
                    outgoing.push((at + into.round() as u64, message_of(message)));
                }
            });
            self.outgoing.sort_unstable_by_key(|(frame, _)| *frame);
            let until = self.frames_done + n as u64;
            let ready = self
                .outgoing
                .iter()
                .take_while(|(frame, _)| *frame < until)
                .count();
            for (frame, message) in self.outgoing.drain(..ready) {
                midi_out(HostMidi {
                    offset: (done as u64 + frame.saturating_sub(self.frames_done)) as u32,
                    bytes: message.bytes,
                });
            }
            self.frames_done = until;
            done = end;
        }
        // Whatever of the call's last stretch the parameter list still holds.
        for change in &events.params[next_param..] {
            self.link.set_value(change.slot, change.value);
        }
        for channel in outputs.iter_mut() {
            if channel.len() > frames {
                channel[frames..].fill(0.0);
            }
        }
    }

    /// Plays and stops with the host, takes its tempo, and moves the playhead
    /// when its song position isn't where it would be if the last block had
    /// carried on: a seek, a loop's wrap or a locate.
    fn follow(&mut self, host: &HostTransport, frames: usize) {
        self.link
            .tempo
            .store(host.tempo.unwrap_or(f64::NAN).to_bits(), Ordering::Relaxed);
        self.link.playing.store(host.playing, Ordering::Relaxed);
        self.processor.set_host_tempo(host.tempo);
        if self.was_playing != Some(host.playing) {
            if host.playing {
                self.transport.play();
            } else {
                self.transport.stop();
            }
            self.was_playing = Some(host.playing);
        }
        let rate = f64::from(self.settings.sample_rate);
        let bpm = host.tempo.unwrap_or(120.0);
        if let Some(beats) = host.beats {
            let jumped = match self.expected_beats {
                Some(expected) => {
                    (beats - expected).abs() * 60.0 * rate / bpm.max(1.0) > JUMP_SAMPLES
                }
                None => true,
            };
            if jumped {
                self.processor
                    .start_at(Tick((beats * TICKS_PER_QUARTER as f64).round() as i64));
            }
            self.expected_beats = Some(if host.playing {
                beats + frames as f64 * bpm / 60.0 / rate
            } else {
                beats
            });
        } else {
            self.expected_beats = None;
        }
    }
}

fn message_of(message: crate::midi_out::OutMessage) -> HostMidi {
    let mut bytes = [0u8; 3];
    for (slot, &b) in bytes.iter_mut().zip(message.bytes()) {
        *slot = b;
    }
    HostMidi { offset: 0, bytes }
}
