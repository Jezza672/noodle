//! The system library's playable instruments.
//!
//! Unlike the song kit's synths (`Mono Synth`, `Poly Synth`, which take pitch,
//! gate and velocity lanes from a Voices node the track supplies), these take
//! `notes` and make sound: each is a group of primitive nodes with the Voices
//! (or Mono Note) node, the synth voice as a nested group, and the Voice Mix
//! inside, so adding one to a track and playing MIDI into it is all there is
//! to do. Its controls (`cutoff`, `resonance`, `amount`, `detune`, `attack`,
//! `release`) are group inputs; open the group, and the nested `Voice`, to
//! change anything else. The sampled instruments play recordings shipped with
//! the program (`noodle_nodes::system_samples`, sources in
//! `crates/noodle-nodes/assets/samples/SOURCES.md`).
//!
//! `system::files` makes a template of the first variant of each family and a
//! group preset of every variant, which must have the same shape.

use super::kit::{self, Env, Filter, Instrument, Osc, Rack, Slot, Tone, Wave, env};
use super::patch::{Patch, Port, spec};
use super::system::{Build, Family, effect_family};
use noodle_core::{Command, NodeId, Value};
use noodle_nodes::{VOICE_MIX_ID, VOICES_ID, system_samples};

/// The controls of a synth voice, in the order the group lists them.
const CONTROLS: [&str; 6] = [
    "cutoff",
    "resonance",
    "amount",
    "detune",
    "attack",
    "release",
];

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

/// A synth that plays notes: a group with `notes` and the synth's controls in
/// and `out` out. A tone with more than one voice goes through Voices and
/// Voice Mix, a single voice through Mono Note.
fn playable(p: &mut Patch, name: &str, mut tone: Tone, trim: f32) -> NodeId {
    for osc in &mut tone.oscs {
        osc.db += trim;
    }
    let poly = tone.voices > 1;
    let voices = tone.voices;
    let group = p.enter(name, 0.0, 0.0);
    let notes = p.input("notes");
    let controls: Vec<Port> = CONTROLS.iter().map(|name| p.input(name)).collect();
    let source = if poly {
        p.add(
            spec(VOICES_ID).config("voices", Value::Int(i64::from(voices))),
            1.0,
            0.0,
        )
    } else {
        p.add(spec("noodle.event.mono"), 1.0, 0.0)
    };
    p.connect(notes, (source, "in"));
    let synth = kit::build(p, &Instrument::Tone(tone), 3.0, 0.0);
    rename(p, synth, "Voice");
    let lanes: &[&'static str] = if poly {
        &["pitch", "gate", "velocity", "fade"]
    } else {
        &["pitch", "gate", "velocity"]
    };
    for &lane in lanes {
        p.connect((source, lane), (synth, lane));
    }
    for (name, port) in CONTROLS.iter().zip(controls) {
        p.connect(port, (synth, name));
    }
    let out = p.output("out");
    if poly {
        p.connect((synth, "active"), (source, "busy"));
        let sum = p.add(spec(VOICE_MIX_ID), 5.0, 0.0);
        p.connect((synth, "out"), (sum, "in"));
        p.connect((sum, "out"), out);
    } else {
        p.connect((synth, "out"), out);
    }
    p.leave(group);
    group
}

/// `trim` (decibels) sets the level of a three-note chord in the middle of
/// the range to about -9 dBFS, so every instrument starts at a usable volume.
fn family(
    slug: &'static str,
    name: &'static str,
    trim: f32,
    voices: Vec<(&'static str, Tone)>,
) -> Family {
    effect_family(
        slug,
        name,
        voices
            .into_iter()
            .map(|(preset, tone)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| playable(p, name, tone.clone(), trim));
                (preset, build)
            })
            .collect(),
    )
}

fn saw(detune: f64, pan: f32, db: f32) -> Osc {
    Osc::new(Wave::Saw).detuned(detune).panned(pan).level(db)
}

fn sine(ratio: f64, db: f32) -> Osc {
    Osc::new(Wave::Sine).octave(ratio).level(db)
}

fn lead(cutoff: f32, resonance: f32, amount: f32, filter_env: Env, amp: Env, sub: f32) -> Tone {
    Tone::new(vec![
        saw(-1.0, -0.2, -6.0),
        saw(1.0, 0.2, -6.0),
        Osc::new(Wave::Square).octave(0.5).level(sub),
    ])
    .filter(Filter::Ladder, cutoff, resonance, amount)
    .filter_env(filter_env)
    .amp_env(amp)
    .detune(8.0)
}

fn bass(cutoff: f32, resonance: f32, amount: f32, decay: f32, sub: f32) -> Tone {
    Tone::new(vec![
        Osc::new(Wave::Saw).level(-5.0),
        Osc::new(Wave::Square).octave(0.5).level(sub),
        Osc::new(Wave::Sine).octave(0.5).level(-4.0),
    ])
    .filter(Filter::Ladder, cutoff, resonance, amount)
    .filter_env(env(0.001, decay, 0.1, 0.08))
    .amp_env(env(0.002, 0.2, 0.85, 0.08))
}

fn keys(decay: f32, release: f32, tine: f32, bell: f32, cutoff: f32) -> Tone {
    Tone::new(vec![
        sine(1.0, -4.0),
        sine(2.0, tine),
        Osc::new(Wave::Triangle).octave(4.0).level(bell),
    ])
    .filter(Filter::Svf, cutoff, 0.1, 0.1)
    .filter_env(env(0.001, 0.25, 0.1, 0.2))
    .amp_env(env(0.002, decay, 0.15, release))
    .voices(8)
}

/// Drawbars: sines at 16', 8', 5⅓' (not shown), 4', 2⅔', 2' as levels.
fn organ(levels: [f32; 5], release: f32, cutoff: f32) -> Tone {
    let ratios = [0.5, 1.0, 2.0, 3.0, 4.0];
    Tone::new(
        ratios
            .iter()
            .zip(levels)
            .map(|(&r, db)| sine(r, db))
            .collect(),
    )
    .filter(Filter::Ladder, cutoff, 0.05, 0.0)
    .filter_env(env(0.001, 0.1, 1.0, 0.1))
    .amp_env(env(0.004, 0.05, 1.0, release))
    .voices(8)
}

fn strings(cutoff: f32, attack: f32, release: f32, sustain: f32) -> Tone {
    Tone::new(vec![
        saw(-1.0, -0.7, -8.0),
        saw(-0.4, -0.25, -8.0),
        saw(0.4, 0.25, -8.0),
        saw(1.0, 0.7, -8.0),
    ])
    .filter(Filter::Svf, cutoff, 0.1, 0.15)
    .filter_env(env(attack, 0.6, 0.8, release))
    .amp_env(env(attack, 0.3, sustain, release))
    .detune(14.0)
    .voices(8)
}

fn brass(cutoff: f32, amount: f32, attack: f32, release: f32) -> Tone {
    Tone::new(vec![
        saw(-1.0, -0.3, -6.0),
        saw(0.0, 0.0, -6.0),
        saw(1.0, 0.3, -6.0),
    ])
    .filter(Filter::Ladder, cutoff, 0.15, amount)
    .filter_env(env(attack, 0.25, 0.6, release))
    .amp_env(env(attack * 0.5, 0.2, 0.9, release))
    .detune(6.0)
    .voices(6)
}

/// Inharmonic partials for bells and mallets.
fn bells(r2: f64, r3: f64, decay: f32, upper: f32) -> Tone {
    Tone::new(vec![
        sine(1.0, -5.0),
        sine(r2, upper),
        Osc::new(Wave::Triangle).octave(r3).level(upper - 6.0),
    ])
    .filter(Filter::Svf, 9000.0, 0.1, 0.0)
    .filter_env(env(0.001, 0.2, 0.5, 0.2))
    .amp_env(env(0.001, decay, 0.0, decay * 0.5))
    .voices(8)
}

fn chip(duty_cutoff: f32, decay: f32, sustain: f32) -> Tone {
    Tone::new(vec![
        Osc::new(Wave::Square).level(-9.0),
        Osc::new(Wave::Square).octave(2.0).detuned(1.0).level(-14.0),
        Osc::new(Wave::Triangle).octave(0.5).level(-8.0),
    ])
    .filter(Filter::Svf, duty_cutoff, 0.1, 0.0)
    .filter_env(env(0.001, 0.1, 1.0, 0.05))
    .amp_env(env(0.001, decay, sustain, 0.04))
    .detune(5.0)
    .voices(4)
}

fn pluck_synth(cutoff: f32, amount: f32, decay: f32, release: f32) -> Tone {
    Tone::new(vec![
        saw(-1.0, -0.4, -5.0),
        saw(1.0, 0.4, -5.0),
        Osc::new(Wave::Square).octave(2.0).level(-14.0),
    ])
    .filter(Filter::Svf, cutoff, 0.25, amount)
    .filter_env(env(0.001, 0.18, 0.0, 0.1))
    .amp_env(env(0.001, decay, 0.0, release))
    .detune(9.0)
    .voices(8)
}

fn warm_pad(cutoff: f32, attack: f32, release: f32) -> Tone {
    Tone::new(vec![
        saw(-1.0, -0.6, -6.0),
        saw(0.0, 0.0, -6.0),
        saw(1.0, 0.6, -6.0),
        sine(0.5, -8.0),
    ])
    .filter(Filter::Ladder, cutoff, 0.15, 0.1)
    .filter_env(env(attack, 0.8, 0.7, release))
    .amp_env(env(attack, 0.3, 0.85, release))
    .detune(12.0)
    .voices(8)
}

/// A multisample: a Sample Set into a gated Sampler, then a low-pass. Inputs
/// are `notes`, `tune` (cents), `bend range` (semitones), `release` (seconds),
/// `gain` (decibels) and `cutoff` (hertz); the output is `out`.
fn sampled(p: &mut Patch, name: &str, set: &str, release: f32, cutoff: f32, gain: f32) -> NodeId {
    let group = p.enter(name, 0.0, 0.0);
    let notes = p.input("notes");
    let tune = p.input("tune");
    let bend_range = p.input("bend range");
    let fade = p.input("release");
    let level = p.input("gain");
    let tone = p.input("cutoff");
    let set = p.add(
        spec("noodle.instrument.sample_set").config("sample", Value::Text(set.into())),
        1.0,
        2.0,
    );
    let sampler = p.add(
        spec("noodle.instrument.sampler")
            .param("mode", 1.0)
            .param("release", release)
            .param("gain", gain)
            .param("bend_range", 2.0),
        2.0,
        0.0,
    );
    p.connect((set, "sample"), (sampler, "sample"));
    p.connect(notes, (sampler, "in"));
    p.connect(tune, (sampler, "tune"));
    p.connect(bend_range, (sampler, "bend_range"));
    p.connect(fade, (sampler, "release"));
    p.connect(level, (sampler, "gain"));
    let filter = p.add(spec("noodle.filter.svf").param("cutoff", cutoff), 3.0, 0.0);
    p.connect((sampler, "out"), (filter, "in"));
    p.connect(tone, (filter, "cutoff"));
    let out = p.output("out");
    p.connect((filter, "low"), out);
    p.leave(group);
    group
}

/// The Sample Set text for shipped samples: `hash@root` for each of `roots`
/// (`name`, key).
fn set_text(roots: &[(&str, u8)]) -> String {
    roots
        .iter()
        .map(|(name, key)| format!("{}@{key}", system_samples::hash(name)))
        .collect::<Vec<_>>()
        .join(";")
}

const STEEL_DRUM: [(&str, u8); 8] = [
    ("steel-drum-60", 60),
    ("steel-drum-63", 63),
    ("steel-drum-66", 66),
    ("steel-drum-69", 69),
    ("steel-drum-72", 72),
    ("steel-drum-75", 75),
    ("steel-drum-78", 78),
    ("steel-drum-81", 81),
];

const BANJO: [(&str, u8); 8] = [
    ("banjo-Ds3", 39),
    ("banjo-G3", 43),
    ("banjo-As3", 46),
    ("banjo-Cs4", 49),
    ("banjo-E4", 52),
    ("banjo-G4", 55),
    ("banjo-As4", 58),
    ("banjo-Cs5", 61),
];

fn sampled_family(
    slug: &'static str,
    name: &'static str,
    roots: &'static [(&'static str, u8)],
    voices: [(&'static str, f32, f32, f32); 3],
) -> Family {
    effect_family(
        slug,
        name,
        voices
            .into_iter()
            .map(|(preset, release, cutoff, gain)| {
                let build: Build = Box::new(move |p: &mut Patch| {
                    sampled(p, name, &set_text(roots), release, cutoff, gain)
                });
                (preset, build)
            })
            .collect(),
    )
}

/// The eight body sounds on General MIDI drum keys.
fn body_kit(db: f32, spread: f32) -> Instrument {
    let slots = [
        ("Stomp", "body-stomp", 36, -6.0, 0.0),
        ("Chest Slap", "body-chest-slap", 38, -6.0, 0.1),
        ("Clap", "body-clap", 39, -6.0, 0.0),
        ("Finger Snap", "body-finger-snap", 42, -6.0 + db, -0.5),
        ("Snap", "body-snap", 46, -6.0 + db, 0.5),
        ("Belly", "body-belly", 45, -6.0, -0.4),
        ("Chest Thump", "body-chest-thump", 50, -6.0, 0.4),
        ("Cheek Pop", "body-cheek-pop", 49, -6.0 + db, 0.3),
    ];
    Instrument::Rack(Rack {
        slots: slots
            .into_iter()
            .map(|(label, sample, key, db, pan)| Slot {
                sample: format!("system:{sample}"),
                label: Some(label),
                drum: kit::Drum::Clap {
                    cutoff: 1500.0,
                    decay: 0.1,
                },
                key,
                db,
                pan: pan * spread / 0.6,
            })
            .collect(),
        controls: true,
    })
}

pub(super) fn families() -> Vec<Family> {
    vec![
        family(
            "lead-synth",
            "Lead Synth",
            -4.5,
            vec![
                (
                    "Classic Lead",
                    lead(
                        1800.0,
                        0.2,
                        0.3,
                        env(0.005, 0.3, 0.5, 0.2),
                        env(0.005, 0.2, 0.8, 0.15),
                        -12.0,
                    ),
                ),
                (
                    "Soft Lead",
                    lead(
                        900.0,
                        0.1,
                        0.15,
                        env(0.04, 0.4, 0.6, 0.3),
                        env(0.03, 0.2, 0.85, 0.3),
                        -18.0,
                    ),
                ),
                (
                    "Screamer",
                    lead(
                        2600.0,
                        0.55,
                        0.45,
                        env(0.002, 0.2, 0.4, 0.1),
                        env(0.002, 0.15, 0.9, 0.1),
                        -9.0,
                    ),
                ),
            ],
        ),
        family(
            "bass-synth",
            "Bass Synth",
            -4.5,
            vec![
                ("Moog Bass", bass(260.0, 0.3, 0.3, 0.2, -8.0)),
                ("Sub Bass", bass(140.0, 0.1, 0.05, 0.3, -4.0)),
                ("Acid Squelch", bass(380.0, 0.65, 0.55, 0.14, -14.0)),
            ],
        ),
        family(
            "electric-piano",
            "Electric Piano",
            -14.0,
            vec![
                ("Tine Piano", keys(1.2, 0.35, -12.0, -22.0, 6000.0)),
                ("Mellow Keys", keys(1.6, 0.4, -18.0, -40.0, 2500.0)),
                ("Bright Keys", keys(0.7, 0.25, -9.0, -16.0, 9000.0)),
            ],
        ),
        family(
            "organ",
            "Organ",
            -18.0,
            vec![
                (
                    "Full Organ",
                    organ([-8.0, -4.0, -6.0, -10.0, -10.0], 0.04, 9000.0),
                ),
                (
                    "Jazz Organ",
                    organ([-6.0, -3.0, -40.0, -9.0, -40.0], 0.05, 7000.0),
                ),
                (
                    "Flute Organ",
                    organ([-40.0, -3.0, -12.0, -40.0, -40.0], 0.08, 5000.0),
                ),
            ],
        ),
        family(
            "string-ensemble",
            "String Ensemble",
            -15.0,
            vec![
                ("Full Strings", strings(3200.0, 0.25, 0.5, 0.9)),
                ("Slow Strings", strings(2200.0, 0.8, 1.2, 0.9)),
                ("Staccato Strings", strings(4500.0, 0.01, 0.12, 0.0)),
            ],
        ),
        family(
            "brass-section",
            "Brass Section",
            -16.0,
            vec![
                ("Brass", brass(900.0, 0.35, 0.06, 0.2)),
                ("Soft Horns", brass(600.0, 0.25, 0.12, 0.3)),
                ("Brass Stab", brass(1400.0, 0.4, 0.01, 0.1)),
            ],
        ),
        family(
            "bells",
            "Bells",
            -13.5,
            vec![
                ("Glass Bell", bells(2.76, 5.4, 2.0, -12.0)),
                ("Marimba", bells(4.0, 10.0, 0.6, -20.0)),
                ("Music Box", bells(3.0, 6.0, 1.0, -16.0)),
            ],
        ),
        family(
            "chip-synth",
            "Chip Synth",
            -16.5,
            vec![
                ("Chip Lead", chip(8000.0, 0.1, 0.9)),
                ("Chip Arp", chip(6000.0, 0.12, 0.0)),
                ("Chip Bass", chip(2500.0, 0.2, 0.7)),
            ],
        ),
        family(
            "pluck-synth",
            "Pluck Synth",
            -5.5,
            vec![
                ("Pluck", pluck_synth(1800.0, 0.3, 0.35, 0.2)),
                ("Harp", pluck_synth(3000.0, 0.15, 0.9, 0.5)),
                ("Short Stab", pluck_synth(1200.0, 0.4, 0.12, 0.08)),
            ],
        ),
        family(
            "warm-pad",
            "Warm Pad",
            -10.5,
            vec![
                ("Warm Pad", warm_pad(1200.0, 0.5, 1.0)),
                ("Bright Pad", warm_pad(3000.0, 0.3, 0.7)),
                ("Slow Swell", warm_pad(700.0, 1.6, 2.0)),
            ],
        ),
        sampled_family(
            "steel-drum",
            "Steel Drum",
            &STEEL_DRUM,
            [
                ("Open", 0.5, 20_000.0, -10.0),
                ("Muted", 0.08, 3500.0, -10.0),
                ("Mellow", 0.3, 1800.0, -10.0),
            ],
        ),
        sampled_family(
            "banjo",
            "Banjo",
            &BANJO,
            [
                ("Open", 0.25, 20_000.0, -5.0),
                ("Muted", 0.06, 3000.0, -5.0),
                ("Warm", 0.2, 2200.0, -5.0),
            ],
        ),
        effect_family(
            "body-percussion",
            "Body Percussion Kit",
            [
                ("Natural", 0.0, 0.0),
                ("Wide", -2.0, 0.6),
                ("Close", 3.0, 0.0),
            ]
            .into_iter()
            .map(|(name, db, spread)| {
                let build: Build = Box::new(move |p: &mut Patch| {
                    let group = kit::build(p, &body_kit(db, spread), 0.0, 0.0);
                    rename(p, group, "Body Percussion Kit");
                    group
                });
                (name, build)
            })
            .collect(),
        ),
    ]
}
