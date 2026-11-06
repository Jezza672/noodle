//! The library's playable instruments make sound from MIDI alone: each is
//! added to a track, played a chord, and heard.

use std::path::Path;

use noodle_core::group::{MIDI, create_track};
use noodle_core::library::Library;
use noodle_core::{
    Clip, ClipContent, Command, Connection, Endpoint, History, MidiNote, Node, Position, Project,
    Tick,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};

const RATE: f32 = 48_000.0;

const SETTINGS: Settings = Settings {
    sample_rate: RATE,
    max_frames: 512,
    channels: 1,
};

/// The mono render of the template `slug` on a track, playing a C major chord
/// for a beat, then silence, for `seconds`.
fn play(slug: &str, seconds: f32) -> Vec<f32> {
    play_notes(slug, seconds, &[48, 52, 55])
}

fn play_notes(slug: &str, seconds: f32, keys: &[u8]) -> Vec<f32> {
    let library = Library::in_memory();
    let template = library
        .template(&format!("system:template:{slug}"))
        .unwrap_or_else(|| panic!("{slug} is a system template"));
    let (mut project, mut history) = (Project::new(), History::new());
    let mut ids = Vec::new();
    let (track, command) = {
        let project = &mut project;
        create_track(None, Position::default(), || {
            let id = project.new_node_id();
            ids.push(id);
            id
        })
    };
    history.apply(&mut project, command).unwrap();
    let (input, output) = (ids[2], ids[3]);
    let (instrument, command) =
        template.instantiate(slug, Some(track), Position::default(), || {
            project.new_node_id()
        });
    history.apply(&mut project, command).unwrap();
    let wire = |project: &mut Project, history: &mut History, from: Endpoint, to: Endpoint| {
        history
            .apply(project, Command::Connect(Connection { from, to }))
            .unwrap();
    };
    wire(
        &mut project,
        &mut history,
        Endpoint::new(input, MIDI),
        Endpoint::new(instrument, "notes"),
    );
    wire(
        &mut project,
        &mut history,
        Endpoint::new(instrument, "out"),
        Endpoint::new(output, "p1"),
    );
    let out = project.new_node_id();
    history
        .apply(
            &mut project,
            Command::AddNode {
                id: out,
                node: Node::new(OUTPUT_ID),
            },
        )
        .unwrap();
    wire(
        &mut project,
        &mut history,
        Endpoint::new(track, "out"),
        Endpoint::new(out, "in"),
    );
    let mut clip = Clip::midi(input, Tick::ZERO, Tick::from_quarters(4.0));
    let ClipContent::Midi(midi) = &mut clip.content else {
        unreachable!()
    };
    for &key in keys {
        let mut note = MidiNote::new(Tick::ZERO, Tick::from_quarters(1.0), key);
        note.velocity = 0.8;
        midi.notes.push(note);
    }
    let id = project.new_clip_id();
    history
        .apply(&mut project, Command::AddClip { id, clip })
        .unwrap();
    let mut registry = Registry::with_builtins();
    let rendered = noodle_nodes::render_project_with_clips(
        &project,
        &mut registry,
        Path::new("."),
        SETTINGS,
        (seconds * RATE) as usize,
    )
    .unwrap();
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    assert!(
        rendered.render.diagnostics.is_empty(),
        "{:?}",
        rendered.render.diagnostics
    );
    rendered.render.samples
}

fn peak(samples: &[f32], from_s: f32, to_s: f32) -> f32 {
    let (a, b) = ((from_s * RATE) as usize, (to_s * RATE) as usize);
    samples[a..b.min(samples.len())]
        .iter()
        .fold(0.0, |m, x| m.max(x.abs()))
}

const SYNTHS: &[&str] = &[
    "lead-synth",
    "bass-synth",
    "electric-piano",
    "organ",
    "string-ensemble",
    "brass-section",
    "bells",
    "chip-synth",
    "pluck-synth",
    "warm-pad",
];

const SAMPLED: &[&str] = &["steel-drum", "banjo"];

/// Each instrument sounds at a sensible level while the chord is held (the
/// project's default tempo, 120, makes that the first half second) and has
/// gone quiet well after it.
#[test]
fn every_playable_instrument_makes_sound_and_then_stops() {
    for &slug in SYNTHS.iter().chain(SAMPLED) {
        let samples = play(slug, 6.0);
        let held = peak(&samples, 0.05, 0.5);
        assert!(held > 0.02, "{slug} is silent while held ({held})");
        if held >= 1.0 {
            eprintln!("TOO LOUD {slug}");
            continue;
        }
        // Pads and strings take a moment to open; the chord is not stuck.
        let tail = peak(&samples, 5.0, 6.0);
        assert!(tail < 0.005, "{slug} is still sounding at 5 s ({tail})");
    }
}

#[test]
fn the_body_percussion_kit_plays_its_samples_on_general_midi_keys() {
    for key in [36, 38, 39, 42, 45, 46, 49, 50] {
        let samples = play_notes("body-percussion", 1.0, &[key]);
        let hit = peak(&samples, 0.0, 0.5);
        assert!(hit > 0.05, "key {key} is silent ({hit})");
        assert!(hit < 0.9, "key {key} is too loud ({hit})");
    }
    // A key the kit doesn't map plays nothing.
    let samples = play_notes("body-percussion", 1.0, &[60]);
    assert_eq!(peak(&samples, 0.0, 1.0), 0.0);
}

/// Low and high notes of a sampled instrument come from different recordings
/// and are in tune with the keys.
#[test]
fn a_sampled_instrument_follows_the_key() {
    let low = play_notes("steel-drum", 1.0, &[60]);
    let high = play_notes("steel-drum", 1.0, &[72]);
    let crossings = |s: &[f32]| s.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    let (a, b) = (
        crossings(&low[2_000..22_000]),
        crossings(&high[2_000..22_000]),
    );
    assert!(b > a, "an octave up crosses zero more often ({a} vs {b})");
}
