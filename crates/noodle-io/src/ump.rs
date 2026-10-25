//! Universal MIDI Packets (the transport of MIDI 2.0) and the MIDI 2.0
//! channel voice messages they carry.
//!
//! A packet is one to four 32-bit words; the top four bits of the first say
//! what kind it is and so how long. The engine takes the two kinds that
//! carry channel voice messages: type 2 (a MIDI 1.0 message, 32 bits) and
//! type 4 (a MIDI 2.0 message, 64 bits, with 16-bit velocity, 32-bit
//! controllers, pitch bend and pressure, per-note controllers and pitch
//! bend, and note attributes). Everything else (utility, system, sysex,
//! flex data, streams) is carried and then dropped by the nodes.
//!
//! [`Midi2`] decodes and encodes type 4, and converts to and from MIDI 1.0
//! with the scaling the specification gives: values widen by repeating their
//! bits above the centre so that minimum, centre and maximum stay put, and
//! narrow by dropping the low bits.

use crate::midi::MidiMessage;

/// One packet: up to two words are kept (the longest the engine takes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    words: [u32; 2],
    len: u8,
}

/// The number of 32-bit words in a packet of this message type.
pub const fn packet_words(message_type: u8) -> usize {
    match message_type {
        0..=2 | 6 | 7 => 1,
        3 | 4 | 8 | 9 | 0xA => 2,
        0xB | 0xC => 3,
        _ => 4,
    }
}

impl Packet {
    /// A packet from its words, or `None` if they are too few or the packet
    /// is longer than two words (types the engine doesn't take).
    pub fn from_words(words: &[u32]) -> Option<Packet> {
        let first = *words.first()?;
        let len = packet_words(message_type_of(first));
        if len > 2 || words.len() < len {
            return None;
        }
        Some(Packet {
            words: [first, if len == 2 { words[1] } else { 0 }],
            len: len as u8,
        })
    }

    /// A MIDI 1.0 channel voice message in a type 2 packet.
    pub fn midi1(group: u8, [status, a, b]: MidiMessage) -> Packet {
        Packet {
            words: [
                0x2 << 28
                    | u32::from(group & 0xF) << 24
                    | u32::from(status) << 16
                    | u32::from(a & 0x7F) << 8
                    | u32::from(b & 0x7F),
                0,
            ],
            len: 1,
        }
    }

    pub fn words(&self) -> &[u32] {
        &self.words[..usize::from(self.len)]
    }

    pub fn message_type(&self) -> u8 {
        message_type_of(self.words[0])
    }

    pub fn group(&self) -> u8 {
        (self.words[0] >> 24 & 0xF) as u8
    }

    /// The message if this is a type 2 packet (MIDI 1.0 channel voice).
    pub fn as_midi1(&self) -> Option<MidiMessage> {
        if self.message_type() != 2 {
            return None;
        }
        let w = self.words[0];
        let status = (w >> 16) as u8;
        (0x80..0xF0)
            .contains(&status)
            .then_some([status, (w >> 8) as u8 & 0x7F, w as u8 & 0x7F])
    }

    /// The message if this is a type 4 packet (MIDI 2.0 channel voice) the
    /// engine understands.
    pub fn as_midi2(&self) -> Option<Midi2> {
        if self.message_type() != 4 {
            return None;
        }
        Midi2::decode(self.words[0], self.words[1])
    }

    /// Whether this is a note-on or note-off of either kind.
    pub fn is_note(&self) -> bool {
        match self.message_type() {
            2 => self
                .as_midi1()
                .is_some_and(|m| matches!(m[0] & 0xF0, 0x80 | 0x90)),
            4 => matches!(
                self.as_midi2(),
                Some(Midi2::NoteOn { .. } | Midi2::NoteOff { .. })
            ),
            _ => false,
        }
    }

    /// The same as MIDI 1.0 messages, for what reads only those (the first
    /// of up to four; see [`Midi2::to_midi1`]). `None` for a packet that has
    /// no MIDI 1.0 form.
    pub fn to_midi1(&self) -> Option<MidiMessage> {
        match self.message_type() {
            2 => self.as_midi1(),
            4 => {
                let mut out = [[0; 3]; 4];
                let n = self.as_midi2()?.to_midi1(&mut out);
                (n > 0).then_some(out[0])
            }
            _ => None,
        }
    }
}

fn message_type_of(word: u32) -> u8 {
    (word >> 28) as u8
}

/// Splits a stream of words into packets, for a source that delivers them
/// in arbitrary pieces. Packets the engine doesn't take are skipped whole.
#[derive(Default)]
pub struct PacketReader {
    pending: [u32; 4],
    have: usize,
}

impl PacketReader {
    /// Feeds one word; returns a packet when it completes one the engine
    /// takes.
    pub fn push(&mut self, word: u32) -> Option<Packet> {
        self.pending[self.have] = word;
        self.have += 1;
        let need = packet_words(message_type_of(self.pending[0]));
        if self.have < need {
            return None;
        }
        self.have = 0;
        Packet::from_words(&self.pending[..need])
    }
}

/// A note attribute (type 4 note-on and note-off).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Attribute {
    None,
    ManufacturerSpecific(u16),
    ProfileSpecific(u16),
    /// The note's pitch in semitones as a fraction of key numbers (7.9
    /// fixed point on the wire), for tunings that put a key between others.
    Pitch(f32),
}

impl Attribute {
    fn decode(kind: u8, data: u16) -> Attribute {
        match kind {
            1 => Attribute::ManufacturerSpecific(data),
            2 => Attribute::ProfileSpecific(data),
            3 => Attribute::Pitch(f32::from(data) / 512.0),
            _ => Attribute::None,
        }
    }

    fn encode(self) -> (u8, u16) {
        match self {
            Attribute::None => (0, 0),
            Attribute::ManufacturerSpecific(d) => (1, d),
            Attribute::ProfileSpecific(d) => (2, d),
            Attribute::Pitch(p) => (3, (p.clamp(0.0, 127.998) * 512.0).round() as u16),
        }
    }
}

/// A MIDI 2.0 channel voice message. Velocities are 16 bits; every other
/// value is 32.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Midi2 {
    NoteOn {
        channel: u8,
        key: u8,
        velocity: u16,
        attribute: Attribute,
    },
    NoteOff {
        channel: u8,
        key: u8,
        velocity: u16,
        attribute: Attribute,
    },
    PolyPressure {
        channel: u8,
        key: u8,
        value: u32,
    },
    /// A per-note controller: registered (index 1 modulation, 7 volume, 10
    /// pan, 74 brightness...) or assignable.
    PerNoteController {
        channel: u8,
        key: u8,
        registered: bool,
        index: u8,
        value: u32,
    },
    /// Centre is `0x8000_0000`.
    PerNotePitchBend {
        channel: u8,
        key: u8,
        value: u32,
    },
    /// Bit 1 detaches per-note controllers from the key, bit 0 resets them.
    PerNoteManagement {
        channel: u8,
        key: u8,
        flags: u8,
    },
    Controller {
        channel: u8,
        index: u8,
        value: u32,
    },
    /// A registered or assignable parameter: one 32-bit message for what
    /// MIDI 1.0 sent as four controllers.
    Parameter {
        channel: u8,
        registered: bool,
        bank: u8,
        index: u8,
        value: u32,
    },
    Program {
        channel: u8,
        program: u8,
        bank: Option<(u8, u8)>,
    },
    ChannelPressure {
        channel: u8,
        value: u32,
    },
    /// Centre is `0x8000_0000`.
    PitchBend {
        channel: u8,
        value: u32,
    },
}

impl Midi2 {
    pub fn channel(&self) -> u8 {
        match *self {
            Midi2::NoteOn { channel, .. }
            | Midi2::NoteOff { channel, .. }
            | Midi2::PolyPressure { channel, .. }
            | Midi2::PerNoteController { channel, .. }
            | Midi2::PerNotePitchBend { channel, .. }
            | Midi2::PerNoteManagement { channel, .. }
            | Midi2::Controller { channel, .. }
            | Midi2::Parameter { channel, .. }
            | Midi2::Program { channel, .. }
            | Midi2::ChannelPressure { channel, .. }
            | Midi2::PitchBend { channel, .. } => channel,
        }
    }

    /// Reads the two words of a type 4 packet. `None` for a status the
    /// engine doesn't take (relative parameters).
    pub fn decode(w0: u32, w1: u32) -> Option<Midi2> {
        let opcode = (w0 >> 20 & 0xF) as u8;
        let channel = (w0 >> 16 & 0xF) as u8;
        let a = (w0 >> 8 & 0xFF) as u8;
        let b = (w0 & 0xFF) as u8;
        let key = a & 0x7F;
        Some(match opcode {
            0x8 | 0x9 => {
                let velocity = (w1 >> 16) as u16;
                let attribute = Attribute::decode(b, w1 as u16);
                if opcode == 0x9 {
                    Midi2::NoteOn {
                        channel,
                        key,
                        velocity,
                        attribute,
                    }
                } else {
                    Midi2::NoteOff {
                        channel,
                        key,
                        velocity,
                        attribute,
                    }
                }
            }
            0xA => Midi2::PolyPressure {
                channel,
                key,
                value: w1,
            },
            0x0 | 0x1 => Midi2::PerNoteController {
                channel,
                key,
                registered: opcode == 0,
                index: b,
                value: w1,
            },
            0x6 => Midi2::PerNotePitchBend {
                channel,
                key,
                value: w1,
            },
            0xF => Midi2::PerNoteManagement {
                channel,
                key,
                flags: b,
            },
            0xB => Midi2::Controller {
                channel,
                index: a & 0x7F,
                value: w1,
            },
            0x2 | 0x3 => Midi2::Parameter {
                channel,
                registered: opcode == 2,
                bank: a & 0x7F,
                index: b & 0x7F,
                value: w1,
            },
            0xC => Midi2::Program {
                channel,
                program: (w1 >> 24) as u8 & 0x7F,
                bank: (b & 1 == 1).then_some(((w1 >> 8) as u8 & 0x7F, w1 as u8 & 0x7F)),
            },
            0xD => Midi2::ChannelPressure { channel, value: w1 },
            0xE => Midi2::PitchBend { channel, value: w1 },
            _ => return None,
        })
    }

    /// The type 4 packet for this message, in `group`.
    pub fn encode(&self, group: u8) -> Packet {
        let head = |opcode: u32, channel: u8, a: u8, b: u8| -> u32 {
            0x4 << 28
                | u32::from(group & 0xF) << 24
                | opcode << 20
                | u32::from(channel & 0xF) << 16
                | u32::from(a) << 8
                | u32::from(b)
        };
        let (w0, w1) = match *self {
            Midi2::NoteOn {
                channel,
                key,
                velocity,
                attribute,
            }
            | Midi2::NoteOff {
                channel,
                key,
                velocity,
                attribute,
            } => {
                let opcode = if matches!(self, Midi2::NoteOn { .. }) {
                    0x9
                } else {
                    0x8
                };
                let (kind, data) = attribute.encode();
                (
                    head(opcode, channel, key, kind),
                    u32::from(velocity) << 16 | u32::from(data),
                )
            }
            Midi2::PolyPressure {
                channel,
                key,
                value,
            } => (head(0xA, channel, key, 0), value),
            Midi2::PerNoteController {
                channel,
                key,
                registered,
                index,
                value,
            } => (
                head(if registered { 0 } else { 1 }, channel, key, index),
                value,
            ),
            Midi2::PerNotePitchBend {
                channel,
                key,
                value,
            } => (head(0x6, channel, key, 0), value),
            Midi2::PerNoteManagement {
                channel,
                key,
                flags,
            } => (head(0xF, channel, key, flags), 0),
            Midi2::Controller {
                channel,
                index,
                value,
            } => (head(0xB, channel, index, 0), value),
            Midi2::Parameter {
                channel,
                registered,
                bank,
                index,
                value,
            } => (
                head(if registered { 2 } else { 3 }, channel, bank, index),
                value,
            ),
            Midi2::Program {
                channel,
                program,
                bank,
            } => {
                let (msb, lsb) = bank.unwrap_or((0, 0));
                (
                    head(0xC, channel, 0, u8::from(bank.is_some())),
                    u32::from(program) << 24 | u32::from(msb) << 8 | u32::from(lsb),
                )
            }
            Midi2::ChannelPressure { channel, value } => (head(0xD, channel, 0, 0), value),
            Midi2::PitchBend { channel, value } => (head(0xE, channel, 0, 0), value),
        };
        Packet {
            words: [w0, w1],
            len: 2,
        }
    }

    /// Widens a MIDI 1.0 message, as the specification's translation does.
    /// A note-on with velocity 0 becomes a note-off. `None` for what has no
    /// MIDI 2.0 channel voice form (a lone controller that is part of a
    /// parameter sequence is kept as a controller).
    pub fn from_midi1([status, a, b]: MidiMessage) -> Option<Midi2> {
        let channel = status & 0x0F;
        let a = a & 0x7F;
        let b = b & 0x7F;
        Some(match status & 0xF0 {
            0x80 | 0x90 if status & 0xF0 == 0x80 || b == 0 => Midi2::NoteOff {
                channel,
                key: a,
                velocity: widen(u32::from(b), 7, 16) as u16,
                attribute: Attribute::None,
            },
            0x90 => Midi2::NoteOn {
                channel,
                key: a,
                velocity: widen(u32::from(b), 7, 16) as u16,
                attribute: Attribute::None,
            },
            0xA0 => Midi2::PolyPressure {
                channel,
                key: a,
                value: widen(u32::from(b), 7, 32),
            },
            0xB0 => Midi2::Controller {
                channel,
                index: a,
                value: widen(u32::from(b), 7, 32),
            },
            0xC0 => Midi2::Program {
                channel,
                program: a,
                bank: None,
            },
            0xD0 => Midi2::ChannelPressure {
                channel,
                value: widen(u32::from(a), 7, 32),
            },
            0xE0 => Midi2::PitchBend {
                channel,
                value: widen(u32::from(b) << 7 | u32::from(a), 14, 32),
            },
            _ => return None,
        })
    }

    /// Narrows to MIDI 1.0 messages, written to `out`; returns how many (up
    /// to four). Per-note messages have no MIDI 1.0 form and give none, and a
    /// parameter becomes its four controllers. A note-on keeps a velocity of
    /// at least 1, since 0 would end the note.
    pub fn to_midi1(&self, out: &mut [MidiMessage; 4]) -> usize {
        let seven = |v: u32, bits: u32| (v >> (bits - 7)) as u8;
        match *self {
            Midi2::NoteOn {
                channel,
                key,
                velocity,
                ..
            } => {
                out[0] = [0x90 | channel, key, seven(u32::from(velocity), 16).max(1)];
                1
            }
            Midi2::NoteOff {
                channel,
                key,
                velocity,
                ..
            } => {
                out[0] = [0x80 | channel, key, seven(u32::from(velocity), 16)];
                1
            }
            Midi2::PolyPressure {
                channel,
                key,
                value,
            } => {
                out[0] = [0xA0 | channel, key, seven(value, 32)];
                1
            }
            Midi2::Controller {
                channel,
                index,
                value,
            } => {
                out[0] = [0xB0 | channel, index, seven(value, 32)];
                1
            }
            Midi2::Parameter {
                channel,
                registered,
                bank,
                index,
                value,
            } => {
                let (hi, lo) = if registered { (101, 100) } else { (99, 98) };
                let status = 0xB0 | channel;
                out[0] = [status, hi, bank];
                out[1] = [status, lo, index];
                out[2] = [status, 6, seven(value, 32)];
                out[3] = [status, 38, (value >> 18) as u8 & 0x7F];
                4
            }
            Midi2::Program {
                channel, program, ..
            } => {
                out[0] = [0xC0 | channel, program, 0];
                1
            }
            Midi2::ChannelPressure { channel, value } => {
                out[0] = [0xD0 | channel, seven(value, 32), 0];
                1
            }
            Midi2::PitchBend { channel, value } => {
                let v = value >> 18;
                out[0] = [0xE0 | channel, v as u8 & 0x7F, (v >> 7) as u8 & 0x7F];
                1
            }
            Midi2::PerNoteController { .. }
            | Midi2::PerNotePitchBend { .. }
            | Midi2::PerNoteManagement { .. } => 0,
        }
    }
}

/// Widens `value` of `from` bits to `to` bits so that 0, the centre and the
/// maximum keep their places: the bits are shifted up, and above the centre
/// the bits below the top one repeat to fill the gap.
pub fn widen(value: u32, from: u32, to: u32) -> u32 {
    let shift = to - from;
    let shifted = value << shift;
    let centre = 1u32 << (from - 1);
    if value <= centre {
        return shifted;
    }
    let repeat_bits = from - 1;
    let mut repeat = value & ((1 << repeat_bits) - 1);
    repeat = if shift > repeat_bits {
        repeat << (shift - repeat_bits)
    } else {
        repeat >> (repeat_bits - shift)
    };
    let mut out = shifted;
    while repeat != 0 {
        out |= repeat;
        repeat >>= repeat_bits;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widening_keeps_the_ends_and_the_centre() {
        assert_eq!(widen(0, 7, 16), 0);
        assert_eq!(widen(64, 7, 16), 0x8000);
        assert_eq!(widen(127, 7, 16), 0xFFFF);
        assert_eq!(widen(127, 7, 32), 0xFFFF_FFFF);
        assert_eq!(widen(0x2000, 14, 32), 0x8000_0000);
        assert_eq!(widen(0x3FFF, 14, 32), 0xFFFF_FFFF);
        // Below the centre it is only shifted.
        assert_eq!(widen(1, 7, 16), 1 << 9);
        // Every widened value narrows back to itself.
        for v in 0..128 {
            assert_eq!(widen(v, 7, 16) >> 9, v);
        }
    }

    #[test]
    fn packets_round_trip_through_their_words() {
        let notes = [
            Midi2::NoteOn {
                channel: 3,
                key: 60,
                velocity: 0xABCD,
                attribute: Attribute::None,
            },
            Midi2::NoteOff {
                channel: 3,
                key: 60,
                velocity: 0x1234,
                attribute: Attribute::None,
            },
            Midi2::NoteOn {
                channel: 15,
                key: 127,
                velocity: 1,
                attribute: Attribute::Pitch(60.5),
            },
            Midi2::PolyPressure {
                channel: 1,
                key: 5,
                value: 0xDEAD_BEEF,
            },
            Midi2::PerNoteController {
                channel: 2,
                key: 70,
                registered: true,
                index: 74,
                value: 0x8000_0000,
            },
            Midi2::PerNoteController {
                channel: 2,
                key: 70,
                registered: false,
                index: 9,
                value: 7,
            },
            Midi2::PerNotePitchBend {
                channel: 4,
                key: 9,
                value: 0x9000_0000,
            },
            Midi2::PerNoteManagement {
                channel: 4,
                key: 9,
                flags: 3,
            },
            Midi2::Controller {
                channel: 0,
                index: 1,
                value: 0x1234_5678,
            },
            Midi2::Parameter {
                channel: 0,
                registered: true,
                bank: 0,
                index: 2,
                value: 99,
            },
            Midi2::Parameter {
                channel: 0,
                registered: false,
                bank: 3,
                index: 4,
                value: 5,
            },
            Midi2::Program {
                channel: 9,
                program: 12,
                bank: Some((1, 2)),
            },
            Midi2::Program {
                channel: 9,
                program: 12,
                bank: None,
            },
            Midi2::ChannelPressure {
                channel: 7,
                value: 0x4000_0000,
            },
            Midi2::PitchBend {
                channel: 7,
                value: 0x8000_0000,
            },
        ];
        for message in notes {
            let packet = message.encode(5);
            assert_eq!(packet.message_type(), 4);
            assert_eq!(packet.group(), 5);
            assert_eq!(packet.words().len(), 2);
            assert_eq!(packet.as_midi2(), Some(message), "{message:?}");
            assert_eq!(Packet::from_words(packet.words()), Some(packet));
        }
    }

    #[test]
    fn midi1_packets_carry_the_message_and_widen_to_midi2() {
        let packet = Packet::midi1(2, [0x93, 60, 100]);
        assert_eq!(packet.words(), [0x2293_3C64]);
        assert_eq!(packet.as_midi1(), Some([0x93, 60, 100]));
        assert_eq!(packet.as_midi2(), None);
        assert!(packet.is_note());
        assert_eq!(
            Midi2::from_midi1([0x93, 60, 100]),
            Some(Midi2::NoteOn {
                channel: 3,
                key: 60,
                velocity: widen(100, 7, 16) as u16,
                attribute: Attribute::None
            })
        );
        // A note-on with no velocity is a note-off.
        assert!(matches!(
            Midi2::from_midi1([0x90, 60, 0]),
            Some(Midi2::NoteOff { .. })
        ));
        assert_eq!(
            Midi2::from_midi1([0xE0, 0, 0x40]),
            Some(Midi2::PitchBend {
                channel: 0,
                value: 0x8000_0000
            })
        );
    }

    #[test]
    fn narrowing_gives_midi1_messages() {
        let mut out = [[0; 3]; 4];
        let on = Midi2::NoteOn {
            channel: 1,
            key: 64,
            velocity: 0x0001,
            attribute: Attribute::None,
        };
        // A tiny velocity must not become a note-off.
        assert_eq!(on.to_midi1(&mut out), 1);
        assert_eq!(out[0], [0x91, 64, 1]);
        let rpn = Midi2::Parameter {
            channel: 2,
            registered: true,
            bank: 0,
            index: 2,
            value: 0x8000_0000,
        };
        assert_eq!(rpn.to_midi1(&mut out), 4);
        assert_eq!(
            out,
            [[0xB2, 101, 0], [0xB2, 100, 2], [0xB2, 6, 64], [0xB2, 38, 0]]
        );
        let bend = Midi2::PitchBend {
            channel: 0,
            value: 0x8000_0000,
        };
        assert_eq!(bend.to_midi1(&mut out), 1);
        assert_eq!(out[0], [0xE0, 0, 0x40]);
        // Per-note messages have no MIDI 1.0 form.
        let per_note = Midi2::PerNotePitchBend {
            channel: 0,
            key: 60,
            value: 0,
        };
        assert_eq!(per_note.to_midi1(&mut out), 0);
        // Widening and narrowing a MIDI 1.0 message gives it back.
        for msg in [
            [0xB4, 7, 100],
            [0xE1, 5, 0x7F],
            [0xD0, 99, 0],
            [0xA0, 60, 1],
        ] {
            let wide = Midi2::from_midi1(msg).unwrap();
            assert_eq!(wide.to_midi1(&mut out), 1);
            assert_eq!(out[0], msg);
        }
    }

    #[test]
    fn a_reader_splits_words_into_packets_and_skips_the_rest() {
        let mut reader = PacketReader::default();
        let note = Midi2::NoteOn {
            channel: 0,
            key: 60,
            velocity: 0xFFFF,
            attribute: Attribute::None,
        }
        .encode(0);
        let mut got = Vec::new();
        // A utility packet (1 word), a sysex7 packet (2 words, not taken
        // here as a message but whole), a MIDI 1.0 packet, then the note.
        let words = [
            0x0000_0000,
            0x3010_0102,
            0x0304_0000,
            Packet::midi1(0, [0x90, 60, 100]).words()[0],
            note.words()[0],
            note.words()[1],
        ];
        for word in words {
            got.extend(reader.push(word));
        }
        assert_eq!(got.len(), 4);
        assert_eq!(got[2].as_midi1(), Some([0x90, 60, 100]));
        assert_eq!(got[3], note);
        // A 128-bit packet is skipped whole.
        let mut reader = PacketReader::default();
        let mut got = Vec::new();
        for word in [0xF000_0000, 1, 2, 3, note.words()[0], note.words()[1]] {
            got.extend(reader.push(word));
        }
        assert_eq!(got, vec![note]);
    }
}
