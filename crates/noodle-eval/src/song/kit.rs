//! The instruments of the songs.
//!
//! Every instrument is a group node built from primitive nodes (oscillators,
//! noise, filters, envelopes, VCAs, Math, Mix, Pan), with its controls as the
//! group's inputs. An input nobody wires leaves the node inside at the value
//! the instrument was made with, so a song only sets presets, and a user who
//! opens the group can change anything. Nothing here is a monolithic synth
//! node.
//!
//! The `cutoff` input of a tonal instrument, a hat or a clap is an *offset*
//! along the filter's cutoff slider (it is added to the cutoff set inside),
//! not a frequency in hertz.
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

/// A compressor's settings.
#[derive(Debug, Clone, Copy)]
pub struct Comp {
    pub threshold: f32,
    pub ratio: f32,
    /// Seconds.
    pub attack: f32,
    pub release: f32,
    pub knee: f32,
    pub makeup: f32,
}

/// An effect in a chain, on a track or on the master.
#[derive(Debug, Clone)]
pub enum Fx {
    /// A Parametric EQ with these bands: (type, frequency, gain in dB, Q).
    Eq(Vec<(u8, f32, f32, f32)>),
    /// A Compressor group (a Dynamics node with its controls exposed).
    Compressor(Comp),
    /// A Limiter group: a Dynamics in limiter mode, with `ceiling` dB.
    Limiter {
        ceiling: f32,
    },
    Reverb {
        decay: f32,
        damping: f32,
        mix: f32,
    },
}

/// One drum of a rack: the sample that plays, the synthesised drum it was
/// made from, and where it sits.
#[derive(Debug, Clone)]
pub struct Slot {
    /// The sample's name in `songs/samples/index.txt`.
    pub sample: String,
    pub drum: Drum,
    /// The key that plays it.
    pub key: u8,
    pub db: f32,
    pub pan: f32,
}

/// A drum rack: a Note Split in front of a Sampler for each slot, mixed.
#[derive(Debug, Clone)]
pub struct Rack {
    pub slots: Vec<Slot>,
}

/// What a track plays.
#[derive(Debug, Clone)]
pub enum Instrument {
    Tone(Tone),
    Drum(Drum),
    Rack(Rack),
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
            Self::Rack(_) => "Drum Rack",
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
        Instrument::Rack(rack) => drum_rack(p, rack),
    }
    p.leave(group);
    group
}

/// The drum rack group. Its input is `notes` (events), and each slot has a
/// `level` (decibels, along the Sampler's gain) and a `tune` (cents) control,
/// named after the slot; its output is `out`. Inside are a Note Split with an
/// output per slot, a Sampler and a Pan for each, and a Mix.
fn drum_rack(p: &mut Patch, rack: &Rack) {
    let n = rack.slots.len();
    let notes = p.input("notes", 0.0);
    let split = p.add(
        spec("noodle.event.note_split").config("outputs", Value::Int(n as i64)),
        2.0,
        0.0,
    );
    p.connect(notes, (split, "in"));
    let mix = p.add(
        spec("noodle.util.mix").config("inputs", Value::Int(n as i64)),
        6.0,
        0.0,
    );
    let out = p.output("out", 7.0, 0.0);
    p.connect((mix, "out"), out);
    for (i, slot) in rack.slots.iter().enumerate() {
        let row = i as f32;
        let key = f32::from(slot.key);
        set_param(p, split, &format!("low{}", i + 1), key);
        set_param(p, split, &format!("high{}", i + 1), key);
        let label = format!("{} {}", drum_label(slot.drum), i + 1);
        let level = p.input(&format!("{label} level"), row * 2.0 + 1.0);
        let tune = p.input(&format!("{label} tune"), row * 2.0 + 2.0);
        let sampler = p.add(
            spec("noodle.instrument.sampler")
                .config("sample", Value::Text(super::samples::hash(&slot.sample)))
                .param("keytrack", 0.0)
                .param("mode", 0.0)
                .param("gain", slot.db),
            3.0,
            row,
        );
        let pan = p.add(spec("noodle.util.pan").param("pan", slot.pan), 4.0, row);
        p.connect((split, out_key(i)), (sampler, "in"));
        p.connect(level, (sampler, "gain"));
        p.connect(tune, (sampler, "tune"));
        p.connect((sampler, "out"), (pan, "in"));
        p.connect((pan, "out"), (mix, wire_in(i)));
    }
}

/// Adds `effects` one after another, starting from `from`, in columns from
/// `column` at `row`. Returns the last port.
pub fn chain(p: &mut Patch, effects: &[Fx], from: Port, column: f32, row: f32) -> Port {
    let mut last = from;
    for (i, effect) in effects.iter().enumerate() {
        let column = column + i as f32;
        let (node, input, output): (NodeId, &'static str, &'static str) = match effect {
            Fx::Eq(bands) => {
                let mut eq =
                    spec("noodle.effect.eq").config("bands", Value::Int(bands.len() as i64));
                for (n, &(kind, freq, gain, q)) in bands.iter().enumerate() {
                    eq = eq
                        .param(band_key("type", n), f32::from(kind))
                        .param(band_key("freq", n), freq)
                        .param(band_key("gain", n), gain)
                        .param(band_key("q", n), q);
                }
                (p.add(eq, column, row), "in", "out")
            }
            Fx::Compressor(c) => (
                dynamics_group(p, "Compressor", 0.0, *c, column, row),
                "in",
                "out",
            ),
            Fx::Limiter { ceiling } => {
                let c = Comp {
                    threshold: *ceiling,
                    ratio: 20.0,
                    attack: 0.001,
                    release: 0.08,
                    knee: 0.0,
                    makeup: 0.0,
                };
                (
                    dynamics_group(p, "Limiter", 1.0, c, column, row),
                    "in",
                    "out",
                )
            }
            Fx::Reverb {
                decay,
                damping,
                mix,
            } => (
                p.add(
                    spec("noodle.effect.reverb")
                        .param("decay", *decay)
                        .param("damping", *damping)
                        .param("mix", *mix),
                    column,
                    row,
                ),
                "in",
                "out",
            ),
        };
        p.connect(last, (node, input));
        last = (node, output);
    }
    last
}

fn band_key(base: &'static str, n: usize) -> &'static str {
    const KEYS: [[&str; 4]; 4] = [
        ["type1", "freq1", "gain1", "q1"],
        ["type2", "freq2", "gain2", "q2"],
        ["type3", "freq3", "gain3", "q3"],
        ["type4", "freq4", "gain4", "q4"],
    ];
    let which = ["type", "freq", "gain", "q"]
        .iter()
        .position(|&b| b == base)
        .expect("a band parameter");
    KEYS[n][which]
}

/// A Dynamics node as a group with its controls as the group's inputs, the
/// way every instrument here is: Compressor, Limiter and Gate are settings of
/// the one node, not nodes of their own. `mode` is the node's `mode` value.
fn dynamics_group(p: &mut Patch, name: &str, mode: f32, c: Comp, column: f32, row: f32) -> NodeId {
    let group = p.enter(name, column, row);
    let input = p.input("in", 0.0);
    let threshold = p.input("threshold", 1.0);
    let ratio = p.input("ratio", 2.0);
    let attack = p.input("attack", 3.0);
    let release = p.input("release", 4.0);
    let knee = p.input("knee", 5.0);
    let makeup = p.input("makeup", 6.0);
    let node = p.add(
        spec("noodle.effect.dynamics")
            .param("mode", mode)
            .param("threshold", c.threshold)
            .param("ratio", c.ratio)
            .param("attack", c.attack)
            .param("release", c.release)
            .param("knee", c.knee)
            .param("makeup", c.makeup),
        2.0,
        0.0,
    );
    p.connect(input, (node, "in"));
    p.connect(threshold, (node, "threshold"));
    p.connect(ratio, (node, "ratio"));
    p.connect(attack, (node, "attack"));
    p.connect(release, (node, "release"));
    p.connect(knee, (node, "knee"));
    p.connect(makeup, (node, "makeup"));
    let out = p.output("out", 3.0, 0.0);
    p.connect((node, "out"), out);
    p.leave(group);
    group
}

fn drum_label(drum: Drum) -> &'static str {
    match drum {
        Drum::Kick { .. } => "Kick",
        Drum::Snare { .. } => "Snare",
        Drum::Hat { .. } => "Hat",
        Drum::Clap { .. } => "Clap",
    }
}

fn out_key(i: usize) -> &'static str {
    const KEYS: [&str; 8] = [
        "out1", "out2", "out3", "out4", "out5", "out6", "out7", "out8",
    ];
    KEYS[i]
}

fn tonal(p: &mut Patch, tone: &Tone) {
    let poly = tone.voices > 1;
    let pitch = p.input("pitch", 0.0);
    let gate = p.input("gate", 1.0);
    let velocity = p.input("velocity", 2.0);
    let fade: Option<Port> = poly.then(|| p.input("fade", 3.0));
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
        p.connect(pitch, (frequency, "a"));
        p.connect(detune, (frequency, "b"));
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
    p.connect(gate, (filter_env, "gate"));
    let open = p.add(math("a * b + c").param("b", tone.amount), 6.0, 2.0);
    p.connect((filter_env, "out"), (open, "a"));
    p.connect(amount, (open, "b"));
    p.connect(cutoff, (open, "c"));
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
    p.connect(resonance, (filter, "resonance"));

    // Amplitude: an envelope through a VCA, then the velocity through another,
    // and Voices' steal fade through a last.
    let amp = p.add(adsr(tone.amp_env), 5.0, 4.0);
    p.connect(gate, (amp, "gate"));
    p.connect(attack, (amp, "attack"));
    p.connect(release, (amp, "release"));
    let shaped = p.add(spec("noodle.util.vca"), 8.0, 0.0);
    p.connect((filter, filtered), (shaped, "in"));
    p.connect((amp, "out"), (shaped, "level"));
    let touch = p.add(spec("noodle.util.vca"), 9.0, 0.0);
    p.connect((shaped, "out"), (touch, "in"));
    p.connect(velocity, (touch, "level"));
    let mut last: Port = (touch, "out");
    if let Some(fade) = fade {
        let faded = p.add(spec("noodle.util.vca"), 10.0, 0.0);
        p.connect(last, (faded, "in"));
        p.connect(fade, (faded, "level"));
        last = (faded, "out");
    }
    let out = p.output("out", 11.0, 0.0);
    p.connect(last, out);
    if poly {
        let active = p.output("active", 11.0, 4.0);
        p.connect((amp, "active"), active);
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
    gate: Port,
    velocity: Port,
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
    p.connect(frame.velocity, (touch, "level"));
    let out = p.output("out", column + 1.0, 0.0);
    p.connect((touch, "out"), out);
}

fn kick(p: &mut Patch, base: f32, sweep: f32, decay: f32) {
    let frame = drum_frame(p);
    let tune = p.input("tune", 3.0);
    let punch = p.input("sweep", 4.0);
    let length = p.input("decay", 5.0);
    let fall = p.add(adsr(env(0.001, 0.04, 0.0, 0.02)), 2.0, 1.0);
    p.connect(frame.gate, (fall, "gate"));
    let frequency = p.add(
        math("a * b + c").param("b", sweep).param("c", base),
        3.0,
        1.0,
    );
    p.connect((fall, "out"), (frequency, "a"));
    p.connect(punch, (frequency, "b"));
    p.connect(tune, (frequency, "c"));
    let body = p.add(spec("noodle.osc.sine"), 4.0, 1.0);
    p.connect((frequency, "out"), (body, "frequency"));
    let amp = p.add(adsr(env(0.001, decay, 0.0, 0.05)), 4.0, 3.0);
    p.connect(frame.gate, (amp, "gate"));
    p.connect(length, (amp, "decay"));
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
    p.connect(frame.gate, (noise_env, "gate"));
    p.connect(length, (noise_env, "decay"));
    let snap_vca = p.add(spec("noodle.util.vca"), 4.0, 0.0);
    p.connect((band, "band"), (snap_vca, "in"));
    p.connect((noise_env, "out"), (snap_vca, "level"));
    let body = p.add(spec("noodle.osc.sine").param("frequency", tone), 2.0, 4.0);
    p.connect(tune, (body, "frequency"));
    let body_env = p.add(adsr(env(0.001, decay * 0.5, 0.0, 0.03)), 3.0, 4.0);
    p.connect(frame.gate, (body_env, "gate"));
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
    p.connect(brightness, (high, "cutoff"));
    let amp = p.add(adsr(env(0.001, decay, 0.0, 0.01)), 3.0, 2.0);
    p.connect(frame.gate, (amp, "gate"));
    p.connect(length, (amp, "decay"));
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
    p.connect(brightness, (band, "cutoff"));
    let amp = p.add(adsr(env(0.008, decay, 0.0, 0.04)), 3.0, 2.0);
    p.connect(frame.gate, (amp, "gate"));
    p.connect(length, (amp, "decay"));
    let shaped = p.add(spec("noodle.util.vca"), 4.0, 0.0);
    p.connect((band, "band"), (shaped, "in"));
    p.connect((amp, "out"), (shaped, "level"));
    finish(p, &frame, (shaped, "out"), 5.0);
}
