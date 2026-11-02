//! S09: program a drum pattern.
//!
//! Partial. The Drum Rack and the piano roll cover the pattern. Swing does
//! not exist (#330). A closed hat choking the open hat has no setting either,
//! so the route uses the workaround: a Script node between the track and the
//! rack passes the notes on and ends the open hat when a closed hat starts,
//! with the open hat's Sampler set to Gate so a note-off stops it. Writing
//! that script is scored as code (see `effort.rs`). Loading the samples
//! into the rack's slots is not routed either: a sample reaches a slot by
//! dragging an audio clip across from the timeline onto a Sampler inside the
//! group, which no route reaches yet, so the fixture starts with the kit
//! loaded. Swapping the snare sample is therefore not routed.

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{ClipId, Command, Endpoint, Node, NodeId, Position, Project, spare};
use noodle_io::write_wav;
use noodle_nodes::{SAMPLER_ID, SCRIPT_ID, SCRIPT_KEY, TRACK_INPUT_ID};

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

/// The choke: every note passes, and a closed hat ends the open hat first.
const CHOKE: &str = "events in notes; events out played; \
    if notes_on { if notes_key == 42 { note_off(played, 46) }; \
    note_on(played, notes_key, notes_velocity) }; \
    if notes_off { note_off(played, notes_key) }";

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

/// The rack's Sampler for the open hat: its fourth slot.
fn open_hat_sampler(p: &Project) -> NodeId {
    let rack = rack_of(p);
    p.graph()
        .nodes()
        .filter(|(_, n)| n.type_id == SAMPLER_ID && n.parent == Some(rack))
        .map(|(id, _)| id)
        .nth(3)
        .expect("the open hat's Sampler")
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

fn choke(h: &mut Handle) {
    let (track, rack, open) = {
        let probe = h.probe();
        let p = probe.project();
        (node_of(p, TRACK_INPUT_ID), rack_of(p), open_hat_sampler(p))
    };
    h.step("add a Script node");
    let script = add_node(h, Pos2::new(600.0, 320.0), "script");
    h.step("write the choke");
    let text = h.probe().text_box();
    h.click(text, Modifiers::NONE);
    h.press(text, Modifiers::COMMAND, Key::A);
    h.type_code(text, CHOKE);
    // Clicking away commits the script.
    let away = h.probe().canvas().right_top() + egui::Vec2::new(-20.0, 20.0);
    h.click(away, Modifiers::NONE);
    h.step("put it between the track and the rack");
    wire(h, (track, "midi"), (script, "notes"));
    wire(h, (script, "played"), (rack, "notes"));
    h.step("let the open hat stop when it is cut");
    let title = title_of(h, rack, "notes");
    h.click(title, Modifiers::NONE);
    h.press(title, Modifiers::NONE, Key::Tab);
    let title = title_of(h, open, "in");
    h.click(title, Modifiers::NONE);
    let gate = h.find("Gate");
    h.click(gate, Modifiers::NONE);
    h.press(away, Modifiers::NONE, Key::Tab);
}

fn steps(h: &mut Handle, _size: usize) {
    choke(h);
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

/// The loudest sample heard while the pattern played.
static LEVEL: std::sync::Mutex<f32> = std::sync::Mutex::new(0.0);

fn choked(p: &Project) -> Result<(), String> {
    let rack = rack_of(p);
    let source = p
        .graph()
        .source(&Endpoint::new(rack, "notes"))
        .ok_or("nothing plays the rack")?;
    let script = p
        .graph()
        .node(source.node)
        .filter(|n| n.type_id == SCRIPT_ID)
        .ok_or("the rack is not fed through a Script node")?;
    let text = match script.config.get(SCRIPT_KEY) {
        Some(noodle_core::Value::Text(text)) => text.clone(),
        _ => String::new(),
    };
    if !text.contains("note_off(played, 46)") {
        return Err(format!("the script does not choke the open hat: {text:?}"));
    }
    let fed = p
        .graph()
        .source(&Endpoint::new(source.node, "notes"))
        .and_then(|s| p.graph().node(s.node))
        .is_some_and(|n| n.type_id == TRACK_INPUT_ID);
    if !fed {
        return Err("the track does not play the script".into());
    }
    let mode = p
        .graph()
        .node(open_hat_sampler(p))
        .and_then(|n| n.params.get("mode"))
        .copied()
        .unwrap_or(0.0);
    if mode < 0.5 {
        return Err("the open hat's Sampler is not in Gate mode".into());
    }
    Ok(())
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    choked(p.project())?;
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
