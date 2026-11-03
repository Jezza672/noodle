//! Musical keys, and the shift that carries one onto another.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Major or minor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    Major,
    Minor,
}

/// A key: a tonic (0 is C, 11 is B) and a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key {
    pub tonic: u8,
    pub mode: Mode,
}

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

impl Key {
    pub fn new(tonic: u8, mode: Mode) -> Self {
        Self {
            tonic: tonic % 12,
            mode,
        }
    }

    /// The semitones that move a clip in `self` into `to`, the shortest way
    /// (-6 to 5). Keys of different modes are matched on their relative
    /// major or minor, so A minor to C major is no shift at all.
    pub fn shift_to(self, to: Key) -> i32 {
        let tonic = |k: Key| match k.mode {
            Mode::Major => i32::from(k.tonic),
            // The relative major of a minor key is three semitones up.
            Mode::Minor => (i32::from(k.tonic) + 3) % 12,
        };
        (tonic(to) - tonic(self) + 18).rem_euclid(12) - 6
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = match self.mode {
            Mode::Major => "major",
            Mode::Minor => "minor",
        };
        write!(f, "{} {mode}", NAMES[usize::from(self.tonic % 12)])
    }
}

/// What a clip is carried to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyTarget {
    /// The key of the timeline where the clip plays.
    Follow,
    Fixed(Key),
}

/// A clip's transposition: the key it is in, and the key it is carried to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Transpose {
    pub from: Key,
    pub to: KeyTarget,
}

impl Transpose {
    /// The shift in semitones, given the timeline's key where the clip plays.
    pub fn semitones(&self, timeline: Key) -> i32 {
        let to = match self.to {
            KeyTarget::Follow => timeline,
            KeyTarget::Fixed(k) => k,
        };
        self.from.shift_to(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_take_the_short_way() {
        let c = Key::new(0, Mode::Major);
        assert_eq!(c.shift_to(Key::new(7, Mode::Major)), -5);
        assert_eq!(c.shift_to(Key::new(2, Mode::Major)), 2);
        assert_eq!(c.shift_to(Key::new(6, Mode::Major)), -6);
        assert_eq!(c.shift_to(c), 0);
    }

    #[test]
    fn relative_keys_need_no_shift() {
        let c = Key::new(0, Mode::Major);
        assert_eq!(c.shift_to(Key::new(9, Mode::Minor)), 0);
    }

    #[test]
    fn follow_uses_the_timeline() {
        let t = Transpose {
            from: Key::new(0, Mode::Major),
            to: KeyTarget::Follow,
        };
        assert_eq!(t.semitones(Key::new(3, Mode::Major)), 3);
    }
}
