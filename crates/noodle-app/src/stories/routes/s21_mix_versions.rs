//! S21: recall alternate mix versions.
//!
//! The project keeps a list of versions, and a Version Value node holds one
//! value per version. Right-click a parameter, "Vary per version", and a
//! Version Value node is wired into it (the first time, it also starts the
//! list with "Main" and "Alt"). The toolbar's version menu switches the
//! version while playing, adds, renames and removes versions, and the export
//! dialog lists the versions to write in one batch. Everything that is not
//! varied is shared, so changing the vocal EQ in every version is an ordinary
//! edit. Choosing the folder for the batch is a native dialog that a route
//! cannot drive, so the route ticks the versions and stops before "Export…".

use std::path::Path;

use egui::{Key, Modifiers};
use noodle_core::{Command, Endpoint, Node, NodeId, Project, VERSION_VALUE_ID, spare};
use noodle_nodes::SVF_ID;

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

const SINE: &str = "noodle.osc.sine";

/// A sine through a filter (the "vocal EQ") to an output.
fn fixture(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    let mut add = |node: Node| {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        id
    };
    let source = add(Node::new(SINE).at(100.0, 300.0));
    let filter = add(Node::new(SVF_ID).at(450.0, 300.0));
    let out = add(Node::new(noodle_engine::OUTPUT_ID).at(850.0, 300.0));
    for (from, to) in [(source, filter), (filter, out)] {
        let (a, b) = if to == filter {
            ("out", "in")
        } else {
            ("low", "in")
        };
        session.edit([Edit::Apply(spare::wire(
            Endpoint::new(from, a),
            Endpoint::new(to, b),
        ))]);
    }
    finished(session, dir)
}

fn filter_of(p: &Project) -> NodeId {
    p.graph()
        .nodes()
        .find(|(_, n)| n.type_id == SVF_ID)
        .map(|(id, _)| id)
        .expect("the fixture has a filter")
}

fn varied(p: &Project) -> Option<NodeId> {
    p.graph()
        .source(&Endpoint::new(filter_of(p), "cutoff"))
        .map(|s| s.node)
        .filter(|&n| {
            p.graph()
                .node(n)
                .is_some_and(|n| n.type_id == VERSION_VALUE_ID)
        })
}

/// Clicks a parameter's field on `node` and types `value` into it.
fn set(h: &mut Handle, node: NodeId, key: &str, value: &str) {
    let at = h.port_row(node, false, key);
    h.click(at, Modifiers::NONE);
    h.press(at, Modifiers::COMMAND, Key::A);
    h.type_text(at, value);
    h.press(at, Modifiers::NONE, Key::Enter);
}

/// Opens the toolbar's version menu and chooses `entry`.
fn menu_button(h: &mut Handle) -> egui::Pos2 {
    h.find_like("Ver ")
}

fn version_menu(h: &mut Handle, entry: &str) {
    let menu = menu_button(h);
    h.click(menu, Modifiers::NONE);
    h.probe().settle();
    let item = h.find(entry);
    h.click(item, Modifiers::NONE);
    h.probe().settle();
    // The menu stays open until a click outside it.
    h.press(item, Modifiers::NONE, Key::Escape);
    h.probe().settle();
}

fn steps(h: &mut Handle, _size: usize) {
    let filter = filter_of(h.probe().project());
    h.step("vary the cutoff per version");
    let cutoff = h.port_row(filter, false, "cutoff");
    h.right_click(cutoff);
    let vary = h.find("Vary per version");
    h.click(vary, Modifiers::NONE);
    h.probe().settle();
    let node = varied(h.probe().project()).expect("a Version Value node is wired in");
    h.step("set the alternate version's value");
    set(h, node, "v2", "0.9");
    h.step("add a third version");
    version_menu(h, "Add version");
    h.step("list the versions to export in one batch");
    let file = h.find("File");
    h.click(file, Modifiers::NONE);
    h.probe().settle();
    let export = h.find("Export Audio…");
    h.click(export, Modifiers::NONE);
    h.probe().settle();
    let alt = h.find("Alt");
    h.click(alt, Modifiers::NONE);
    h.probe().settle();
    // The folder is chosen in a native dialog, which a route cannot drive.
    let close = h.find("Close");
    h.press(close, Modifiers::NONE, Key::Escape);
    h.probe().settle();
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let node = varied(project).ok_or("the cutoff is not varied per version")?;
    if project.versions().len() < 3 {
        return Err(format!("{} versions, wanted 3", project.versions().len()));
    }
    let node = project.graph().node(node).expect("just found");
    let slot = |k: &str| node.params.get(k).copied().unwrap_or(0.0);
    if (slot("v1") - slot("v2")).abs() < 1e-3 {
        return Err("both versions hold the same cutoff".into());
    }
    if noodle_core::version_slots(node) != project.versions().len() {
        return Err("the node's slots do not follow the version list".into());
    }
    Ok(())
}

fn fourth(h: &mut Handle, _size: usize) {
    h.step("add a fourth version");
    version_menu(h, "Add version");
}

fn has_four(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    if project.versions().len() != 4 {
        return Err(format!("{} versions, wanted 4", project.versions().len()));
    }
    let node = varied(project).ok_or("the cutoff is no longer varied")?;
    let slots = noodle_core::version_slots(project.graph().node(node).expect("found"));
    if slots != 4 {
        return Err(format!("the node has {slots} slots, wanted 4"));
    }
    Ok(())
}

fn shared_eq(h: &mut Handle, _size: usize) {
    let filter = filter_of(h.probe().project());
    h.step("raise the resonance, which no version varies");
    set(h, filter, "resonance", "0.6");
}

fn eq_is_shared(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let filter = project.graph().node(filter_of(project)).expect("found");
    if filter.params.get("resonance").copied().unwrap_or(0.0) < 0.5 {
        return Err("the resonance did not change".into());
    }
    if project.versions().len() < 3 {
        return Err("the edit disturbed the versions".into());
    }
    Ok(())
}

fn rename(h: &mut Handle, _size: usize) {
    h.step("rename the active version");
    let menu = menu_button(h);
    h.click(menu, Modifiers::NONE);
    h.probe().settle();
    let at = h.probe().line_box();
    h.click(at, Modifiers::NONE);
    h.press(at, Modifiers::COMMAND, Key::A);
    h.type_text(at, "Vocal up");
    h.press(at, Modifiers::NONE, Key::Enter);
    h.probe().settle();
}

fn renamed(p: &mut Probe, _size: usize) -> Result<(), String> {
    let names = p.project().versions();
    if names.iter().any(|n| n == "Vocal up") {
        Ok(())
    } else {
        Err(format!("no version is called Vocal up: {names:?}"))
    }
}

pub fn route() -> Route {
    Route {
        story: "S21",
        tag: Tag::Default,
        name: "vary the cutoff per version",
        fixture,
        size: None,
        steps,
        done,
        late: &[
            LateChange {
                name: "change the vocal EQ in every version at once",
                steps: shared_eq,
                done: eq_is_shared,
                keep: &[],
            },
            LateChange {
                name: "add a fourth version",
                steps: fourth,
                done: has_four,
                keep: &[],
            },
            LateChange {
                name: "rename a version",
                steps: rename,
                done: renamed,
                keep: &[],
            },
        ],
    }
}
