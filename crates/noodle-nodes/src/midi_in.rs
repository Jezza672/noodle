//! The MIDI In node: notes and controllers from the MIDI input port the
//! app has open.
//!
//! The port itself belongs to the app (a choice in the settings, not part of
//! the project, like the audio device), and its messages reach every MIDI In
//! node through a [`MidiBus`]. Each message carries the time it arrived. The
//! device callback says which span of arrivals each block plays (see
//! `AudioClock`), so a note played at some point of one device period sounds
//! at the same point of the next: live playing is one device period late and
//! has no jitter beyond the callbacks' own, whatever the period is.
//!
//! Note-ons and note-offs become note events; everything else (controllers,
//! pitch bend, aftertouch, program changes) passes on as raw MIDI events.
//! MIDI 2.0 packets (see `noodle_io::ump`) are played too: 16-bit velocity,
//! note pitch attributes, per-note pressure, pitch bend and controllers
//! become note events and per-note expressions, and what is channel-wide is
//! narrowed to the MIDI 1.0 messages the rest of the engine speaks.
//! The note ID is made from the channel and key, so a note-off finds its
//! note, and its top bit is set, which keeps it apart from the IDs notes in
//! MIDI clips have.

use noodle_engine::{
    ChannelControl, Config, Context, Event, EventKind, Expression, Instance, Io, Layout, Node,
    NodeError, NodeInfo, NodeType, NoteId, ParamInfo, Ports, Setup,
};
use noodle_io::{Attribute, Midi2, MidiBus, MidiReceiver, Packet, TimedPacket, now_micros};

pub const MIDI_IN_ID: &str = "noodle.event.midi_in";

#[derive(Ports)]
#[ports(nondeterministic)]
struct MidiInPorts {
    /// Only this channel is played; 0 plays them all.
    #[param("channel", "Channel", ParamInfo::new(0.0, 16.0, 0.0).smoothing(0.0))]
    channel: (),
    /// What a MIDI 2.0 per-note pitch bend at its extreme bends by, in
    /// semitones (MPE's default is 48).
    #[param("note_bend", "Note bend", ParamInfo::new(0.0, 96.0, 48.0).smoothing(0.0))]
    note_bend: (),
    #[event_output("out", "Events")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: MIDI_IN_ID,
    version: 1,
    name: "MIDI In",
    category: "Events",
};

/// Registered by [`register_library`](crate::register_library).
pub struct MidiIn {
    bus: MidiBus,
    clock: Option<Clock>,
}

/// The time now, in microseconds on the clock the bus stamps messages with.
pub type Clock = fn() -> u64;

impl MidiIn {
    pub fn new(bus: &MidiBus) -> Self {
        Self {
            bus: bus.clone(),
            clock: None,
        }
    }

    /// Ignoring what the device says about its blocks, and treating each
    /// block as beginning when `clock` says. For tests, and it is what a node
    /// does anyway when no device drives the engine.
    pub fn with_clock(bus: &MidiBus, clock: Clock) -> Self {
        Self {
            bus: bus.clone(),
            clock: Some(clock),
        }
    }
}

/// The frame of a block that a message arriving at `arrival` plays at, when
/// the previous block began at `previous_start` (both in microseconds). The
/// first block, and messages from before the previous one began, play at the
/// start.
fn frame_offset(arrival: u64, previous_start: Option<u64>, sample_rate: f32, frames: usize) -> u32 {
    let Some(start) = previous_start else {
        return 0;
    };
    let micros = arrival.saturating_sub(start) as f64;
    let frame = (micros * f64::from(sample_rate) / 1e6) as usize;
    frame.min(frames.saturating_sub(1)) as u32
}

impl NodeType for MidiIn {
    fn always_runs(&self) -> bool {
        true
    }

    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(MidiInPorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(MidiInNode {
            live: LiveMidi::new(&self.bus, self.clock, NOTE_ID_BASE),
        }))
    }
}

struct MidiInNode {
    live: LiveMidi,
}

/// The top bit of the note IDs MIDI In makes, which keeps them apart from
/// the IDs notes in MIDI clips have.
const NOTE_ID_BASE: u32 = 0x8000_0000;

/// What turns the messages on a [`MidiBus`] into events, block by block: the
/// MIDI In node, and the track inputs of armed tracks (live monitoring).
pub(crate) struct LiveMidi {
    bus: MidiBus,
    input: MidiReceiver,
    clock: Option<Clock>,
    /// The note IDs it makes are this with the channel and key.
    id_base: u32,
    /// A message taken from the queue that belongs to a later block.
    pending: Option<TimedPacket>,
    /// Semitones a full per-note pitch bend moves a note.
    note_bend: f32,
    /// Per channel and key, the note's pitch from its attribute or Pitch 7.25
    /// (semitones from the key) and its per-note bend. The `Pitch`
    /// expression sent is their sum, whichever changed last.
    pitch_base: Box<[[f32; 128]; 16]>,
    pitch_bend: Box<[[f32; 128]; 16]>,
    /// When the previous block began, for the lack of a device saying so.
    previous_start: Option<u64>,
    /// Per channel, the keys that have a note-on and no note-off yet, one
    /// bit each, so "all notes off" can end them.
    held: [u128; 16],
    /// Channels with an "all notes off" still to carry out.
    ending: u16,
}

impl LiveMidi {
    pub fn new(bus: &MidiBus, clock: Option<Clock>, id_base: u32) -> Self {
        Self {
            bus: bus.clone(),
            input: bus.subscribe(),
            clock,
            id_base,
            pending: None,
            note_bend: 48.0,
            pitch_base: Box::new([[0.0; 128]; 16]),
            pitch_bend: Box::new([[0.0; 128]; 16]),
            previous_start: None,
            held: [0; 16],
            ending: 0,
        }
    }

    fn note_id(&self, channel: u8, key: u8) -> NoteId {
        NoteId(self.id_base | u32::from(channel) << 8 | u32::from(key))
    }

    /// Pushes the events a MIDI 1.0 message stands for. Returns false if the
    /// buffer is full.
    fn translate(
        &mut self,
        [status, a, b]: [u8; 3],
        time: u32,
        out: &mut noodle_engine::EventsOut<'_>,
    ) -> bool {
        let channel = status & 0x0F;
        let kind = match status & 0xF0 {
            0x90 if b > 0 => {
                self.held[usize::from(channel)] |= 1 << a;
                EventKind::NoteOn {
                    note: self.note_id(channel, a),
                    channel,
                    key: a,
                    velocity: f32::from(b) / 127.0,
                }
            }
            0x80 | 0x90 => {
                self.held[usize::from(channel)] &= !(1 << a);
                EventKind::NoteOff {
                    note: self.note_id(channel, a),
                    velocity: f32::from(b) / 127.0,
                }
            }
            _ => EventKind::Midi([status, a, b]),
        };
        out.push(Event { time, kind }).is_ok()
    }

    /// The same for a MIDI 2.0 message. Returns false if the buffer filled.
    fn translate2(
        &mut self,
        message: Midi2,
        time: u32,
        out: &mut noodle_engine::EventsOut<'_>,
    ) -> bool {
        let unit = |v: u32| v as f32 / u32::MAX as f32;
        // -1 to 1 around the centre of a 32-bit bend.
        let signed = |v: u32| (f64::from(v) - 2_147_483_648.0) as f32 / 2_147_483_648.0;
        let mut push = |kind| out.push(Event { time, kind }).is_ok();
        match message {
            Midi2::NoteOn {
                channel,
                key,
                velocity,
                attribute,
            } => {
                self.held[usize::from(channel)] |= 1 << key;
                let note = self.note_id(channel, key);
                let (c, k) = (usize::from(channel), usize::from(key));
                self.pitch_base[c][k] = 0.0;
                self.pitch_bend[c][k] = 0.0;
                // A velocity of 0 is not a note-off here: it is the softest.
                let velocity = f32::from(velocity.max(1)) / 65_535.0;
                let on = EventKind::NoteOn {
                    note,
                    channel,
                    key,
                    velocity,
                };
                if !push(on) {
                    return false;
                }
                match attribute {
                    Attribute::Pitch(pitch) if (pitch - f32::from(key)).abs() > 1e-4 => {
                        self.pitch_base[c][k] = pitch - f32::from(key);
                        push(EventKind::Expression {
                            note,
                            expression: Expression::Pitch,
                            value: self.pitch_base[c][k],
                        })
                    }
                    _ => true,
                }
            }
            Midi2::NoteOff {
                channel,
                key,
                velocity,
                ..
            } => {
                self.held[usize::from(channel)] &= !(1 << key);
                push(EventKind::NoteOff {
                    note: self.note_id(channel, key),
                    velocity: f32::from(velocity) / 65_535.0,
                })
            }
            Midi2::PolyPressure {
                channel,
                key,
                value,
            } => push(EventKind::Expression {
                note: self.note_id(channel, key),
                expression: Expression::Pressure,
                value: unit(value),
            }),
            Midi2::PerNotePitchBend {
                channel,
                key,
                value,
            } => {
                let (c, k) = (usize::from(channel), usize::from(key));
                self.pitch_bend[c][k] = signed(value) * self.note_bend;
                push(EventKind::Expression {
                    note: self.note_id(channel, key),
                    expression: Expression::Pitch,
                    value: self.pitch_base[c][k] + self.pitch_bend[c][k],
                })
            }
            Midi2::PerNoteController {
                channel,
                key,
                registered: true,
                index,
                value,
            } => {
                let note = self.note_id(channel, key);
                let (c, k) = (usize::from(channel), usize::from(key));
                let (expression, value) = match index {
                    // Modulation and brightness.
                    1 | 74 => (Expression::Timbre, unit(value)),
                    // Channel volume.
                    7 => (Expression::Volume, unit(value)),
                    10 => (Expression::Pan, unit(value) * 2.0 - 1.0),
                    // Pitch 7.25: the note's pitch as key numbers with a
                    // 25-bit fraction.
                    3 => {
                        self.pitch_base[c][k] =
                            (f64::from(value) / f64::from(1u32 << 25) - f64::from(key)) as f32;
                        (
                            Expression::Pitch,
                            self.pitch_base[c][k] + self.pitch_bend[c][k],
                        )
                    }
                    _ => return true,
                };
                push(EventKind::Expression {
                    note,
                    expression,
                    value,
                })
            }
            // Assignable per-note controllers mean what their maker says,
            // and a note's management flags change nothing the engine keeps.
            Midi2::PerNoteController { .. } | Midi2::PerNoteManagement { .. } => true,
            Midi2::Controller {
                channel,
                index,
                value,
            } => push(EventKind::Channel {
                channel,
                control: ChannelControl::Controller(index),
                value,
            }),
            Midi2::PitchBend { channel, value } => push(EventKind::Channel {
                channel,
                control: ChannelControl::PitchBend,
                value,
            }),
            Midi2::ChannelPressure { channel, value } => push(EventKind::Channel {
                channel,
                control: ChannelControl::Pressure,
                value,
            }),
            channel_wide => {
                let mut narrowed = [[0; 3]; 4];
                let count = channel_wide.to_midi1(&mut narrowed);
                // A program change with a bank sends the bank first.
                if let Midi2::Program {
                    channel,
                    bank: Some((msb, lsb)),
                    ..
                } = channel_wide
                {
                    for (index, value) in [(0, msb), (32, lsb)] {
                        if !push(EventKind::Midi([0xB0 | channel, index, value])) {
                            return false;
                        }
                    }
                }
                narrowed[..count]
                    .iter()
                    .all(|&message| push(EventKind::Midi(message)))
            }
        }
    }

    /// Ends the held notes on channels with an "all notes off" to carry out.
    /// Each note is forgotten only once its note-off is in the buffer, so if
    /// the buffer fills the rest wait for the next block. Returns whether it
    /// finished.
    fn flush_ending(&mut self, out: &mut noodle_engine::EventsOut<'_>, time: u32) -> bool {
        for channel in 0..16u8 {
            let bit = 1u16 << channel;
            if self.ending & bit == 0 {
                continue;
            }
            while self.held[usize::from(channel)] != 0 {
                let key = self.held[usize::from(channel)].trailing_zeros() as u8;
                let off = Event {
                    time,
                    kind: EventKind::NoteOff {
                        note: self.note_id(channel, key),
                        velocity: 0.0,
                    },
                };
                if out.push(off).is_err() {
                    return false;
                }
                self.held[usize::from(channel)] &= !(1 << key);
            }
            self.ending &= !bit;
        }
        true
    }

    /// Writes the events for the block. `channel` is the one channel to play
    /// (0 for all of them), or `None` to play nothing: what arrives is
    /// dropped and the notes held are ended.
    pub fn process(
        &mut self,
        ctx: &Context,
        channel: Option<u8>,
        out: &mut noodle_engine::EventsOut<'_>,
    ) {
        // Which arrivals this block plays. With a device driving the engine,
        // the callback says: a span as long as the block, starting one device
        // period before the block's place in the callback, so what was played
        // at some point of a period sounds at the same point of the next, and
        // what is later than the span waits. Without one, whatever arrived
        // since the previous block began plays in this one, late ones bunched
        // at its end.
        let block_micros = (ctx.frames as f64 * 1e6 / f64::from(ctx.sample_rate)) as u64;
        let (start, limit) = match (self.clock, self.bus.clock().window()) {
            (None, Some(window)) => (Some(window), Some(window.saturating_add(block_micros))),
            (clock, _) => {
                let now = clock.map_or_else(now_micros, |clock| clock());
                (self.previous_start.replace(now), None)
            }
        };
        if channel.is_none() && self.held.iter().any(|&keys| keys != 0) {
            self.ending = u16::MAX;
        }
        // Left over from a block where the buffer filled.
        if !self.flush_ending(out, 0) {
            return;
        }
        // Events have to be in time order, and the bus can hand over two
        // messages out of order when the keyboard and a port send together.
        let mut last = 0;
        while let Some((arrival, packet)) = self.pending.take().or_else(|| self.input.pop_packet())
        {
            if limit.is_some_and(|limit| arrival >= limit) {
                self.pending = Some((arrival, packet));
                break;
            }
            let Some(message) = Incoming::of(packet) else {
                // Not a channel voice message: utility, system and sysex
                // packets, and statuses the engine has no use for.
                continue;
            };
            let message_channel = message.channel();
            // A note-off for a note that is held always gets through, so
            // changing the channel can't leave it sounding.
            let ends_held = message
                .note_off_key()
                .is_some_and(|key| self.held[usize::from(message_channel)] >> key & 1 == 1);
            let wanted = channel.is_some_and(|wanted| wanted == 0 || message_channel + 1 == wanted);
            if !wanted && !ends_held {
                continue;
            }
            let time = frame_offset(arrival, start, ctx.sample_rate, ctx.frames).max(last);
            last = time;
            // Sound off (120) and all notes off (123) end what is held.
            if message.is_all_notes_off() {
                self.ending |= 1 << message_channel;
                if !self.flush_ending(out, time) {
                    // The controller itself is dropped with the rest of the
                    // block; the notes still held end next block.
                    return;
                }
            }
            let fits = match message {
                Incoming::One(bytes) => self.translate(bytes, time, out),
                Incoming::Two(message) => self.translate2(message, time, out),
            };
            if !fits {
                break;
            }
        }
    }
}

/// A channel voice message of either kind, which is all the node plays.
#[derive(Clone, Copy)]
enum Incoming {
    One([u8; 3]),
    Two(Midi2),
}

impl Incoming {
    fn of(packet: Packet) -> Option<Incoming> {
        packet
            .as_midi1()
            .map(Incoming::One)
            .or_else(|| packet.as_midi2().map(Incoming::Two))
    }

    fn channel(&self) -> u8 {
        match self {
            Incoming::One(bytes) => bytes[0] & 0x0F,
            Incoming::Two(message) => message.channel(),
        }
    }

    /// The key of a note-off (a note-on with velocity 0 is one in MIDI 1.0).
    fn note_off_key(&self) -> Option<u8> {
        match *self {
            Incoming::One([status, key, velocity]) => {
                (status & 0xF0 == 0x80 || (status & 0xF0 == 0x90 && velocity == 0)).then_some(key)
            }
            Incoming::Two(Midi2::NoteOff { key, .. }) => Some(key),
            Incoming::Two(_) => None,
        }
    }

    fn is_all_notes_off(&self) -> bool {
        match *self {
            Incoming::One([status, controller, _]) => {
                status & 0xF0 == 0xB0 && matches!(controller, 120 | 123)
            }
            Incoming::Two(Midi2::Controller { index, .. }) => matches!(index, 120 | 123),
            Incoming::Two(_) => false,
        }
    }
}

impl Node for MidiInNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let wanted = io.inputs[MidiInPorts::CHANNEL].lane(0, 0)[0].round() as u8;
        self.live.note_bend = io.inputs[MidiInPorts::NOTE_BEND].lane(0, 0)[0];
        let out = &mut io.event_outputs[MidiInPorts::OUT];
        self.live.process(ctx, Some(wanted), out);
    }

    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    fn note_id(channel: u8, key: u8) -> NoteId {
        NoteId(NOTE_ID_BASE | u32::from(channel) << 8 | u32::from(key))
    }

    fn node() -> (Harness, MidiBus) {
        let bus = MidiBus::new();
        let h = Harness::new(&MidiIn::new(&bus), &Config::new(), &[], 48_000.0, 16).unwrap();
        (h, bus)
    }

    fn key_on(channel: u8, key: u8, velocity: u8) -> [u8; 3] {
        [0x90 | channel, key, velocity]
    }

    #[test]
    fn frame_offsets_follow_the_arrival_time() {
        // 48 kHz: 1000 microseconds is 48 frames.
        assert_eq!(frame_offset(5_000, None, 48_000.0, 128), 0);
        assert_eq!(frame_offset(5_000, Some(4_000), 48_000.0, 128), 48);
        // From before the previous block, or past the end of this one.
        assert_eq!(frame_offset(3_000, Some(4_000), 48_000.0, 128), 0);
        assert_eq!(frame_offset(90_000, Some(4_000), 48_000.0, 128), 127);
    }

    #[test]
    fn a_device_callback_with_several_blocks_places_messages_by_the_span_each_plays() {
        let bus = MidiBus::new();
        let mut h = Harness::new(&MidiIn::new(&bus), &Config::new(), &[], 48_000.0, 128).unwrap();
        // 128 frames at 48 kHz last 2666 microseconds.
        let first = 100_000;
        let second = first + 2_666;
        bus.send_at(key_on(0, 59, 100), first - 100); // older than the span
        bus.send_at(key_on(0, 60, 100), first + 500);
        bus.send_at(key_on(0, 61, 100), second + 1_000); // the next block's
        bus.clock().set_window(first);
        h.run(128).unwrap();
        let notes = |h: &Harness| -> Vec<(u32, u8)> {
            h.events(MidiInPorts::OUT)
                .iter()
                .filter_map(|e| match e.kind {
                    EventKind::NoteOn { key, .. } => Some((e.time, key)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(notes(&h), vec![(0, 59), (24, 60)]);
        bus.clock().set_window(second);
        h.run(128).unwrap();
        assert_eq!(notes(&h), vec![(48, 61)]);
    }

    #[test]
    fn messages_stamped_out_of_order_still_give_ordered_events() {
        use std::sync::atomic::Ordering;
        let bus = MidiBus::new();
        let mut h = Harness::new(
            &MidiIn::with_clock(&bus, || ORDER.load(Ordering::Relaxed)),
            &Config::new(),
            &[],
            48_000.0,
            128,
        )
        .unwrap();
        ORDER.store(10_000, Ordering::Relaxed);
        h.run(128).unwrap();
        // 1000 microseconds in is frame 48, and 500 is frame 24: the second
        // message arrived first, as two threads sending at once can manage.
        bus.send_at(key_on(0, 60, 100), 11_000);
        bus.send_at(key_on(0, 61, 100), 10_500);
        ORDER.store(12_667, Ordering::Relaxed);
        h.run(128).unwrap();
        let times: Vec<u32> = h.events(MidiInPorts::OUT).iter().map(|e| e.time).collect();
        assert_eq!(times, vec![48, 48]);
    }

    static ORDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    static NOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    #[test]
    fn a_message_plays_at_the_offset_it_arrived_at_in_the_previous_block() {
        use std::sync::atomic::Ordering;
        let bus = MidiBus::new();
        let mut h = Harness::new(
            &MidiIn::with_clock(&bus, || NOW.load(Ordering::Relaxed)),
            &Config::new(),
            &[],
            48_000.0,
            128,
        )
        .unwrap();
        NOW.store(10_000, Ordering::Relaxed);
        h.run(128).unwrap();
        // 500 microseconds after the first block began is frame 24.
        bus.send_at(key_on(0, 60, 100), 10_500);
        bus.send_at([0x80, 60, 0], 11_000);
        NOW.store(12_667, Ordering::Relaxed);
        h.run(128).unwrap();
        let times: Vec<u32> = h.events(MidiInPorts::OUT).iter().map(|e| e.time).collect();
        assert_eq!(times, vec![24, 48]);
    }

    #[test]
    fn notes_become_note_events_and_the_rest_passes_through() {
        let (mut h, bus) = node();
        bus.send(key_on(0, 60, 127));
        bus.send([0xB0, 1, 64]);
        bus.send([0x80, 60, 0]);
        h.run(16).unwrap();
        let events = h.events(MidiInPorts::OUT).to_vec();
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].kind,
            EventKind::NoteOn {
                note: note_id(0, 60),
                channel: 0,
                key: 60,
                velocity: 1.0
            }
        );
        assert_eq!(events[1].kind, EventKind::Midi([0xB0, 1, 64]));
        assert!(
            matches!(events[2].kind, EventKind::NoteOff { note, .. } if note == note_id(0, 60))
        );
        // A block with nothing new is empty.
        h.run(16).unwrap();
        assert!(h.events(MidiInPorts::OUT).is_empty());
    }

    fn send2(bus: &MidiBus, message: Midi2) {
        bus.send_packet(message.encode(0));
    }

    fn note_on2(channel: u8, key: u8, velocity: u16, attribute: Attribute) -> Midi2 {
        Midi2::NoteOn {
            channel,
            key,
            velocity,
            attribute,
        }
    }

    #[test]
    fn midi2_notes_keep_their_16_bit_velocity_and_pitch_attribute() {
        let (mut h, bus) = node();
        send2(&bus, note_on2(1, 60, 0xFFFF, Attribute::None));
        // Velocity 0 is not a note-off in MIDI 2.0: it is the softest note.
        send2(&bus, note_on2(1, 61, 0, Attribute::None));
        send2(&bus, note_on2(1, 62, 0x8000, Attribute::Pitch(62.5)));
        send2(
            &bus,
            Midi2::NoteOff {
                channel: 1,
                key: 60,
                velocity: 0x4000,
                attribute: Attribute::None,
            },
        );
        h.run(16).unwrap();
        let events: Vec<EventKind> = h.events(MidiInPorts::OUT).iter().map(|e| e.kind).collect();
        assert_eq!(
            events[0],
            EventKind::NoteOn {
                note: note_id(1, 60),
                channel: 1,
                key: 60,
                velocity: 1.0
            }
        );
        assert!(
            matches!(events[1], EventKind::NoteOn { key: 61, velocity, .. } if velocity > 0.0 && velocity < 1e-4)
        );
        assert!(matches!(
            events[2],
            EventKind::NoteOn { key: 62, velocity, .. } if (velocity - 0.5).abs() < 1e-4
        ));
        // The attribute puts the note half a semitone up.
        assert_eq!(
            events[3],
            EventKind::Expression {
                note: note_id(1, 62),
                expression: Expression::Pitch,
                value: 0.5
            }
        );
        assert!(matches!(
            events[4],
            EventKind::NoteOff { note, velocity } if note == note_id(1, 60) && (velocity - 0.25).abs() < 1e-3
        ));
    }

    #[test]
    fn a_per_note_bend_adds_to_the_pitch_attribute_instead_of_replacing_it() {
        let (mut h, bus) = node();
        send2(&bus, note_on2(0, 62, 0x8000, Attribute::Pitch(62.5)));
        // A controller starting a touch sends a centred bend.
        let bend = |value| Midi2::PerNotePitchBend {
            channel: 0,
            key: 62,
            value,
        };
        send2(&bus, bend(0x8000_0000));
        // A quarter of the way up: 12 semitones at the default range of 48.
        send2(&bus, bend(0xA000_0000));
        // Pitch 7.25 sets a new base, which the bend still rides on.
        send2(
            &bus,
            Midi2::PerNoteController {
                channel: 0,
                key: 62,
                registered: true,
                index: 3,
                value: 63 << 25,
            },
        );
        h.run(16).unwrap();
        let pitches: Vec<f32> = h
            .events(MidiInPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::Expression {
                    expression: Expression::Pitch,
                    value,
                    ..
                } => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(pitches, vec![0.5, 0.5, 12.5, 13.0]);
    }

    #[test]
    fn midi2_per_note_messages_become_expressions_on_their_note() {
        let (mut h, bus) = node();
        send2(&bus, note_on2(0, 60, 0x8000, Attribute::None));
        send2(
            &bus,
            Midi2::PerNotePitchBend {
                channel: 0,
                key: 60,
                value: 0xC000_0000, // halfway up
            },
        );
        send2(
            &bus,
            Midi2::PolyPressure {
                channel: 0,
                key: 60,
                value: u32::MAX,
            },
        );
        for (index, value) in [(74, u32::MAX), (10, 0), (7, u32::MAX), (9, 5)] {
            send2(
                &bus,
                Midi2::PerNoteController {
                    channel: 0,
                    key: 60,
                    registered: true,
                    index,
                    value,
                },
            );
        }
        h.run(16).unwrap();
        let expressions: Vec<(Expression, f32)> = h
            .events(MidiInPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::Expression {
                    note,
                    expression,
                    value,
                } => {
                    assert_eq!(note, note_id(0, 60));
                    Some((expression, value))
                }
                _ => None,
            })
            .collect();
        // The default note bend is 48 semitones; controller 9 is unknown.
        assert_eq!(
            expressions,
            vec![
                (Expression::Pitch, 24.0),
                (Expression::Pressure, 1.0),
                (Expression::Timbre, 1.0),
                (Expression::Pan, -1.0),
                (Expression::Volume, 1.0),
            ]
        );
    }

    #[test]
    fn midi2_controllers_bend_and_pressure_keep_their_32_bits() {
        let (mut h, bus) = node();
        for (value, control) in [
            (0x1234_5678, ChannelControl::Controller(74)),
            (0x8000_0001, ChannelControl::PitchBend),
            (0xFEDC_BA98, ChannelControl::Pressure),
        ] {
            send2(
                &bus,
                match control {
                    ChannelControl::Controller(index) => Midi2::Controller {
                        channel: 3,
                        index,
                        value,
                    },
                    ChannelControl::PitchBend => Midi2::PitchBend { channel: 3, value },
                    ChannelControl::Pressure => Midi2::ChannelPressure { channel: 3, value },
                },
            );
        }
        h.run(16).unwrap();
        let kinds: Vec<EventKind> = h.events(MidiInPorts::OUT).iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::Channel {
                    channel: 3,
                    control: ChannelControl::Controller(74),
                    value: 0x1234_5678
                },
                EventKind::Channel {
                    channel: 3,
                    control: ChannelControl::PitchBend,
                    value: 0x8000_0001
                },
                EventKind::Channel {
                    channel: 3,
                    control: ChannelControl::Pressure,
                    value: 0xFEDC_BA98
                },
            ]
        );
    }

    #[test]
    fn midi2_channel_messages_are_narrowed_to_midi1() {
        let (mut h, bus) = node();
        send2(
            &bus,
            Midi2::Controller {
                channel: 2,
                index: 1,
                value: 0x8000_0000,
            },
        );
        send2(
            &bus,
            Midi2::PitchBend {
                channel: 2,
                value: 0x8000_0000,
            },
        );
        send2(
            &bus,
            Midi2::Program {
                channel: 2,
                program: 5,
                bank: Some((1, 2)),
            },
        );
        send2(
            &bus,
            Midi2::Parameter {
                channel: 2,
                registered: true,
                bank: 0,
                index: 0,
                value: 0x8000_0000,
            },
        );
        // A packet that is not channel voice is dropped.
        bus.send_packet(Packet::from_words(&[0x1000_0000]).unwrap());
        h.run(16).unwrap();
        let midi: Vec<[u8; 3]> = h
            .events(MidiInPorts::OUT)
            .iter()
            .map(|e| e.kind.midi1().unwrap_or_else(|| panic!("{:?}", e.kind)))
            .collect();
        assert_eq!(
            midi,
            vec![
                [0xB2, 1, 64],
                [0xE2, 0, 64],
                [0xB2, 0, 1],
                [0xB2, 32, 2],
                [0xC2, 5, 0],
                [0xB2, 101, 0],
                [0xB2, 100, 0],
                [0xB2, 6, 64],
                [0xB2, 38, 0],
            ]
        );
    }

    #[test]
    fn midi2_notes_follow_the_channel_filter_and_all_notes_off() {
        let (mut h, bus) = node();
        send2(&bus, note_on2(0, 60, 0x8000, Attribute::None));
        send2(&bus, note_on2(0, 64, 0x8000, Attribute::None));
        send2(
            &bus,
            Midi2::Controller {
                channel: 0,
                index: 123,
                value: 0,
            },
        );
        h.run(16).unwrap();
        let ended: Vec<u8> = h
            .events(MidiInPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOff { note, .. } => Some(note.0 as u8),
                _ => None,
            })
            .collect();
        assert_eq!(ended, vec![60, 64]);
        // Another channel is filtered out when `channel` asks for one.
        h.set(MidiInPorts::CHANNEL, 2.0);
        send2(&bus, note_on2(0, 60, 0x8000, Attribute::None));
        send2(&bus, note_on2(1, 61, 0x8000, Attribute::None));
        h.run(16).unwrap();
        let keys: Vec<u8> = h
            .events(MidiInPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOn { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec![61]);
    }

    #[test]
    fn a_note_on_with_no_velocity_is_a_note_off() {
        let (mut h, bus) = node();
        bus.send(key_on(2, 64, 100));
        bus.send(key_on(2, 64, 0));
        h.run(16).unwrap();
        let events = h.events(MidiInPorts::OUT);
        assert!(matches!(
            events[0].kind,
            EventKind::NoteOn { channel: 2, .. }
        ));
        assert!(
            matches!(events[1].kind, EventKind::NoteOff { note, .. } if note == note_id(2, 64))
        );
    }

    #[test]
    fn the_channel_parameter_filters() {
        let (mut h, bus) = node();
        h.set(MidiInPorts::CHANNEL, 3.0);
        bus.send(key_on(0, 60, 100));
        bus.send(key_on(2, 61, 100));
        bus.send(key_on(3, 62, 100));
        h.run(16).unwrap();
        let keys: Vec<u8> = h
            .events(MidiInPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOn { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(keys, [61]);
    }

    #[test]
    fn changing_the_channel_does_not_strand_a_held_note() {
        let (mut h, bus) = node();
        bus.send(key_on(2, 64, 100));
        h.run(16).unwrap();
        h.set(MidiInPorts::CHANNEL, 1.0);
        bus.send([0x82, 64, 0]);
        bus.send(key_on(2, 65, 100));
        h.run(16).unwrap();
        let events = h.events(MidiInPorts::OUT);
        // The held note ends; the new one is on a filtered channel.
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(
            matches!(events[0].kind, EventKind::NoteOff { note, .. } if note == note_id(2, 64))
        );
        // And a stray note-off for a note that isn't held is filtered.
        bus.send([0x82, 70, 0]);
        h.run(16).unwrap();
        assert!(h.events(MidiInPorts::OUT).is_empty());
    }

    #[test]
    fn all_notes_off_is_finished_in_a_later_block_if_the_buffer_fills() {
        let (mut h, bus) = node();
        for key in 0..60 {
            bus.send(key_on(0, key, 100));
        }
        h.run(16).unwrap();
        // 984 controllers leave room for 40 of the 60 note-offs.
        for i in 0..984u16 {
            bus.send([0xB0, 1, (i % 128) as u8]);
        }
        bus.send([0xB0, 123, 0]);
        h.run(16).unwrap();
        let count = |h: &Harness| {
            h.events(MidiInPorts::OUT)
                .iter()
                .filter(|e| matches!(e.kind, EventKind::NoteOff { .. }))
                .count()
        };
        let first = count(&h);
        assert_eq!(first, 40);
        h.run(16).unwrap();
        assert_eq!(first + count(&h), 60, "every held note ends, none is lost");
    }

    #[test]
    fn all_notes_off_ends_what_is_held() {
        let (mut h, bus) = node();
        bus.send(key_on(0, 60, 100));
        bus.send(key_on(0, 64, 100));
        bus.send(key_on(1, 67, 100));
        h.run(16).unwrap();
        bus.send([0xB0, 123, 0]);
        h.run(16).unwrap();
        let offs: Vec<NoteId> = h
            .events(MidiInPorts::OUT)
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOff { note, .. } => Some(note),
                _ => None,
            })
            .collect();
        // Channel 1's note is left alone.
        assert_eq!(offs, [note_id(0, 60), note_id(0, 64)]);
    }
}
