//! Following the pitch of a monophonic sound, and cutting it into notes.
//!
//! [`PitchTracker`] is YIN over a window that slides by 10 ms, run on the
//! audio reduced to about 8 kHz, so it is cheap enough for the audio thread
//! and does not allocate once built. [`NoteSegmenter`] turns its frames into
//! note starts and ends. The Pitch Track and To Notes nodes run both live;
//! [`transcribe`] runs them over a recording and cuts the notes at the
//! recording's onsets as well ([`detect_onsets_with`], the same detector the
//! slicer and the transient pins use, at the same sensitivity).

use crate::onsets::{DEFAULT_SENSITIVITY, detect_onsets_with, rise_db};

/// The pitch range looked for, in Hz.
const MIN_HZ: f32 = 40.0;
const MAX_HZ: f32 = 1_000.0;
/// The rate the audio is reduced to, near enough: a whole number of samples
/// is averaged into one.
const LOW_RATE: f32 = 8_000.0;
/// Seconds between the frames of [`PitchTracker`].
pub const HOP_SECONDS: f32 = 0.01;

/// What the tracker heard in one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PitchFrame {
    /// The key number (fractional; 69 is A 440), or `None` when the frame is
    /// not a pitched sound (noise, silence, a click).
    pub key: Option<f32>,
    /// How sure the pitch is, 0 to 1 (`None` frames give 0).
    pub confidence: f32,
    /// The level of the frame, in decibels relative to full scale.
    pub level_db: f32,
}

/// The key number of a frequency.
pub fn key_of_hz(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

/// The YIN threshold at a sensitivity: a dip of the difference function counts
/// as a period when it is below this, so a higher sensitivity accepts rougher
/// sounds (a breathy hum) as pitched.
fn yin_threshold(sensitivity: f32) -> f32 {
    0.08 + 0.3 * sensitivity.clamp(0.0, 1.0)
}

/// A pitch tracker that takes one sample at a time.
pub struct PitchTracker {
    factor: usize,
    low_rate: f32,
    threshold: f32,
    tau_min: usize,
    tau_max: usize,
    hop: usize,
    /// The last `2 * tau_max` reduced samples, oldest first.
    frame: Vec<f32>,
    acc: f32,
    acc_n: usize,
    since_hop: usize,
    d: Vec<f32>,
    cmnd: Vec<f32>,
}

impl PitchTracker {
    /// A tracker for audio at `rate`, accepting pitches at `sensitivity`
    /// (0 to 1; [`DEFAULT_SENSITIVITY`] is the middle).
    pub fn new(rate: u32, sensitivity: f32) -> Self {
        let factor = ((rate as f32 / LOW_RATE).round() as usize).max(1);
        let low_rate = rate as f32 / factor as f32;
        let tau_max = (low_rate / MIN_HZ).ceil() as usize;
        let tau_min = ((low_rate / MAX_HZ).floor() as usize).max(2);
        let hop = ((HOP_SECONDS * low_rate).round() as usize).max(1);
        Self {
            factor,
            low_rate,
            threshold: yin_threshold(sensitivity),
            tau_min,
            tau_max,
            hop,
            frame: vec![0.0; 2 * tau_max],
            acc: 0.0,
            acc_n: 0,
            since_hop: 0,
            d: vec![0.0; tau_max + 1],
            cmnd: vec![1.0; tau_max + 1],
        }
    }

    /// Sets the sensitivity without losing the audio heard so far.
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.threshold = yin_threshold(sensitivity);
    }

    /// How many input samples a frame reaches back: the tracker's delay is
    /// half of this.
    pub fn window(&self) -> usize {
        self.frame.len() * self.factor
    }

    /// Forgets the audio heard.
    pub fn reset(&mut self) {
        self.frame.fill(0.0);
        self.acc = 0.0;
        self.acc_n = 0;
        self.since_hop = 0;
    }

    /// Takes the next sample; every 10 ms it also returns the frame that ends
    /// there.
    pub fn push(&mut self, x: f32) -> Option<PitchFrame> {
        self.acc += x;
        self.acc_n += 1;
        if self.acc_n < self.factor {
            return None;
        }
        let low = self.acc / self.factor as f32;
        self.acc = 0.0;
        self.acc_n = 0;
        let n = self.frame.len();
        self.frame.copy_within(1.., 0);
        self.frame[n - 1] = low;
        self.since_hop += 1;
        if self.since_hop < self.hop {
            return None;
        }
        self.since_hop = 0;
        Some(self.analyse())
    }

    fn analyse(&mut self) -> PitchFrame {
        let x = &self.frame;
        let power = x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32;
        let level_db = 10.0 * (power + 1e-12).log10();
        let silent = PitchFrame {
            key: None,
            confidence: 0.0,
            level_db,
        };
        if level_db < -70.0 {
            return silent;
        }
        let window = x.len() - self.tau_max;
        for tau in 1..=self.tau_max {
            let mut sum = 0.0;
            for j in 0..window {
                let delta = x[j] - x[j + tau];
                sum += delta * delta;
            }
            self.d[tau] = sum;
        }
        let mut running = 0.0;
        for tau in 1..=self.tau_max {
            running += self.d[tau];
            self.cmnd[tau] = if running > 0.0 {
                self.d[tau] * tau as f32 / running
            } else {
                1.0
            };
        }
        let mut tau = self.tau_min;
        while tau < self.tau_max {
            if self.cmnd[tau] < self.threshold {
                while tau + 1 < self.tau_max && self.cmnd[tau + 1] < self.cmnd[tau] {
                    tau += 1;
                }
                let (a, b, c) = (self.cmnd[tau - 1], self.cmnd[tau], self.cmnd[tau + 1]);
                let denominator = a - 2.0 * b + c;
                let shift = if denominator.abs() > 1e-9 {
                    (0.5 * (a - c) / denominator).clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                let hz = self.low_rate / (tau as f32 + shift);
                return PitchFrame {
                    key: Some(key_of_hz(hz)),
                    confidence: (1.0 - b).clamp(0.0, 1.0),
                    level_db,
                };
            }
            tau += 1;
        }
        silent
    }
}

/// A note start or end found by the [`NoteSegmenter`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NoteStep {
    /// A note that ended: its key.
    pub off: Option<u8>,
    /// A note that began: its key and velocity (1 to 127).
    pub on: Option<(u8, u8)>,
    /// How many frames before this one the note that began really started
    /// (a note is confirmed after a few frames of steady pitch).
    pub started_ago: u32,
}

/// Frames of steady pitch before a note is believed.
const CONFIRM: u32 = 3;
/// Frames of no pitch before a note ends.
const RELEASE: u32 = 3;
/// How far, in semitones, a pitch may drift from its note before it counts
/// as another note.
const SPLIT: f32 = 0.75;
/// How far a candidate's frames may differ from each other, in semitones.
const STEADY: f32 = 0.6;
/// Frames of the level kept to see a rise in it.
const LEVELS: usize = 5;

/// Turns frames of pitch into the starts and ends of notes: a note begins when
/// the pitch has held steady for a few frames, changes when the pitch moves
/// off it by more than a semitone for a few frames, ends when the pitch is
/// gone, and begins again at a sudden rise in level (a repeated note).
pub struct NoteSegmenter {
    gate_db: f32,
    retrigger_db: f32,
    current: Option<u8>,
    /// The steady pitch being confirmed: its mean key, how many frames, the
    /// loudest level among them.
    candidate: Option<(f32, u32, f32)>,
    /// A pitch away from the current note, being confirmed the same way.
    moving: Option<(f32, u32, f32)>,
    unvoiced: u32,
    since_start: u32,
    levels: [f32; LEVELS],
}

impl NoteSegmenter {
    pub fn new(sensitivity: f32) -> Self {
        let mut s = Self {
            gate_db: 0.0,
            retrigger_db: 0.0,
            current: None,
            candidate: None,
            moving: None,
            unvoiced: 0,
            since_start: 0,
            levels: [-120.0; LEVELS],
        };
        s.set_sensitivity(sensitivity);
        s
    }

    /// Sets the sensitivity: the quietest sound that counts and how small a
    /// rise in level restarts a note.
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        let s = sensitivity.clamp(0.0, 1.0);
        self.gate_db = -30.0 - 25.0 * s;
        self.retrigger_db = rise_db(s) + 3.0;
    }

    /// The note sounding now, if any.
    pub fn current(&self) -> Option<u8> {
        self.current
    }

    /// Ends the note sounding, if there is one.
    pub fn finish(&mut self) -> Option<u8> {
        self.candidate = None;
        self.moving = None;
        self.unvoiced = 0;
        self.current.take()
    }

    /// Takes the next frame. `onset` says whether the sound restarts at this
    /// frame (from an onset detector); `None` works it out from the level.
    pub fn step(&mut self, frame: PitchFrame, onset: Option<bool>) -> NoteStep {
        let mut out = NoteStep::default();
        let min_before = self.levels.iter().copied().fold(f32::INFINITY, f32::min);
        let rose = frame.level_db - min_before >= self.retrigger_db;
        self.levels.copy_within(1.., 0);
        self.levels[LEVELS - 1] = frame.level_db;
        let onset = onset.unwrap_or(rose);
        let voiced = frame.key.is_some() && frame.level_db >= self.gate_db;
        if !voiced {
            self.candidate = None;
            self.moving = None;
            self.unvoiced += 1;
            if self.unvoiced >= RELEASE {
                out.off = self.current.take();
            }
            return out;
        }
        self.unvoiced = 0;
        let key = frame.key.unwrap_or(0.0);
        let velocity = |db: f32| (((db + 40.0) / 34.0).clamp(0.0, 1.0) * 107.0) as u8 + 20;
        let near = |c: &Option<(f32, u32, f32)>| matches!(c, Some((mean, _, _)) if (mean - key).abs() <= STEADY);
        let fold = |c: Option<(f32, u32, f32)>| match c {
            Some((mean, n, peak)) => (
                (mean * n as f32 + key) / (n + 1) as f32,
                n + 1,
                peak.max(frame.level_db),
            ),
            None => (key, 1, frame.level_db),
        };
        match self.current {
            None => {
                self.candidate = Some(if near(&self.candidate) || self.candidate.is_none() {
                    fold(self.candidate)
                } else {
                    fold(None)
                });
                let (mean, n, peak) = self.candidate.expect("set above");
                if n >= CONFIRM {
                    let note = mean.round().clamp(0.0, 127.0) as u8;
                    out.on = Some((note, velocity(peak)));
                    out.started_ago = n - 1;
                    self.current = Some(note);
                    self.candidate = None;
                    self.since_start = 0;
                }
            }
            Some(current) => {
                self.since_start += 1;
                let away = (key - f32::from(current)).abs() > SPLIT;
                if onset && self.since_start >= CONFIRM + 2 {
                    // The same sound struck again, or a new note straight
                    // after: end this one and begin the next.
                    out.off = Some(current);
                    let note = key.round().clamp(0.0, 127.0) as u8;
                    out.on = Some((note, velocity(frame.level_db)));
                    self.current = Some(note);
                    self.moving = None;
                    self.since_start = 0;
                } else if away {
                    self.moving = Some(if near(&self.moving) || self.moving.is_none() {
                        fold(self.moving)
                    } else {
                        fold(None)
                    });
                    let (mean, n, peak) = self.moving.expect("set above");
                    if n >= CONFIRM {
                        out.off = Some(current);
                        let note = mean.round().clamp(0.0, 127.0) as u8;
                        out.on = Some((note, velocity(peak)));
                        out.started_ago = n - 1;
                        self.current = Some(note);
                        self.moving = None;
                        self.since_start = 0;
                    }
                } else {
                    self.moving = None;
                }
            }
        }
        out
    }
}

/// What [`transcribe`] needs to know.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transcription {
    /// 0 to 1, as for [`detect_onsets_with`]: how readily a sound counts as a
    /// pitched note and a rise in level as a new one.
    pub sensitivity: f32,
    /// Notes shorter than this, in seconds, are dropped.
    pub min_length: f32,
}

impl Default for Transcription {
    fn default() -> Self {
        Self {
            sensitivity: DEFAULT_SENSITIVITY,
            min_length: 0.06,
        }
    }
}

/// A note found in a recording.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FoundNote {
    /// Where it starts, in frames of the recording, and how long it is.
    pub start: u64,
    pub length: u64,
    pub key: u8,
    pub velocity: u8,
}

/// The notes of a monophonic recording: a hummed melody or a bass line.
/// Notes start where the pitch settles and at the recording's onsets, end
/// where the pitch is lost, take their key from the median pitch of the note
/// and their velocity from its peak level. Notes shorter than
/// `settings.min_length` are dropped.
pub fn transcribe(mono: &[f32], rate: u32, settings: &Transcription) -> Vec<FoundNote> {
    let mut tracker = PitchTracker::new(rate, settings.sensitivity);
    let mut segmenter = NoteSegmenter::new(settings.sensitivity);
    let onsets = detect_onsets_with(mono, rate, settings.sensitivity);
    let hop = (tracker.hop * tracker.factor) as u64;
    let delay = (tracker.window() / 2) as u64;
    let mut next_onset = 0;
    let mut notes: Vec<FoundNote> = Vec::new();
    // The note being built: start, key, velocity.
    let mut open: Option<(u64, u8, u8)> = None;
    let close = |open: &mut Option<(u64, u8, u8)>, end: u64, notes: &mut Vec<FoundNote>| {
        if let Some((start, key, velocity)) = open.take() {
            let length = end.saturating_sub(start);
            if length as f64 >= f64::from(settings.min_length) * f64::from(rate) {
                notes.push(FoundNote {
                    start,
                    length,
                    key,
                    velocity,
                });
            }
        }
    };
    for (i, &x) in mono.iter().enumerate() {
        let Some(frame) = tracker.push(x) else {
            continue;
        };
        // The frame is centred half a window back.
        let at = (i as u64 + 1).saturating_sub(delay);
        let mut onset = false;
        while next_onset < onsets.len() && onsets[next_onset] < at + hop {
            onset |= onsets[next_onset] + hop >= at;
            next_onset += 1;
        }
        let step = segmenter.step(frame, Some(onset));
        if let Some(_key) = step.off {
            close(&mut open, at, &mut notes);
        }
        if let Some((key, velocity)) = step.on {
            let start = at.saturating_sub(u64::from(step.started_ago) * hop);
            open = Some((start, key, velocity));
        }
    }
    close(&mut open, mono.len() as u64, &mut notes);
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: u32 = 48_000;

    fn tone(hz: f32, seconds: f32, amp: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|i| amp * (TAU * hz * i as f32 / RATE as f32).sin())
            .collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * RATE as f32) as usize]
    }

    /// A tone with a harmonic, so it is more like a voice than a sine.
    fn voice(hz: f32, seconds: f32) -> Vec<f32> {
        let a = tone(hz, seconds, 0.3);
        let b = tone(hz * 2.0, seconds, 0.15);
        a.iter().zip(&b).map(|(x, y)| x + y).collect()
    }

    fn frames_of(x: &[f32], sensitivity: f32) -> Vec<PitchFrame> {
        let mut t = PitchTracker::new(RATE, sensitivity);
        x.iter().filter_map(|&s| t.push(s)).collect()
    }

    #[test]
    fn it_follows_the_pitch_of_a_tone() {
        for (hz, key) in [(110.0, 45.0), (220.0, 57.0), (440.0, 69.0), (659.26, 76.0)] {
            let frames = frames_of(&voice(hz, 0.5), DEFAULT_SENSITIVITY);
            let last = frames.last().unwrap();
            let heard = last.key.unwrap_or_else(|| panic!("no pitch for {hz} Hz"));
            assert!((heard - key).abs() < 0.2, "{hz} Hz heard as key {heard}");
            assert!(last.confidence > 0.8);
        }
    }

    #[test]
    fn noise_and_silence_have_no_pitch() {
        let mut state = 12345u32;
        let noise: Vec<f32> = (0..RATE / 2)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
            .map(|x| x * 0.3)
            .collect();
        let frames = frames_of(&noise, 0.2);
        let pitched = frames.iter().filter(|f| f.key.is_some()).count();
        assert!(pitched * 10 < frames.len(), "{pitched} of {}", frames.len());
        let quiet = frames_of(&silence(0.5), DEFAULT_SENSITIVITY);
        assert!(quiet.iter().all(|f| f.key.is_none()));
    }

    #[test]
    fn it_reports_the_level() {
        let frames = frames_of(&tone(220.0, 0.5, 0.5), DEFAULT_SENSITIVITY);
        let db = frames.last().unwrap().level_db;
        // A sine of amplitude 0.5 has a power of 0.125.
        assert!((db - -9.0).abs() < 0.5, "{db}");
    }

    fn melody() -> Vec<f32> {
        // C4, a gap, E4 and G4 played legato, then a bass A1.
        let mut x = silence(0.1);
        x.extend(voice(261.63, 0.4));
        x.extend(silence(0.15));
        x.extend(voice(329.63, 0.3));
        x.extend(voice(392.0, 0.3));
        x.extend(silence(0.15));
        x.extend(voice(55.0, 0.5));
        x.extend(silence(0.1));
        x
    }

    #[test]
    fn it_finds_the_notes_of_a_melody() {
        let notes = transcribe(&melody(), RATE, &Transcription::default());
        let keys: Vec<u8> = notes.iter().map(|n| n.key).collect();
        assert_eq!(keys, [60, 64, 67, 33], "{notes:?}");
        // The first starts at 0.1 s and is 0.4 s long, to within a few frames.
        let at = |s: f32| (s * RATE as f32) as i64;
        assert!(
            (notes[0].start as i64 - at(0.1)).abs() < at(0.04),
            "{notes:?}"
        );
        assert!(
            (notes[0].length as i64 - at(0.4)).abs() < at(0.06),
            "{notes:?}"
        );
        // Legato notes join: the second ends where the third starts.
        let end = notes[1].start + notes[1].length;
        assert!(
            (end as i64 - notes[2].start as i64).abs() < at(0.04),
            "{notes:?}"
        );
    }

    #[test]
    fn the_same_note_struck_twice_is_two_notes() {
        let mut x = silence(0.05);
        for _ in 0..2 {
            // Each strike decays, and the next comes in at full level.
            let decay: Vec<f32> = voice(220.0, 0.3)
                .iter()
                .enumerate()
                .map(|(i, s)| s * (-6.0 * i as f32 / RATE as f32).exp())
                .collect();
            x.extend(decay);
        }
        x.extend(silence(0.1));
        let notes = transcribe(&x, RATE, &Transcription::default());
        let keys: Vec<u8> = notes.iter().map(|n| n.key).collect();
        assert_eq!(keys, [57, 57], "{notes:?}");
    }

    #[test]
    fn short_blips_are_dropped_and_loudness_sets_velocity() {
        let mut x = silence(0.1);
        x.extend(voice(220.0, 0.02));
        x.extend(silence(0.2));
        x.extend(
            voice(220.0, 0.3)
                .iter()
                .map(|s| s * 0.2)
                .collect::<Vec<_>>(),
        );
        x.extend(silence(0.2));
        x.extend(voice(220.0, 0.3));
        x.extend(silence(0.1));
        let notes = transcribe(&x, RATE, &Transcription::default());
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[1].velocity > notes[0].velocity, "{notes:?}");
    }

    #[test]
    fn a_higher_sensitivity_hears_a_quieter_sound() {
        let quiet: Vec<f32> = {
            let mut x = silence(0.1);
            x.extend(
                voice(220.0, 0.4)
                    .iter()
                    .map(|s| s * 0.01)
                    .collect::<Vec<_>>(),
            );
            x.extend(silence(0.1));
            x
        };
        let low = Transcription {
            sensitivity: 0.0,
            ..Transcription::default()
        };
        let high = Transcription {
            sensitivity: 1.0,
            ..Transcription::default()
        };
        assert!(transcribe(&quiet, RATE, &low).is_empty());
        assert_eq!(transcribe(&quiet, RATE, &high).len(), 1);
    }

    #[test]
    fn the_tracker_does_not_allocate_once_built() {
        // Pushing is the whole of the tracker's audio-thread work; this just
        // checks it keeps its buffers (their capacity is fixed at the start).
        let mut t = PitchTracker::new(RATE, 0.5);
        let before = (t.frame.capacity(), t.d.capacity(), t.cmnd.capacity());
        for &s in &voice(220.0, 0.3) {
            t.push(s);
        }
        assert_eq!(
            before,
            (t.frame.capacity(), t.d.capacity(), t.cmnd.capacity())
        );
    }
}
