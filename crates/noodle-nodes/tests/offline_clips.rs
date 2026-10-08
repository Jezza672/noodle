//! A project with audio clips renders offline to the same samples every time,
//! with every clip where the project puts it.

use std::path::{Path, PathBuf};

use noodle_core::{
    Clip, ClipContent, Command, Connection, Endpoint, History, Node, NodeId, Project, Tick,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_io::write_wav;
use noodle_nodes::{ClipRender, TRACK_INPUT_ID, render_project_with_clips};

const RATE: u32 = 48_000;
/// One beat at 120 beats per minute, in samples, and in ticks.
const BEAT: usize = 24_000;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};

/// A ramp whose value says which frame it is.
fn ramp(channel: usize, frame: usize) -> f32 {
    let x = frame as f32 / 100_000.0;
    if channel == 0 { x } else { -x }
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("noodle-offline-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let samples: Vec<f32> = (0..200_000)
        .flat_map(|i| [ramp(0, i), ramp(1, i)])
        .collect();
    write_wav(&dir.join("ramp.wav"), &samples, 2, RATE).unwrap();
    dir
}

/// A track input wired to the output, with a clip of `(start sample, offset,
/// length)` in `ramp.wav` for each entry. Starts must be multiples of 25
/// samples, so they fall on whole ticks.
fn project(clips: &[(usize, u64, u64)], file: &str) -> Project {
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
    for &(start, offset, length) in clips {
        assert_eq!(start % 25, 0);
        let id = project.new_clip_id();
        let mut clip = Clip::audio(track, Tick(start as i64 / 25), file, length);
        let ClipContent::Audio(audio) = &mut clip.content;
        audio.offset = offset;
        history
            .apply(&mut project, Command::AddClip { id, clip })
            .unwrap();
    }
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
    rendered
}

/// What a clip of `ramp.wav` leaves in the left channel.
fn expect(samples: &[f32], start: usize, offset: usize, length: usize) {
    for i in 0..length {
        let got = samples[(start + i) * 2];
        let want = ramp(0, offset + i);
        assert!(
            (got - want).abs() < 1e-6,
            "frame {} is {got}, wanted {want}",
            start + i
        );
    }
}

#[test]
fn clips_come_out_where_the_project_puts_them() {
    let dir = folder("placed");
    let project = project(&[(0, 0, 30_000), (2 * BEAT, 50_000, 20_000)], "ramp.wav");
    let rendered = render(&project, &dir, SETTINGS, 4 * BEAT);
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    assert_eq!(rendered.underruns, 0);
    let samples = &rendered.render.samples;
    // The node fades in over its first few milliseconds; look past that.
    expect(samples, 300, 300, 30_000 - 300);
    // The first clip ends where it was cut to, and silence follows.
    assert!(samples[30_000 * 2..2 * BEAT * 2].iter().all(|&s| s == 0.0));
    expect(samples, 2 * BEAT, 50_000, 20_000);
    assert!(samples[(2 * BEAT + 20_000) * 2..].iter().all(|&s| s == 0.0));
}

#[test]
fn many_short_clips_close_together_all_play() {
    // More clips than the track holds streams for in its lookahead.
    let dir = folder("dense");
    let clips: Vec<_> = (0..24)
        .map(|n| (n * 2_000, 1_000 * n as u64, 1_500))
        .collect();
    let project = project(&clips, "ramp.wav");
    let rendered = render(&project, &dir, SETTINGS, 24 * 2_000 + 1_000);
    assert_eq!(rendered.underruns, 0);
    let samples = &rendered.render.samples;
    for (n, &(start, offset, length)) in clips.iter().enumerate().skip(1) {
        expect(samples, start, offset as usize, length as usize);
        let gap = &samples[(start + length as usize) * 2..(start + 2_000) * 2];
        assert!(
            gap.iter().all(|&s| s == 0.0),
            "clip {n} isn't followed by silence"
        );
    }
}

#[test]
fn rendering_twice_gives_the_same_samples() {
    let dir = folder("repeat");
    let clips: Vec<_> = (0..10)
        .map(|n| (n * 5_000, 3_000 * n as u64, 4_000))
        .collect();
    let project = project(&clips, "ramp.wav");
    let first = render(&project, &dir, SETTINGS, 60_000).render.samples;
    for _ in 0..4 {
        assert_eq!(
            render(&project, &dir, SETTINGS, 60_000).render.samples,
            first
        );
    }
    // Other block sizes agree past the start-up fade.
    let small = Settings {
        max_frames: 64,
        ..SETTINGS
    };
    let other = render(&project, &dir, small, 60_000).render.samples;
    assert_eq!(other[600..], first[600..]);
}

#[test]
fn a_clip_that_cannot_be_read_is_reported_and_left_out() {
    let dir = folder("missing");
    let mut project = project(&[(0, 0, 10_000)], "ramp.wav");
    let id = project.new_clip_id();
    let mut history = History::new();
    let clip = Clip::audio(NodeId(1), Tick(2_000), "gone.wav", 5_000);
    history
        .apply(&mut project, Command::AddClip { id, clip })
        .unwrap();
    let rendered = render(&project, &dir, SETTINGS, 4 * BEAT);
    assert_eq!(rendered.problems.len(), 1);
    assert_eq!(rendered.problems[0].clip, id);
    expect(&rendered.render.samples, 300, 300, 9_700);
}
