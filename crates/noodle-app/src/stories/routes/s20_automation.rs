//! S20: automate a mix in real time.
//!
//! The gain is automated, a knob on a control surface is learned onto the
//! Automation node (its `record` input), and the pass is ridden on the knob
//! in Touch mode. The surface is the session's MIDI bus, fed by
//! [`Handle::control`].
//!
//! The lane itself can be trimmed over a range (Shift-drag a span, then
//! the Trim menu), but an Automation node's lane is in its 0 to 1 value,
//! not dB, so "-2 dB" is not one typed amount there. The route still adds a
//! trim stage: a second Gain after the fader, automated by a lane that is 0 outside the
//! chorus and 1 inside it, with the Automation node's range set to 0 to
//! -2 dB. The performed pass is left alone.

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{Command, Endpoint, Node, NodeId, Project, spare};
use noodle_nodes::{AUTOMATION_NODE_ID, AUTOMATION_RECORD, GAIN_ID, MIDI_CONTROL_ID};

use super::super::handle::{Handle, Probe};
use super::super::{Keep, LateChange, Route, Tag};
use super::{add_node_playing, finished, virtual_session};
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
        name: "ride a knob on a control surface (Touch, the default mode)",
        fixture: sine_gain_output,
        size: None,
        steps,
        done,
        late: &[
            LateChange {
                name: "ride the fader again",
                steps: ride_again,
                done: rode_again,
                keep: &[],
            },
            LateChange {
                name: "draw a fade-out over the lane's tail",
                steps: draw_fade,
                done: faded,
                keep: &[],
            },
            LateChange {
                name: "trim the whole chorus by -2 dB",
                steps: trim_chorus,
                done: chorus_trimmed,
                keep: &[Keep {
                    name: "the performed pass",
                    of: lane_text,
                }],
            },
        ],
    }
}

fn node_of(p: &Project, type_id: &str) -> NodeId {
    p.graph()
        .nodes()
        .find(|(_, n)| n.type_id == type_id)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no {type_id} node"))
}

/// The nodes of a type, in ID order.
fn all_of(p: &Project, type_id: &str) -> Vec<NodeId> {
    p.graph()
        .nodes()
        .filter(|(_, n)| n.type_id == type_id)
        .map(|(id, _)| id)
        .collect()
}

/// The knob the pass is ridden on.
const KNOB: u8 = 21;

/// The lane as text, taken after the first pass for the late change to
/// compare with.
static FIRST_PASS: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

fn lane_text(p: &Project) -> String {
    let auto = node_of(p, AUTOMATION_NODE_ID);
    p.lane_for(&Endpoint::new(auto, noodle_nodes::AUTOMATION_VALUE))
        .map_or(String::new(), |(_, lane)| format!("{:?}", lane.points))
}

fn lane_points(p: &Project) -> usize {
    // The lane belongs to the Automation node feeding the gain.
    let auto = node_of(p, AUTOMATION_NODE_ID);
    p.lane_for(&Endpoint::new(auto, noodle_nodes::AUTOMATION_VALUE))
        .map_or(0, |(_, lane)| lane.points.len())
}

fn ride(h: &mut Handle, moves: [u8; 3]) {
    h.step("play");
    h.press_playing(Pos2::new(640.0, 300.0), Modifiers::NONE, Key::Space);
    h.step("turn the knob");
    h.unseen("the recorded points are on the lane in the timeline, not on the node");
    for value in moves {
        // The pass needs time to pass between moves.
        h.probe().hear(std::time::Duration::from_millis(400));
        h.waited(0.4);
        h.control(KNOB, value);
    }
    h.probe().hear(std::time::Duration::from_millis(400));
    h.step("stop");
    h.press_playing(Pos2::new(640.0, 300.0), Modifiers::NONE, Key::Space);
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("automate the gain");
    let gain = h.find("Gain");
    h.right_click(gain);
    let automate = h.find("Automate");
    h.click(automate, Modifiers::NONE);
    let auto = node_of(h.probe().project(), AUTOMATION_NODE_ID);
    h.step("learn a knob on the control surface");
    // The Value field is greyed while its lane drives it, so the knob is
    // learned on the Record input in the properties, which is what it
    // records from.
    let title = h.socket(auto, true, "out") - egui::Vec2::new(40.0, 0.0);
    h.click(title, Modifiers::NONE);
    let record = h.find("Record");
    h.right_click(record);
    let learn = h.find("Learn MIDI");
    // The app keeps polling while it listens, so this doesn't wait to settle.
    h.drag_playing(&[learn]);
    h.control(KNOB, 80);
    h.step("arm it");
    let arm = h.arm_button(auto);
    h.click(arm, Modifiers::NONE);
    ride(h, [100, 60, 90]);
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let n = lane_points(p.project());
    if n < 3 {
        return Err(format!("the lane has {n} points after the pass"));
    }
    let auto = node_of(p.project(), AUTOMATION_NODE_ID);
    let knob = p
        .project()
        .graph()
        .source(&Endpoint::new(auto, AUTOMATION_RECORD))
        .and_then(|s| p.project().graph().node(s.node))
        .is_some_and(|n| {
            n.type_id == MIDI_CONTROL_ID
                && n.params.get("control").copied() == Some(f32::from(KNOB))
        });
    if !knob {
        return Err("the knob that was turned is not learned onto the Automation node".into());
    }
    // The route relies on Touch being the default mode; the transport bar's
    // dropdown is not reachable by label, so the mode is checked here.
    if p.automation_mode() != crate::session::record_automation::AutomationMode::Touch {
        return Err("the pass was not ridden in Touch mode".into());
    }
    if p.transport_running() {
        return Err("the transport still runs".into());
    }
    *FIRST_PASS.lock().unwrap() = lane_text(p.project());
    Ok(())
}

fn ride_again(h: &mut Handle, _size: usize) {
    ride(h, [40, 110, 70]);
}

fn rode_again(p: &mut Probe, _size: usize) -> Result<(), String> {
    let now = lane_text(p.project());
    if now == *FIRST_PASS.lock().unwrap() {
        return Err("the second pass left the lane as it was".into());
    }
    Ok(())
}

const BEAT: i64 = 960;

/// Where the drawn fade-out starts and ends, in beats.
const FADE: (i64, i64) = (4, 8);

fn performed_lane(p: &Project) -> Option<noodle_core::LaneId> {
    let auto = node_of(p, AUTOMATION_NODE_ID);
    p.lane_for(&Endpoint::new(auto, noodle_nodes::AUTOMATION_VALUE))
        .map(|(id, _)| id)
}

/// Picks the lane's Draw tool and paints a ramp down to nothing past the
/// pass, a stroke a person could not place point by point.
fn draw_fade(h: &mut Handle, _size: usize) {
    h.step("pick the Draw tool on the lane");
    h.probe().settle();
    let draw = h.find_first("Draw");
    h.click(draw, Modifiers::NONE);
    h.step("draw a fade-out");
    let lane = performed_lane(h.probe().project()).expect("the performed lane");
    let path: Vec<Pos2> = (FADE.0..=FADE.1)
        .map(|beat| {
            let along = 0.9 - 0.9 * (beat - FADE.0) as f32 / (FADE.1 - FADE.0) as f32;
            h.probe()
                .lane_point(lane, beat * BEAT, along)
                .expect("the lane is on screen")
        })
        .collect();
    h.drag(Modifiers::NONE, &path);
}

fn faded(p: &mut Probe, _size: usize) -> Result<(), String> {
    let lane = performed_lane(p.project()).ok_or("the lane is gone")?;
    let lane = p.project().lane(lane).ok_or("the lane is gone")?;
    let at = |beat: i64| lane.value_at((beat * BEAT) as f64).unwrap_or(f32::NAN);
    let (start, end) = (at(FADE.0 + 1), at(FADE.1));
    if !(start > 0.4 && end < 0.2 && start > end) {
        return Err(format!(
            "the lane is {start:.2} a beat into the fade and {end:.2} at its end"
        ));
    }
    if lane.points.len() < 5 {
        return Err(format!("{} points on the lane", lane.points.len()));
    }
    Ok(())
}

/// The chorus: bars 3 and 4.
const CHORUS: (i64, i64) = (8 * BEAT, 16 * BEAT);
const TRIM_DB: f32 = -2.0;

/// The trim's Automation node: the one that is not the performed pass's.
fn trim_automation(p: &Project) -> Option<NodeId> {
    all_of(p, AUTOMATION_NODE_ID).get(1).copied()
}

fn trim_chorus(h: &mut Handle, _size: usize) {
    let (gain, out) = {
        let probe = h.probe();
        let p = probe.project();
        (node_of(p, GAIN_ID), node_of(p, noodle_engine::OUTPUT_ID))
    };
    h.step("add a trim stage after the fader");
    let trim = add_node_playing(h, Pos2::new(640.0, 520.0), "gain");
    wire(h, (gain, "out"), (trim, "in"));
    wire(h, (trim, "out"), (out, "in"));
    h.step("automate the trim");
    let at = h.port_row(trim, false, "gain");
    h.right_click_playing(at);
    let automate = h.find("Automate");
    h.drag_playing(&[automate]);
    let auto = trim_automation(h.probe().project()).expect("the trim's Automation node");
    h.step("make its range 0 to -2 dB");
    set(h, auto, noodle_nodes::AUTOMATION_MIN, "0");
    set(h, auto, noodle_nodes::AUTOMATION_MAX, &TRIM_DB.to_string());
    h.step("draw the trim over the chorus");
    let lane = h
        .probe()
        .project()
        .lane_for(&Endpoint::new(auto, noodle_nodes::AUTOMATION_VALUE))
        .map(|(id, _)| id)
        .expect("the trim's lane");
    h.probe().settle();
    // Zoom the arrangement out until the bar after the chorus shows.
    for _ in 0..20 {
        let row = h
            .probe()
            .lane_row(lane)
            .expect("the trim's lane is on screen");
        let end = h
            .probe()
            .lane_point(lane, CHORUS.1 + 2 * BEAT, 0.0)
            .expect("the lane");
        if end.x < row.right() - 20.0 {
            break;
        }
        h.wheel_with(
            row.center(),
            egui::MouseWheelUnit::Line,
            egui::Vec2::new(0.0, -1.0),
            Modifiers::COMMAND,
        );
        h.probe().settle();
    }
    // Automate starts the lane at the trim's value: pull that point down to
    // no trim.
    let first = h
        .probe()
        .lane_point_drawn(lane, 0)
        .expect("the lane's first point");
    let off = h.probe().lane_point(lane, 0, 0.0).expect("the lane");
    h.drag_playing(&[first, Pos2::new(first.x, off.y)]);
    // Off up to a beat before the chorus, on through it, off a beat after.
    for (tick, value) in [
        (CHORUS.0 - BEAT, 0.0),
        (CHORUS.0, 1.0),
        (CHORUS.1, 1.0),
        (CHORUS.1 + BEAT, 0.0),
    ] {
        let at = h
            .probe()
            .lane_point(lane, tick, value)
            .expect("the trim's lane is on screen");
        h.drag_playing(&[at]);
    }
}

fn wire(h: &mut Handle, from: (NodeId, &str), to: (NodeId, &str)) {
    let a = h.socket(from.0, true, from.1);
    let b = h.socket(to.0, false, to.1);
    h.drag_playing(&[a, b]);
}

/// Clicks a parameter's field on `node` and types `value` into it.
fn set(h: &mut Handle, node: NodeId, key: &str, value: &str) {
    let at = h.port_row(node, false, key);
    h.drag_playing(&[at]);
    h.press_playing(at, Modifiers::COMMAND, Key::A);
    h.type_text_playing(at, value);
    h.press_playing(at, Modifiers::NONE, Key::Enter);
}

/// The trim in dB at `tick`.
fn trim_at(p: &Project, tick: i64) -> Result<f32, String> {
    let auto = trim_automation(p).ok_or("there is no trim automation")?;
    let node = p.graph().node(auto).ok_or("the trim automation is gone")?;
    let get = |key: &str| node.params.get(key).copied().unwrap_or(0.0);
    if get(noodle_nodes::AUTOMATION_TAPER) != 0.0 {
        return Err("the trim's taper is not linear".into());
    }
    let (_, lane) = p
        .lane_for(&Endpoint::new(auto, noodle_nodes::AUTOMATION_VALUE))
        .ok_or("the trim has no lane")?;
    let v = lane.value_at(tick as f64).ok_or("the trim lane is empty")?;
    let (min, max) = (
        get(noodle_nodes::AUTOMATION_MIN),
        get(noodle_nodes::AUTOMATION_MAX),
    );
    Ok(min + v * (max - min))
}

fn chorus_trimmed(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let gain = node_of(project, GAIN_ID);
    let trim = all_of(project, GAIN_ID)
        .into_iter()
        .find(|&g| g != gain)
        .ok_or("there is no trim stage")?;
    let fed = project
        .graph()
        .source(&Endpoint::new(trim, "in"))
        .is_some_and(|s| s.node == gain);
    if !fed {
        return Err("the trim does not follow the fader".into());
    }
    let out = node_of(project, noodle_engine::OUTPUT_ID);
    let heard = project
        .graph()
        .source(&Endpoint::new(out, "in"))
        .is_some_and(|s| s.node == trim);
    if !heard {
        return Err("the trim does not feed the output".into());
    }
    let auto = trim_automation(project).ok_or("there is no trim automation")?;
    let drives = project
        .graph()
        .source(&Endpoint::new(trim, "gain"))
        .is_some_and(|s| s.node == auto);
    if !drives {
        return Err("the trim's gain is not automated".into());
    }
    let mid = (CHORUS.0 + CHORUS.1) / 2;
    for (tick, want, part) in [
        (CHORUS.0 / 2, 0.0, "the verse"),
        (mid, TRIM_DB, "the chorus"),
        (CHORUS.1 + 4 * BEAT, 0.0, "after the chorus"),
    ] {
        let db = trim_at(project, tick)?;
        if (db - want).abs() > 0.1 {
            return Err(format!("{part} is trimmed by {db} dB, wanted {want}"));
        }
    }
    Ok(())
}
