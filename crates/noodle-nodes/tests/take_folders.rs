//! Take folders: one clip holding several takes of the same music and a comp
//! that says which plays where. The comp's joins crossfade, a warp places the
//! spans like any section, and a flat comp is the plain clip.

use std::path::PathBuf;

use noodle_core::{
    AudioClip, Clip, ClipContent, ClipWarp, Command, Connection, Endpoint, History, Node, NodeId,
    Project, Take, TakeFolder, Tick, WarpMarker,
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
/// Frames in a take: the clip's window.
const N: u64 = 24_000;
/// How loud each take is, so which one plays can be told by the level.
const LEVELS: [f32; 3] = [1.0, 2.0, 3.0];
/// Half of the default 10 ms crossfade, in frames.
const HALF_FADE: usize = 240;
/// Frames the output fades in over at the start of a render.
const START: usize = 300;

fn ramp(frame: f64) -> f32 {
    (frame / 10_000.0) as f32
}

/// A file of three takes back to back, each the same ramp at its own level.
fn folder() -> PathBuf {
    let dir = std::env::temp_dir().join("noodle-take-folders");
    std::fs::create_dir_all(&dir).unwrap();
    let samples: Vec<f32> = (0..3 * N)
        .flat_map(|i| [ramp((i % N) as f64) * LEVELS[(i / N) as usize], 0.0])
        .collect();
    write_wav(&dir.join("takes.wav"), &samples, 2, RATE).unwrap();
    dir
}

/// One track straight to the output, playing `audio` from the start.
fn project_of(audio: AudioClip) -> Project {
    let mut project = Project::new();
    let mut history = History::new();
    let id = project.new_clip_id();
    let mut commands = vec![
        Command::AddNode {
            id: NodeId(1),
            node: Node::new(TRACK_INPUT_ID),
        },
        Command::AddNode {
            id: NodeId(2),
            node: Node::new(OUTPUT_ID),
        },
        Command::Connect(Connection {
            from: Endpoint::new(NodeId(1), "audio"),
            to: Endpoint::new(NodeId(2), "in"),
        }),
    ];
    if let Some(id) = audio.warp {
        commands.push(Command::AddWarp {
            id,
            warp: warp(),
        });
    }
    commands.push(Command::AddClip {
        id,
        clip: Clip {
            node: NodeId(1),
            start: Tick(0),
            content: ClipContent::Audio(audio),
        },
    });
    history
        .apply(&mut project, Command::Batch(commands))
        .unwrap();
    project
}

fn warp() -> ClipWarp {
    let mut warp = ClipWarp::at_tempo(120.0);
    warp.set_marker(WarpMarker {
        seconds: 0.25,
        beat: 1.0,
    });
    warp.set_marker(WarpMarker {
        seconds: 0.5,
        beat: 1.5,
    });
    warp
}

fn render(project: &Project, frames: usize) -> Vec<f32> {
    let dir = folder();
    let mut registry = Registry::with_builtins();
    let rendered =
        render_project_with_clips(project, &mut registry, &dir, SETTINGS, frames).unwrap();
    assert!(rendered.render.diagnostics.is_empty());
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    rendered.render.samples.iter().step_by(2).copied().collect()
}

fn takes() -> Vec<Take> {
    (0..3).map(|k| Take { delta: k * N }).collect()
}

fn audio_with(comp: &[(u64, usize)]) -> AudioClip {
    let mut audio = AudioClip::new("takes.wav", N);
    let mut takes = TakeFolder::new(takes(), 0);
    for (i, &(at, take)) in comp.iter().enumerate() {
        let to = comp.get(i + 1).map_or(N, |next| next.0);
        takes.swipe(at, to, take);
    }
    audio.takes = Some(takes);
    audio
}

#[test]
fn a_comp_plays_each_take_where_it_says_and_crossfades_the_joins() {
    let comp = [(0, 0), (8_000, 1), (16_000, 2)];
    let out = render(&project_of(audio_with(&comp)), N as usize + 2_000);
    let level_at = |f: usize| LEVELS[comp.iter().rposition(|&(at, _)| at as usize <= f).unwrap()];
    for (f, &got) in out.iter().enumerate().take(N as usize - 2).skip(START) {
        // Not inside a join: the take's own sound, sample for sample.
        if comp
            .iter()
            .skip(1)
            .any(|&(at, _)| f.abs_diff(at as usize) <= HALF_FADE + 2)
        {
            continue;
        }
        let want = ramp(f as f64) * level_at(f);
        assert!((got - want).abs() < 1e-3, "frame {f} is {got}, not {want}");
    }
    // The middle of a join is the two takes at equal power.
    let mid = out[8_000];
    let want = ramp(8_000.0) * (LEVELS[0] + LEVELS[1]) * std::f32::consts::FRAC_1_SQRT_2;
    assert!((mid - want).abs() < 2e-3, "{mid} vs {want}");
    // And no sample jumps: a join without a crossfade steps by a whole ramp.
    for f in (8_000 - 2 * HALF_FADE)..(8_000 + 2 * HALF_FADE) {
        assert!((out[f + 1] - out[f]).abs() < 0.02, "a step at {f}");
    }
}

#[test]
fn a_join_crossfade_can_be_set_and_is_laid_out_in_ticks() {
    let mut audio = audio_with(&[(0, 0), (12_000, 1)]);
    // 240 ticks is a quarter beat: 6000 frames at 120 bpm, each side.
    audio.takes.as_mut().unwrap().set_join(
        1,
        Some(noodle_core::Crossfade {
            before: Tick(240),
            after: Tick(240),
        }),
    );
    let out = render(&project_of(audio), N as usize);
    // Before the fade starts the first take plays alone; its middle is the
    // blend at equal power; after it the second take plays alone.
    let before = 12_000 - 6_000 - 200;
    assert!((out[before] - ramp(before as f64) * LEVELS[0]).abs() < 1e-3);
    let mid = out[12_000];
    let want = ramp(12_000.0) * (LEVELS[0] + LEVELS[1]) * std::f32::consts::FRAC_1_SQRT_2;
    assert!((mid - want).abs() < 5e-3, "{mid} vs {want}");
    let after = 12_000 + 6_000 + 200;
    assert!((out[after] - ramp(after as f64) * LEVELS[1]).abs() < 1e-3);
}

#[test]
fn a_comp_of_one_take_is_that_take_and_a_trimmed_folder_keeps_every_take() {
    // Take 1 throughout, the window's second half only: the window moves,
    // the takes follow.
    let mut audio = audio_with(&[(0, 1)]);
    audio.offset = 12_000;
    audio.length = 12_000;
    let out = render(&project_of(audio), 12_000);
    for f in START..11_000 {
        let want = ramp((12_000 + f) as f64) * LEVELS[1];
        assert!((out[f] - want).abs() < 1e-3, "frame {f}");
    }
}

#[test]
fn a_warp_places_the_spans_like_any_section() {
    // The same warped window as a plain clip is the reference for where
    // the source plays; the comp scales it by the take's level.
    let mut plain = AudioClip::new("takes.wav", N);
    let mut folded = audio_with(&[(0, 0), (8_000, 1), (16_000, 2)]);
    let project_warp = noodle_core::WarpId(1);
    plain.warp = Some(project_warp);
    folded.warp = Some(project_warp);
    let frames = 2 * N as usize;
    let reference = render(&project_of(plain), frames);
    let out = render(&project_of(folded), frames);
    let mut checked = 0;
    for (f, (&got, &base)) in out.iter().zip(&reference).enumerate() {
        // `base` is ramp(source frame), so the source frame is known.
        let source = f64::from(base) * 10_000.0;
        let near_join = [8_000.0, 16_000.0]
            .iter()
            .any(|&at| (source - at).abs() < 2.0 * HALF_FADE as f64 + 40.0);
        if f < START || near_join || source < 1.0 || source > N as f64 - 600.0 {
            continue;
        }
        let level = LEVELS[if source < 8_000.0 {
            0
        } else if source < 16_000.0 {
            1
        } else {
            2
        }];
        assert!(
            (got - base * level).abs() < 2e-3,
            "frame {f} is {got}, not {} (source {source})",
            base * level
        );
        checked += 1;
    }
    assert!(checked > 10_000, "only {checked} frames were compared");
}

#[test]
fn spans_that_the_file_cannot_supply_are_silent_and_the_rest_plays() {
    // A third take past the end of the file: the comp's last span plays
    // nothing, and the others are unharmed.
    let mut audio = audio_with(&[(0, 0), (12_000, 1)]);
    audio.takes.as_mut().unwrap().takes.push(Take { delta: 9 * N });
    audio.takes.as_mut().unwrap().swipe(18_000, N, 3);
    let out = render(&project_of(audio), N as usize);
    assert!((out[5_000] - ramp(5_000.0)).abs() < 1e-3);
    assert!((out[15_000] - ramp(15_000.0) * LEVELS[1]).abs() < 1e-3);
    assert!(out[20_000].abs() < 1e-3);
}
