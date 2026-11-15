//! The system library's spectral and convolution effects.
//!
//! Each is a group of primitive nodes (an Analyze, a spectral node, a
//! Synthesize; a Sample into a Convolver), with the controls as group inputs,
//! so it can be opened and changed once added. `system::files` makes a
//! template of the first variant of each family and a group preset of every
//! variant. See `docs/features/spectral-effects.md`.

use super::patch::{Patch, spec};
use super::system::{Build, Family, effect_family};
use noodle_core::{NodeId, Value};
use noodle_nodes::system_samples;

/// A spectral effect: `in` through an Analyze of `size`, the `middle` nodes
/// (one spectral node, each parameter a group input of the same name) and a
/// Synthesize, to `out`.
fn spectral_group(
    p: &mut Patch,
    name: &str,
    size: i64,
    kind: &'static str,
    settings: &[(&'static str, f32)],
    controls: &[&'static str],
) -> NodeId {
    let group = p.enter(name, 0.0, 0.0);
    let audio = p.input("in");
    let analyze = p.add(
        spec("noodle.spectral.analyze").config("size", Value::Int(size)),
        1.0,
        0.0,
    );
    p.connect(audio, (analyze, "in"));
    let mut node = spec(kind);
    for (key, value) in settings {
        node = node.param(key, *value);
    }
    let middle = p.add(node, 2.0, 0.0);
    p.connect((analyze, "spectrum"), (middle, "spectrum"));
    for control in controls {
        let input = p.input(control);
        p.connect(input, (middle, control));
    }
    let synth = p.add(spec("noodle.spectral.synthesize"), 3.0, 0.0);
    p.connect((middle, "out"), (synth, "spectrum"));
    let out = p.output("out");
    p.connect((synth, "out"), out);
    p.leave(group);
    group
}

/// A pitch shifter: Analyze, Spectral Pitch, Formant Shift, Synthesize. Its
/// inputs are `in`, `semitones` (the pitch) and `formant` (semitones; set it to
/// minus the pitch to keep a voice's character); its output is `out`.
fn pitch_shifter(p: &mut Patch, semitones: f32, formant: f32, size: i64) -> NodeId {
    let group = p.enter("Pitch Shifter", 0.0, 0.0);
    let audio = p.input("in");
    let pitch_input = p.input("semitones");
    let formant_input = p.input("formant");
    let analyze = p.add(
        spec("noodle.spectral.analyze").config("size", Value::Int(size)),
        1.0,
        0.0,
    );
    p.connect(audio, (analyze, "in"));
    let pitch = p.add(
        spec("noodle.spectral.pitch").param("shift", semitones),
        2.0,
        0.0,
    );
    p.connect((analyze, "spectrum"), (pitch, "spectrum"));
    p.connect(pitch_input, (pitch, "shift"));
    let warp = p.add(
        spec("noodle.spectral.formant").param("shift", formant),
        3.0,
        0.0,
    );
    p.connect((pitch, "out"), (warp, "spectrum"));
    p.connect(formant_input, (warp, "shift"));
    let synth = p.add(spec("noodle.spectral.synthesize"), 4.0, 0.0);
    p.connect((warp, "out"), (synth, "spectrum"));
    let out = p.output("out");
    p.connect((synth, "out"), out);
    p.leave(group);
    group
}

/// A convolution reverb: the impulse response (a Sample) into a Convolver
/// that the pre-delayed input goes through, mixed with the dry sound. Its
/// inputs are `in`, `predelay` (seconds, through a Delay), `dry` and `wet`
/// (decibels) and `gain` (the Convolver's); its output is `out`. Change the
/// room by changing the Sample inside.
fn convolution_reverb(p: &mut Patch, ir: &str, predelay: f32, dry: f32, wet: f32) -> NodeId {
    let group = p.enter("Convolution Reverb", 0.0, 0.0);
    let audio = p.input("in");
    let wait = p.input("predelay");
    let dry_level = p.input("dry");
    let wet_level = p.input("wet");
    let response = p.add(
        spec("noodle.instrument.sample")
            .config("sample", Value::Text(system_samples::hash(ir).into())),
        1.0,
        2.0,
    );
    let delay = p.add(spec("noodle.util.delay").param("time", predelay), 1.0, 0.0);
    p.connect(audio, (delay, "in"));
    p.connect(wait, (delay, "time"));
    let convolver = p.add(spec("noodle.effect.convolver"), 2.0, 0.0);
    p.connect((delay, "out"), (convolver, "in"));
    p.connect((response, "sample"), (convolver, "ir"));
    let blend = p.add(
        spec("noodle.util.mix")
            .config("inputs", Value::Int(2))
            .param("gain1", dry)
            .param("gain2", wet),
        3.0,
        0.0,
    );
    p.connect(audio, (blend, "in1"));
    p.connect((convolver, "out"), (blend, "in2"));
    p.connect(dry_level, (blend, "gain1"));
    p.connect(wet_level, (blend, "gain2"));
    let out = p.output("out");
    p.connect((blend, "out"), out);
    p.leave(group);
    group
}

/// A denoiser: two Analyzes (the sound, and an optional `noise` to learn the
/// profile from) into a Spectral Denoise, into a Synthesize. Its inputs are
/// `in`, `noise`, `learn`, `reduction`, `threshold` and `smooth`.
fn denoiser(p: &mut Patch, reduction: f32, threshold: f32, smooth: f32) -> NodeId {
    let group = p.enter("Denoiser", 0.0, 0.0);
    let audio = p.input("in");
    let noise = p.input("noise");
    let learn = p.input("learn");
    let amount = p.input("reduction");
    let margin = p.input("threshold");
    let slow = p.input("smooth");
    let sound = p.add(spec("noodle.spectral.analyze"), 1.0, 0.0);
    p.connect(audio, (sound, "in"));
    let profile = p.add(spec("noodle.spectral.analyze"), 1.0, 2.0);
    p.connect(noise, (profile, "in"));
    let clean = p.add(
        spec("noodle.spectral.denoise")
            .param("reduction", reduction)
            .param("threshold", threshold)
            .param("smooth", smooth),
        2.0,
        0.0,
    );
    p.connect((sound, "spectrum"), (clean, "spectrum"));
    p.connect((profile, "spectrum"), (clean, "noise"));
    p.connect(learn, (clean, "learn"));
    p.connect(amount, (clean, "reduction"));
    p.connect(margin, (clean, "threshold"));
    p.connect(slow, (clean, "smooth"));
    let synth = p.add(spec("noodle.spectral.synthesize"), 3.0, 0.0);
    p.connect((clean, "out"), (synth, "spectrum"));
    let out = p.output("out");
    p.connect((synth, "out"), out);
    p.leave(group);
    group
}

pub(super) fn families() -> Vec<Family> {
    vec![
        effect_family(
            "pitch-shifter",
            "Pitch Shifter",
            [
                ("Octave Up", 12.0, 0.0, 2048),
                ("Octave Down", -12.0, 0.0, 4096),
                ("Voice Up a Fifth", 7.0, -7.0, 2048),
                ("Voice Down a Third", -4.0, 4.0, 2048),
                ("Quick Fourth", 5.0, 0.0, 1024),
            ]
            .into_iter()
            .map(|(name, semitones, formant, size)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| pitch_shifter(p, semitones, formant, size));
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "time-stretch",
            "Time Stretch",
            [
                ("Half Speed", 0.5, 4.0),
                ("Slow Motion", 0.25, 4.0),
                ("Frozen Wash", 0.05, 8.0),
                ("Double Time", 2.0, 1.0),
            ]
            .into_iter()
            .map(|(name, rate, length)| {
                let build: Build = Box::new(move |p: &mut Patch| {
                    spectral_group(
                        p,
                        "Time Stretch",
                        2048,
                        "noodle.spectral.stretch",
                        &[("rate", rate), ("length", length)],
                        &["rate", "length"],
                    )
                });
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "spectral-compressor",
            "Spectral Compressor",
            [
                ("Tame Harshness", -40.0, 4.0, 0.02, 0.2, 0.0, 0.0),
                ("Even Out", -60.0, 3.0, 0.05, 0.3, 3.0, 0.0),
                ("Lift Detail", -50.0, 2.0, 0.02, 0.2, 0.0, 1.0),
                ("Hard Squash", -70.0, 10.0, 0.005, 0.1, 6.0, 0.0),
            ]
            .into_iter()
            .map(|(name, threshold, ratio, attack, release, makeup, mode)| {
                let build: Build = Box::new(move |p: &mut Patch| {
                    spectral_group(
                        p,
                        "Spectral Compressor",
                        2048,
                        "noodle.spectral.compress",
                        &[
                            ("threshold", threshold),
                            ("ratio", ratio),
                            ("attack", attack),
                            ("release", release),
                            ("makeup", makeup),
                            ("mode", mode),
                        ],
                        &[
                            "threshold",
                            "ratio",
                            "attack",
                            "release",
                            "makeup",
                            "bands",
                            "mode",
                        ],
                    )
                });
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "denoiser",
            "Denoiser",
            [
                ("Standard", 20.0, 6.0, 0.5),
                ("Gentle", 12.0, 3.0, 0.3),
                ("Heavy Hiss", 30.0, 9.0, 0.7),
            ]
            .into_iter()
            .map(|(name, reduction, threshold, smooth)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| denoiser(p, reduction, threshold, smooth));
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "convolution-reverb",
            "Convolution Reverb",
            [
                ("Small Room", "ir-room", 0.0, 0.0, -9.0),
                ("Concert Hall", "ir-hall", 0.02, -3.0, -6.0),
                ("Bright Plate", "ir-plate", 0.0, 0.0, -8.0),
            ]
            .into_iter()
            .map(|(name, ir, predelay, dry, wet)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| convolution_reverb(p, ir, predelay, dry, wet));
                (name, build)
            })
            .collect(),
        ),
    ]
}
