//! The system library's instruments: the song kit's synths and drums, and the
//! subtractive example, as group templates, each with a few group presets.
//!
//! The files in `crates/noodle-core/library/system/` are made from this code
//! (`UPDATE_LIBRARY=1 cargo test -p noodle-eval --test library` rewrites them)
//! and a test keeps them in step. A template is the instrument's group as
//! built here, so it reads exactly as one the songs use, and can be opened
//! and changed. Each preset is another instrument of the same shape, taken
//! as the values of every node inside.

use super::kit::{self, Drum, Env, Filter, Instrument, Osc, Tone, Wave, env};
use super::patch::Patch;
use noodle_core::library::{GroupPreset, GroupTemplate, Item};
use noodle_core::{Command, History, NodeId, Project};
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

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    let _ = noodle_nodes::register_library(&mut registry);
    registry
}

/// A family: one template and the instruments its presets come from. The
/// first is the template's own values.
struct Family {
    file: &'static str,
    name: &'static str,
    slug: &'static str,
    voices: Vec<(&'static str, Instrument)>,
}

fn tone_family(
    file: &'static str,
    name: &'static str,
    voices: Vec<(&'static str, Instrument)>,
) -> Family {
    Family {
        file,
        name,
        slug: file,
        voices,
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
    vec![
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
    ]
}

/// The instrument built alone in a scratch project, returned with it.
fn built(instrument: &Instrument) -> (Project, NodeId) {
    let (mut project, mut history) = (Project::new(), History::new());
    let group = kit::build(
        &mut Patch::new(&mut project, &mut history),
        instrument,
        0.0,
        0.0,
    );
    (project, group)
}

pub fn files() -> Vec<File> {
    let registry = registry();
    let layout = |node: &noodle_core::Node| registry.layout_signature(node);
    let mut files = Vec::new();
    for family in families() {
        let mut items = Vec::new();
        let mut key = None;
        for (i, (preset, instrument)) in family.voices.iter().enumerate() {
            let (project, group) = built(instrument);
            if i == 0 {
                let mut template = GroupTemplate::from_group(project.graph(), group, family.name)
                    .expect("an instrument is a group");
                template.slug = family.slug.into();
                items.push(Item::Template(template));
            }
            let mut preset = GroupPreset::capture(project.graph(), group, &layout, *preset);
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
    files.push(subtractive_voice(&layout));
    files
}

/// Node IDs of the example's group: the filter, the two envelopes.
const FILTER: u64 = 17;
const FILTER_ENV: u64 = 18;
const AMP_ENV: u64 = 20;

/// The Subtractive Voice of `examples/subtractive-synth.ron`, with presets
/// that change its filter and envelopes.
fn subtractive_voice(layout: &dyn Fn(&noodle_core::Node) -> Option<String>) -> File {
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
        let mut preset = GroupPreset::capture(project.graph(), group, layout, name);
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
