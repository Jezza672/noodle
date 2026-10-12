//! The instruments of the songs.
//!
//! Every instrument is a group node built from primitive nodes (oscillators,
//! noise, filters, envelopes, VCAs, Math, Mix, Pan), with its controls as the
//! group's inputs. An input nobody wires leaves the node inside at the value
//! the instrument was made with, so a song only sets presets, and a user who
//! opens the group can change anything. Nothing here is a monolithic synth
//! node.
//!
//! The group's name is its kind ("Pluck"), and the track that plays it is
//! named after the part.

use super::patch::{Patch, Port, spec};
use noodle_core::{NodeId, Value};

/// Oscillator shapes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wave {
    Sine,
    Saw,
    Square,
    Triangle,
}

impl Wave {
    fn id(self) -> &'static str {
        match self {
            Self::Sine => "noodle.osc.sine",
            Self::Saw => "noodle.osc.saw",
            Self::Square => "noodle.osc.square",
            Self::Triangle => "noodle.osc.triangle",
        }
    }
}

/// One oscillator of a tonal instrument.
#[derive(Debug, Clone, Copy)]
pub struct Osc {
    pub wave: Wave,
    /// The pitch as a multiple of the note's (0.5 is an octave down).
    pub ratio: f64,
    /// How far the `detune` control moves this oscillator, in units of its
    /// value in cents: -1 and 1 are the two sides of a pair.
    pub spread: f64,
    pub pan: f32,
    pub db: f32,
}

impl Osc {
    pub fn new(wave: Wave) -> Self {
        Self {
            wave,
            ratio: 1.0,
            spread: 0.0,
            pan: 0.0,
            db: 0.0,
        }
    }

    pub fn octave(self, ratio: f64) -> Self {
        Self { ratio, ..self }
    }

    pub fn detuned(self, spread: f64) -> Self {
        Self { spread, ..self }
    }

    pub fn panned(self, pan: f32) -> Self {
        Self { pan, ..self }
    }

    pub fn level(self, db: f32) -> Self {
        Self { db, ..self }
    }
}

/// An attack, decay, sustain and release, in seconds and a fraction.
#[derive(Debug, Clone, Copy)]
pub struct Env {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
}

pub const fn env(attack: f32, decay: f32, sustain: f32, release: f32) -> Env {
    Env {
        attack,
        decay,
        sustain,
        release,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Filter {
    Ladder,
    /// A state variable filter, low-pass output.
    Svf,
}

/// A tonal instrument: oscillators into a filter that an envelope opens, into
/// an amplitude envelope. Played by a Mono Note node, or by Voices when
/// `voices` is more than one.
#[derive(Debug, Clone)]
pub struct Tone {
    pub oscs: Vec<Osc>,
    pub filter: Filter,
    /// The filter's cutoff with the envelope at rest, in hertz.
    pub cutoff: f32,
    pub resonance: f32,
    /// How far the filter envelope opens the cutoff, along the slider (0 to
    /// 1: the cutoff slider is logarithmic from 20 Hz to 20 kHz, so 0.1 is
    /// about a third of an octave per percent).
    pub amount: f32,
    pub filter_env: Env,
    pub amp_env: Env,
    /// Cents of detune on a full `spread`.
    pub detune: f32,
    pub voices: u32,
}

impl Tone {
    pub fn new(oscs: Vec<Osc>) -> Self {
        Self {
            oscs,
            filter: Filter::Ladder,
            cutoff: 2000.0,
            resonance: 0.1,
            amount: 0.0,
            filter_env: env(0.005, 0.2, 0.3, 0.2),
            amp_env: env(0.005, 0.2, 0.7, 0.15),
            detune: 0.0,
            voices: 1,
        }
    }

    pub fn filter(self, filter: Filter, cutoff: f32, resonance: f32, amount: f32) -> Self {
        Self {
            filter,
            cutoff,
            resonance,
            amount,
            ..self
        }
    }

    pub fn filter_env(self, filter_env: Env) -> Self {
        Self { filter_env, ..self }
    }

    pub fn amp_env(self, amp_env: Env) -> Self {
        Self { amp_env, ..self }
    }

    pub fn detune(self, detune: f32) -> Self {
        Self { detune, ..self }
    }

    pub fn voices(self, voices: u32) -> Self {
        Self { voices, ..self }
    }
}

/// A drum.
#[derive(Debug, Clone, Copy)]
pub enum Drum {
    /// A sine that falls from `base + sweep` to `base` hertz.
    Kick { base: f32, sweep: f32, decay: f32 },
    /// Band-passed noise over a short sine body.
    Snare { tone: f32, noise: f32, decay: f32 },
    /// High-passed noise; the decay tells closed from open.
    Hat { cutoff: f32, decay: f32 },
    /// Band-passed noise with a soft attack.
    Clap { cutoff: f32, decay: f32 },
}

/// What a track plays.
#[derive(Debug, Clone)]
pub enum Instrument {
    Tone(Tone),
    Drum(Drum),
}

impl Instrument {
    /// The name of the group.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Tone(t) if t.voices > 1 => "Poly Synth",
            Self::Tone(_) => "Mono Synth",
            Self::Drum(Drum::Kick { .. }) => "Kick",
            Self::Drum(Drum::Snare { .. }) => "Snare",
            Self::Drum(Drum::Hat { .. }) => "Hat",
            Self::Drum(Drum::Clap { .. }) => "Clap",
        }
    }
}

fn adsr(e: Env) -> super::patch::Spec {
    spec("noodle.mod.adsr")
        .param("attack", e.attack)
        .param("decay", e.decay)
        .param("sustain", e.sustain)
        .param("release", e.release)
}

fn math(expr: &str) -> super::patch::Spec {
    spec("noodle.util.math").config("expr", Value::Text(expr.into()))
}

/// Builds the instrument's group in the current group and returns it. Its
/// inputs are `pitch`, `gate` and `velocity` (and `fade` when polyphonic),
/// then the instrument's own controls; its output is `out` (and `active`
/// when polyphonic, for Voices' `busy`).
pub fn build(p: &mut Patch, instrument: &Instrument, column: f32, row: f32) -> NodeId {
    let group = p.enter(instrument.name(), column, row);
    match instrument {
        Instrument::Tone(tone) => tonal(p, tone),
        Instrument::Drum(Drum::Kick { base, sweep, decay }) => kick(p, *base, *sweep, *decay),
        Instrument::Drum(Drum::Snare { tone, noise, decay }) => snare(p, *tone, *noise, *decay),
        Instrument::Drum(Drum::Hat { cutoff, decay }) => hat(p, *cutoff, *decay),
        Instrument::Drum(Drum::Clap { cutoff, decay }) => clap(p, *cutoff, *decay),
    }
    p.leave(group);
    group
}

fn tonal(p: &mut Patch, tone: &Tone) {
    let poly = tone.voices > 1;
    let pitch = p.input("pitch", 0.0);
    let gate = p.input("gate", 1.0);
    let velocity = p.input("velocity", 2.0);
    let fade = poly.then(|| p.input("fade", 3.0));
    let cutoff = p.input("cutoff", 4.0);
    let resonance = p.input("resonance", 5.0);
    let amount = p.input("amount", 6.0);
    let detune = p.input("detune", 7.0);
    let attack = p.input("attack", 8.0);
    let release = p.input("release", 9.0);

    // The oscillators, each at its own multiple of the pitch and its own
    // spread of the detune control.
    let mix = p.add(
        spec("noodle.util.mix").config("inputs", Value::Int(tone.oscs.len() as i64)),
        5.0,
        0.0,
    );
    for (i, osc) in tone.oscs.iter().enumerate() {
        let row = i as f32 * 1.2;
        let expr = format!("a * {} * pow(2, b * {} / 1200)", osc.ratio, osc.spread);
        let frequency = p.add(math(&expr).param("b", tone.detune), 2.0, row);
        let oscillator = p.add(spec(osc.wave.id()), 3.0, row);
        let pan = p.add(spec("noodle.util.pan").param("pan", osc.pan), 4.0, row);
        p.connect((pitch, "out"), (frequency, "a"));
        p.connect((detune, "out"), (frequency, "b"));
        p.connect((frequency, "out"), (oscillator, "frequency"));
        p.connect((oscillator, "out"), (pan, "in"));
        p.connect((pan, "out"), (mix, wire_in(i)));
    }
    for (i, osc) in tone.oscs.iter().enumerate() {
        // `Mix` takes its gains as parameters in decibels.
        set_param(p, mix, &format!("gain{}", i + 1), osc.db);
    }

    // The filter, opened by an envelope: Math scales the envelope by `amount`
    // and adds the `cutoff` control, and the filter adds that to its own
    // cutoff along the slider.
    let filter_env = p.add(adsr(tone.filter_env), 5.0, 2.0);
    p.connect((gate, "out"), (filter_env, "gate"));
    let open = p.add(math("a * b + c").param("b", tone.amount), 6.0, 2.0);
    p.connect((filter_env, "out"), (open, "a"));
    p.connect((amount, "out"), (open, "b"));
    p.connect((cutoff, "out"), (open, "c"));
    let (filter, filtered): (NodeId, &'static str) = match tone.filter {
        Filter::Ladder => (
            p.add(
                spec("noodle.filter.ladder")
                    .param("cutoff", tone.cutoff)
                    .param("resonance", tone.resonance),
                7.0,
                0.0,
            ),
            "out",
        ),
        Filter::Svf => (
            p.add(
                spec("noodle.filter.svf")
                    .param("cutoff", tone.cutoff)
                    .param("resonance", tone.resonance),
                7.0,
                0.0,
            ),
            "low",
        ),
    };
    p.connect((mix, "out"), (filter, "in"));
    p.connect((open, "out"), (filter, "cutoff"));
    p.connect((resonance, "out"), (filter, "resonance"));

    // Amplitude: an envelope through a VCA, then the velocity through another,
    // and Voices' steal fade through a last.
    let amp = p.add(adsr(tone.amp_env), 5.0, 4.0);
    p.connect((gate, "out"), (amp, "gate"));
    p.connect((attack, "out"), (amp, "attack"));
    p.connect((release, "out"), (amp, "release"));
    let shaped = p.add(spec("noodle.util.vca"), 8.0, 0.0);
    p.connect((filter, filtered), (shaped, "in"));
    p.connect((amp, "out"), (shaped, "level"));
    let touch = p.add(spec("noodle.util.vca"), 9.0, 0.0);
    p.connect((shaped, "out"), (touch, "in"));
    p.connect((velocity, "out"), (touch, "level"));
    let mut last: Port = (touch, "out");
    if let Some(fade) = fade {
        let faded = p.add(spec("noodle.util.vca"), 10.0, 0.0);
        p.connect(last, (faded, "in"));
        p.connect((fade, "out"), (faded, "level"));
        last = (faded, "out");
    }
    let out = p.output("out", 11.0, 0.0);
    p.connect(last, (out, "in"));
    if poly {
        let active = p.output("active", 11.0, 4.0);
        p.connect((amp, "active"), (active, "in"));
    }
}

fn wire_in(i: usize) -> &'static str {
    // Port keys outlive the patch; the songs have a handful.
    const KEYS: [&str; 8] = ["in1", "in2", "in3", "in4", "in5", "in6", "in7", "in8"];
    KEYS[i]
}

fn set_param(p: &mut Patch, node: NodeId, key: &str, value: f32) {
    p.history
        .apply(
            p.project,
            noodle_core::Command::SetParam {
                node,
                key: key.into(),
                value: Some(value),
            },
        )
        .expect("a valid parameter");
}

/// The group's `gate` and `velocity` inputs, and its `out`, shared by drums.
struct DrumFrame {
    gate: NodeId,
    velocity: NodeId,
}

fn drum_frame(p: &mut Patch) -> DrumFrame {
    // A drum is played by its gate; a pitch input keeps the same wiring as the
    // tonal instruments, and is unused.
    let _pitch = p.input("pitch", 0.0);
    DrumFrame {
        gate: p.input("gate", 1.0),
        velocity: p.input("velocity", 2.0),
    }
}

/// Velocity through a VCA into the group's output.
fn finish(p: &mut Patch, frame: &DrumFrame, signal: Port, column: f32) {
    let touch = p.add(spec("noodle.util.vca"), column, 0.0);
    p.connect(signal, (touch, "in"));
    p.connect((frame.velocity, "out"), (touch, "level"));
    let out = p.output("out", column + 1.0, 0.0);
    p.connect((touch, "out"), (out, "in"));
}

fn kick(p: &mut Patch, base: f32, sweep: f32, decay: f32) {
    let frame = drum_frame(p);
    let tune = p.input("tune", 3.0);
    let punch = p.input("sweep", 4.0);
    let length = p.input("decay", 5.0);
    let fall = p.add(adsr(env(0.001, 0.04, 0.0, 0.02)), 2.0, 1.0);
    p.connect((frame.gate, "out"), (fall, "gate"));
    let frequency = p.add(
        math("a * b + c").param("b", sweep).param("c", base),
        3.0,
        1.0,
    );
    p.connect((fall, "out"), (frequency, "a"));
    p.connect((punch, "out"), (frequency, "b"));
    p.connect((tune, "out"), (frequency, "c"));
    let body = p.add(spec("noodle.osc.sine"), 4.0, 1.0);
    p.connect((frequency, "out"), (body, "frequency"));
    let amp = p.add(adsr(env(0.001, decay, 0.0, 0.05)), 4.0, 3.0);
    p.connect((frame.gate, "out"), (amp, "gate"));
    p.connect((length, "out"), (amp, "decay"));
    let shaped = p.add(spec("noodle.util.vca"), 5.0, 1.0);
    p.connect((body, "out"), (shaped, "in"));
    p.connect((amp, "out"), (shaped, "level"));
    finish(p, &frame, (shaped, "out"), 6.0);
}

fn snare(p: &mut Patch, tone: f32, noise: f32, decay: f32) {
    let frame = drum_frame(p);
    let tune = p.input("tune", 3.0);
    let length = p.input("decay", 4.0);
    let hiss = p.add(spec("noodle.noise.white"), 2.0, 0.0);
    let band = p.add(
        spec("noodle.filter.svf")
            .param("cutoff", noise)
            .param("resonance", 0.25),
        3.0,
        0.0,
    );
    p.connect((hiss, "out"), (band, "in"));
    let noise_env = p.add(adsr(env(0.001, decay, 0.0, 0.04)), 3.0, 2.0);
    p.connect((frame.gate, "out"), (noise_env, "gate"));
    p.connect((length, "out"), (noise_env, "decay"));
    let snap_vca = p.add(spec("noodle.util.vca"), 4.0, 0.0);
    p.connect((band, "band"), (snap_vca, "in"));
    p.connect((noise_env, "out"), (snap_vca, "level"));
    let body = p.add(spec("noodle.osc.sine").param("frequency", tone), 2.0, 4.0);
    p.connect((tune, "out"), (body, "frequency"));
    let body_env = p.add(adsr(env(0.001, decay * 0.5, 0.0, 0.03)), 3.0, 4.0);
    p.connect((frame.gate, "out"), (body_env, "gate"));
    let body_vca = p.add(spec("noodle.util.vca"), 4.0, 4.0);
    p.connect((body, "out"), (body_vca, "in"));
    p.connect((body_env, "out"), (body_vca, "level"));
    let mix = p.add(
        spec("noodle.util.mix")
            .config("inputs", Value::Int(2))
            .param("gain1", 0.0)
            .param("gain2", -4.0),
        5.0,
        2.0,
    );
    p.connect((snap_vca, "out"), (mix, "in1"));
    p.connect((body_vca, "out"), (mix, "in2"));
    finish(p, &frame, (mix, "out"), 6.0);
}

fn hat(p: &mut Patch, cutoff: f32, decay: f32) {
    let frame = drum_frame(p);
    let brightness = p.input("cutoff", 3.0);
    let length = p.input("decay", 4.0);
    let hiss = p.add(spec("noodle.noise.white"), 2.0, 0.0);
    let high = p.add(
        spec("noodle.filter.svf")
            .param("cutoff", cutoff)
            .param("resonance", 0.15),
        3.0,
        0.0,
    );
    p.connect((hiss, "out"), (high, "in"));
    p.connect((brightness, "out"), (high, "cutoff"));
    let amp = p.add(adsr(env(0.001, decay, 0.0, 0.01)), 3.0, 2.0);
    p.connect((frame.gate, "out"), (amp, "gate"));
    p.connect((length, "out"), (amp, "decay"));
    let shaped = p.add(spec("noodle.util.vca"), 4.0, 0.0);
    p.connect((high, "high"), (shaped, "in"));
    p.connect((amp, "out"), (shaped, "level"));
    finish(p, &frame, (shaped, "out"), 5.0);
}

fn clap(p: &mut Patch, cutoff: f32, decay: f32) {
    let frame = drum_frame(p);
    let brightness = p.input("cutoff", 3.0);
    let length = p.input("decay", 4.0);
    let hiss = p.add(spec("noodle.noise.white"), 2.0, 0.0);
    let band = p.add(
        spec("noodle.filter.svf")
            .param("cutoff", cutoff)
            .param("resonance", 0.3),
        3.0,
        0.0,
    );
    p.connect((hiss, "out"), (band, "in"));
    p.connect((brightness, "out"), (band, "cutoff"));
    let amp = p.add(adsr(env(0.008, decay, 0.0, 0.04)), 3.0, 2.0);
    p.connect((frame.gate, "out"), (amp, "gate"));
    p.connect((length, "out"), (amp, "decay"));
    let shaped = p.add(spec("noodle.util.vca"), 4.0, 0.0);
    p.connect((band, "band"), (shaped, "in"));
    p.connect((amp, "out"), (shaped, "level"));
    finish(p, &frame, (shaped, "out"), 5.0);
}
