//! A free automation track: an Automation node with a lane on its value is a
//! 0 to 1 signal that any input can take, and the lane's shape is what comes
//! out. The Envelope and Clock nodes ride on the same transport.

use std::path::Path;

use noodle_core::{
    AutomationLane, AutomationPoint, Command, Connection, Curve, Endpoint, History, LaneId, Node,
    NodeId, Project, Tick,
};
use noodle_engine::{OUTPUT_ID, Registry, Settings};
use noodle_nodes::{AUTOMATION_NODE_ID, AUTOMATION_VALUE, render_project_with_clips};

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 256,
    channels: 1,
};
/// One beat at 120 beats per minute.
const BEAT: usize = 24_000;

fn render(project: &Project, frames: usize) -> Vec<f32> {
    let mut registry = Registry::with_builtins();
    let out = render_project_with_clips(project, &mut registry, Path::new("."), SETTINGS, frames)
        .unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    out.render.samples
}

fn apply(project: &mut Project, history: &mut History, command: Command) {
    history.apply(project, command).unwrap();
}

/// `source` wired to an Output, plus the extra nodes and wires given.
fn project(nodes: &[(u64, Node)], wires: &[(u64, &str, u64, &str)]) -> Project {
    let (mut project, mut history) = (Project::new(), History::new());
    for (id, node) in nodes {
        let node = node.clone();
        apply(
            &mut project,
            &mut history,
            Command::AddNode {
                id: NodeId(*id),
                node,
            },
        );
    }
    for &(from, from_port, to, to_port) in wires {
        apply(
            &mut project,
            &mut history,
            Command::Connect(Connection {
                from: Endpoint::new(NodeId(from), from_port),
                to: Endpoint::new(NodeId(to), to_port),
            }),
        );
    }
    project
}

fn lane(project: &mut Project, node: u64, points: &[(i64, f32, Curve)]) {
    let id: LaneId = project.new_lane_id();
    let points = points
        .iter()
        .map(|&(tick, value, curve)| AutomationPoint {
            tick: Tick(tick),
            value,
            curve,
        })
        .collect();
    Command::AddLane {
        id,
        lane: AutomationLane::new(Endpoint::new(NodeId(node), AUTOMATION_VALUE), points),
    }
    .apply(project)
    .unwrap();
}

#[test]
fn the_lane_drawn_on_an_automation_track_is_the_signal_it_outputs() {
    let mut p = project(
        &[
            (1, Node::new(AUTOMATION_NODE_ID)),
            (2, Node::new(OUTPUT_ID)),
        ],
        &[(1, "out", 2, "in")],
    );
    // 0 to 1 over the first beat, held at 1, then back to 0.25 over the
    // second beat's start (points are in ticks, 960 to a beat).
    lane(
        &mut p,
        1,
        &[
            (0, 0.0, Curve::Linear),
            (960, 1.0, Curve::Linear),
            (1920, 0.25, Curve::Linear),
        ],
    );
    let out = render(&p, 2 * BEAT);
    assert!(out[0].abs() < 1e-3);
    assert!((out[BEAT / 2] - 0.5).abs() < 1e-2, "{}", out[BEAT / 2]);
    assert!((out[BEAT] - 1.0).abs() < 1e-2);
    assert!((out[2 * BEAT - 1] - 0.25).abs() < 1e-2);
    assert!(out.iter().all(|x| (0.0..=1.0).contains(x)));
}

#[test]
fn an_automation_track_drives_an_input_it_is_wired_to() {
    // The track sets a VCA's level, and the VCA scales a constant 1 (a Mix
    // with one input set), so the output is the lane.
    let mut p = project(
        &[
            (1, Node::new(AUTOMATION_NODE_ID)),
            (2, Node::new("noodle.util.vca")),
            (3, Node::new("noodle.util.mix")),
            (4, Node::new(OUTPUT_ID)),
        ],
        &[
            (1, "out", 2, "level"),
            (3, "out", 2, "in"),
            (2, "out", 4, "in"),
        ],
    );
    Command::SetParam {
        node: NodeId(3),
        key: "in1".into(),
        value: Some(1.0),
    }
    .apply(&mut p)
    .unwrap();
    lane(
        &mut p,
        1,
        &[(0, 0.0, Curve::Linear), (960, 0.8, Curve::Linear)],
    );
    let out = render(&p, BEAT);
    assert!((out[BEAT / 2] - 0.4).abs() < 2e-2, "{}", out[BEAT / 2]);
    assert!((out[BEAT - 1] - 0.8).abs() < 2e-2);
}

#[test]
fn a_clock_triggers_an_envelope_on_the_beat() {
    // Quarter notes, an envelope that rises over 0.1 s and holds its last
    // height of 1: after each pulse it climbs from the rest height.
    let curve = "0 0 0;1 1 0";
    let p = project(
        &[
            (
                1,
                Node::new("noodle.generator.clock")
                    .with_param("rate", 2.0)
                    .with_param("width", 0.5),
            ),
            (
                2,
                Node::new("noodle.mod.envelope")
                    .with_config(
                        noodle_core::Config::new()
                            .with("curve", noodle_core::Value::Text(curve.into())),
                    )
                    .with_param("length", 0.1),
            ),
            (3, Node::new(OUTPUT_ID)),
        ],
        &[(1, "out", 2, "trigger"), (2, "out", 3, "in")],
    );
    let out = render(&p, 2 * BEAT);
    // A fresh envelope rests at its last height, and each pulse restarts the
    // climb from the bottom.
    assert!(out[0] < 0.01, "the pulse at tick 0 restarts the climb");
    assert!((out[2_400] - 0.5).abs() < 0.05, "half way after 0.05 s");
    assert!(out[5_000] > 0.99, "arrived after 0.1 s");
    assert!(out[BEAT - 1] > 0.99);
    // The second beat's pulse drops it back to 0 and climbs again.
    assert!(out[BEAT + 5] < 0.05, "{}", out[BEAT + 5]);
    assert!(out[BEAT + 5_000] > 0.99);
}
