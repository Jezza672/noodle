//! S09: program a drum pattern.
//!
//! Partial. The Drum Rack and the piano roll cover the pattern. Swing and each
//! slot's choke group are controls in the rack's panel (#433): Swing sets a
//! Swing node in front of the Note Split (#330), and the closed and open hat
//! share a choke group in the template, so a closed hat cuts the open one off
//! with no setting to make. Loading the samples into
//! the rack's slots is not routed: a sample reaches a slot by dragging an
//! audio clip across from the timeline onto a Sampler inside the group,
//! which no route reaches yet, so the fixture starts with the kit loaded.
//! Swapping the snare sample is therefore not routed.

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{ClipId, Command, Endpoint, Node, NodeId, Position, Project, spare};
use noodle_io::write_wav;
use noodle_nodes::{SAMPLER_ID, TRACK_INPUT_ID};

use super::super::handle::{Handle, Probe, peak};
use super::super::{LateChange, Route, Tag};
use super::{add_node, finished, reveal, virtual_session};
use crate::app::App;
use crate::session::Edit;

const KICK: u8 = 36;
const SNARE: u8 = 38;
const CLOSED_HAT: u8 = 42;
const OPEN_HAT: u8 = 46;
const BEAT: i64 = 960;
const BAR: i64 = 4 * BEAT;

/// A short decaying tone, so each drum is told apart by pitch.
fn drum_wav(path: &Path, hz: f32) {
    let samples: Vec<f32> = (0..9_600)
        .map(|i| {
            let t = i as f32 / 48_000.0;
            0.6 * (-t * 25.0).exp() * (std::f32::consts::TAU * hz * t).sin()
        })
        .collect();
    write_wav(path, &samples, 1, 48_000).expect("the drum sample is written");
}

/// A track, a Drum Rack with a sample in its first four slots (kick, snare,
/// closed hat, open hat), and an output.
fn kit(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    let track = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: track,
        node: Node::new(TRACK_INPUT_ID).at(0.0, 300.0),
    })]);
    let template = session
        .library()
        .templates()
        .find(|(_, t)| t.name == "Drum Rack")
        .map(|(id, t)| (id.to_string(), t.clone()))
        .expect("the library has a Drum Rack");
    let mut next = session.new_node_id().0;
    let mut ids = || {
        let id = NodeId(next);
        next += 1;
        id
    };
    let (rack, add) =
        template
            .1
            .instantiate(&template.0, None, Position { x: 300.0, y: 300.0 }, &mut ids);
    session.edit([Edit::Apply(add)]);
    // Make sure later IDs are fresh.
    let out = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: out,
        node: Node::new(noodle_engine::OUTPUT_ID).at(700.0, 300.0),
    })]);
    for (from, to) in [
        (Endpoint::new(track, "midi"), Endpoint::new(rack, "notes")),
        (Endpoint::new(rack, "out"), Endpoint::new(out, "in")),
    ] {
        session.edit([Edit::Apply(spare::wire(from, to))]);
    }
    let samplers: Vec<NodeId> = session
        .project()
        .graph()
        .nodes()
        .filter(|(_, n)| n.type_id == SAMPLER_ID && n.parent == Some(rack))
        .map(|(id, _)| id)
        .collect();
    for (slot, (name, hz)) in [
        ("kick", 60.0),
        ("snare", 200.0),
        ("hat", 3000.0),
        ("open", 2000.0),
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.join(format!("{name}.wav"));
        drum_wav(&path, hz);
        let hash = session.samples().import(&path).expect("the sample imports");
        let command = crate::sample_drop::choose(&session, samplers[slot], &hash);
        session.edit([Edit::Apply(command)]);
    }
    finished(session, dir)
}

pub fn route() -> Route {
    Route {
        story: "S09",
        tag: Tag::Default,
        name: "two-bar pattern in the piano roll",
        fixture: kit,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "extend to four bars with a variation in bar 4",
            steps: extend,
            done: extended,
            keep: &[],
        }],
    }
}

fn clip_of(p: &Project) -> ClipId {
    p.clips()
        .find(|(_, c)| c.as_midi().is_some())
        .map(|(id, _)| id)
        .expect("the MIDI clip")
}

/// One note, drawn by dragging across `beats` quarter-beats on its row.
fn hit(h: &mut Handle, at: i64, key: u8) {
    // Scrolling to the end can move the start, so look at the start again.
    h.roll_point(at + 20, key);
    let to = h.roll_point(at + 240, key);
    let from = h.roll_point(at + 20, key);
    h.drag_playing(&[from, to]);
}

fn bar(h: &mut Handle, start: i64) {
    for beat in [0, 2] {
        hit(h, start + beat * BEAT, KICK);
    }
    for beat in [1, 3] {
        hit(h, start + beat * BEAT, SNARE);
    }
    for eighth in 0..8 {
        if eighth == 7 {
            hit(h, start + eighth * BEAT / 2, OPEN_HAT);
        } else {
            hit(h, start + eighth * BEAT / 2, CLOSED_HAT);
        }
    }
}

fn node_of(p: &Project, type_id: &str) -> NodeId {
    p.graph()
        .nodes()
        .find(|(_, n)| n.type_id == type_id)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no {type_id} node"))
}

/// The Drum Rack group: the top-level group.
fn rack_of(p: &Project) -> NodeId {
    p.graph()
        .nodes()
        .find(|(_, n)| n.parent.is_none() && n.type_id == noodle_core::group::GROUP)
        .map(|(id, _)| id)
        .expect("the Drum Rack")
}

const SWING: f32 = 0.3;

/// Swing is a control in the rack's panel, under Controls. It sets the Swing
/// node's amount inside the group. The closed and open hats already share a
/// choke group in the template, shown there as the slots' choke controls.
fn swing(h: &mut Handle) {
    h.step("select the rack");
    let rack = rack_of(h.probe().project());
    let title = h.port_row(rack, false, "Swing");
    h.click(title, Modifiers::NONE);
    h.probe().settle();
    h.step("set its swing in the panel");
    let field = h.find("Swing");
    h.double_click(field);
    h.press(field, Modifiers::COMMAND, Key::A);
    h.type_text(field, &format!("{SWING}"));
    h.press(field, Modifiers::NONE, Key::Enter);
    h.probe().settle();
}

fn steps(h: &mut Handle, _size: usize) {
    swing(h);
    h.step("make a clip");
    h.probe().settle();
    let lane = Pos2::new(190.0, 100.0);
    h.right_click(lane);
    let new = h.find("New MIDI clip");
    h.click(new, Modifiers::NONE);
    h.step("stretch it to two bars");
    let id = clip_of(h.probe().project());
    let rect = h.clip_rect(id);
    h.drag_playing(&[
        Pos2::new(rect.right() - 2.0, rect.center().y),
        Pos2::new(rect.right() - 2.0 + rect.width(), rect.center().y),
    ]);
    h.step("snap to eighths");
    h.probe().settle();
    let snap = h.find_first("Snap: Beat");
    h.click(snap, Modifiers::NONE);
    h.probe().settle();
    let half = h.find_first("1/2 beat");
    h.click(half, Modifiers::NONE);
    h.step("program bar 1");
    bar(h, 0);
    h.step("program bar 2");
    bar(h, BAR);
    h.step("listen");
    let at = Pos2::new(640.0, 100.0);
    h.press_playing(at, Modifiers::NONE, Key::Space);
    let heard = h.probe().hear(std::time::Duration::from_millis(1500));
    *LEVEL.lock().unwrap() = peak(&heard);
    h.press_playing(at, Modifiers::NONE, Key::Space);
}

fn wire(h: &mut Handle, from: (NodeId, &str), to: (NodeId, &str)) {
    let a = h.socket(from.0, true, from.1);
    let b = h.socket(to.0, false, to.1);
    h.drag(Modifiers::NONE, &[a, b]);
}

/// A point on a node's title bar, just above the row of its input `key`.
/// Pans it into view first if it is off the canvas.
fn title_of(h: &mut Handle, node: NodeId, key: &str) -> Pos2 {
    let at = |h: &mut Handle| {
        let row = h.probe().port_rect(node, false, key);
        Pos2::new(row.center().x, row.top() - 8.0)
    };
    let first = at(h);
    reveal(h, &[first]);
    at(h)
}

/// The loudest sample heard while the pattern played.
static LEVEL: std::sync::Mutex<f32> = std::sync::Mutex::new(0.0);

fn swung_and_choked(p: &Project) -> Result<(), String> {
    let swing = node_of(p, noodle_nodes::SWING_ID);
    let amount = p
        .graph()
        .node(swing)
        .and_then(|n| n.params.get("amount"))
        .copied()
        .unwrap_or(0.0);
    if (amount - SWING).abs() > 0.01 {
        return Err(format!("the swing amount is {amount}, wanted {SWING}"));
    }
    let split = node_of(p, noodle_nodes::NOTE_SPLIT_ID);
    let group = |n: usize| {
        p.graph()
            .node(split)
            .and_then(|s| s.params.get(&format!("choke{n}")))
            .copied()
            .unwrap_or(0.0)
    };
    // Slots 3 and 4 are the closed and open hat.
    if group(3) < 0.5 || group(3) != group(4) {
        return Err(format!(
            "the hats are not in one choke group ({} and {})",
            group(3),
            group(4)
        ));
    }
    Ok(())
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    swung_and_choked(p.project())?;
    let heard = *LEVEL.lock().unwrap();
    if heard < 0.05 {
        return Err(format!("the pattern is silent (peak {heard})"));
    }
    let id = clip_of(p.project());
    let notes = p.roll_notes(id);
    let count = |key: u8| notes.iter().filter(|n| n.2 == key).count();
    let starts = |key: u8| {
        let mut s: Vec<i64> = notes.iter().filter(|n| n.2 == key).map(|n| n.0).collect();
        s.sort();
        s
    };
    if starts(KICK) != [0, 2 * BEAT, BAR, BAR + 2 * BEAT] {
        return Err(format!("the kicks are at {:?}", starts(KICK)));
    }
    if starts(SNARE) != [BEAT, 3 * BEAT, BAR + BEAT, BAR + 3 * BEAT] {
        return Err(format!("the snares are at {:?}", starts(SNARE)));
    }
    for (key, want, name) in [
        (KICK, 4, "kick"),
        (SNARE, 4, "snare"),
        (CLOSED_HAT, 14, "closed hat"),
        (OPEN_HAT, 2, "open hat"),
    ] {
        if count(key) != want {
            return Err(format!(
                "{} {name} notes, wanted {want}: {notes:?}",
                count(key)
            ));
        }
    }
    Ok(())
}

fn extend(h: &mut Handle, _size: usize) {
    h.step("stretch the clip to four bars");
    let id = clip_of(h.probe().project());
    let rect = h.clip_rect(id);
    h.drag_playing(&[
        Pos2::new(rect.right() - 2.0, rect.center().y),
        Pos2::new(rect.right() - 2.0 + rect.width(), rect.center().y),
    ]);
    h.step("program bars 3 and 4");
    // Variation in bar 4: a second snare on the last eighth of bar 4.
    for bar_start in [2 * BAR, 3 * BAR] {
        bar(h, bar_start);
    }
    h.step("add the variation");
    hit(h, 3 * BAR + 3 * BEAT + BEAT / 2, SNARE);
}

fn extended(p: &mut Probe, _size: usize) -> Result<(), String> {
    let id = clip_of(p.project());
    let notes = p.roll_notes(id);
    let in_bar4 = notes
        .iter()
        .filter(|n| n.0 >= 3 * BAR && n.2 == SNARE)
        .count();
    if in_bar4 != 3 {
        return Err(format!("{in_bar4} snares in bar 4, wanted 3: {notes:?}"));
    }
    Ok(())
}

// --- The Pattern Player route ---

pub fn pattern_route() -> Route {
    Route {
        story: "S09",
        tag: Tag::Expert,
        name: "one-bar pattern in a Pattern Player's step grid",
        fixture: kit,
        size: None,
        steps: player_steps,
        done: player_done,
        late: &[],
    }
}

/// The steps (of sixteen, from 1) each drum plays on.
const GRID: [(u8, &[usize]); 4] = [
    (KICK, &[1, 9]),
    (SNARE, &[5, 13]),
    (CLOSED_HAT, &[1, 3, 5, 7, 9, 11, 13]),
    (OPEN_HAT, &[15]),
];

fn player_steps(h: &mut Handle, _size: usize) {
    let rack = rack_of(h.probe().project());
    h.step("add a Pattern Player");
    let player = add_node(h, Pos2::new(60.0, 520.0), "pattern player");
    h.step("play the rack from it");
    wire(h, (player, "notes"), (rack, "notes"));
    h.step("select the player");
    let title = title_of(h, player, "mode");
    h.click(title, Modifiers::NONE);
    h.step("add a pattern");
    let new = h.find("+ New slot");
    h.click(new, Modifiers::NONE);
    // The field for the next row's key starts at 36 and keeps the last one.
    let mut shown = 36;
    for (key, steps) in GRID {
        h.step("add a row for the drum");
        let field = h.probe().find_value(&shown.to_string());
        shown = key;
        h.click(field, Modifiers::NONE);
        h.press(field, Modifiers::COMMAND, Key::A);
        h.type_text(field, &key.to_string());
        h.press(field, Modifiers::NONE, Key::Enter);
        let row = h.find("+ Row");
        h.click(row, Modifiers::NONE);
        h.step("click the steps in");
        for step in steps {
            let cell = h.find(&format!("Step {step} on key {key}"));
            h.click(cell, Modifiers::NONE);
        }
    }
    h.step("listen");
    let at = Pos2::new(640.0, 100.0);
    h.press_playing(at, Modifiers::NONE, Key::Space);
    let heard = h.probe().hear(std::time::Duration::from_millis(1500));
    *LEVEL.lock().unwrap() = peak(&heard);
    h.press_playing(at, Modifiers::NONE, Key::Space);
}

fn player_done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let source = p
        .project()
        .graph()
        .source(&Endpoint::new(rack_of(p.project()), "notes"))
        .and_then(|s| p.project().graph().node(s.node))
        .is_some_and(|n| n.type_id == noodle_nodes::PATTERN_ID);
    if !source {
        return Err("the Pattern Player does not play the rack".into());
    }
    let heard = *LEVEL.lock().unwrap();
    if heard < 0.05 {
        return Err(format!("the pattern is silent (peak {heard})"));
    }
    let project = p.project();
    let player = node_of(project, noodle_nodes::PATTERN_ID);
    let notes: Vec<(i64, u8)> = project
        .patterns_of(player)
        .filter_map(|(_, c)| c.as_midi())
        .flat_map(|m| m.notes.iter().map(|n| (n.start.0, n.key)))
        .collect();
    for (key, steps) in GRID {
        let mut found: Vec<i64> = notes.iter().filter(|n| n.1 == key).map(|n| n.0).collect();
        found.sort();
        let wanted: Vec<i64> = steps.iter().map(|s| (*s as i64 - 1) * 240).collect();
        if found != wanted {
            return Err(format!("key {key} is on {found:?}, not {wanted:?}"));
        }
    }
    Ok(())
}
