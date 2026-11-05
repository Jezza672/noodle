//! S10: multi-articulation parts.
//!
//! The project keeps a list of named articulations. In the piano roll the
//! Articulation menu adds to the list and gives the selected notes one; the
//! track sends the change ahead of each note that has it, and again after a
//! jump, so a start mid-cue gets the right switch. An Articulation Map node
//! between the track and the instrument turns each articulation into a
//! keyswitch, a controller or a program change. The fixture has the map
//! already set up for the library (staccato on C0, pizzicato on D0), as a
//! library preset for the instrument would be; the project's articulations
//! get IDs 1 and 2 in the order they are added. Renaming an articulation is
//! one edit of the list, since notes and the map hold its ID. The chase on a
//! mid-cue start is tested in the track input (`track_input_midi`), not here,
//! because a route cannot hear the events between nodes.

use std::path::Path;

use egui::Modifiers;
use noodle_core::{
    ARTICULATION_MAP_ID, ARTICULATION_MAP_KEY, Command, Config, Endpoint, Node, Value, spare,
};
use noodle_nodes::TRACK_INPUT_ID;

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::roll::{BEAT, clip_of, hit, new_clip, select};
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

const PLAYED: u8 = 60;

/// A track through an Articulation Map to an output.
fn fixture(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    let mut add = |node: Node| {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        id
    };
    let track = add(Node::new(TRACK_INPUT_ID).at(0.0, 300.0));
    let map = add(Node::new(ARTICULATION_MAP_ID)
        .at(350.0, 300.0)
        .with_config(Config::new().with(
            ARTICULATION_MAP_KEY,
            Value::Text("1 key 24 100 50\n2 key 26 100 50".into()),
        )));
    let out = add(Node::new(noodle_engine::OUTPUT_ID).at(700.0, 300.0));
    for (from, port, to) in [(track, "midi", map), (map, "out", out)] {
        session.edit([Edit::Apply(spare::wire(
            Endpoint::new(from, port),
            Endpoint::new(to, "in"),
        ))]);
    }
    finished(session, dir)
}

/// Adds an articulation called `name` from the piano roll's menu.
fn add_articulation(h: &mut Handle, name: &str) {
    let menu = h.find("Articulation");
    h.click(menu, Modifiers::NONE);
    h.probe().settle();
    let at = h.probe().line_box();
    h.click(at, Modifiers::NONE);
    h.type_text(at, name);
    h.probe().settle();
    let add = h.find("Add");
    h.click(add, Modifiers::NONE);
    h.probe().settle();
    // Close the menu again.
    h.press(at, Modifiers::NONE, egui::Key::Escape);
    h.probe().settle();
}

/// Gives the note starting at `tick` the articulation called `name`.
fn give(h: &mut Handle, tick: i64, name: &str) {
    select(h, &[(tick, PLAYED)]);
    let menu = h.find("Articulation");
    h.click(menu, Modifiers::NONE);
    h.probe().settle();
    let item = h.find(name);
    h.click(item, Modifiers::NONE);
    h.probe().settle();
}

fn steps(h: &mut Handle, _size: usize) {
    new_clip(h);
    h.step("play the phrase");
    hit(h, 0, PLAYED);
    hit(h, 2 * BEAT, PLAYED);
    h.step("name the articulations");
    add_articulation(h, "Staccato");
    add_articulation(h, "Pizzicato");
    h.step("paint the first phrase staccato");
    give(h, 0, "Staccato");
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let names: Vec<_> = project.articulations().iter().map(|a| &a.name).collect();
    if names.len() < 2 {
        return Err(format!("{} articulations, wanted 2", names.len()));
    }
    let id = clip_of(project);
    let midi = project
        .clip(id)
        .and_then(|c| c.as_midi())
        .ok_or("the clip is gone")?;
    let first = midi
        .notes
        .iter()
        .find(|n| n.start.0 < BEAT)
        .ok_or("no first note")?;
    if first.articulation != Some(1) {
        return Err(format!("the first note has {:?}", first.articulation));
    }
    let wired = project
        .graph()
        .nodes()
        .any(|(_, n)| n.type_id == ARTICULATION_MAP_ID);
    if !wired {
        return Err("the Articulation Map is gone".into());
    }
    Ok(())
}

fn second_phrase(h: &mut Handle, _size: usize) {
    h.step("paint the second phrase pizzicato");
    give(h, 2 * BEAT, "Pizzicato");
}

fn two_articulations(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let id = clip_of(project);
    let midi = project.clip(id).and_then(|c| c.as_midi()).ok_or("gone")?;
    let used: std::collections::BTreeSet<_> =
        midi.notes.iter().filter_map(|n| n.articulation).collect();
    if used.len() == 2 {
        Ok(())
    } else {
        Err(format!("the notes use articulations {used:?}, wanted 2"))
    }
}

fn rename(h: &mut Handle, _size: usize) {
    h.step("rename an articulation");
    let menu = h.find("Articulation");
    h.click(menu, Modifiers::NONE);
    h.probe().settle();
    let rename = h.find("Rename");
    h.click(rename, Modifiers::NONE);
    h.probe().settle();
    let at = h.probe().find_value("Staccato");
    h.click(at, Modifiers::NONE);
    h.press(at, Modifiers::COMMAND, egui::Key::A);
    h.type_text(at, "Short");
    h.press(at, Modifiers::NONE, egui::Key::Enter);
    h.probe().settle();
}

fn renamed(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let first = project.articulation(1).ok_or("articulation 1 is gone")?;
    if first.name != "Short" {
        return Err(format!("articulation 1 is called {}", first.name));
    }
    // The notes still have it: they hold the ID.
    let id = clip_of(project);
    let midi = project.clip(id).and_then(|c| c.as_midi()).ok_or("gone")?;
    if !midi.notes.iter().any(|n| n.articulation == Some(1)) {
        return Err("the rename took the articulation off the notes".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S10",
        tag: Tag::Default,
        name: "paint notes with named articulations",
        fixture,
        size: None,
        steps,
        done,
        late: &[
            LateChange {
                name: "switch articulation for a second phrase",
                steps: second_phrase,
                done: two_articulations,
                keep: &[],
            },
            LateChange {
                name: "rename an articulation",
                steps: rename,
                done: renamed,
                keep: &[],
            },
        ],
    }
}
