//! Probes: short, controlled signals that each show one way a warp fails.
//!
//! A probe is written in beats, so it can be rendered *natively* at any tempo
//! map: the notes land where the map puts their beats, and each sound keeps
//! its own length in seconds, as it would from a synth that knows the tempo.
//! That render is the answer key for a warp of the same probe rendered at
//! another tempo.
//!
//! All probes are stereo, [`BEATS`] beats long with the last two silent, and
//! made only from sines and a fixed noise seed, so they are the same every
//! time.

use noodle_core::{TICKS_PER_QUARTER, TempoMap, Tick};
use std::f64::consts::TAU;

/// The length of every probe, in quarter notes. The last two beats are silent
/// so the whole of the last sound lies inside it.
pub const BEATS: f64 = 16.0;

/// What a probe shows up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// A click on every beat, louder on the bar, with a softer click between:
    /// timing, and whether the clicks stay sharp.
    ClickTrain,
    /// One kick and then silence: pre-echo and smearing around a single attack.
    LoneKick,
    /// Two held chords: phasiness and pitch on sustained, tonal sound.
    HeldChord,
    /// A sine gliding six octaves in step with the beats: how a stretcher
    /// follows a moving partial.
    Sweep,
    /// Bursts of noise at uneven beats, left and right independent: transients
    /// in a noisy sound, and the stereo image.
    NoiseBursts,
}

impl Probe {
    pub const ALL: [Probe; 5] = [
        Probe::ClickTrain,
        Probe::LoneKick,
        Probe::HeldChord,
        Probe::Sweep,
        Probe::NoiseBursts,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::ClickTrain => "click-train",
            Self::LoneKick => "lone-kick",
            Self::HeldChord => "held-chord",
            Self::Sweep => "sweep",
            Self::NoiseBursts => "noise-bursts",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The probe played at `map`, as interleaved stereo. It lasts exactly
    /// [`BEATS`] beats of the map.
    pub fn render(self, map: &TempoMap, rate: u32) -> Vec<f32> {
        let frames = frames_in(map, rate);
        let mut canvas = Canvas {
            rate: f64::from(rate),
            data: vec![0.0; frames * 2],
        };
        match self {
            Self::ClickTrain => click_train(&mut canvas, map),
            Self::LoneKick => lone_kick(&mut canvas, map),
            Self::HeldChord => held_chord(&mut canvas, map),
            Self::Sweep => sweep(&mut canvas, map),
            Self::NoiseBursts => noise_bursts(&mut canvas, map),
        }
        canvas.data
    }
}

/// How many frames [`BEATS`] beats of `map` take.
pub fn frames_in(map: &TempoMap, rate: u32) -> usize {
    map.sample_at(Tick::from_quarters(BEATS), f64::from(rate)) as usize
}

struct Canvas {
    rate: f64,
    data: Vec<f32>,
}

impl Canvas {
    /// Mixes stereo sound in at `seconds`.
    fn add(&mut self, seconds: f64, left: &[f32], right: &[f32]) {
        let start = (seconds * self.rate).round() as usize;
        for (i, (l, r)) in left.iter().zip(right).enumerate() {
            if let Some(frame) = self.data.get_mut((start + i) * 2..(start + i) * 2 + 2) {
                frame[0] += l;
                frame[1] += r;
            }
        }
    }

    fn add_mono(&mut self, seconds: f64, signal: &[f32]) {
        self.add(seconds, signal, signal);
    }

    /// `length` seconds of a sound given by its value `t` seconds in.
    fn make(&self, length: f64, mut f: impl FnMut(f64) -> f64) -> Vec<f32> {
        (0..(length * self.rate) as usize)
            .map(|i| f(i as f64 / self.rate) as f32)
            .collect()
    }
}

fn at(map: &TempoMap, beat: f64) -> f64 {
    map.seconds_at(Tick::from_quarters(beat))
}

/// A small deterministic noise source.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(self.0 >> 8) / f64::from(1u32 << 23) - 1.0
    }
}

/// Six milliseconds of a 3 kHz sine that dies away in under a millisecond.
fn click(canvas: &Canvas, level: f64) -> Vec<f32> {
    canvas.make(0.006, |t| {
        level * (TAU * 3000.0 * t).sin() * (-t / 0.0008).exp()
    })
}

fn click_train(canvas: &mut Canvas, map: &TempoMap) {
    for beat in 1..=13 {
        let level = if beat % 4 == 1 { 0.9 } else { 0.6 };
        let sound = click(canvas, level);
        canvas.add_mono(at(map, f64::from(beat)), &sound);
        let softer = click(canvas, 0.3);
        canvas.add_mono(at(map, f64::from(beat) + 0.5), &softer);
    }
}

fn lone_kick(canvas: &mut Canvas, map: &TempoMap) {
    let (mut phase, rate) = (0.0, canvas.rate);
    let kick = canvas.make(0.6, |t| {
        let hz = 45.0 + 120.0 * (-t / 0.03).exp();
        phase += TAU * hz / rate;
        0.9 * phase.sin() * (-t / 0.18).exp() * (1.0 - (-t / 0.001).exp())
    });
    canvas.add_mono(at(map, 2.0), &kick);
}

fn held_chord(canvas: &mut Canvas, map: &TempoMap) {
    // A minor, then F major, an octave and a half up so the harmonics stay
    // below the Nyquist frequency at any rate in use.
    let chords: [(f64, f64, [f64; 3]); 2] = [
        (1.0, 7.0, [220.0, 261.63, 329.63]),
        (8.0, 14.0, [174.61, 220.0, 261.63]),
    ];
    let release = 0.15;
    for (from, to, notes) in chords {
        let length = at(map, to) - at(map, from) + release;
        for hz in notes {
            let (left, right): (Vec<f32>, Vec<f32>) = [0.0, 3.0]
                .iter()
                .map(|cents| {
                    let f = hz * 2f64.powf(cents / 1200.0);
                    canvas.make(length, |t| {
                        let held = length - release;
                        let envelope = (t / 0.02).min(1.0)
                            * if t > held {
                                (1.0 - (t - held) / release).max(0.0)
                            } else {
                                1.0
                            };
                        let tone: f64 = (1..=6)
                            .map(|h| (TAU * f * f64::from(h) * t).sin() / f64::from(h))
                            .sum();
                        0.09 * envelope * tone
                    })
                })
                .fold((Vec::new(), Vec::new()), |(l, r), channel| {
                    if l.is_empty() {
                        (channel, r)
                    } else {
                        (l, channel)
                    }
                });
            canvas.add(at(map, from), &left, &right);
        }
    }
}

fn sweep(canvas: &mut Canvas, map: &TempoMap) {
    let (from, to) = (1.0, 13.0);
    let (low, octaves) = (110.0, 6.0);
    let mut phase = 0.0;
    let rate = canvas.rate;
    let first = (at(map, from) * rate) as usize;
    let last = (at(map, to) * rate) as usize;
    for n in first..last.min(canvas.data.len() / 2) {
        let beat = map.tick_at_sample(n as u64, rate) / TICKS_PER_QUARTER as f64;
        let progress = ((beat - from) / (to - from)).clamp(0.0, 1.0);
        phase += TAU * low * 2f64.powf(octaves * progress) / rate;
        // Short fades at both ends keep the sweep from clicking.
        let fade = ((n - first) as f64 / (0.05 * rate))
            .min((last - n) as f64 / (0.05 * rate))
            .min(1.0);
        let x = (0.4 * fade * phase.sin()) as f32;
        canvas.data[n * 2] += x;
        canvas.data[n * 2 + 1] += x;
    }
}

fn noise_bursts(canvas: &mut Canvas, map: &TempoMap) {
    let beats = [1.0, 2.0, 2.5, 4.0, 5.5, 6.0, 8.0, 9.0, 9.5, 10.0, 12.0];
    let mut noise = Noise(0x0b57);
    for (i, beat) in beats.into_iter().enumerate() {
        let level = [0.8, 0.5, 0.65][i % 3];
        let mut channel =
            |canvas: &Canvas| canvas.make(0.15, |t| level * noise.next() * (-t / 0.03).exp());
        let left = channel(canvas);
        let right = channel(canvas);
        canvas.add(at(map, beat), &left, &right);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tempo::steady;

    const RATE: u32 = 48_000;

    #[test]
    fn every_probe_is_sound_of_the_right_length_and_the_same_every_time() {
        let map = steady(120.0);
        for probe in Probe::ALL {
            let a = probe.render(&map, RATE);
            assert_eq!(a.len(), 2 * 16 * RATE as usize / 2, "{}", probe.name());
            assert!(a.iter().any(|x| x.abs() > 0.1), "{}", probe.name());
            assert!(a.iter().all(|x| x.abs() <= 1.0), "{}", probe.name());
            assert_eq!(a, probe.render(&map, RATE), "{}", probe.name());
            // The last two beats are silent.
            let quiet = &a[2 * (14.4 * 0.5 * f64::from(RATE)) as usize..];
            assert!(quiet.iter().all(|x| x.abs() < 1e-3), "{}", probe.name());
        }
    }

    #[test]
    fn events_follow_the_tempo_map_but_sounds_keep_their_length() {
        // The kick at beat 2 starts at 1 s at 120 bpm and 0.5 s at 240.
        for (bpm, start) in [(120.0, 1.0), (240.0, 0.5)] {
            let x = Probe::LoneKick.render(&steady(bpm), RATE);
            let first = x.iter().position(|v| v.abs() > 0.05).unwrap() / 2;
            assert!(
                (first as f64 / f64::from(RATE) - start).abs() < 0.01,
                "{bpm}: {first}"
            );
        }
        let ends = |bpm| {
            let x = Probe::LoneKick.render(&steady(bpm), RATE);
            let last = x.iter().rposition(|v| v.abs() > 0.01).unwrap() / 2;
            last as f64 / f64::from(RATE)
        };
        // Same length of sound after the start: 1 s - 0.5 s apart in starts.
        assert!(((ends(120.0) - 1.0) - (ends(240.0) - 0.5)).abs() < 0.01);
    }

    #[test]
    fn the_sweep_rises_and_the_noise_is_independent_left_to_right() {
        let map = steady(120.0);
        let sweep = Probe::Sweep.render(&map, RATE);
        let crossings = |from: f64, to: f64| {
            let (a, b) = (
                (from * f64::from(RATE)) as usize,
                (to * f64::from(RATE)) as usize,
            );
            sweep[a * 2..b * 2]
                .iter()
                .step_by(2)
                .collect::<Vec<_>>()
                .windows(2)
                .filter(|w| *w[0] <= 0.0 && *w[1] > 0.0)
                .count()
        };
        assert!(crossings(5.0, 5.5) > 3 * crossings(1.0, 1.5));
        let noise = Probe::NoiseBursts.render(&map, RATE);
        let (l, r): (Vec<f32>, Vec<f32>) = noise
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| (f[0], f[1]))
            .unzip();
        let dot: f32 = l.iter().zip(&r).map(|(a, b)| a * b).sum();
        let norm: f32 = l.iter().map(|a| a * a).sum::<f32>().sqrt()
            * r.iter().map(|a| a * a).sum::<f32>().sqrt();
        assert!((dot / norm).abs() < 0.1, "{}", dot / norm);
    }
}
