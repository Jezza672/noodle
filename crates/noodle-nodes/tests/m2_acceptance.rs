//! M2's "done when", at the engine: arrange audio clips on tracks, process
//! them through a node graph, automate a parameter, and mix them down. The
//! arrangement is rendered offline, sample by sample, and again live: the
//! real-time engine, the hub thread and the disk streams, paced roughly like
//! a device, with the levels read back at the middle of each beat.
//! (Whether a user can build this in the app is the app's acceptance test,
//! not this one.)
//!
//! Three tracks, each a group made by `create_track`, with constant clips so
//! the levels are easy to read:
//!
//! - Track 1 plays a 0.5 clip for beats 0-2, through a gain node set to
//!   -6.02 dB (a half), so it is heard at 0.25.
//! - Track 2 plays a 0.5 clip for beats 1-4, and an automation lane on its
//!   output gain drops it by 6.02 dB at beat 3.
//! - Track 3 plays a quiet mono 44.1 kHz clip, trimmed into its file, for
//!   beat 3 only, so decoding, resampling and trimming are in the mix.
//!
//! All three feed a mix node and the output, so the mixdown steps through
//! 0.25, 0.75, 0.5 and 0.35 a beat at a time, then silence.

use std::path::Path;
use std::time::{Duration, Instant};

use noodle_core::group::{GAIN, GROUP_OUTPUT, TRACK_INPUT, create_track};
use noodle_core::{
    AutomationLane, AutomationPoint, Clip, ClipContent, Command, Config, Connection, Curve,
    Endpoint, History, Node, NodeId, Position, Project, Tick, Value,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings, TempoTable, engine};
use noodle_io::write_wav;
use noodle_nodes::{register_library, render_project_with_clips};

const RATE: u32 = 48_000;
/// One beat at 120 beats per minute, in samples, and in ticks.
const BEAT: usize = 24_000;
const BEAT_TICKS: i64 = 960;
const HALF_DB: f32 = -6.0206;
const BLOCK: usize = 512;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: BLOCK,
    channels: 2,
};

/// A folder holding the audio files the arrangement uses.
fn folder() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // Four beats of 0.5, in stereo.
    write_wav(
        &dir.path().join("tone.wav"),
        &vec![0.5; 4 * BEAT * 2],
        2,
        RATE,
    )
    .unwrap();
    // A second of mono 0.1 at 44.1 kHz.
    write_wav(&dir.path().join("quiet.wav"), &vec![0.1; 44_100], 1, 44_100).unwrap();
    dir
}

fn apply(project: &mut Project, history: &mut History, command: Command) {
    history.apply(project, command).unwrap();
}

/// A track with its group boundary nodes: (group, track input, group output).
fn track(project: &mut Project, history: &mut History) -> (NodeId, NodeId, NodeId) {
    let (group, command) =
        create_track(None, Position { x: 0.0, y: 0.0 }, || project.new_node_id());
    apply(project, history, command);
    let inside = |kind: &str| {
        project
            .graph()
            .children(Some(group))
            .find(|(_, node)| node.type_id == kind)
            .map(|(id, _)| id)
            .unwrap()
    };
    (group, inside(TRACK_INPUT), inside(GROUP_OUTPUT))
}

/// Puts `length` frames of `file` on a track input from `beat`, starting
/// `offset` frames into the file (both counted in the file's own frames).
fn add_clip(
    project: &mut Project,
    history: &mut History,
    track: NodeId,
    (file, offset): (&str, u64),
    beat: i64,
    length: usize,
) {
    let id = project.new_clip_id();
    let mut clip = Clip::audio(track, Tick(beat * BEAT_TICKS), file, length as u64);
    let ClipContent::Audio(audio) = &mut clip.content;
    audio.offset = offset;
    apply(project, history, Command::AddClip { id, clip });
}

/// The arrangement, and the group output of track 2, whose gain is automated.
struct Arrangement {
    project: Project,
    two_out: NodeId,
    /// The track input nodes, which the clips are on.
    inputs: [NodeId; 3],
}

fn arrangement() -> Arrangement {
    let mut project = Project::new();
    let mut history = History::new();
    let (one, one_in, one_out) = track(&mut project, &mut history);
    let (two, two_in, two_out) = track(&mut project, &mut history);
    let (three, three_in, _) = track(&mut project, &mut history);

    // Track 1 runs through a gain node inside the group.
    let gain = project.new_node_id();
    let node = Node::new("noodle.util.gain")
        .with_param("gain", HALF_DB)
        .in_group(one);
    apply(
        &mut project,
        &mut history,
        Command::AddNode { id: gain, node },
    );
    apply(
        &mut project,
        &mut history,
        Command::Disconnect {
            input: Endpoint::new(one_out, "in"),
        },
    );
    for (from, to) in [
        (Endpoint::new(one_in, "audio"), Endpoint::new(gain, "in")),
        (Endpoint::new(gain, "out"), Endpoint::new(one_out, "in")),
    ] {
        apply(
            &mut project,
            &mut history,
            Command::Connect(Connection { from, to }),
        );
    }

    add_clip(
        &mut project,
        &mut history,
        one_in,
        ("tone.wav", 0),
        0,
        2 * BEAT,
    );
    add_clip(
        &mut project,
        &mut history,
        two_in,
        ("tone.wav", 0),
        1,
        3 * BEAT,
    );
    // One beat of the quiet file, from partway into it.
    add_clip(
        &mut project,
        &mut history,
        three_in,
        ("quiet.wav", 5_000),
        3,
        22_050,
    );

    // Track 2's output gain drops by half at beat 3.
    let lane = project.new_lane_id();
    let point = |beat: i64, value: f32| AutomationPoint {
        tick: Tick(beat * BEAT_TICKS),
        value,
        curve: Curve::Hold,
    };
    let automation = AutomationLane::new(
        Endpoint::new(two_out, GAIN),
        vec![point(0, 0.0), point(3, HALF_DB)],
    );
    apply(
        &mut project,
        &mut history,
        Command::AddLane {
            id: lane,
            lane: automation,
        },
    );

    // Mix the tracks into the output.
    let mix = project.new_node_id();
    let out = project.new_node_id();
    for (id, node) in [
        (
            mix,
            Node::new("noodle.util.mix").with_config(Config::new().with("inputs", Value::Int(3))),
        ),
        (out, Node::new(OUTPUT_ID)),
    ] {
        apply(&mut project, &mut history, Command::AddNode { id, node });
    }
    for (from, to) in [
        (Endpoint::new(one, "out"), Endpoint::new(mix, "in1")),
        (Endpoint::new(two, "out"), Endpoint::new(mix, "in2")),
        (Endpoint::new(three, "out"), Endpoint::new(mix, "in3")),
        (Endpoint::new(mix, "out"), Endpoint::new(out, "in")),
    ] {
        apply(
            &mut project,
            &mut history,
            Command::Connect(Connection { from, to }),
        );
    }
    Arrangement {
        project,
        two_out,
        inputs: [one_in, two_in, three_in],
    }
}

fn mixdown(project: &Project, dir: &Path) -> Vec<f32> {
    let mut registry = Registry::with_builtins();
    let rendered =
        render_project_with_clips(project, &mut registry, dir, SETTINGS, 5 * BEAT).unwrap();
    assert!(
        rendered.render.diagnostics.is_empty(),
        "{:?}",
        rendered.render.diagnostics
    );
    assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
    assert_eq!(rendered.underruns, 0);
    rendered.render.samples
}

/// Every left-channel frame in `from..to`, past the edges where clips fade
/// in and out and the lane's step is smoothed, is within a hair of `level`.
fn expect_level(samples: &[f32], from: usize, to: usize, level: f32) {
    for frame in from + 3_000..to - 3_000 {
        let got = samples[frame * 2];
        assert!(
            (got - level).abs() < 2e-3,
            "frame {frame} is {got}, wanted {level}"
        );
    }
}

/// Checks the mix at the middle of each beat, clear of the edges where clips
/// and the lane's step fade, with `check` handed each beat's range and level.
fn check_mix(samples: &[f32], check: impl Fn(&[f32], usize, usize, f32)) {
    check(samples, 0, BEAT, 0.25);
    check(samples, BEAT, 2 * BEAT, 0.75);
    check(samples, 2 * BEAT, 3 * BEAT, 0.5);
    check(samples, 3 * BEAT, 4 * BEAT, 0.35);
}

#[test]
fn arranged_clips_go_through_a_graph_with_automation_and_mix_down() {
    let dir = folder();
    let arrangement = arrangement();
    let samples = mixdown(&arrangement.project, dir.path());
    check_mix(&samples, expect_level);
    // The last beat is empty, and both channels carry the same signal.
    assert!(
        samples[4 * BEAT * 2 + 6_000..]
            .iter()
            .all(|&s| s.abs() < 1e-6)
    );
    assert!(samples.chunks(2).all(|frame| frame[0] == frame[1]));
}

#[test]
fn the_mix_is_the_same_every_time_and_after_saving_and_loading() {
    let dir = folder();
    let project = arrangement().project;
    let first = mixdown(&project, dir.path());
    assert_eq!(mixdown(&project, dir.path()), first);

    // The saved project, reopened from its own folder, mixes the same.
    let path = dir.path().join("song.ron");
    std::fs::write(&path, project.to_ron()).unwrap();
    let reopened = Project::from_ron(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(mixdown(&reopened, dir.path()), first);
}

#[test]
fn soloing_a_track_leaves_only_that_track_in_the_mix() {
    let dir = folder();
    let Arrangement {
        mut project,
        two_out,
        ..
    } = arrangement();
    let mut history = History::new();
    apply(
        &mut project,
        &mut history,
        Command::SetParam {
            node: two_out,
            key: "solo".into(),
            value: Some(1.0),
        },
    );
    let samples = mixdown(&project, dir.path());
    // Tracks 1 and 3 are gone; track 2's clip plays alone.
    expect_level(&samples, 0, BEAT, 0.0);
    expect_level(&samples, BEAT, 3 * BEAT, 0.5);
    expect_level(&samples, 3 * BEAT, 4 * BEAT, 0.25);
}

/// The same arrangement through the live engine: the track inputs feed from
/// their hub thread and disk streams as they do under a device, with the
/// blocks paced at several times real time instead of by a sound card. The
/// levels at the middle of each beat should come out the same.
#[test]
fn the_same_mix_plays_live() {
    let dir = folder();
    let arrangement = arrangement();
    let mut registry = Registry::with_builtins();
    let library = register_library(&mut registry);
    let (mut controller, mut processor) = engine(SETTINGS).unwrap();
    let transport = controller.transport();
    // Hold the timeline at the start while the disk streams get ready.
    transport.stop();
    let diagnostics = controller.update_project(&arrangement.project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let table = TempoTable::new(arrangement.project.tempo_map(), SETTINGS.sample_rate);
    let problems = library
        .clips
        .update(&arrangement.project, &table, RATE, dir.path());
    assert!(problems.is_empty(), "{problems:?}");

    let mut block = vec![0.0; BLOCK * SETTINGS.channels];
    let ready_by = Instant::now() + Duration::from_secs(10);
    while library.clips.status(arrangement.inputs[0]).streams == 0 {
        assert!(Instant::now() < ready_by, "the first clip never got ready");
        processor.process(&mut block);
        controller.maintain();
        std::thread::sleep(Duration::from_millis(5));
    }
    // Give the others their lookahead too.
    for _ in 0..40 {
        processor.process(&mut block);
        std::thread::sleep(Duration::from_millis(5));
    }
    transport.play();

    let mut samples = Vec::with_capacity(5 * BEAT * SETTINGS.channels);
    while samples.len() < 5 * BEAT * SETTINGS.channels {
        processor.process(&mut block);
        samples.extend_from_slice(&block);
        controller.maintain();
        std::thread::sleep(Duration::from_millis(3));
    }
    check_mix(&samples, expect_level);
}
