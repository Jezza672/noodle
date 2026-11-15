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
    render(slug, seconds, keys, true)
}

/// Like [`play_notes`]; an instrument with no `notes` input (a drone) isn't
/// wired to the track's MIDI when `wire_notes` is off.
fn render(slug: &str, seconds: f32, keys: &[u8], wire_notes: bool) -> Vec<f32> {
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
    if wire_notes {
        wire(
            &mut project,
            &mut history,
            Endpoint::new(input, MIDI),
            Endpoint::new(instrument, "notes"),
        );
    }
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
    "plucked-string",
    "wavetable-synth",
    "grain-pad",
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

/// The frequency of `samples` by autocorrelation, searching `lo..hi` hertz.
fn pitch(samples: &[f32], lo: f32, hi: f32) -> f32 {
    let (min, max) = ((RATE / hi) as usize, (RATE / lo) as usize);
    let n = samples.len() - max;
    let mut best = (0.0f32, min);
    for lag in min..=max {
        let sum: f32 = (0..n).map(|i| samples[i] * samples[i + lag]).sum();
        if sum > best.0 {
            best = (sum, lag);
        }
    }
    // Refine between lags by a parabola through the peak.
    let at = |lag: usize| -> f32 { (0..n).map(|i| samples[i] * samples[i + lag]).sum() };
    let (a, b, c) = (at(best.1 - 1), at(best.1), at(best.1 + 1));
    let shift = 0.5 * (a - c) / (a - 2.0 * b + c);
    RATE / (best.1 as f32 + shift)
}

/// A plucked string is a loop of Delay a few dozen samples long at the top of
/// the keyboard, which only a loop that runs in chunks can be: it follows the
/// key across four octaves.
#[test]
fn a_plucked_string_follows_the_key_across_four_octaves() {
    for key in [36u8, 48, 60, 72, 84] {
        let expected = 440.0 * 2f32.powf((f32::from(key) - 69.0) / 12.0);
        let samples = play_notes("plucked-string", 1.0, &[key]);
        let held = peak(&samples, 0.02, 0.4);
        assert!(held > 0.02, "key {key} is silent ({held})");
        assert!(held < 1.0, "key {key} clips ({held})");
        let heard = pitch(&samples[500..3_500], expected * 0.8, expected * 1.25);
        let cents = 1200.0 * (heard / expected).log2();
        assert!(
            cents.abs() < 45.0,
            "key {key}: wanted {expected} Hz, heard {heard} Hz ({cents:.1} cents)"
        );
    }
}

#[test]
fn a_plucked_string_dies_away_and_the_voice_is_freed() {
    let samples = play_notes("plucked-string", 8.0, &[60]);
    assert!(peak(&samples, 0.02, 0.4) > 0.02);
    assert!(peak(&samples, 6.0, 8.0) < 0.002);
}

/// A Grain Freeze has no notes: it drones until it is stopped.
#[test]
fn the_grain_freeze_drones() {
    let samples = render("grain-freeze", 3.0, &[], false);
    let held = peak(&samples, 0.5, 1.0);
    assert!(held > 0.02 && held < 1.0, "{held}");
    let later = peak(&samples, 2.0, 3.0);
    assert!(later > 0.02, "still droning at 2 s ({later})");
}

/// A Grain Pad's cloud keeps starting grains through the envelope's release,
/// so the release is heard (the chord is held for 0.5 s and a grain
/// is 0.18 s long, so sound after 0.72 s is from grains started after the
/// note-off).
#[test]
fn the_grain_pad_sounds_through_its_release() {
    let samples = play("grain-pad", 3.0);
    let release = peak(&samples, 0.72, 0.9);
    assert!(release > 0.008, "silent in the release ({release})");
}
