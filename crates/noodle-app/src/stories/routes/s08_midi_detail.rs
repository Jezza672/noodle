//! S08: edit MIDI performance detail.
//!
//! Partial. The piano roll draws and moves notes, and Ctrl+A with the arrow
//! keys transposes a selection. Humanise has two routes (#350):
//!
//! - **In the roll** (this file's default route): select the notes and press
//!   Humanise. Each gets stored random offsets, and the clip's Timing and
//!   Velocity amounts scale them when it plays, so the drawn notes stay put,
//!   only the selection is humanised, and "halve the amount" is one field.
//! - **On the wire** (`Tag::Expert`): a Humanise node between the track and
//!   the instrument, with its `timing` and `velocity` parameters. To
//!   humanise only some notes, automate its `timing` over them (the
//!   automation lane is not routed here, so the route sets the amount and
//!   halves it by hand).
//!
//! Recording the piano part and redrawing the sustain pedal's lane are not
//! routed.

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::roll::{BEAT, clip_of, hit, new_clip, one_track, select};
use egui::{Key, Modifiers};
use noodle_core::{Command, Endpoint, Node, NodeId, Project};
use noodle_nodes::{HUMANISE_ID, TRACK_INPUT_ID};

use super::patch::{Op, Ref, run};

const KEYS: [u8; 4] = [60, 64, 67, 72];

fn steps(h: &mut Handle, _size: usize) {
    new_clip(h);
    h.step("play in four notes");
    for (i, key) in KEYS.iter().enumerate() {
        hit(h, i as i64 * BEAT, *key);
    }
}

fn keys(p: &mut Probe) -> Vec<u8> {
    let id = clip_of(p.project());
    let mut keys: Vec<u8> = p.roll_notes(id).iter().map(|n| n.2).collect();
    keys.sort();
    keys
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    // A played part, whatever the notes: at least as many as the route plays,
    // and not all one pitch.
    let got = keys(p);
    if got.len() < KEYS.len() {
        return Err(format!(
            "{} notes, wanted at least {}",
            got.len(),
            KEYS.len()
        ));
    }
    if got.iter().all(|&k| k == got[0]) {
        return Err("every note is the same pitch".into());
    }
    Ok(())
}

fn down_a_tone(h: &mut Handle, _size: usize) {
    h.step("select the part and move it down a tone");
    let part: Vec<(i64, u8)> = KEYS
        .iter()
        .enumerate()
        .map(|(i, k)| (i as i64 * BEAT, *k))
        .collect();
    select(h, &part);
    let at = h.probe().roll_grid().expect("the roll").center();
    h.press(at, egui::Modifiers::NONE, Key::ArrowDown);
    h.press(at, egui::Modifiers::NONE, Key::ArrowDown);
}

fn lowered(p: &mut Probe, _size: usize) -> Result<(), String> {
    let got = keys(p);
    let want: Vec<u8> = KEYS.iter().map(|k| k - 2).collect();
    if got != want {
        return Err(format!("the part is {got:?}, wanted {want:?}"));
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S08",
        tag: Tag::Default,
        name: "play in a part and transpose it down a tone",
        fixture: one_track,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "transpose the part down a tone",
            steps: down_a_tone,
            done: lowered,
            keep: &[],
        }],
    }
}

const PART: [(i64, u8); 4] = [(0, 60), (BEAT, 64), (2 * BEAT, 67), (3 * BEAT, 72)];

fn humanise(h: &mut Handle, _size: usize) {
    steps(h, 1);
    h.step("select the part and humanise it");
    select(h, &PART);
    let button = h.find("Humanise");
    h.click(button, Modifiers::NONE);
    h.probe().settle();
}

fn clip_midi(p: &Project) -> noodle_core::MidiClip {
    p.clip(clip_of(p))
        .and_then(|c| c.as_midi().cloned())
        .expect("the MIDI clip")
}

fn humanised(p: &mut Probe, _size: usize) -> Result<(), String> {
    done(p, 1)?;
    let midi = clip_midi(p.project());
    if midi.notes.iter().any(|n| n.humanise.is_none()) {
        return Err("a note was not humanised".into());
    }
    if midi.humanise_timing.0 <= 0 {
        return Err("the clip's timing amount is zero".into());
    }
    Ok(())
}

const FIRST: i64 = 30;

fn halve(h: &mut Handle, _size: usize) {
    h.step("halve the timing amount");
    let at = h.field_after("Timing");
    h.click(at, Modifiers::NONE);
    h.press(at, Modifiers::COMMAND, Key::A);
    h.type_text(at, &format!("{}", FIRST / 2));
    h.press(at, Modifiers::NONE, Key::Enter);
    h.probe().settle();
}

fn halved(p: &mut Probe, _size: usize) -> Result<(), String> {
    let midi = clip_midi(p.project());
    if midi.humanise_timing.0 != FIRST / 2 {
        return Err(format!(
            "the timing amount is {}, wanted {}",
            midi.humanise_timing.0,
            FIRST / 2
        ));
    }
    // The drawn notes did not move.
    let got = keys(p);
    if got != KEYS {
        return Err(format!("the part is {got:?}, wanted {KEYS:?}"));
    }
    let id = clip_of(p.project());
    let starts: Vec<i64> = {
        let mut s: Vec<i64> = p.roll_notes(id).iter().map(|n| n.0).collect();
        s.sort();
        s
    };
    let want: Vec<i64> = PART.iter().map(|n| n.0).collect();
    if starts != want {
        return Err(format!("the notes are at {starts:?}, wanted {want:?}"));
    }
    Ok(())
}

pub fn humanise_route() -> Route {
    Route {
        story: "S08",
        tag: Tag::Default,
        name: "humanise the selected notes in the piano roll",
        fixture: one_track,
        size: None,
        steps: humanise,
        done: humanised,
        late: &[LateChange {
            name: "halve the humanise amount",
            steps: halve,
            done: halved,
            keep: &[],
        }],
    }
}

// ---- The wire route.

const NODE: Ref = Ref("event.humanise", 0);

fn wire_fixture(dir: &std::path::Path) -> crate::app::App {
    let mut session = super::virtual_session(dir);
    let track = session.new_node_id();
    session.edit([crate::session::Edit::Apply(Command::AddNode {
        id: track,
        node: Node::new(TRACK_INPUT_ID).at(0.0, 300.0),
    })]);
    let sampler = session.new_node_id();
    session.edit([crate::session::Edit::Apply(Command::AddNode {
        id: sampler,
        node: Node::new(noodle_nodes::SAMPLER_ID).at(700.0, 300.0),
    })]);
    super::finished(session, dir)
}

fn human_node(p: &Project) -> Option<NodeId> {
    p.graph()
        .nodes()
        .find(|(_, n)| n.type_id == HUMANISE_ID)
        .map(|(id, _)| id)
}

fn node_ops(timing: f32) -> Vec<Op> {
    vec![
        Op::Step("add a Humanise node"),
        Op::Add(NODE, 380.0, 460.0),
        Op::Step("set its timing"),
        Op::Set(NODE, "timing", timing),
    ]
}

fn wire_steps(h: &mut Handle, _size: usize) {
    run(h, &node_ops(12.0));
    h.step("put it between the track and the instrument");
    let (track, node, out) = {
        let p = h.probe();
        let project = p.project();
        let track = project
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == TRACK_INPUT_ID)
            .map(|(id, _)| id)
            .expect("the track");
        let out = project
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == noodle_nodes::SAMPLER_ID)
            .map(|(id, _)| id)
            .expect("the instrument");
        (track, human_node(project).expect("the Humanise node"), out)
    };
    for (from, to) in [
        ((track, "midi"), (node, "in")),
        ((node, "out"), (out, "in")),
    ] {
        let a = h.socket(from.0, true, from.1);
        let b = h.socket(to.0, false, to.1);
        h.drag(Modifiers::NONE, &[a, b]);
    }
}

fn wire_done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let node = human_node(project).ok_or("no Humanise node")?;
    let source = project.graph().source(&Endpoint::new(node, "in"));
    let from_track = source
        .and_then(|s| project.graph().node(s.node))
        .is_some_and(|n| n.type_id == TRACK_INPUT_ID);
    if !from_track {
        return Err("the Humanise node is not fed by the track".into());
    }
    let heard = project
        .graph()
        .nodes()
        .find(|(_, n)| n.type_id == noodle_nodes::SAMPLER_ID)
        .and_then(|(id, _)| project.graph().source(&Endpoint::new(id, "in")))
        .is_some_and(|s| s.node == node);
    if !heard {
        return Err(format!(
            "the Humanise node does not feed the instrument: {:?}",
            project
                .graph()
                .nodes()
                .find(|(_, n)| n.type_id == noodle_nodes::SAMPLER_ID)
                .map(|(id, _)| project.graph().source(&Endpoint::new(id, "in")))
        ));
    }
    let timing = project
        .graph()
        .node(node)
        .and_then(|n| n.params.get("timing"))
        .copied()
        .unwrap_or(0.0);
    if (timing - 12.0).abs() > 0.2 {
        return Err(format!("timing is {timing}, wanted 12"));
    }
    Ok(())
}

fn wire_halve(h: &mut Handle, _size: usize) {
    let node = human_node(h.probe().project()).expect("the Humanise node");
    h.step("halve the timing");
    super::patch::set(h, node, "timing", 6.0);
}

fn wire_halved(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let node = human_node(project).ok_or("no Humanise node")?;
    let timing = project
        .graph()
        .node(node)
        .and_then(|n| n.params.get("timing"))
        .copied()
        .unwrap_or(0.0);
    if (timing - 6.0).abs() > 0.2 {
        return Err(format!("timing is {timing}, wanted 6"));
    }
    Ok(())
}

pub fn wire_route() -> Route {
    Route {
        story: "S08",
        tag: Tag::Expert,
        name: "humanise on the note wire with a Humanise node",
        fixture: wire_fixture,
        size: None,
        steps: wire_steps,
        done: wire_done,
        late: &[LateChange {
            name: "halve the humanise amount",
            steps: wire_halve,
            done: wire_halved,
            keep: &[],
        }],
    }
}
