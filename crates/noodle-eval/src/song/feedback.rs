//! The feedback templates: a plucked string, a comb and a flanger, each a
//! group of primitives with a loop of Delay in it. The loop is as tight as the
//! pitch asks (a string at 1 kHz is a delay of 48 samples), which the loop's
//! region runs in chunks that short (see `docs/architecture/feedback-and-silence.md`).
//!
//! The delay's time is `1 / frequency`, less the samples the loop's filter
//! holds the signal back by, so the pitch is the one asked for.

use super::patch::{Patch, Port, spec};
use super::system::{Build, Family, effect_family};
use noodle_core::{Command, NodeId, Value};
use noodle_nodes::{VOICE_MIX_ID, VOICES_ID};

fn math(expr: &str) -> super::patch::Spec {
    spec("noodle.util.math").config("expr", Value::Text(expr.into()))
}

fn rename(p: &mut Patch, group: NodeId, name: &str) {
    p.history
        .apply(
            p.project,
            Command::SetConfig {
                node: group,
                key: "name".into(),
                value: Some(Value::Text(name.into())),
            },
        )
        .expect("a group has a name");
}

/// One voice of the string: noise burst into a loop of Delay, a low-pass and
/// a Gain. Inputs are `pitch`, `gate`, `velocity`, `fade`, `damping` (hertz)
/// `decay` (decibels lost per second) and `release` (seconds);
/// outputs are `out` and `active`.
fn string_voice(p: &mut Patch, damping: f32, decay: f32, release: f32) -> NodeId {
    let group = p.enter("String", 3.0, 0.0);
    let pitch = p.input("pitch");
    let gate = p.input("gate");
    let velocity = p.input("velocity");
    let fade = p.input("fade");
    let tone = p.input("damping");
    let loss = p.input("decay");
    let ring = p.input("release");

    // The pluck: a few milliseconds of noise.
    let hiss = p.add(spec("noodle.noise.white"), 1.0, 2.0);
    let burst = p.add(
        spec("noodle.mod.adsr")
            .param("attack", 0.001)
            .param("decay", 0.004)
            .param("sustain", 0.0)
            .param("release", 0.001),
        1.0,
        3.0,
    );
    p.connect(gate, (burst, "gate"));
    let pluck = p.add(spec("noodle.util.vca"), 2.0, 2.0);
    p.connect((hiss, "out"), (pluck, "in"));
    p.connect((burst, "out"), (pluck, "level"));
    let struck = p.add(spec("noodle.util.vca"), 3.0, 2.0);
    p.connect((pluck, "out"), (struck, "in"));
    p.connect(velocity, (struck, "level"));

    // The loop. A wire into Delay's `time` moves the slider along its travel
    // (0 to 2 s), so seconds go through a Math node to a fraction of that. A
    // voice that is not playing has a pitch of 0, held to 20 Hz so the idle
    // lanes don't shorten the loop's chunks.
    let time = p.add(
        math("(1 / max(a, 20) - 0.000024 - 1.4 / (6.2832 * max(b, 100))) / 2").param("b", damping),
        2.0,
        0.0,
    );
    p.connect(pitch, (time, "a"));
    p.connect(tone, (time, "b"));
    let feed = p.add(
        spec("noodle.util.mix").config("inputs", Value::Int(2)),
        3.0,
        0.0,
    );
    p.connect((struck, "out"), (feed, "in1"));
    let delay = p.add(spec("noodle.util.delay").param("time", 0.0), 4.0, 0.0);
    p.connect((feed, "out"), (delay, "in"));
    p.connect((time, "out"), (delay, "time"));
    let damp = p.add(
        spec("noodle.filter.svf")
            .param("cutoff", damping)
            .param("resonance", 0.0),
        5.0,
        0.0,
    );
    p.connect((delay, "out"), (damp, "in"));
    p.connect(tone, (damp, "cutoff"));
    // The loss is in decibels per second, so a high note rings as long as a
    // low one: each trip round the loop loses that over the pitch. A wire
    // moves Gain's slider along its travel (-60 to 24 dB), so the decibels
    // go through a Math node to a fraction of that, onto a slider left at -60.
    let per_trip = p.add(
        math("(60 - a / max(b, 20)) / 84").param("a", decay),
        5.0,
        1.0,
    );
    p.connect(loss, (per_trip, "a"));
    p.connect(pitch, (per_trip, "b"));
    let back = p.add(spec("noodle.util.gain").param("gain", -60.0), 6.0, 0.0);
    p.connect((damp, "low"), (back, "in"));
    p.connect((per_trip, "out"), (back, "gain"));
    p.connect((back, "out"), (feed, "in2"));

    // The note ends when the key does, over `release`; the same envelope
    // tells Voices when the voice is free.
    let amp = p.add(
        spec("noodle.mod.adsr")
            .param("attack", 0.001)
            .param("decay", 0.1)
            .param("sustain", 1.0)
            .param("release", release),
        5.0,
        2.0,
    );
    p.connect(gate, (amp, "gate"));
    p.connect(ring, (amp, "release"));
    let held = p.add(spec("noodle.util.vca"), 7.0, 0.0);
    p.connect((damp, "low"), (held, "in"));
    p.connect((amp, "out"), (held, "level"));
    let faded = p.add(spec("noodle.util.vca"), 8.0, 0.0);
    p.connect((held, "out"), (faded, "in"));
    p.connect(fade, (faded, "level"));
    let out = p.output("out");
    p.connect((faded, "out"), out);
    let active = p.output("active");
    p.connect((amp, "active"), active);
    p.leave(group);
    group
}

/// A plucked string that plays notes: a group with `notes`, `damping`
/// (hertz), `decay` (decibels per second) and `release` (seconds) in and `out` out.
fn plucked_string(p: &mut Patch, damping: f32, decay: f32, release: f32, trim: f32) -> NodeId {
    let group = p.enter("Plucked String", 0.0, 0.0);
    let notes = p.input("notes");
    let tone = p.input("damping");
    let loss = p.input("decay");
    let ring = p.input("release");
    let source = p.add(spec(VOICES_ID).config("voices", Value::Int(6)), 1.0, 0.0);
    p.connect(notes, (source, "in"));
    let string = string_voice(p, damping, decay, release);
    rename(p, string, "String");
    for lane in ["pitch", "gate", "velocity", "fade"] {
        p.connect((source, lane), (string, lane));
    }
    p.connect(tone, (string, "damping"));
    p.connect(loss, (string, "decay"));
    p.connect(ring, (string, "release"));
    p.connect((string, "active"), (source, "busy"));
    let sum = p.add(spec(VOICE_MIX_ID), 5.0, 0.0);
    p.connect((string, "out"), (sum, "in"));
    let level = p.add(spec("noodle.util.gain").param("gain", trim), 6.0, 0.0);
    p.connect((sum, "out"), (level, "in"));
    let out = p.output("out");
    p.connect((level, "out"), out);
    p.leave(group);
    group
}

/// A comb filter: the signal into a loop of Delay tuned by `frequency`, with
/// a low-pass and `feedback` in the loop, blended with the dry signal. Inputs
/// are `in`, `frequency` (hertz), `feedback` (decibels), `damping` (hertz) and
/// `wet` (decibels); the output is `out`.
fn comb(p: &mut Patch, frequency: f32, feedback: f32, damping: f32, wet: f32) -> NodeId {
    let group = p.enter("Comb", 0.0, 0.0);
    let audio = p.input("in");
    let pitch = p.input("frequency");
    let repeats = p.input("feedback");
    let tone = p.input("damping");
    let level = p.input("wet");
    let time = p.add(
        math("(1 / clamp(a, 20, 5000) - 0.000025) / 2").param("a", frequency),
        1.0,
        1.0,
    );
    p.connect(pitch, (time, "a"));
    let feed = p.add(
        spec("noodle.util.mix").config("inputs", Value::Int(2)),
        2.0,
        0.0,
    );
    p.connect(audio, (feed, "in1"));
    let delay = p.add(spec("noodle.util.delay").param("time", 0.0), 3.0, 0.0);
    p.connect((feed, "out"), (delay, "in"));
    p.connect((time, "out"), (delay, "time"));
    let damp = p.add(
        spec("noodle.filter.svf")
            .param("cutoff", damping)
            .param("resonance", 0.0),
        4.0,
        0.0,
    );
    p.connect((delay, "out"), (damp, "in"));
    p.connect(tone, (damp, "cutoff"));
    let back = p.add(spec("noodle.util.gain").param("gain", feedback), 5.0, 0.0);
    p.connect((damp, "low"), (back, "in"));
    p.connect(repeats, (back, "gain"));
    p.connect((back, "out"), (feed, "in2"));
    let blend = p.add(
        spec("noodle.util.mix")
            .config("inputs", Value::Int(2))
            .param("gain2", wet),
        6.0,
        0.0,
    );
    p.connect(audio, (blend, "in1"));
    p.connect((damp, "low"), (blend, "in2"));
    p.connect(level, (blend, "gain2"));
    let out = p.output("out");
    p.connect((blend, "out"), out);
    p.leave(group);
    group
}

/// A flanger: a short Delay swept by an LFO, with `feedback` round it, blended
/// with the dry signal. Inputs are `in`, `rate` (hertz), `depth` (0 to 1),
/// `feedback` (decibels) and `wet` (decibels); the output is `out`.
fn flanger(p: &mut Patch, rate: f32, depth: f32, feedback: f32, wet: f32) -> NodeId {
    let group = p.enter("Flanger", 0.0, 0.0);
    let audio = p.input("in");
    let speed = p.input("rate");
    let amount = p.input("depth");
    let repeats = p.input("feedback");
    let level = p.input("wet");
    // A wire moves the LFO's rate slider along its travel (0.01 to 100 Hz,
    // logarithmic), so hertz become a fraction of that range.
    let travel = p.add(math("ln(a * 100) / ln(10000)").param("a", rate), 1.0, 2.0);
    p.connect(speed, (travel, "a"));
    let lfo = p.add(spec("noodle.mod.lfo").param("rate", 0.01), 2.0, 2.0);
    p.connect((travel, "out"), (lfo, "rate"));
    // The delay swings between 0.5 and 4.5 ms about 2.5 ms, as seconds that
    // Math turns into a fraction of the slider's travel (2 s).
    let time = p.add(
        math("(0.0025 + 0.002 * a * b) / 2").param("a", depth),
        3.0,
        2.0,
    );
    p.connect(amount, (time, "a"));
    p.connect((lfo, "out"), (time, "b"));
    let feed = p.add(
        spec("noodle.util.mix").config("inputs", Value::Int(2)),
        3.0,
        0.0,
    );
    p.connect(audio, (feed, "in1"));
    let delay = p.add(spec("noodle.util.delay").param("time", 0.0), 4.0, 0.0);
    p.connect((feed, "out"), (delay, "in"));
    p.connect((time, "out"), (delay, "time"));
    let back = p.add(spec("noodle.util.gain").param("gain", feedback), 5.0, 0.0);
    p.connect((delay, "out"), (back, "in"));
    p.connect(repeats, (back, "gain"));
    p.connect((back, "out"), (feed, "in2"));
    let blend = p.add(
        spec("noodle.util.mix")
            .config("inputs", Value::Int(2))
            .param("gain1", -3.0)
            .param("gain2", wet),
        6.0,
        0.0,
    );
    p.connect(audio, (blend, "in1"));
    p.connect((delay, "out"), (blend, "in2"));
    p.connect(level, (blend, "gain2"));
    let out = p.output("out");
    p.connect((blend, "out"), out);
    p.leave(group);
    group
}

pub(super) fn families() -> Vec<Family> {
    let _: Option<Port> = None;
    vec![
        effect_family(
            "plucked-string",
            "Plucked String",
            [
                ("Nylon", 3500.0, 35.0, 0.4),
                ("Steel", 7000.0, 20.0, 0.6),
                ("Muted Pluck", 1800.0, 110.0, 0.15),
            ]
            .into_iter()
            .map(|(name, damping, decay, release)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| plucked_string(p, damping, decay, release, 0.0));
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "comb",
            "Comb",
            [
                ("Metal Ring", 220.0, -0.8, 6000.0, -6.0),
                ("Resonator", 440.0, -0.3, 3500.0, -9.0),
                ("Pipe", 110.0, -0.5, 2500.0, -6.0),
            ]
            .into_iter()
            .map(|(name, frequency, feedback, damping, wet)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| comb(p, frequency, feedback, damping, wet));
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "flanger",
            "Flanger",
            [
                ("Classic", 0.25, 0.8, -4.0, -3.0),
                ("Jet", 0.08, 1.0, -1.5, -3.0),
                ("Shimmer", 1.2, 0.4, -8.0, -6.0),
            ]
            .into_iter()
            .map(|(name, rate, depth, feedback, wet)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| flanger(p, rate, depth, feedback, wet));
                (name, build)
            })
            .collect(),
        ),
    ]
}
