//! S20: automate a mix in real time.
//!
//! Partial. A control surface cannot be driven from a test, so the pass is
//! ridden with the mouse on the Automation node's own slider. Trimming a
//! range by a relative amount does not exist, so that late change is not
//! routed (see the registry).

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{Command, Endpoint, Node, NodeId, Project, spare};
use noodle_nodes::{AUTOMATION_NODE_ID, GAIN_ID};

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

const SINE: &str = "noodle.osc.sine";

/// A sine through a Gain to the output: the fader to ride.
fn sine_gain_output(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    let add = |session: &mut crate::session::Session, type_id: &str, x: f32| {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new(type_id).at(x, 300.0),
        })]);
        id
    };
    let sine = add(&mut session, SINE, 250.0);
    let gain = add(&mut session, GAIN_ID, 500.0);
    let out = add(&mut session, noodle_engine::OUTPUT_ID, 750.0);
    for (from, to) in [((sine, "out"), (gain, "in")), ((gain, "out"), (out, "in"))] {
        let wire = spare::wire(Endpoint::new(from.0, from.1), Endpoint::new(to.0, to.1));
        session.edit([Edit::Apply(wire)]);
    }
    finished(session, dir)
}

pub fn route() -> Route {
    Route {
        story: "S20",
        tag: Tag::Default,
        name: "ride a fader in touch mode",
        fixture: sine_gain_output,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "ride the fader again",
            steps: ride_again,
            done: rode_again,
            keep: &[],
        }],
    }
}

fn node_of(p: &Project, type_id: &str) -> NodeId {
    p.graph()
        .nodes()
        .find(|(_, n)| n.type_id == type_id)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no {type_id} node"))
}

fn lane_points(p: &Project) -> usize {
    let gain = node_of(p, GAIN_ID);
    // The lane belongs to the Automation node feeding the gain.
    let auto = node_of(p, AUTOMATION_NODE_ID);
    let _ = gain;
    p.lane_for(&Endpoint::new(auto, noodle_nodes::AUTOMATION_VALUE))
        .map_or(0, |(_, lane)| lane.points.len())
}

fn ride(h: &mut Handle) {
    let auto = node_of(h.probe().project(), AUTOMATION_NODE_ID);
    h.step("play");
    h.press_playing(Pos2::new(640.0, 300.0), Modifiers::NONE, Key::Space);
    h.step("move the fader");
    let value = h.find("Value");
    for dx in [30.0, -30.0, 10.0] {
        // The pass needs time to pass between moves.
        h.probe().hear(std::time::Duration::from_millis(400));
        h.waited(0.4);
        h.drag_playing(&[value, value + egui::Vec2::new(dx, 0.0)]);
    }
    h.probe().hear(std::time::Duration::from_millis(400));
    let _ = auto;
    h.step("stop");
    h.press_playing(Pos2::new(640.0, 300.0), Modifiers::NONE, Key::Space);
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("automate the gain");
    let gain = h.find("Gain");
    h.right_click(gain);
    let automate = h.find("Automate");
    h.click(automate, Modifiers::NONE);
    h.step("arm it");
    let auto = node_of(h.probe().project(), AUTOMATION_NODE_ID);
    let arm = h.arm_button(auto);
    h.click(arm, Modifiers::NONE);
    ride(h);
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let n = lane_points(p.project());
    if n < 3 {
        return Err(format!("the lane has {n} points after the pass"));
    }
    Ok(())
}

fn ride_again(h: &mut Handle, _size: usize) {
    ride(h);
}

fn rode_again(p: &mut Probe, _size: usize) -> Result<(), String> {
    done(p, 1)
}
