//! S27: jam live with pads that launch loops and patterns.
//!
//! Partial. The Loop Pads library template is a Launch Quantise note-wire node
//! in front of eight looping Samplers, so a pad pressed anywhere in the bar
//! starts its loop on the next bar line, and pressing it again stops it on a
//! later one. There is no clip grid or session view, on purpose (#364). The
//! pad here is a Button into a Key; a controller's notes come in the same
//! way. Loading the loop into a pad's Sampler is not routed (a sample reaches
//! it by dragging an audio clip across, which no route reaches yet), so the
//! fixture starts with it loaded. Ableton Link is not built (#365), and tap
//! tempo and MIDI clock in are not routed, since the tempo source is session
//! state that a route cannot read.

use std::path::Path;
use std::time::Duration;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{Command, Endpoint, Node, NodeId, Position, Project, spare};
use noodle_io::write_wav;
use noodle_nodes::{LAUNCH_QUANTISE_ID, SAMPLER_ID};

use super::super::handle::peak;
use super::super::{Route, Tag};
use super::patch::set;
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

/// Two seconds of tone: one bar at 120 beats per minute.
fn loop_wav(path: &Path) {
    let samples: Vec<f32> = (0..96_000)
        .map(|i| 0.5 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin())
        .collect();
    write_wav(path, &samples, 1, 48_000).expect("the loop is written");
}

const BUTTON_ID: &str = "noodle.input.button";
const KEY_ID: &str = "noodle.event.key";

/// A Button into a Key (the pad, on the first pad's note), the Loop Pads
/// group with a loop in its first pad, and an output.
fn fixture(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    let button = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: button,
        node: Node::new(BUTTON_ID).at(0.0, 300.0),
    })]);
    let key = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: key,
        node: Node::new(KEY_ID).at(250.0, 300.0).with_param("note", 36.0),
    })]);
    let template = session
        .library()
        .templates()
        .find(|(_, t)| t.name == "Loop Pads")
        .map(|(id, t)| (id.to_string(), t.clone()))
        .expect("the library has Loop Pads");
    let mut next = session.new_node_id().0;
    let mut ids = || {
        let id = NodeId(next);
        next += 1;
        id
    };
    let (pads, add) =
        template
            .1
            .instantiate(&template.0, None, Position { x: 550.0, y: 300.0 }, &mut ids);
    session.edit([Edit::Apply(add)]);
    let out = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: out,
        node: Node::new(noodle_engine::OUTPUT_ID).at(950.0, 300.0),
    })]);
    for (from, to) in [
        (Endpoint::new(button, "out"), Endpoint::new(key, "gate")),
        (Endpoint::new(key, "out"), Endpoint::new(pads, "notes")),
        (Endpoint::new(pads, "out"), Endpoint::new(out, "in")),
    ] {
        session.edit([Edit::Apply(spare::wire(from, to))]);
    }
    let sampler = session
        .project()
        .graph()
        .nodes()
        .find(|(_, n)| n.type_id == SAMPLER_ID && n.parent == Some(pads))
        .map(|(id, _)| id)
        .expect("the first pad's Sampler");
    let path = dir.join("loop.wav");
    loop_wav(&path);
    let hash = session.samples().import(&path).expect("the loop imports");
    let command = crate::sample_drop::choose(&session, sampler, &hash);
    session.edit([Edit::Apply(command)]);
    finished(session, dir)
}

fn button_of(p: &Project) -> NodeId {
    p.graph()
        .nodes()
        .find(|(_, n)| n.type_id == BUTTON_ID)
        .map(|(id, _)| id)
        .expect("the pad's Button")
}

pub fn route() -> Route {
    Route {
        story: "S27",
        tag: Tag::Default,
        name: "launch a loop from a pad on the bar",
        fixture,
        size: None,
        steps: |h, _| {
            let _busy = crate::testkit::playing_mode();
            h.step("start the transport");
            h.press_playing(Pos2::new(640.0, 760.0), Modifiers::NONE, Key::Space);
            h.step("press the pad");
            let button = button_of(h.probe().project());
            set(h, button, "state", 1.0);
        },
        done: |p, _| {
            let project = p.project();
            let launcher = project
                .graph()
                .nodes()
                .find(|(_, n)| n.type_id == LAUNCH_QUANTISE_ID)
                .map(|(_, n)| n.params.get("grid").copied())
                .ok_or("the Loop Pads group has no Launch Quantise")?;
            if launcher.is_some_and(|g| (g - 3.0).abs() > 0.01) {
                return Err(format!("the launch grid is {launcher:?}, not a bar"));
            }
            // The loop starts on the next bar line, at most two seconds off,
            // and goes on until the pad is pressed again.
            let level = peak(&p.hear(Duration::from_millis(2_400)));
            if level < 0.01 {
                return Err(format!("the pad's loop never started (peak {level})"));
            }
            Ok(())
        },
        late: &[],
    }
}
