//! The system library's instruments: the song kit's synths and drums, and the
//! subtractive example, as group templates, each with a few group presets.
//!
//! The files in `crates/noodle-core/library/system/` are made from this code
//! (`UPDATE_LIBRARY=1 cargo test -p noodle-eval --test library` rewrites them)
//! and a test keeps them in step. A template is the instrument's group as
//! built here, so it reads exactly as one the songs use, and can be opened
//! and changed. Each preset is another instrument of the same shape, taken
//! as the values of every node inside.

use super::kit::{self, Drum, Env, Filter, Instrument, Osc, Rack, Slot, Tone, Wave, env};
use super::patch::{Patch, spec};
use noodle_core::library::{GroupPreset, GroupTemplate, Item};
use noodle_core::{Command, History, NodeId, Project, Value};
use noodle_engine::Registry;

/// One file of the system library: a template and its presets.
pub struct File {
    /// The file's name in `library/system/`.
    pub name: &'static str,
    pub items: Vec<Item>,
}

impl File {
    pub fn to_ron(&self) -> String {
        noodle_core::library::pack_to_ron(&self.items)
    }
}

pub(super) fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    let _ = noodle_nodes::register_library(&mut registry);
    registry
}

/// A family: one template and the instruments its presets come from. The
/// first is the template's own values.
pub(super) struct Family {
    file: &'static str,
    name: &'static str,
    slug: &'static str,
    voices: Vec<(&'static str, Build)>,
}

/// Builds one variant of a family's group in a scratch patch.
pub(super) type Build = Box<dyn Fn(&mut Patch) -> NodeId>;

fn tone_family(
    file: &'static str,
    name: &'static str,
    voices: Vec<(&'static str, Instrument)>,
) -> Family {
    Family {
        file,
        name,
        slug: file,
        voices: voices
            .into_iter()
            .map(|(preset, instrument)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| kit::build(p, &instrument, 0.0, 0.0));
                (preset, build)
            })
            .collect(),
    }
}

fn mono(cutoff: f32, resonance: f32, amount: f32, filter: Env, amp: Env) -> Instrument {
    Instrument::Tone(
        Tone::new(vec![
            Osc::new(Wave::Saw).level(-3.0),
            Osc::new(Wave::Sine).octave(0.5).level(-1.0),
        ])
        .filter(Filter::Ladder, cutoff, resonance, amount)
        .filter_env(filter)
        .amp_env(amp),
    )
}

fn pluck(cutoff: f32, amount: f32, decay: f32, release: f32) -> Instrument {
    Instrument::Tone(
        Tone::new(vec![
            Osc::new(Wave::Saw).detuned(-1.0).panned(-0.3).level(-3.0),
            Osc::new(Wave::Saw).detuned(1.0).panned(0.3).level(-3.0),
        ])
        .filter(Filter::Svf, cutoff, 0.2, amount)
        .filter_env(env(0.002, 0.15, 0.0, 0.1))
        .amp_env(env(0.002, decay, 0.0, release))
        .detune(7.0)
        .voices(4),
    )
}

fn pad(cutoff: f32, attack: f32, release: f32) -> Instrument {
    Instrument::Tone(
        Tone::new(vec![
            Osc::new(Wave::Saw).detuned(-1.0).panned(-0.6).level(-5.0),
            Osc::new(Wave::Saw).panned(0.0).level(-5.0),
            Osc::new(Wave::Saw).detuned(1.0).panned(0.6).level(-5.0),
        ])
        .filter(Filter::Ladder, cutoff, 0.15, 0.1)
        .filter_env(env(attack, 0.5, 0.7, release))
        .amp_env(env(attack, 0.3, 0.85, release))
        .detune(11.0)
        .voices(6),
    )
}

fn drum(drum: Drum) -> Instrument {
    Instrument::Drum(drum)
}

fn families() -> Vec<Family> {
    let mut families = vec![
        tone_family(
            "mono-synth",
            "Mono Synth",
            vec![
                (
                    "Saw Bass",
                    mono(
                        250.0,
                        0.25,
                        0.25,
                        env(0.002, 0.12, 0.2, 0.1),
                        env(0.003, 0.12, 0.8, 0.08),
                    ),
                ),
                (
                    "Sub Bass",
                    mono(
                        120.0,
                        0.1,
                        0.05,
                        env(0.003, 0.2, 0.5, 0.1),
                        env(0.004, 0.2, 0.9, 0.1),
                    ),
                ),
                (
                    "Acid Bass",
                    mono(
                        400.0,
                        0.6,
                        0.5,
                        env(0.001, 0.15, 0.1, 0.05),
                        env(0.001, 0.15, 0.6, 0.05),
                    ),
                ),
            ],
        ),
        tone_family(
            "poly-synth",
            "Poly Synth",
            vec![
                ("Pluck", pluck(700.0, 0.35, 0.28, 0.15)),
                ("Short Pluck", pluck(1400.0, 0.4, 0.14, 0.1)),
                ("Soft Keys", pluck(900.0, 0.2, 0.6, 0.3)),
            ],
        ),
        tone_family(
            "pad",
            "Pad",
            vec![
                ("Slow Pad", pad(700.0, 0.35, 0.7)),
                ("Bright Pad", pad(2400.0, 0.25, 0.6)),
                ("Dark Swell", pad(500.0, 1.2, 1.6)),
            ],
        ),
        tone_family(
            "kick",
            "Kick",
            vec![
                (
                    "Club Kick",
                    drum(Drum::Kick {
                        base: 48.0,
                        sweep: 120.0,
                        decay: 0.32,
                    }),
                ),
                (
                    "Deep Kick",
                    drum(Drum::Kick {
                        base: 40.0,
                        sweep: 60.0,
                        decay: 0.5,
                    }),
                ),
                (
                    "Tight Kick",
                    drum(Drum::Kick {
                        base: 52.0,
                        sweep: 140.0,
                        decay: 0.22,
                    }),
                ),
            ],
        ),
        tone_family(
            "snare",
            "Snare",
            vec![
                (
                    "Snare",
                    drum(Drum::Snare {
                        tone: 190.0,
                        noise: 2200.0,
                        decay: 0.2,
                    }),
                ),
                (
                    "Tight Snare",
                    drum(Drum::Snare {
                        tone: 210.0,
                        noise: 2600.0,
                        decay: 0.16,
                    }),
                ),
                (
                    "Loose Snare",
                    drum(Drum::Snare {
                        tone: 180.0,
                        noise: 1800.0,
                        decay: 0.3,
                    }),
                ),
            ],
        ),
        tone_family(
            "hat",
            "Hat",
            vec![
                (
                    "Closed Hat",
                    drum(Drum::Hat {
                        cutoff: 7500.0,
                        decay: 0.045,
                    }),
                ),
                (
                    "Open Hat",
                    drum(Drum::Hat {
                        cutoff: 6000.0,
                        decay: 0.22,
                    }),
                ),
                (
                    "Bright Hat",
                    drum(Drum::Hat {
                        cutoff: 9000.0,
                        decay: 0.05,
                    }),
                ),
            ],
        ),
        tone_family(
            "clap",
            "Clap",
            vec![
                (
                    "Clap",
                    drum(Drum::Clap {
                        cutoff: 1500.0,
                        decay: 0.16,
                    }),
                ),
                (
                    "Low Clap",
                    drum(Drum::Clap {
                        cutoff: 1000.0,
                        decay: 0.2,
                    }),
                ),
                (
                    "Snappy Clap",
                    drum(Drum::Clap {
                        cutoff: 2000.0,
                        decay: 0.1,
                    }),
                ),
            ],
        ),
        effect_family(
            "tempo-echo",
            "Tempo Echo",
            [
                ("Dotted Eighth", 14.0, -8.0, -6.0),
                ("Eighth Echo", 3.0, -9.0, -6.0),
                ("Quarter Slap", 2.0, -14.0, -8.0),
                ("Long Tail", 1.0, -5.0, -7.0),
            ]
            .into_iter()
            .map(|(name, division, feedback, wet)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| tempo_echo(p, division, feedback, wet));
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "tempo-tremolo",
            "Tempo Tremolo",
            [
                ("Eighth Tremolo", 3.0, 0.7, 0.0),
                ("Slow Swell", 1.0, 0.5, 0.0),
                ("Choppy Gate", 4.0, 0.9, 3.0),
            ]
            .into_iter()
            .map(|(name, division, depth, shape)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| tempo_tremolo(p, division, depth, shape));
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "multisample-instrument",
            "Multisample Instrument",
            [
                ("Sustained", 0.4, 2.0),
                ("Short Notes", 0.08, 2.0),
                ("Wide Bend", 0.25, 12.0),
            ]
            .into_iter()
            .map(|(name, release, bend)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| multisample_instrument(p, release, bend));
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "step-sequencer",
            "Step Sequencer",
            [
                ("Sixteenths", 4.0, 16.0, 0.0),
                ("Eighths", 3.0, 8.0, 0.0),
                ("Sixteenth Triplets", 9.0, 12.0, 0.0),
                ("Octave Up Eighths", 3.0, 8.0, 12.0),
            ]
            .into_iter()
            .map(|(name, division, length, transpose)| {
                let build: Build =
                    Box::new(move |p: &mut Patch| step_sequencer(p, division, length, transpose));
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "step-gate",
            "Step Gate",
            [
                ("Sixteenth Gate", 4.0, 1.0),
                ("Eighth Chop", 3.0, 0.9),
                ("Soft Triplets", 9.0, 0.6),
            ]
            .into_iter()
            .map(|(name, step, depth)| {
                let build: Build = Box::new(move |p: &mut Patch| step_gate(p, step, depth));
                (name, build)
            })
            .collect(),
        ),
        effect_family(
            "pattern-pads",
            "Pattern Pads",
            [
                ("Held Loops", 1.0, 0.0),
                ("Toggled Loops", 2.0, 0.0),
                ("Half-time Pads", 1.0, 1.0),
            ]
            .into_iter()
            .map(|(name, play, rate)| {
                let build: Build = Box::new(move |p: &mut Patch| pattern_pads(p, play, rate));
                (name, build)
            })
            .collect(),
        ),
        tone_family(
            "drum-rack",
            "Drum Rack",
            vec![
                ("General MIDI", drum_rack(0.0, 0.0)),
                ("Wide Kit", drum_rack(-2.0, 0.6)),
                ("Quiet Cymbals", drum_rack(-8.0, 0.2)),
            ],
        ),
        effect_family(
            "loop-pads",
            "Loop Pads",
            [
                ("Bar Toggle", 3.0, 1.0),
                ("Beat Hold", 1.0, 0.0),
                ("Two Bar Toggle", 4.0, 1.0),
            ]
            .into_iter()
            .map(|(name, grid, mode)| {
                let build: Build = Box::new(move |p: &mut Patch| loop_pads(p, grid, mode));
                (name, build)
            })
            .collect(),
        ),
    ];
    families.extend(super::instruments::families());
    families
}

pub(super) fn effect_family(
    file: &'static str,
    name: &'static str,
    voices: Vec<(&'static str, Build)>,
) -> Family {
    Family {
        file,
        name,
        slug: file,
        voices,
    }
}

/// A step sequencer: a Clock steps a Pattern Player through its pattern, one
/// window of the pattern per pulse. Draw the pattern in the player's
/// properties (the step grid or the piano roll). Its inputs are `rate` (the
/// Clock's choice), `length` (steps in the pattern) and `transpose`
/// (semitones); its outputs are `notes` and `gate`.
fn step_sequencer(p: &mut Patch, division: f32, length: f32, transpose: f32) -> NodeId {
    let group = p.enter("Step Sequencer", 0.0, 0.0);
    let rate = p.input("rate", 0.0);
    let steps = p.input("length", 1.0);
    let shift = p.input("transpose", 2.0);
    let clock = p.add(
        spec("noodle.generator.clock").param("rate", division),
        1.0,
        0.0,
    );
    p.connect(rate, (clock, "rate"));
    let player = p.add(
        spec("noodle.events.pattern")
            .param("mode", 3.0)
            .param("length", length)
            .param("transpose", transpose),
        2.0,
        0.0,
    );
    p.connect((clock, "out"), (player, "advance"));
    p.connect(steps, (player, "length"));
    p.connect(shift, (player, "transpose"));
    let notes = p.output("notes", 3.0, 0.0);
    p.connect((player, "notes"), notes);
    let gate = p.output("gate", 3.0, 1.0);
    p.connect((player, "gate"), gate);
    p.leave(group);
    group
}

/// A rhythmic gate: a Pattern Player loops on the bar and opens a VCA where
/// its pattern has notes. `depth` is how far the gaps close (1 is silence).
/// Its inputs are `in`, `step` (the player's division) and `depth`; its output
/// is `out`.
fn step_gate(p: &mut Patch, step: f32, depth: f32) -> NodeId {
    let group = p.enter("Step Gate", 0.0, 0.0);
    let audio = p.input("in", 0.0);
    let division = p.input("step", 1.0);
    let amount = p.input("depth", 2.0);
    let player = p.add(
        spec("noodle.events.pattern")
            .param("mode", 0.0)
            .param("step", step)
            .param("length", 16.0),
        1.0,
        1.0,
    );
    p.connect(division, (player, "step"));
    // The gaps close by `depth`: the level is 1 where the player's gate is
    // high and 1 - depth where it is low.
    let level = p.add(
        spec("noodle.util.math")
            .config("expr", Value::Text("1 - a * (1 - b)".into()))
            .param("a", depth),
        2.0,
        1.0,
    );
    p.connect(amount, (level, "a"));
    p.connect((player, "gate"), (level, "b"));
    let vca = p.add(spec("noodle.util.vca"), 3.0, 0.0);
    p.connect(audio, (vca, "in"));
    p.connect((level, "out"), (vca, "level"));
    let out = p.output("out", 4.0, 0.0);
    p.connect((vca, "out"), out);
    p.leave(group);
    group
}

/// Loop pads for MIDI parts: a pad note into `trigger` starts the player's
/// pattern, locked to the bar. `play` is 1 to loop while the pad is held, 2 to
/// toggle it on and off; `rate` picks the player's speed. Its inputs are
/// `trigger`, `slot` (which pattern plays) and `transpose`; its outputs are
/// `notes` and `gate`.
fn pattern_pads(p: &mut Patch, play: f32, rate: f32) -> NodeId {
    let group = p.enter("Pattern Pads", 0.0, 0.0);
    let trigger = p.input("trigger", 0.0);
    let slot = p.input("slot", 1.0);
    let shift = p.input("transpose", 2.0);
    let player = p.add(
        spec("noodle.events.pattern")
            .param("mode", 0.0)
            .param("play", play)
            .param("rate", 3.0 + rate)
            .param("length", 16.0),
        1.0,
        0.0,
    );
    p.connect(trigger, (player, "trigger"));
    p.connect(slot, (player, "slot"));
    p.connect(shift, (player, "transpose"));
    let notes = p.output("notes", 2.0, 0.0);
    p.connect((player, "notes"), notes);
    let gate = p.output("gate", 2.0, 1.0);
    p.connect((player, "gate"), gate);
    p.leave(group);
    group
}

/// An echo whose time is a division of the beat: Tempo into a Lag into a
/// Delay, which feeds back through a Gain, and a wet Mix. Its inputs are `in`,
/// `division` (the Tempo's choice), `feedback` (decibels) and `wet`
/// (decibels); its output is `out`.
fn tempo_echo(p: &mut Patch, division: f32, feedback: f32, wet: f32) -> NodeId {
    let group = p.enter("Tempo Echo", 0.0, 0.0);
    let audio = p.input("in", 0.0);
    let choice = p.input("division", 1.0);
    let repeats = p.input("feedback", 2.0);
    let level = p.input("wet", 3.0);
    let tempo = p.add(
        spec("noodle.mod.tempo").param("division", division),
        1.0,
        1.0,
    );
    p.connect(choice, (tempo, "division"));
    // A wire into Delay's `time` moves the slider along its travel (0 to 2 s)
    // rather than setting it, so seconds go through a Math node to a fraction
    // of that, onto a slider left at 0. The Lag turns a tempo change into a
    // glide, so the delay line is not jumped (ADR 0008).
    let travel = p.add(
        spec("noodle.util.math").config("expr", Value::Text("a / 2".into())),
        2.0,
        1.0,
    );
    p.connect((tempo, "time"), (travel, "a"));
    let smooth = p.add(
        spec("noodle.mod.lag")
            .param("rise", 0.05)
            .param("fall", 0.05),
        2.5,
        1.0,
    );
    p.connect((travel, "out"), (smooth, "in"));
    let feed = p.add(
        spec("noodle.util.mix").config("inputs", Value::Int(2)),
        2.0,
        0.0,
    );
    p.connect(audio, (feed, "in1"));
    let delay = p.add(spec("noodle.util.delay").param("time", 0.0), 3.0, 0.0);
    p.connect((feed, "out"), (delay, "in"));
    p.connect((smooth, "out"), (delay, "time"));
    let back = p.add(spec("noodle.util.gain").param("gain", feedback), 3.0, 2.0);
    p.connect((delay, "out"), (back, "in"));
    p.connect(repeats, (back, "gain"));
    p.connect((back, "out"), (feed, "in2"));
    let blend = p.add(
        spec("noodle.util.mix")
            .config("inputs", Value::Int(2))
            .param("gain2", wet),
        4.0,
        0.0,
    );
    p.connect(audio, (blend, "in1"));
    p.connect((delay, "out"), (blend, "in2"));
    p.connect(level, (blend, "gain2"));
    let out = p.output("out", 5.0, 0.0);
    p.connect((blend, "out"), out);
    p.leave(group);
    group
}

/// A tremolo whose rate is a division of the beat: Tempo into an LFO, turned
/// into a gain that dips by `depth`, on a VCA. Its inputs are `in`,
/// `division`, `depth` (0 to 1) and `shape` (the LFO's choice); its output is
/// `out`.
fn tempo_tremolo(p: &mut Patch, division: f32, depth: f32, shape: f32) -> NodeId {
    let group = p.enter("Tempo Tremolo", 0.0, 0.0);
    let audio = p.input("in", 0.0);
    let choice = p.input("division", 1.0);
    let amount = p.input("depth", 2.0);
    let wave = p.input("shape", 3.0);
    let tempo = p.add(
        spec("noodle.mod.tempo").param("division", division),
        1.0,
        1.0,
    );
    p.connect(choice, (tempo, "division"));
    // As for the echo, a wire moves the LFO's rate slider along its travel
    // (0.01 to 100 Hz, logarithmic), so hertz become a fraction of that
    // range, onto a slider left at 0.01.
    let travel = p.add(
        spec("noodle.util.math").config("expr", Value::Text("ln(a * 100) / ln(10000)".into())),
        1.5,
        1.0,
    );
    p.connect((tempo, "rate"), (travel, "a"));
    let lfo = p.add(
        spec("noodle.mod.lfo")
            .param("rate", 0.01)
            .param("shape", shape),
        2.0,
        1.0,
    );
    p.connect((travel, "out"), (lfo, "rate"));
    p.connect(wave, (lfo, "shape"));
    // The LFO swings between -1 and 1; the gain between 1 - depth and 1.
    let gain = p.add(
        spec("noodle.util.math")
            .config("expr", Value::Text("1 - a * (0.5 - 0.5 * b)".into()))
            .param("a", depth),
        3.0,
        1.0,
    );
    p.connect(amount, (gain, "a"));
    p.connect((lfo, "out"), (gain, "b"));
    let vca = p.add(spec("noodle.util.vca"), 4.0, 0.0);
    p.connect(audio, (vca, "in"));
    p.connect((gain, "out"), (vca, "level"));
    let out = p.output("out", 5.0, 0.0);
    p.connect((vca, "out"), out);
    p.leave(group);
    group
}

/// A multisampled instrument: a Sample Set into a Sampler that holds notes
/// and fades them out over `release`, then a low-pass. The set is filled
/// after adding it (load a folder in the Sample Set's properties). Its inputs
/// are `notes`, `tune` (cents), `bend range` (semitones), `release`
/// (seconds), `gain` (dB) and `cutoff` (hertz); its output is `out`.
fn multisample_instrument(p: &mut Patch, release: f32, bend: f32) -> NodeId {
    let group = p.enter("Multisample Instrument", 0.0, 0.0);
    let notes = p.input("notes", 0.0);
    let tune = p.input("tune", 1.0);
    let bend_range = p.input("bend range", 2.0);
    let fade = p.input("release", 3.0);
    let level = p.input("gain", 4.0);
    let cutoff = p.input("cutoff", 5.0);
    let set = p.add(spec("noodle.instrument.sample_set"), 1.0, 2.0);
    let sampler = p.add(
        spec("noodle.instrument.sampler")
            .param("mode", 1.0)
            .param("release", release)
            .param("bend_range", bend),
        2.0,
        0.0,
    );
    p.connect((set, "sample"), (sampler, "sample"));
    p.connect(notes, (sampler, "in"));
    p.connect(tune, (sampler, "tune"));
    p.connect(bend_range, (sampler, "bend_range"));
    p.connect(fade, (sampler, "release"));
    p.connect(level, (sampler, "gain"));
    let filter = p.add(
        spec("noodle.filter.svf").param("cutoff", 20_000.0),
        3.0,
        0.0,
    );
    p.connect((sampler, "out"), (filter, "in"));
    p.connect(cutoff, (filter, "cutoff"));
    let out = p.output("out", 4.0, 0.0);
    p.connect((filter, "low"), out);
    p.leave(group);
    group
}

/// Eight loop pads: a Note Split (keys 36 to 43) into a Launch Quantise and a
/// looping, gated Sampler for each, mixed. Each pad has its own settings as
/// inputs: `Pad n level` (decibels), `Pad n loop`, `Pad n grid` (when it
/// launches) and `Pad n mode` (hold or toggle), so one pad can launch on the
/// beat and another on the bar. The input is `notes` and the output `out`.
/// The loops are loaded after adding it.
fn loop_pads(p: &mut Patch, grid: f32, mode: f32) -> NodeId {
    let group = p.enter("Loop Pads", 0.0, 0.0);
    let notes = p.input("notes", 0.0);
    let mut split = spec("noodle.event.note_split").config("outputs", Value::Int(8));
    for n in 1..=8u32 {
        let key = 35.0 + n as f32;
        split = split
            .param(Box::leak(format!("low{n}").into_boxed_str()), key)
            .param(Box::leak(format!("high{n}").into_boxed_str()), key);
    }
    let split = p.add(split, 1.0, 0.0);
    p.connect(notes, (split, "in"));
    let mix = p.add(
        spec("noodle.util.mix").config("inputs", Value::Int(8)),
        6.0,
        0.0,
    );
    let out = p.output("out", 7.0, 0.0);
    p.connect((mix, "out"), out);
    for n in 1..=8u32 {
        let row = n as f32 - 1.0;
        let level = p.input(&format!("Pad {n} level"), 3.0 + row);
        let looping = p.input(&format!("Pad {n} loop"), 3.0 + row);
        let grid_in = p.input(&format!("Pad {n} grid"), 3.0 + row);
        let mode_in = p.input(&format!("Pad {n} mode"), 3.0 + row);
        let quantise = p.add(
            spec("noodle.event.launch_quantise")
                .param("grid", grid)
                .param("mode", mode),
            2.0,
            row,
        );
        let sampler = p.add(
            spec("noodle.instrument.sampler")
                .param("keytrack", 0.0)
                .param("mode", 1.0)
                .param("loop", 1.0)
                .param("gain", 0.0),
            4.0,
            row,
        );
        let out_key: &'static str = Box::leak(format!("out{n}").into_boxed_str());
        let in_key: &'static str = Box::leak(format!("in{n}").into_boxed_str());
        p.connect((split, out_key), (quantise, "in"));
        p.connect(grid_in, (quantise, "grid"));
        p.connect(mode_in, (quantise, "mode"));
        p.connect((quantise, "out"), (sampler, "in"));
        p.connect(level, (sampler, "gain"));
        p.connect(looping, (sampler, "loop"));
        p.connect((sampler, "out"), (mix, in_key));
    }
    p.leave(group);
    group
}

/// The Drum Rack with no samples (they are picked per slot after adding it):
/// eight slots on General MIDI keys, at `db` and spread by `pan`.
fn drum_rack(db: f32, spread: f32) -> Instrument {
    let slots = [
        (
            Drum::Kick {
                base: 48.0,
                sweep: 120.0,
                decay: 0.3,
            },
            36,
            0.0,
        ),
        (
            Drum::Snare {
                tone: 190.0,
                noise: 2200.0,
                decay: 0.2,
            },
            38,
            0.1,
        ),
        (
            Drum::Hat {
                cutoff: 7500.0,
                decay: 0.045,
            },
            42,
            -0.5,
        ),
        (
            Drum::Hat {
                cutoff: 6000.0,
                decay: 0.22,
            },
            46,
            0.5,
        ),
        (
            Drum::Clap {
                cutoff: 1500.0,
                decay: 0.16,
            },
            39,
            0.0,
        ),
        (
            Drum::Kick {
                base: 90.0,
                sweep: 60.0,
                decay: 0.3,
            },
            45,
            -0.4,
        ),
        (
            Drum::Snare {
                tone: 250.0,
                noise: 1500.0,
                decay: 0.25,
            },
            50,
            0.4,
        ),
        (
            Drum::Hat {
                cutoff: 5000.0,
                decay: 0.6,
            },
            49,
            0.3,
        ),
    ];
    Instrument::Rack(Rack {
        slots: slots
            .into_iter()
            .map(|(drum, key, pan)| Slot {
                sample: String::new(),
                label: None,
                drum,
                key,
                // Hats and cymbals sit at `db`; the rest at 0 dB.
                db: if matches!(drum, Drum::Hat { .. }) {
                    db
                } else {
                    0.0
                },
                pan: pan * spread / 0.6,
            })
            .collect(),
        controls: true,
    })
}

/// The variant built alone in a scratch project, returned with it.
fn built(build: &Build) -> (Project, NodeId) {
    let (mut project, mut history) = (Project::new(), History::new());
    let group = build(&mut Patch::new(&mut project, &mut history));
    (project, group)
}

pub fn files() -> Vec<File> {
    let registry = registry();
    let mut files = Vec::new();
    for family in families() {
        let mut items = Vec::new();
        let mut key = None;
        for (i, (preset, build)) in family.voices.iter().enumerate() {
            let (project, group) = built(build);
            if i == 0 {
                let mut template = GroupTemplate::from_group(project.graph(), group, family.name)
                    .expect("an instrument is a group");
                template.slug = family.slug.into();
                items.push(Item::Template(template));
            }
            let mut preset = GroupPreset::capture(project.graph(), group, *preset);
            preset.slug =
                noodle_core::library::slugify(&format!("{} {}", family.name, preset.name));
            let same = key.get_or_insert(preset.topology.clone()).clone();
            assert_eq!(
                same, preset.topology,
                "{}'s presets share its shape",
                family.name
            );
            items.push(Item::GroupPreset(preset));
        }
        files.push(File {
            name: family.file,
            items,
        });
    }
    files.push(subtractive_voice());
    files.push(File {
        name: "nodes",
        items: super::node_presets::presets(&registry),
    });
    files
}

/// Node IDs of the example's group: the filter, the two envelopes.
const FILTER: u64 = 17;
const FILTER_ENV: u64 = 18;
const AMP_ENV: u64 = 20;

/// The Subtractive Voice of `examples/subtractive-synth.ron`, with presets
/// that change its filter and envelopes.
fn subtractive_voice() -> File {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/subtractive-synth.ron");
    let text = std::fs::read_to_string(path).expect("the example is there");
    let mut project = Project::from_ron(&text).expect("the example loads");
    let group = NodeId(3);
    let mut template = GroupTemplate::from_group(project.graph(), group, "Subtractive Voice")
        .expect("the example's synth is a group");
    template.slug = "subtractive-voice".into();
    let mut items = vec![Item::Template(template)];

    let mut history = History::new();
    let mut preset = |name: &str, sets: &[(u64, &str, f32)]| {
        let batch = sets
            .iter()
            .map(|&(node, key, value)| Command::SetParam {
                node: NodeId(node),
                key: key.into(),
                value: Some(value),
            })
            .collect();
        history
            .apply(&mut project, Command::Batch(batch))
            .expect("the example has these");
        let mut preset = GroupPreset::capture(project.graph(), group, name);
        preset.slug = noodle_core::library::slugify(&format!("Subtractive Voice {name}"));
        items.push(Item::GroupPreset(preset));
    };
    // The example's own sound first, then the others from it.
    preset("Warm Chord", &[]);
    preset(
        "Soft Pad",
        &[
            (FILTER, "cutoff", 180.0),
            (FILTER_ENV, "attack", 0.3),
            (FILTER_ENV, "release", 0.8),
            (AMP_ENV, "attack", 0.3),
            (AMP_ENV, "release", 0.9),
        ],
    );
    preset(
        "Bright Stab",
        &[
            (FILTER, "cutoff", 600.0),
            (FILTER, "resonance", 0.5),
            (FILTER_ENV, "attack", 0.002),
            (FILTER_ENV, "release", 0.1),
            (AMP_ENV, "attack", 0.005),
            (AMP_ENV, "release", 0.12),
        ],
    );
    File {
        name: "subtractive-voice",
        items,
    }
}
