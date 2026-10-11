//! The computer keyboard as a MIDI source, like Ableton's: the home row is an
//! octave of piano keys, and a few keys around it change the octave and the
//! velocity.
//!
//! The keys become MIDI messages on the session's bus, so the MIDI In nodes
//! hear them as they hear a hardware port, and recording takes them the same
//! way. The state here is the app's, not the project's, and starts off.

use egui::Key;
use noodle_io::MidiMessage;

/// The semitone above the octave's C that a key plays, if it is a note key.
/// A W S E D F T G Y H U J K is C to the C above, and O L P ; carry on to the
/// next D#.
fn semitone(key: Key) -> Option<u8> {
    Some(match key {
        Key::A => 0,
        Key::W => 1,
        Key::S => 2,
        Key::E => 3,
        Key::D => 4,
        Key::F => 5,
        Key::T => 6,
        Key::G => 7,
        Key::Y => 8,
        Key::H => 9,
        Key::U => 10,
        Key::J => 11,
        Key::K => 12,
        Key::O => 13,
        Key::L => 14,
        Key::P => 15,
        Key::Semicolon => 16,
        _ => return None,
    })
}

/// Whether the keyboard acts on `key` at all.
pub fn is_key(key: Key) -> bool {
    semitone(key).is_some() || matches!(key, Key::Z | Key::X | Key::C | Key::V)
}

/// The lowest and highest octave. C3 is note 60, so octave 3 starts there.
const OCTAVES: std::ops::RangeInclusive<i8> = -2..=8;
const VELOCITY_STEP: u8 = 16;
const CHANNEL: u8 = 0;

/// The state of the computer keyboard.
#[derive(Debug)]
pub struct KeyboardMidi {
    on: bool,
    octave: i8,
    velocity: u8,
    /// The keys down and the note each one started, since the octave may
    /// change before it is let go.
    held: Vec<(Key, u8)>,
}

impl Default for KeyboardMidi {
    fn default() -> Self {
        Self {
            on: false,
            octave: 3,
            velocity: 100,
            held: Vec::new(),
        }
    }
}

impl KeyboardMidi {
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Turns it on or off. Off lets go of every note held.
    pub fn set_on(&mut self, on: bool) -> Vec<MidiMessage> {
        self.on = on;
        if on { Vec::new() } else { self.release_all() }
    }

    pub fn octave(&self) -> i8 {
        self.octave
    }

    pub fn velocity(&self) -> u8 {
        self.velocity
    }

    /// The note a key plays at the current octave, if it is a note key and
    /// the note is a valid MIDI note.
    fn note(&self, key: Key) -> Option<u8> {
        let note = (i16::from(self.octave) + 2) * 12 + i16::from(semitone(key)?);
        u8::try_from(note).ok().filter(|&n| n < 128)
    }

    /// A key went down. Returns the messages it stands for.
    pub fn press(&mut self, key: Key) -> Vec<MidiMessage> {
        if !self.on {
            return Vec::new();
        }
        match key {
            Key::Z => self.octave = (self.octave - 1).max(*OCTAVES.start()),
            Key::X => self.octave = (self.octave + 1).min(*OCTAVES.end()),
            Key::C => self.velocity = self.velocity.saturating_sub(VELOCITY_STEP).max(1),
            Key::V => self.velocity = (self.velocity + VELOCITY_STEP).min(127),
            _ => {
                if self.held.iter().any(|&(k, _)| k == key) {
                    return Vec::new();
                }
                let Some(note) = self.note(key) else {
                    return Vec::new();
                };
                self.held.push((key, note));
                return vec![[0x90 | CHANNEL, note, self.velocity]];
            }
        }
        Vec::new()
    }

    /// A key came up.
    pub fn release(&mut self, key: Key) -> Vec<MidiMessage> {
        match self.held.iter().position(|&(k, _)| k == key) {
            Some(i) => {
                let (_, note) = self.held.remove(i);
                vec![[0x80 | CHANNEL, note, 0]]
            }
            None => Vec::new(),
        }
    }

    /// Lets go of everything held, for when the window loses focus or a text
    /// field takes the keys.
    pub fn release_all(&mut self) -> Vec<MidiMessage> {
        self.held
            .drain(..)
            .map(|(_, note)| [0x80 | CHANNEL, note, 0])
            .collect()
    }

    /// The octave's C as a name, such as "C3", for the button's label.
    pub fn octave_label(&self) -> String {
        format!("C{}", self.octave)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> KeyboardMidi {
        let mut keyboard = KeyboardMidi::default();
        keyboard.set_on(true);
        keyboard
    }

    #[test]
    fn the_home_row_plays_from_middle_c() {
        let mut keyboard = on();
        assert_eq!(keyboard.press(Key::A), vec![[0x90, 60, 100]]);
        assert_eq!(keyboard.press(Key::W), vec![[0x90, 61, 100]]);
        assert_eq!(keyboard.press(Key::K), vec![[0x90, 72, 100]]);
        assert_eq!(keyboard.release(Key::A), vec![[0x80, 60, 0]]);
    }

    #[test]
    fn a_key_held_down_does_not_retrigger() {
        let mut keyboard = on();
        assert_eq!(keyboard.press(Key::A).len(), 1);
        assert!(keyboard.press(Key::A).is_empty());
    }

    #[test]
    fn octave_and_velocity_keys_change_what_follows_and_a_held_note_ends_where_it_began() {
        let mut keyboard = on();
        keyboard.press(Key::A);
        keyboard.press(Key::X);
        assert_eq!(keyboard.octave(), 4);
        keyboard.press(Key::C);
        assert_eq!(keyboard.velocity(), 84);
        assert_eq!(keyboard.press(Key::S), vec![[0x90, 74, 84]]);
        // The first note is let go at the pitch it started.
        assert_eq!(keyboard.release(Key::A), vec![[0x80, 60, 0]]);
    }

    #[test]
    fn octave_and_velocity_stay_in_range() {
        let mut keyboard = on();
        for _ in 0..20 {
            keyboard.press(Key::Z);
            keyboard.press(Key::C);
        }
        assert_eq!((keyboard.octave(), keyboard.velocity()), (-2, 1));
        for _ in 0..20 {
            keyboard.press(Key::X);
            keyboard.press(Key::V);
        }
        assert_eq!((keyboard.octave(), keyboard.velocity()), (8, 127));
        // At octave 8 the top keys would be past note 127: no note.
        assert!(keyboard.press(Key::Semicolon).is_empty());
    }

    #[test]
    fn turning_it_off_lets_go_of_what_is_held() {
        let mut keyboard = on();
        keyboard.press(Key::A);
        keyboard.press(Key::D);
        let off = keyboard.set_on(false);
        assert_eq!(off, vec![[0x80, 60, 0], [0x80, 64, 0]]);
        // And while off, keys do nothing.
        assert!(keyboard.press(Key::A).is_empty());
    }
}
