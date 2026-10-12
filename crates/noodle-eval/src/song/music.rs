//! Notes, chords and the little pattern language the songs are written in.
//!
//! A pattern is one bar written as steps, so the music reads on the page:
//!
//! ```text
//! drum:    X...x...X..ox...     X accent, x hit, o ghost, . rest
//! chord:   1..3..5.7-..5...     1-4 the chord's tones, 5-8 the same an octave up,
//!                               a-d an octave down, - holds the note on
//! melody:  0-2-4..3|2---....    digits are steps of the song's scale
//! ```
//!
//! Spaces and `|` are ignored. Every step is `step` beats long (a sixteenth,
//! 0.25, unless the part says otherwise). A part cycles through its patterns
//! bar by bar, and a fill replaces the last bar of a section.

/// One note, in beats from the start of its bar (in a [`Pattern`]'s result) or
/// of the song (once a part has been laid out).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Note {
    pub beat: f64,
    pub len: f64,
    pub key: u8,
    pub vel: f32,
}

/// A chord: a root and four tones above it, in semitones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chord {
    pub root: u8,
    pub tones: [u8; 4],
}

pub const MIN7: [u8; 4] = [0, 3, 7, 10];
pub const MAJ7: [u8; 4] = [0, 4, 7, 11];
pub const DOM7: [u8; 4] = [0, 4, 7, 10];
pub const MIN: [u8; 4] = [0, 3, 7, 12];
pub const MAJ: [u8; 4] = [0, 4, 7, 12];
pub const SUS: [u8; 4] = [0, 5, 7, 10];
pub const ADD9: [u8; 4] = [0, 4, 7, 14];

pub fn chord(root: u8, tones: [u8; 4]) -> Chord {
    Chord { root, tones }
}

impl Chord {
    /// Tone `index` (0 to 3), moved by `octaves`.
    pub fn key(&self, index: usize, octaves: i32) -> u8 {
        let key = i32::from(self.root) + i32::from(self.tones[index]) + 12 * octaves;
        key.clamp(0, 127) as u8
    }
}

/// Where in the song a bar falls.
#[derive(Debug, Clone, Copy)]
pub struct Bar {
    /// Which bar of its section, from 0.
    pub index: u32,
    /// How many bars the section has.
    pub bars: u32,
    pub chord: Chord,
}

impl Bar {
    pub fn last(&self) -> bool {
        self.index + 1 == self.bars
    }
}

/// What a part plays in a bar.
pub type Pattern = Box<dyn Fn(&Bar) -> Vec<Note> + Send + Sync>;

fn symbols(pattern: &str) -> Vec<char> {
    pattern
        .chars()
        .filter(|c| !matches!(c, ' ' | '|' | '_'))
        .collect()
}

/// A run of steps as notes: `key_of` gives a symbol's key and velocity, and
/// `-` holds the previous note on. `len` is the length of a note that is not
/// held (as a fraction of a step times `step`, in beats).
fn lay(
    pattern: &str,
    step: f64,
    len: f64,
    mut key_of: impl FnMut(char) -> Option<(u8, f32)>,
) -> Vec<Note> {
    let mut notes: Vec<Note> = Vec::new();
    let mut held_until = 0.0;
    for (i, symbol) in symbols(pattern).into_iter().enumerate() {
        let at = i as f64 * step;
        if symbol == '-' {
            if let Some(last) = notes.last_mut()
                && (held_until - at).abs() < 1e-9
            {
                last.len += step;
                held_until += step;
            }
            continue;
        }
        if let Some((key, vel)) = key_of(symbol) {
            notes.push(Note {
                beat: at,
                len,
                key,
                vel,
            });
            held_until = at + len;
            // A note shorter than a step is not held by a `-` after it.
            if len + 1e-9 < step {
                held_until = -1.0;
            }
        }
    }
    notes
}

/// Picks the pattern for this bar: the fill on a section's last bar if there
/// is one, otherwise the patterns in turn.
fn pick<'a>(patterns: &[&'a str], fill: Option<&'a str>, bar: &Bar) -> &'a str {
    match fill {
        Some(fill) if bar.last() => fill,
        _ => patterns[bar.index as usize % patterns.len()],
    }
}

/// A drum part: every hit has the same key (a drum ignores it). `len` is the
/// gate length of a hit in beats.
pub fn drum(patterns: &[&'static str], fill: Option<&'static str>, len: f64) -> Pattern {
    let patterns = patterns.to_vec();
    Box::new(move |bar| {
        lay(pick(&patterns, fill, bar), 0.25, len, |c| match c {
            'X' => Some((60, 1.0)),
            'x' => Some((60, 0.75)),
            'o' => Some((60, 0.45)),
            _ => None,
        })
    })
}

/// Notes on the chord's tones. `octaves` moves the whole part.
pub fn tones(
    patterns: &[&'static str],
    fill: Option<&'static str>,
    step: f64,
    len: f64,
    octaves: i32,
    vel: f32,
) -> Pattern {
    let patterns = patterns.to_vec();
    Box::new(move |bar| {
        lay(pick(&patterns, fill, bar), step, len, |c| {
            let (index, shift) = match c {
                '1'..='4' => (c as usize - '1' as usize, 0),
                '5'..='8' => (c as usize - '5' as usize, 1),
                'a'..='d' => (c as usize - 'a' as usize, -1),
                _ => return None,
            };
            Some((bar.chord.key(index, octaves + shift), vel))
        })
    })
}

/// The whole chord at each listed `(beat, length)`.
pub fn stabs(hits: &[(f64, f64)], octaves: i32, vel: f32) -> Pattern {
    let hits = hits.to_vec();
    Box::new(move |bar| {
        let mut notes = Vec::new();
        for &(beat, len) in &hits {
            for index in 0..4 {
                notes.push(Note {
                    beat,
                    len,
                    key: bar.chord.key(index, octaves),
                    vel,
                });
            }
        }
        notes
    })
}

/// The chord held across the bar, ending a touch early so the release does
/// not run into the next chord.
pub fn pad(octaves: i32, vel: f32) -> Pattern {
    stabs(&[(0.0, 3.9)], octaves, vel)
}

/// A scale: the key's root and the semitones of its steps within an octave.
#[derive(Debug, Clone, Copy)]
pub struct Scale {
    pub root: u8,
    pub steps: &'static [u8],
}

pub const MINOR_PENTATONIC: &[u8] = &[0, 3, 5, 7, 10];
pub const MAJOR_PENTATONIC: &[u8] = &[0, 2, 4, 7, 9];

/// A melody in steps of `scale`: digit `d` is scale step `d`, going on up
/// into the next octave after the last, and a letter `a`-`e` is the same
/// steps an octave down.
pub fn melody(patterns: &[&'static str], step: f64, len: f64, scale: Scale, vel: f32) -> Pattern {
    let patterns = patterns.to_vec();
    Box::new(move |bar| {
        lay(pick(&patterns, None, bar), step, len, |c| {
            let degree: i32 = match c {
                '0'..='9' => c as i32 - '0' as i32,
                'a'..='e' => c as i32 - 'a' as i32 - 5,
                _ => return None,
            };
            let n = scale.steps.len() as i32;
            let octave = degree.div_euclid(n);
            let step_in = scale.steps[degree.rem_euclid(n) as usize];
            let key = i32::from(scale.root) + i32::from(step_in) + 12 * octave;
            Some((key.clamp(0, 127) as u8, vel))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar() -> Bar {
        Bar {
            index: 0,
            bars: 4,
            chord: chord(57, MIN7),
        }
    }

    #[test]
    fn drum_steps_land_on_sixteenths_with_their_accents() {
        let notes = drum(&["X...x...o..x...."], None, 0.2)(&bar());
        let at: Vec<(f64, f32)> = notes.iter().map(|n| (n.beat, n.vel)).collect();
        assert_eq!(at, [(0.0, 1.0), (1.0, 0.75), (2.0, 0.45), (2.75, 0.75)]);
    }

    #[test]
    fn a_fill_replaces_the_last_bar_of_a_section() {
        let pattern = drum(&["X..............."], Some("xxxxxxxxxxxxxxxx"), 0.2);
        assert_eq!(pattern(&bar()).len(), 1);
        let last = Bar { index: 3, ..bar() };
        assert_eq!(pattern(&last).len(), 16);
    }

    #[test]
    fn chord_tones_octaves_and_ties() {
        // A minor 7th from A3: root, a held fifth, the third an octave up, the
        // root an octave down.
        let notes = tones(&["1.3-.6.a"], None, 0.5, 0.5, 0, 0.8)(&bar());
        let keys: Vec<(f64, f64, u8)> = notes.iter().map(|n| (n.beat, n.len, n.key)).collect();
        assert_eq!(
            keys,
            [
                (0.0, 0.5, 57),
                (1.0, 1.0, 64),
                (2.5, 0.5, 72),
                (3.5, 0.5, 45)
            ]
        );
    }

    #[test]
    fn melodies_climb_into_the_next_octave_past_the_last_step() {
        let scale = Scale {
            root: 60,
            steps: MINOR_PENTATONIC,
        };
        let notes = melody(&["0.4.5.a."], 0.5, 0.5, scale, 0.8)(&bar());
        let keys: Vec<u8> = notes.iter().map(|n| n.key).collect();
        // Steps 0, 4, 5 (the root again, an octave up), and a (step -5: an
        // octave down).
        assert_eq!(keys, [60, 70, 72, 48]);
    }
}
