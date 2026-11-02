//! [`MidiControl`]: a MIDI controller, pitch bend or channel pressure as a
//! signal, at the resolution the message came in: 32 bits from MIDI 2.0, 14
//! from a MIDI 1.0 controller pair or pitch bend.
//!
//! It reads its events from a wire, or, with a `port` set, straight from a
//! MIDI input port (or every open port), which is how MIDI learn makes it.
//! The value can be absolute or a relative encoder's turns, can wait for the
//! knob to reach it before following (pickup), and is mapped onto `min` to
//! `max` like the Automation node.

use noodle_engine::{
    ChannelControl, Config, ConfigInfo, Context, Event, EventKind, EventsOut, Instance, Io, Layout,
    Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports, Setup,
};
use noodle_io::MidiBus;

use crate::midi_in::{LiveMidi, MIDI_PORT};

/// Registered by [`register_library`](crate::register_library). Without a
/// bus (in tests of the plain node) it has no live port.
#[derive(Default)]
pub struct MidiControl {
    bus: Option<MidiBus>,
}

impl MidiControl {
    pub fn new(bus: &MidiBus) -> Self {
        Self {
            bus: Some(bus.clone()),
        }
    }
}

pub const MIDI_CONTROL_ID: &str = "noodle.event.control";

/// `control` values past the 128 controllers.
pub const PITCH_BEND: f32 = 128.0;
pub const PRESSURE: f32 = 129.0;

/// How far `min` and `max` can reach, in either direction.
pub const RANGE_LIMIT: f32 = 1.0e6;

/// The keys of the parameters Learn MIDI sets.
pub const CONTROL_CONTROL: &str = "control";
pub const CONTROL_MIN: &str = "min";
pub const CONTROL_MAX: &str = "max";
pub const CONTROL_TAPER: &str = "taper";
pub const CONTROL_START: &str = "start";

/// The encoder codings, as the `encoder` parameter's choices: absolute, and
/// the three common ways an endless encoder sends its turns on a 7-bit
/// controller.
pub const ENCODER_ABSOLUTE: f32 = 0.0;
/// 1 to 63 is clockwise by that many, 127 down to 65 anticlockwise by
/// 128 minus the value.
pub const ENCODER_TWOS_COMPLEMENT: f32 = 1.0;
/// Bit 6 is the sign and the low six bits the amount.
pub const ENCODER_SIGN_MAGNITUDE: f32 = 2.0;
/// 64 is no turn; above is clockwise, below anticlockwise.
pub const ENCODER_BINARY_OFFSET: f32 = 3.0;

/// How much of the 0 to 1 travel one step of a relative encoder moves.
const STEP: f32 = 1.0 / 128.0;

/// A knob this close to the held value has reached it, for pickup.
const PICKUP_WINDOW: f32 = 1.5 / 127.0;

#[derive(Ports)]
struct ControlPorts {
    #[event_input("in", "Events")]
    events: (),
    /// A controller number (0 to 127), 128 for pitch bend or 129 for channel
    /// pressure. Controllers 32 to 63 are the low halves of 0 to 31 and are
    /// read through those.
    #[param("control", "Control", ParamInfo::new(0.0, 129.0, 1.0).smoothing(0.0))]
    control: (),
    /// 0 reads every channel; 1 to 16 only that channel.
    #[param("channel", "Channel", ParamInfo::new(0.0, 16.0, 0.0).smoothing(0.0))]
    channel: (),
    /// How long the output takes to reach a new value, so a 7-bit controller
    /// doesn't zipper.
    #[param("glide", "Glide", ParamInfo::new(0.0, 500.0, 5.0).smoothing(0.0))]
    glide: (),
    /// How the controller's value is read: itself, or turns of an endless
    /// encoder that move the held value.
    #[param(
        "encoder",
        "Encoder",
        ParamInfo::choice(["Absolute", "Relative (two's complement)", "Relative (sign-magnitude)", "Relative (binary offset)"])
    )]
    encoder: (),
    /// Jump follows the knob at once; Pick up ignores it until it passes the
    /// held value, so a knob that isn't where the value is can't make it
    /// leap. (Absolute only.)
    #[param("pickup", "Pickup", ParamInfo::choice(["Jump", "Pick up"]))]
    pickup: (),
    /// Where the value stands until the knob first moves it, 0 to 1 along
    /// the travel. Learn MIDI copies it from the parameter it wires to.
    #[param("start", "Start", ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0))]
    start: (),
    #[param("min", "Min", ParamInfo::new(-RANGE_LIMIT, RANGE_LIMIT, 0.0).smoothing(0.0))]
    min: (),
    #[param("max", "Max", ParamInfo::new(-RANGE_LIMIT, RANGE_LIMIT, 1.0).smoothing(0.0))]
    max: (),
    #[param("taper", "Taper", ParamInfo::choice(["Linear", "Log"]))]
    taper: (),
    /// 0 to 1, or -1 to 1 for pitch bend, mapped onto `min` to `max` unless
    /// that is the default 0 to 1.
    #[control_output("out", "Value")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: MIDI_CONTROL_ID,
    version: 1,
    name: "MIDI Control",
    category: "Events",
};

impl NodeType for MidiControl {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        std::slice::from_ref(&MIDI_PORT)
    }

    /// With a live port it reads what the user plays, so no two runs match.
    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let layout = ControlPorts::layout();
        Ok(if MIDI_PORT.get_text(config).is_empty() {
            layout
        } else {
            layout.nondeterministic()
        })
    }

    /// A live one has to take its messages off the bus every block.
    fn always_runs(&self) -> bool {
        true
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let port = MIDI_PORT.get_text(setup.config);
        // An empty port reads the events wire only; `*` is every port.
        let live = match &self.bus {
            Some(bus) if !port.is_empty() => Some(LiveMidi::new(bus, &port, None, LIVE_ID)),
            _ => None,
        };
        Ok(Instance::realtime(ControlNode {
            live,
            buffer: Vec::with_capacity(setup.max_frames.max(256) * 2),
            ..ControlNode::default()
        }))
    }
}

/// The note-ID base of the live reader, which makes no notes that matter.
const LIVE_ID: u32 = 0x8000_0000;

/// What an event meant for the node.
enum Reading {
    /// The controller's value, 0 to 1 (-1 to 1 for pitch bend).
    Absolute(f32),
    /// A turn of a relative encoder, in steps (negative anticlockwise).
    Turn(f32),
}

#[derive(Default)]
struct ControlNode {
    /// The high and low 7 bits of controllers 0 to 31 (MIDI 1.0 pairs).
    msb: [[u8; 32]; 16],
    lsb: [[u8; 32]; 16],
    /// Which pairs have sent a low half; until one does, the high half
    /// alone is the 7-bit value.
    has_lsb: [u32; 16],
    /// What the output heads for, and what it has reached.
    target: f32,
    current: f32,
    /// Whether the knob has moved the value yet. Until it has, the value is
    /// the `start` parameter.
    touched: bool,
    /// The last value the knob sent while it was being waited for (pickup).
    last_in: Option<f32>,
    /// The messages a live port brought this block.
    live: Option<LiveMidi>,
    buffer: Vec<Event>,
}

impl ControlNode {
    /// What an event says for `control` and `channel` (0 for any), if it is
    /// for them. With a relative `encoder`, a 7-bit controller is a turn.
    fn read(
        &mut self,
        kind: &EventKind,
        control: f32,
        want_channel: u8,
        encoder: f32,
    ) -> Option<Reading> {
        let unit32 = |v: u32| (f64::from(v) / f64::from(u32::MAX)) as f32;
        let (message_channel, which, value) = match *kind {
            EventKind::Channel {
                channel,
                control: ChannelControl::Controller(index),
                value,
            } => (channel, f32::from(index), unit32(value)),
            EventKind::Channel {
                channel,
                control: ChannelControl::PitchBend,
                value,
            } => {
                let signed = ((f64::from(value) - 2_147_483_648.0) / 2_147_483_648.0) as f32;
                (channel, PITCH_BEND, signed)
            }
            EventKind::Channel {
                channel,
                control: ChannelControl::Pressure,
                value,
            } => (channel, PRESSURE, unit32(value)),
            EventKind::Midi([status, a, b]) => {
                let channel = status & 15;
                let c = usize::from(channel);
                match status & 0xF0 {
                    0xB0 => {
                        let index = usize::from(a & 127);
                        let value = f32::from(b & 127) / 127.0;
                        if encoder >= ENCODER_TWOS_COMPLEMENT - 0.5 {
                            // A relative encoder's controller is its own,
                            // not half of a pair.
                            let wanted = (want_channel == 0 || channel + 1 == want_channel)
                                && index as f32 == control;
                            if wanted {
                                let turn = decode_turn(b & 127, encoder);
                                return Some(Reading::Turn(turn));
                            }
                        }
                        if index < 32 {
                            self.msb[c][index] = b & 127;
                            // A new high half starts its low half over.
                            self.lsb[c][index] = 0;
                        } else if index < 64 {
                            self.lsb[c][index - 32] = b & 127;
                            self.has_lsb[c] |= 1 << (index - 32);
                        }
                        // The pair's value, for the high half's controller
                        // whichever half moved.
                        let high = if index < 32 {
                            Some(index)
                        } else if index < 64 {
                            Some(index - 32)
                        } else {
                            None
                        };
                        if let Some(h) = high {
                            let v = if self.has_lsb[c] & (1 << h) != 0 {
                                f32::from(
                                    u16::from(self.msb[c][h]) << 7 | u16::from(self.lsb[c][h]),
                                ) / 16_383.0
                            } else {
                                f32::from(self.msb[c][h]) / 127.0
                            };
                            (channel, h as f32, v)
                        } else {
                            (channel, index as f32, value)
                        }
                    }
                    0xE0 => {
                        let bend = f32::from(u16::from(b & 127) << 7 | u16::from(a & 127));
                        (channel, PITCH_BEND, ((bend - 8192.0) / 8192.0).min(1.0))
                    }
                    0xD0 => (channel, PRESSURE, f32::from(a & 127) / 127.0),
                    _ => return None,
                }
            }
            _ => return None,
        };
        // A controller 32 to 63 is read as the low half of its pair.
        let wanted = (want_channel == 0 || message_channel + 1 == want_channel)
            && (which - control).abs() < 0.5;
        wanted.then_some(Reading::Absolute(value))
    }

    /// Takes a reading into the held value.
    fn take(&mut self, reading: Reading, pickup: bool) {
        match reading {
            Reading::Turn(steps) => {
                self.target = (self.target + steps * STEP).clamp(0.0, 1.0);
                self.touched = true;
            }
            Reading::Absolute(value) => {
                if pickup && !self.touched {
                    let held = self.target;
                    let reached = (value - held).abs() <= PICKUP_WINDOW
                        || self
                            .last_in
                            .is_some_and(|before| (before - held) * (value - held) <= 0.0);
                    self.last_in = Some(value);
                    if !reached {
                        return;
                    }
                }
                self.last_in = None;
                self.touched = true;
                self.target = value;
            }
        }
    }
}

/// The turn a 7-bit relative controller value stands for, in steps.
fn decode_turn(value: u8, encoder: f32) -> f32 {
    let v = i32::from(value & 127);
    let steps = if encoder < ENCODER_TWOS_COMPLEMENT + 0.5 {
        if v < 64 { v } else { v - 128 }
    } else if encoder < ENCODER_SIGN_MAGNITUDE + 0.5 {
        if v & 0x40 != 0 { -(v & 0x3F) } else { v & 0x3F }
    } else {
        v - 64
    };
    steps as f32
}

/// `value` mapped onto `min` to `max`; the default 0 to 1 passes it as it is.
/// Pitch bend (`signed`) is -1 to 1 and maps from its 0 to 1 position.
fn map(value: f32, signed: bool, min: f32, max: f32, log: bool) -> f32 {
    if min == 0.0 && max == 1.0 && !log {
        return value;
    }
    let position = if signed { (value + 1.0) * 0.5 } else { value };
    let position = position.clamp(0.0, 1.0);
    if log && min > 0.0 && max > 0.0 {
        min * (max / min).powf(position)
    } else {
        min + (max - min) * position
    }
}

impl Node for ControlNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            event_inputs,
            outputs,
            ..
        } = io;
        let events = event_inputs[ControlPorts::EVENTS];
        let control = inputs[ControlPorts::CONTROL].lane(0, 0);
        let channel = inputs[ControlPorts::CHANNEL].lane(0, 0);
        let glide = inputs[ControlPorts::GLIDE].lane(0, 0);
        let encoder = inputs[ControlPorts::ENCODER].lane(0, 0)[0].round();
        let pickup = inputs[ControlPorts::PICKUP].lane(0, 0)[0] >= 0.5;
        let start = inputs[ControlPorts::START].lane(0, 0)[0].clamp(0.0, 1.0);
        let min = inputs[ControlPorts::MIN].lane(0, 0)[0];
        let max = inputs[ControlPorts::MAX].lane(0, 0)[0];
        let log = inputs[ControlPorts::TAPER].lane(0, 0)[0] >= 0.5;
        let out = outputs[ControlPorts::OUT].lane_mut(0, 0);

        let mut live = std::mem::take(&mut self.buffer);
        if let Some(reader) = &mut self.live {
            reader.process(ctx, Some(0), &mut EventsOut::new(&mut live));
        }
        let signed = (control[0].round() - PITCH_BEND).abs() < 0.5;
        if !self.touched {
            // Until the knob moves it, the value is where `start` says.
            let held = if signed { start * 2.0 - 1.0 } else { start };
            self.target = held;
            self.current = held;
        }
        let (mut wired, mut heard) = (0, 0);
        for time in 0..ctx.frames {
            let control = control[time].round().clamp(0.0, 129.0);
            let channel = channel[time].round().clamp(0.0, 16.0) as u8;
            while let Some(event) = events.get(wired).filter(|e| e.time as usize <= time) {
                if let Some(reading) = self.read(&event.kind, control, channel, encoder) {
                    self.take(reading, pickup);
                }
                wired += 1;
            }
            while let Some(event) = live.get(heard).filter(|e| e.time as usize <= time) {
                if let Some(reading) = self.read(&event.kind, control, channel, encoder) {
                    self.take(reading, pickup);
                }
                heard += 1;
            }
            let ms = glide[time];
            self.current = if ms <= 0.0 {
                self.target
            } else {
                let keep = (-1000.0 / (ms * ctx.sample_rate)).exp();
                self.target + (self.current - self.target) * keep
            };
            out[time] = map(self.current, signed, min, max, log);
        }
        // Events past the block's end still update the held values.
        for event in events[wired..].iter().chain(&live[heard..]) {
            let _ = self.read(&event.kind, 0.0, 0, ENCODER_ABSOLUTE);
        }
        self.buffer = live;
    }

    fn reset(&mut self) {
        // A live control holds what the knob last said, and the knob stays
        // where it is across a seek or a loop wrap.
        if self.live.is_some() {
            return;
        }
        // In place: this runs on the audio thread.
        self.msb.fill([0; 32]);
        self.lsb.fill([0; 32]);
        self.has_lsb = [0; 16];
        self.target = 0.0;
        self.current = 0.0;
        self.touched = false;
        self.last_in = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Event;
    use noodle_engine::testing::Harness;
    use noodle_io::MidiBus;

    fn harness() -> Harness {
        Harness::new(&MidiControl::default(), &Config::new(), &[], 48_000.0, 16).unwrap()
    }

    fn at(time: u32, kind: EventKind) -> Event {
        Event { time, kind }
    }

    fn channel(channel: u8, control: ChannelControl, value: u32) -> EventKind {
        EventKind::Channel {
            channel,
            control,
            value,
        }
    }

    fn run(h: &mut Harness, events: &[Event]) -> Vec<f32> {
        h.send_events(ControlPorts::EVENTS, events);
        h.run(16).unwrap();
        h.output(ControlPorts::OUT).lane(0, 0).to_vec()
    }

    fn unglided(h: &mut Harness, control: f32, channel: f32) {
        h.set(ControlPorts::GLIDE, 0.0);
        h.set(ControlPorts::CONTROL, control);
        h.set(ControlPorts::CHANNEL, channel);
    }

    #[test]
    fn a_midi2_controller_keeps_more_than_seven_bits() {
        let mut h = harness();
        unglided(&mut h, 74.0, 0.0);
        let out = run(
            &mut h,
            &[at(
                4,
                channel(0, ChannelControl::Controller(74), 0x1234_5678),
            )],
        );
        assert_eq!(out[3], 0.0);
        // A 7-bit controller would give 9/127 = 0.0709.
        assert!(
            (out[4] - 0x1234_5678 as f32 / u32::MAX as f32).abs() < 1e-7,
            "{}",
            out[4]
        );
        // A neighbouring 32-bit value is a different output.
        let out = run(
            &mut h,
            &[at(
                0,
                channel(0, ChannelControl::Controller(74), 0x1234_9999),
            )],
        );
        assert!(out[0] > 0x1234_5678 as f32 / u32::MAX as f32);
    }

    #[test]
    fn a_midi1_controller_pair_is_fourteen_bits() {
        let mut h = harness();
        unglided(&mut h, 1.0, 0.0);
        let out = run(&mut h, &[at(0, EventKind::Midi([0xB0, 1, 64]))]);
        assert!((out[0] - 64.0 / 127.0).abs() < 1e-6);
        // The low half arrives and refines it.
        let out = run(&mut h, &[at(0, EventKind::Midi([0xB0, 33, 100]))]);
        assert!((out[0] - f32::from(64u16 << 7 | 100) / 16_383.0).abs() < 1e-6);
    }

    #[test]
    fn pitch_bend_is_signed_and_pressure_is_not() {
        let mut h = harness();
        unglided(&mut h, PITCH_BEND, 0.0);
        let out = run(
            &mut h,
            &[
                at(0, channel(0, ChannelControl::PitchBend, 0)),
                at(4, channel(0, ChannelControl::PitchBend, 0x8000_0000)),
                at(8, channel(0, ChannelControl::PitchBend, u32::MAX)),
                at(12, EventKind::Midi([0xE0, 0, 0x40])),
            ],
        );
        assert!((out[0] + 1.0).abs() < 1e-6);
        assert!(out[4].abs() < 1e-6);
        assert!((out[8] - 1.0).abs() < 1e-6);
        assert!(out[12].abs() < 1e-6, "MIDI 1.0 centre {}", out[12]);

        let mut h = harness();
        unglided(&mut h, PRESSURE, 0.0);
        let out = run(
            &mut h,
            &[at(2, channel(3, ChannelControl::Pressure, u32::MAX))],
        );
        assert_eq!((out[1], out[2]), (0.0, 1.0));
    }

    #[test]
    fn only_the_chosen_controller_and_channel_count() {
        let mut h = harness();
        unglided(&mut h, 7.0, 2.0);
        let out = run(
            &mut h,
            &[
                // Another controller, and the right one on another channel.
                at(0, channel(1, ChannelControl::Controller(10), u32::MAX)),
                at(2, channel(0, ChannelControl::Controller(7), u32::MAX)),
                // Channel 2 is zero-based 1.
                at(4, channel(1, ChannelControl::Controller(7), u32::MAX / 2)),
            ],
        );
        assert_eq!((out[0], out[3]), (0.0, 0.0));
        assert!((out[4] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn glide_takes_time_and_holds_across_blocks() {
        let mut h = harness();
        unglided(&mut h, 7.0, 0.0);
        h.set(ControlPorts::GLIDE, 1.0);
        let out = run(
            &mut h,
            &[at(0, channel(0, ChannelControl::Controller(7), u32::MAX))],
        );
        assert!(out[0] > 0.0 && out[0] < 0.1, "{}", out[0]);
        assert!(out.windows(2).all(|w| w[1] > w[0]));
        let later = run(&mut h, &[]);
        assert!(later[0] > out[15] && later[15] <= 1.0);
    }

    fn cc(value: u8) -> EventKind {
        EventKind::Midi([0xB0, 16, value])
    }

    fn encoder(h: &mut Harness, mode: f32) {
        unglided(h, 16.0, 0.0);
        h.set(ControlPorts::ENCODER, mode);
    }

    /// The held value after each turn, one block per turn.
    fn turns(h: &mut Harness, values: &[u8]) -> Vec<f32> {
        values
            .iter()
            .map(|&v| run(h, &[at(0, cc(v))])[15])
            .collect()
    }

    #[test]
    fn each_relative_coding_turns_both_ways() {
        let step = STEP;
        // Two's complement: 1 is one clockwise, 127 one anticlockwise.
        let mut h = harness();
        encoder(&mut h, ENCODER_TWOS_COMPLEMENT);
        h.set(ControlPorts::START, 0.5);
        let got = turns(&mut h, &[1, 1, 3, 127, 126]);
        let expect = [1.0, 2.0, 5.0, 4.0, 2.0].map(|n| 0.5 + n * step);
        for (g, e) in got.iter().zip(expect) {
            assert!((g - e).abs() < 1e-6, "{g} vs {e}");
        }
        // Sign-magnitude: bit 6 is the sign.
        let mut h = harness();
        encoder(&mut h, ENCODER_SIGN_MAGNITUDE);
        h.set(ControlPorts::START, 0.5);
        let got = turns(&mut h, &[2, 0x41, 0x43]);
        let expect = [2.0, 1.0, -2.0].map(|n| 0.5 + n * step);
        for (g, e) in got.iter().zip(expect) {
            assert!((g - e).abs() < 1e-6, "{g} vs {e}");
        }
        // Binary offset: 64 is no turn.
        let mut h = harness();
        encoder(&mut h, ENCODER_BINARY_OFFSET);
        h.set(ControlPorts::START, 0.5);
        let got = turns(&mut h, &[65, 64, 61]);
        let expect = [1.0, 1.0, -2.0].map(|n| 0.5 + n * step);
        for (g, e) in got.iter().zip(expect) {
            assert!((g - e).abs() < 1e-6, "{g} vs {e}");
        }
    }

    #[test]
    fn a_relative_value_stops_at_the_ends_and_absolute_ignores_codings() {
        let mut h = harness();
        encoder(&mut h, ENCODER_BINARY_OFFSET);
        h.set(ControlPorts::START, 0.99);
        assert_eq!(turns(&mut h, &[127, 127])[1], 1.0);
        assert_eq!(turns(&mut h, &[0; 40])[39], 0.0);
        // Absolute reads the same byte as a position.
        let mut h = harness();
        encoder(&mut h, ENCODER_ABSOLUTE);
        assert!((turns(&mut h, &[127])[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jump_follows_a_knob_that_is_elsewhere_and_pickup_waits_for_it() {
        let mut h = harness();
        unglided(&mut h, 16.0, 0.0);
        h.set(ControlPorts::START, 0.5);
        // Jump: the first message sets the value.
        assert!((turns(&mut h, &[10])[0] - 10.0 / 127.0).abs() < 1e-6);

        let mut h = harness();
        unglided(&mut h, 16.0, 0.0);
        h.set(ControlPorts::START, 0.5);
        h.set(ControlPorts::PICKUP, 1.0);
        // Below the value: held at it.
        let got = turns(&mut h, &[10, 30, 50]);
        assert!(got.iter().all(|&v| (v - 0.5).abs() < 1e-6), "{got:?}");
        // Passes it (64 is 0.504): picked up, and followed from then on.
        let got = turns(&mut h, &[70, 100, 20]);
        assert!((got[0] - 70.0 / 127.0).abs() < 1e-6, "{got:?}");
        assert!((got[1] - 100.0 / 127.0).abs() < 1e-6);
        assert!((got[2] - 20.0 / 127.0).abs() < 1e-6, "stays picked up");
    }

    #[test]
    fn pickup_also_catches_a_knob_that_starts_above_and_comes_down() {
        let mut h = harness();
        unglided(&mut h, 16.0, 0.0);
        h.set(ControlPorts::START, 0.25);
        h.set(ControlPorts::PICKUP, 1.0);
        let got = turns(&mut h, &[120, 80, 40, 20]);
        assert!(got[..3].iter().all(|&v| (v - 0.25).abs() < 1e-6), "{got:?}");
        assert!((got[3] - 20.0 / 127.0).abs() < 1e-6, "{got:?}");
    }

    #[test]
    fn the_value_maps_onto_min_and_max_with_a_log_taper() {
        let mut h = harness();
        unglided(&mut h, 16.0, 0.0);
        h.set(ControlPorts::MIN, 20.0);
        h.set(ControlPorts::MAX, 20_000.0);
        let out = turns(&mut h, &[0, 127]);
        assert!((out[0] - 20.0).abs() < 1e-3 && (out[1] - 20_000.0).abs() < 1.0);
        h.set(ControlPorts::TAPER, 1.0);
        let half = turns(&mut h, &[64])[0];
        assert!(
            (half - 20.0 * 1000f32.powf(64.0 / 127.0)).abs() < 1.0,
            "{half}"
        );
        // Before it moves, the output is `start` along the range.
        let mut h = harness();
        unglided(&mut h, 16.0, 0.0);
        h.set(ControlPorts::MIN, -60.0);
        h.set(ControlPorts::MAX, 12.0);
        h.set(ControlPorts::START, 0.5);
        let out = run(&mut h, &[]);
        assert!((out[0] + 24.0).abs() < 1e-4, "{}", out[0]);
    }

    fn live(bus: &MidiBus, port: &str) -> Harness {
        let config = Config::new().with(
            crate::midi_in::MIDI_PORT_KEY,
            noodle_core::Value::Text(port.into()),
        );
        Harness::new(&MidiControl::new(bus), &config, &[], 48_000.0, 16).unwrap()
    }

    #[test]
    fn a_port_set_reads_that_port_alone_and_none_reads_the_wire_only() {
        let bus = MidiBus::new();
        let mut keys = live(&bus, "Keys");
        let mut all = live(&bus, "*");
        let mut wired = live(&bus, "");
        for h in [&mut keys, &mut all, &mut wired] {
            unglided(h, 74.0, 0.0);
        }
        bus.send_from("Pads", [0xB0, 74, 127]);
        let blocks = [&mut keys, &mut all, &mut wired].map(|h| {
            h.run(16).unwrap();
            h.output(ControlPorts::OUT).lane(0, 0)[15]
        });
        assert_eq!(blocks, [0.0, 1.0, 0.0]);
        bus.send_from("Keys", [0xB0, 74, 64]);
        let blocks = [&mut keys, &mut wired].map(|h| {
            h.run(16).unwrap();
            h.output(ControlPorts::OUT).lane(0, 0)[15]
        });
        assert!((blocks[0] - 64.0 / 127.0).abs() < 1e-6);
        assert_eq!(blocks[1], 0.0);
    }

    #[test]
    fn a_live_control_keeps_its_value_across_a_reset_and_a_wired_one_does_not() {
        let bus = MidiBus::new();
        let mut h = live(&bus, "Keys");
        unglided(&mut h, 74.0, 0.0);
        bus.send_from("Keys", [0xB0, 74, 127]);
        h.run(16).unwrap();
        h.seek(0);
        h.run(16).unwrap();
        assert_eq!(h.output(ControlPorts::OUT).lane(0, 0)[15], 1.0);
        let mut h = harness();
        unglided(&mut h, 74.0, 0.0);
        run(&mut h, &[at(0, EventKind::Midi([0xB0, 74, 127]))]);
        h.seek(0);
        assert_eq!(run(&mut h, &[])[15], 0.0);
    }
}
