//! A clip carried from one key to another changes pitch and keeps its
//! length, and follows the timeline's key where it changes at a marker.

use std::path::{Path, PathBuf};

use noodle_core::{
    ClipContent, ClipWarp, Command, Connection, Endpoint, History, Key, KeyTarget, Marker, Mode,
    Node, NodeId, Project, Tick, Transpose, WarpMode,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_io::write_wav;
use noodle_nodes::{TRACK_INPUT_ID, render_project_with_clips};

const RATE: u32 = 48_000;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};

/// Four seconds of a 440 Hz tone, louder on the left, with `hits.wav` next to
/// it: a noise burst every half second.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("noodle-stretched-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let tone: Vec<f32> = (0..4 * RATE as usize)
        .flat_map(|i| {
            let x = (i as f32 / RATE as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5;
            [x, x * 0.5]
        })
        .collect();
    write_wav(&dir.join("tone.wav"), &tone, 2, RATE).unwrap();
    dir
}

fn project(file: &str, warp: ClipWarp, length: u64) -> Project {
    let mut project = Project::new();
    let mut history = History::new();
    let (track, out) = (NodeId(1), NodeId(2));
    for (id, kind) in [(track, TRACK_INPUT_ID), (out, OUTPUT_ID)] {
        history
            .apply(
                &mut project,
                Command::AddNode {
                    id,
                    node: Node::new(kind),
                },
            )
            .unwrap();
    }
    let connection = Connection {
        from: Endpoint::new(track, "audio"),
        to: Endpoint::new(out, "in"),
    };
    history
        .apply(&mut project, Command::Connect(connection))
        .unwrap();
    let id = project.new_clip_id();
    let mut clip = noodle_core::Clip::audio(track, Tick(0), file, length);
    let warp_id = project.next_warp_id();
    let ClipContent::Audio(audio) = &mut clip.content else {
        unreachable!("not an audio clip")
    };
    audio.warp = Some(warp_id);
    history
        .apply(
            &mut project,
            Command::Batch(vec![
                Command::AddWarp { id: warp_id, warp },
                Command::AddClip { id, clip },
            ]),
        )
        .unwrap();
    project
}

fn render(project: &Project, dir: &Path, frames: usize) -> Vec<f32> {
    let mut registry = Registry::with_builtins();
    let rendered =
        render_project_with_clips(project, &mut registry, dir, SETTINGS, frames).unwrap();
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    rendered.render.samples
}

fn left(samples: &[f32]) -> Vec<f32> {
    samples.iter().step_by(2).copied().collect()
}

fn crossings(signal: &[f32]) -> usize {
    signal
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count()
}

fn natural_at_120() -> ClipWarp {
    ClipWarp {
        mode: WarpMode::Grain,
        tempo_conversion: false,
        ..ClipWarp::at_tempo(120.0)
    }
}

const C: Key = Key {
    tonic: 0,
    mode: Mode::Major,
};

/// The Hz a tone is at, from its upward zero crossings over `seconds`.
fn hz(signal: &[f32], seconds: f32) -> f32 {
    crossings(signal) as f32 / seconds
}

#[test]
fn a_clip_carried_up_two_semitones_is_two_semitones_higher_and_as_long() {
    let dir = folder("up-two");
    let mut warp = natural_at_120();
    warp.transpose = Some(Transpose {
        from: C,
        to: KeyTarget::Fixed(Key::new(2, Mode::Major)),
    });
    let project = project("tone.wav", warp, 4 * RATE as u64);
    let samples = render(&project, &dir, 3 * RATE as usize);
    let l = left(&samples);
    // The middle second, clear of the ends.
    let mid = &l[RATE as usize..2 * RATE as usize];
    let expected = 440.0 * 2f32.powf(2.0 / 12.0);
    assert!(
        (hz(mid, 1.0) - expected).abs() < 6.0,
        "{} Hz, wanted {expected}",
        hz(mid, 1.0)
    );
    // Still sounding at the end of the three seconds asked for.
    let tail = &l[l.len() - 4800..];
    assert!(tail.iter().any(|x| x.abs() > 0.1));
}

#[test]
fn following_the_timeline_changes_pitch_at_a_marker() {
    let dir = folder("follow");
    let mut warp = natural_at_120();
    warp.transpose = Some(Transpose {
        from: C,
        to: KeyTarget::Follow,
    });
    let mut project = project("tone.wav", warp, 4 * RATE as u64);
    let mut history = History::new();
    // Two seconds at 120 bpm is four beats. D major from the start, E major
    // from the marker.
    history
        .apply(&mut project, Command::SetKey(Key::new(2, Mode::Major)))
        .unwrap();
    history
        .apply(
            &mut project,
            Command::SetMarkers(vec![Marker {
                name: "Chorus".into(),
                at: Tick(4 * noodle_core::TICKS_PER_QUARTER),
                key: Some(Key::new(4, Mode::Major)),
            }]),
        )
        .unwrap();
    let samples = render(&project, &dir, 3 * RATE as usize);
    let l = left(&samples);
    let first = &l[RATE as usize / 2..3 * RATE as usize / 2];
    let second = &l[5 * RATE as usize / 2..3 * RATE as usize - 2400];
    let (a, b) = (
        hz(first, 1.0),
        hz(second, (second.len() as f32) / RATE as f32),
    );
    assert!((a - 440.0 * 2f32.powf(2.0 / 12.0)).abs() < 6.0, "first {a}");
    assert!(
        (b - 440.0 * 2f32.powf(4.0 / 12.0)).abs() < 6.0,
        "second {b}"
    );
}

#[test]
fn a_clip_in_the_timeline_key_is_left_alone() {
    let dir = folder("same");
    let mut warp = natural_at_120();
    warp.transpose = Some(Transpose {
        from: C,
        to: KeyTarget::Follow,
    });
    let project = project("tone.wav", warp, 4 * RATE as u64);
    let samples = render(&project, &dir, 3 * RATE as usize);
    let l = left(&samples);
    let mid = &l[RATE as usize..2 * RATE as usize];
    assert!((hz(mid, 1.0) - 440.0).abs() < 4.0);
}
