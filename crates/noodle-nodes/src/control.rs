//! [`MidiControl`]: a MIDI controller, pitch bend or channel pressure as a
//! signal, at the resolution the message came in: 32 bits from MIDI 2.0, 14
//! from a MIDI 1.0 controller pair or pitch bend.

use noodle_engine::{
    ChannelControl, Config, Context, EventKind, Instance, Io, Layout, Node, NodeError, NodeInfo,
    NodeType, ParamInfo, Ports, Setup,
};

pub struct MidiControl;

pub const MIDI_CONTROL_ID: &str = "noodle.event.control";

/// `control` values past the 128 controllers.
pub const PITCH_BEND: f32 = 128.0;
pub const PRESSURE: f32 = 129.0;

#[derive(Ports)]
struct ControlPorts {
    #[event_input("in", "Events")]
    events: (),
    /// A controller number (0 to 127), 128 for pitch bend or 129 for channel
    /// pressure.
    #[param("control", "Control", ParamInfo::new(0.0, 129.0, 1.0).smoothing(0.0))]
    control: (),
    /// 0 reads every channel; 1 to 16 only that channel.
    #[param("channel", "Channel", ParamInfo::new(0.0, 16.0, 0.0).smoothing(0.0))]
    channel: (),
    /// How long the output takes to reach a new value, so a 7-bit controller
    /// doesn't zipper.
    #[param("glide", "Glide", ParamInfo::new(0.0, 500.0, 5.0).smoothing(0.0))]
    glide: (),
    /// 0 to 1, or -1 to 1 for pitch bend.
    #[output("out", "Value")]
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

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ControlPorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ControlNode::default()))
    }
}

#[derive(Default)]
struct ControlNode {
    /// The high and low 7 bits of controllers 0 to 31 (MIDI 1.0 pairs).
    msb: Box<[[u8; 32]; 16]>,
    lsb: Box<[[u8; 32]; 16]>,
    /// Which pairs have sent a low half; until one does, the high half
    /// alone is the 7-bit value.
    has_lsb: [u32; 16],
    /// What the output heads for, and what it has reached.
    target: f32,
    current: f32,
}

impl ControlNode {
    /// The value an event sets for `control` and `channel` (0 for any), if it
    /// is for them.
    fn read(&mut self, kind: &EventKind, control: f32, channel: u8) -> Option<f32> {
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
                        if index < 32 {
                            self.msb[c][index] = b & 127;
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
        let wanted =
            (channel == 0 || message_channel + 1 == channel) && (which - control).abs() < 0.5;
        wanted.then_some(value)
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
        let out = outputs[ControlPorts::OUT].lane_mut(0, 0);
        let mut next = 0;
        for time in 0..ctx.frames {
            let control = control[time].round().clamp(0.0, 129.0);
            let channel = channel[time].round().clamp(0.0, 16.0) as u8;
            while let Some(event) = events.get(next).filter(|e| e.time as usize <= time) {
                if let Some(value) = self.read(&event.kind, control, channel) {
                    self.target = value;
                }
                next += 1;
            }
            let ms = glide[time];
            self.current = if ms <= 0.0 {
                self.target
            } else {
                let keep = (-1000.0 / (ms * ctx.sample_rate)).exp();
                self.target + (self.current - self.target) * keep
            };
            out[time] = self.current;
        }
        // Events past the block's end still update the held values.
        for event in &events[next..] {
            let _ = self.read(&event.kind, 0.0, 0);
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Event;
    use noodle_engine::testing::Harness;

    fn harness() -> Harness {
        Harness::new(&MidiControl, &Config::new(), &[], 48_000.0, 16).unwrap()
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
}
