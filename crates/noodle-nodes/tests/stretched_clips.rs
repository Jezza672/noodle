//! Audio clips warped in Grain or Beats mode keep their pitch while their
//! speed changes, come out the same whatever the block size, and keep a drum
//! hit's attack where the warp puts it.

use std::path::{Path, PathBuf};

use noodle_core::{
    ClipContent, ClipWarp, Command, Connection, Endpoint, History, Node, NodeId, Project, Tick,
    WarpMarker, WarpMode,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_io::write_wav;
use noodle_nodes::{ClipRender, TRACK_INPUT_ID, render_project_with_clips};

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
    write_wav(&dir.join("hits.wav"), &hits(), 2, RATE).unwrap();
    dir
}

/// A burst of noise that dies away in 60 ms, every 0.5 s for 4 s.
fn hits() -> Vec<f32> {
    let mut out = vec![0.0f32; 4 * RATE as usize * 2];
    let mut seed = 99u32;
    for hit in 0..8 {
        let start = hit * RATE as usize / 2;
        for i in 0..5_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1 << 23) as f32 - 1.0;
            let x = noise * (-(i as f32) / 700.0).exp() * 0.8;
            out[(start + i) * 2] = x;
            out[(start + i) * 2 + 1] = x;
        }
    }
    out
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

fn render(project: &Project, dir: &Path, settings: Settings, frames: usize) -> ClipRender {
    let mut registry = Registry::with_builtins();
    let rendered =
        render_project_with_clips(project, &mut registry, dir, settings, frames).unwrap();
    assert!(
        rendered.render.diagnostics.is_empty(),
        "{:?}",
        rendered.render.diagnostics
    );
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    assert_eq!(rendered.underruns, 0);
    rendered
}

fn warp(bpm: f64, mode: WarpMode) -> ClipWarp {
    ClipWarp {
        mode,
        ..ClipWarp::at_tempo(bpm)
    }
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

fn rms(signal: &[f32]) -> f32 {
    (signal.iter().map(|x| x * x).sum::<f32>() / signal.len() as f32).sqrt()
}

#[test]
fn grain_plays_a_faster_clip_at_the_same_pitch() {
    let dir = folder("faster");
    // Recorded at 60 bpm on a 120 bpm timeline: twice as fast, half as long.
    let project = project("tone.wav", warp(60.0, WarpMode::Grain), 4 * RATE as u64);
    let samples = render(&project, &dir, SETTINGS, 3 * RATE as usize)
        .render
        .samples;
    let l = left(&samples);
    // Two seconds of 440 Hz, then silence.
    let c = crossings(&l[2_000..2 * RATE as usize - 2_000]) as i64;
    assert!((c - 843).abs() <= 6, "{c} cycles");
    let level = rms(&l[10_000..90_000]);
    assert!((level - 0.5 / 2f32.sqrt()).abs() < 0.03, "{level}");
    assert!(
        l[2 * RATE as usize + 2_000..]
            .iter()
            .all(|x| x.abs() < 1e-6)
    );
}

#[test]
fn grain_plays_a_slower_clip_at_the_same_pitch() {
    let dir = folder("slower");
    let project = project("tone.wav", warp(240.0, WarpMode::Grain), 2 * RATE as u64);
    let samples = render(&project, &dir, SETTINGS, 5 * RATE as usize)
        .render
        .samples;
    let l = left(&samples);
    let c = crossings(&l[2_000..4 * RATE as usize - 2_000]) as i64;
    assert!((c - 1_723).abs() <= 12, "{c} cycles");
    // The stereo balance of the source holds.
    let right: Vec<f32> = samples.iter().skip(1).step_by(2).copied().collect();
    let ratio = rms(&right[10_000..180_000]) / rms(&l[10_000..180_000]);
    assert!((ratio - 0.5).abs() < 0.02, "{ratio}");
}

#[test]
fn grain_and_repitch_sound_different() {
    let dir = folder("differ");
    let grain = project("tone.wav", warp(60.0, WarpMode::Grain), 2 * RATE as u64);
    let repitch = project("tone.wav", warp(60.0, WarpMode::Repitch), 2 * RATE as u64);
    let g = left(&render(&grain, &dir, SETTINGS, RATE as usize).render.samples);
    let r = left(
        &render(&repitch, &dir, SETTINGS, RATE as usize)
            .render
            .samples,
    );
    // Repitch doubles the pitch.
    let (gc, rc) = (
        crossings(&g[1_000..RATE as usize - 1_000]),
        crossings(&r[1_000..RATE as usize - 1_000]),
    );
    assert!(rc > gc * 3 / 2, "{rc} vs {gc}");
}

#[test]
fn the_block_size_does_not_change_a_stretched_sound() {
    let dir = folder("blocks");
    for mode in [WarpMode::Grain, WarpMode::Beats] {
        let mut w = warp(97.0, mode);
        w.set_marker(WarpMarker {
            seconds: 0.3,
            beat: 0.2,
        });
        w.set_marker(WarpMarker {
            seconds: 2.1,
            beat: 4.9,
        });
        let project = project("hits.wav", w, 3 * RATE as u64);
        let reference = render(&project, &dir, SETTINGS, 200_000).render.samples;
        assert!(rms(&left(&reference)) > 0.01, "{mode:?} is silent");
        for max_frames in [64, 333, 1024] {
            let settings = Settings {
                max_frames,
                ..SETTINGS
            };
            let other = render(&project, &dir, settings, 200_000).render.samples;
            assert_eq!(other, reference, "{mode:?} at {max_frames}");
        }
        assert_eq!(
            render(&project, &dir, SETTINGS, 200_000).render.samples,
            reference
        );
    }
}

#[test]
fn beats_keeps_the_hits_sharp_where_grain_smears_them() {
    let dir = folder("beats");
    // Half as fast (240 bpm recording on a 120 timeline): each hit's tail is
    // stretched over a second, and the next hit lands at 1 s, 2 s, ...
    let beats = project("hits.wav", warp(240.0, WarpMode::Beats), 4 * RATE as u64);
    let grain = project("hits.wav", warp(240.0, WarpMode::Grain), 4 * RATE as u64);
    let b = left(
        &render(&beats, &dir, SETTINGS, 8 * RATE as usize)
            .render
            .samples,
    );
    let g = left(
        &render(&grain, &dir, SETTINGS, 8 * RATE as usize)
            .render
            .samples,
    );
    let source = left(&hits());
    let onsets = noodle_io::detect_onsets(&source, RATE);
    // The first hit is at the very start, which is never a cut.
    assert_eq!(onsets.len(), 7, "{onsets:?}");
    for (hit, onset) in onsets.iter().enumerate().take(6) {
        // The cut falls on the transient the detector found, which lands
        // twice as late; from there the attack is the source's, to the frame.
        let at = *onset as usize * 2;
        for i in 0..1_000 {
            let (got, want) = (b[at + i], source[*onset as usize + i]);
            assert!(
                (got - want).abs() < 2e-3,
                "hit {hit}, frame {i}: {got} vs {want}"
            );
        }
    }
    // Grain doesn't keep it: somewhere in the first hits its output differs
    // from the source.
    let drift: f32 = onsets
        .iter()
        .take(6)
        .map(|onset| {
            (0..1_000)
                .map(|i| (g[*onset as usize * 2 + i] - source[*onset as usize + i]).abs())
                .fold(0.0, f32::max)
        })
        .fold(0.0, f32::max);
    assert!(drift > 0.05, "{drift}");
}
