//! M2's "done when", offline: arrange audio clips on tracks, process them
//! through a node graph, automate a parameter, and mix them down. Rendering
//! is deterministic, so the mix is checked sample by sample against what the
//! arrangement should add up to.
//!
//! Two tracks, each a group made by `create_track`, with a constant clip on
//! each so the levels are easy to read:
//!
//! - Track 1 plays a 0.5 clip for beats 0-2, through a gain node set to
//!   -6.02 dB (a half), so it is heard at 0.25.
//! - Track 2 plays a 0.5 clip for beats 1-4, and an automation lane on its
//!   output gain drops it by 6.02 dB at beat 3.
//!
//! Both feed a mix node and the output, so the mixdown steps through 0.25,
//! 0.75, 0.5 and 0.25 a beat at a time.

use std::path::{Path, PathBuf};

use noodle_core::group::{GAIN, GROUP_OUTPUT, TRACK_INPUT, create_track};
use noodle_core::{
    AutomationLane, AutomationPoint, Clip, Command, Config, Connection, Curve, Endpoint, History,
    Node, NodeId, Position, Project, Tick, Value,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_io::write_wav;
use noodle_nodes::render_project_with_clips;

const RATE: u32 = 48_000;
/// One beat at 120 beats per minute, in samples, and in ticks.
const BEAT: usize = 24_000;
const BEAT_TICKS: i64 = 960;
const HALF_DB: f32 = -6.0206;
const SETTINGS: Settings = Settings {
    sample_rate: RATE as f32,
    max_frames: 512,
    channels: 2,
};

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("noodle-m2-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    // Four beats of 0.5, in stereo.
    write_wav(&dir.join("tone.wav"), &vec![0.5; 4 * BEAT * 2], 2, RATE).unwrap();
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

fn add_clip(project: &mut Project, history: &mut History, track: NodeId, beat: i64, beats: i64) {
    let id = project.new_clip_id();
    let clip = Clip::audio(
        track,
        Tick(beat * BEAT_TICKS),
        "tone.wav",
        (beats as usize * BEAT) as u64,
    );
    apply(project, history, Command::AddClip { id, clip });
}

fn arrangement() -> Project {
    let mut project = Project::new();
    let mut history = History::new();
    let (one, one_in, one_out) = track(&mut project, &mut history);
    let (two, two_in, two_out) = track(&mut project, &mut history);

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

    add_clip(&mut project, &mut history, one_in, 0, 2);
    add_clip(&mut project, &mut history, two_in, 1, 3);

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

    // Mix both tracks into the output.
    let mix = project.new_node_id();
    let out = project.new_node_id();
    for (id, node) in [
        (
            mix,
            Node::new("noodle.util.mix").with_config(Config::new().with("inputs", Value::Int(2))),
        ),
        (out, Node::new(OUTPUT_ID)),
    ] {
        apply(&mut project, &mut history, Command::AddNode { id, node });
    }
    for (from, to) in [
        (Endpoint::new(one, "out"), Endpoint::new(mix, "in1")),
        (Endpoint::new(two, "out"), Endpoint::new(mix, "in2")),
        (Endpoint::new(mix, "out"), Endpoint::new(out, "in")),
    ] {
        apply(
            &mut project,
            &mut history,
            Command::Connect(Connection { from, to }),
        );
    }
    project
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

fn check_mix(samples: &[f32]) {
    expect_level(samples, 0, BEAT, 0.25);
    expect_level(samples, BEAT, 2 * BEAT, 0.75);
    expect_level(samples, 2 * BEAT, 3 * BEAT, 0.5);
    expect_level(samples, 3 * BEAT, 4 * BEAT, 0.25);
    // The last beat is empty, and both channels carry the same signal.
    assert!(
        samples[4 * BEAT * 2 + 6_000..]
            .iter()
            .all(|&s| s.abs() < 1e-6)
    );
    assert!(samples.chunks(2).all(|frame| frame[0] == frame[1]));
}

#[test]
fn arranged_clips_go_through_a_graph_with_automation_and_mix_down() {
    let dir = folder("mix");
    let project = arrangement();
    let samples = mixdown(&project, &dir);
    check_mix(&samples);
}

#[test]
fn the_mix_is_the_same_every_time_and_after_saving_and_loading() {
    let dir = folder("repeat");
    let project = arrangement();
    let first = mixdown(&project, &dir);
    assert_eq!(mixdown(&project, &dir), first);

    // The saved project, reopened from its own folder, mixes the same.
    let path = dir.join("song.ron");
    std::fs::write(&path, project.to_ron()).unwrap();
    let reopened = Project::from_ron(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(mixdown(&reopened, &dir), first);
}

#[test]
fn soloing_a_track_leaves_only_that_track_in_the_mix() {
    let dir = folder("solo");
    let mut project = arrangement();
    let mut history = History::new();
    let two_out = project
        .graph()
        .nodes()
        .filter(|(_, n)| n.type_id == GROUP_OUTPUT)
        .map(|(id, _)| id)
        .max()
        .unwrap();
    apply(
        &mut project,
        &mut history,
        Command::SetParam {
            node: two_out,
            key: "solo".into(),
            value: Some(1.0),
        },
    );
    let samples = mixdown(&project, &dir);
    // Track 1's first beat is gone; track 2's clip plays alone.
    expect_level(&samples, 0, BEAT, 0.0);
    expect_level(&samples, BEAT, 3 * BEAT, 0.5);
    expect_level(&samples, 3 * BEAT, 4 * BEAT, 0.25);
}
