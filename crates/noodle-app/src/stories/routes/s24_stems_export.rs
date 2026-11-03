//! S24: deliver stems to a collaborator.
//!
//! Partial. The export dialog can write each group output as its own file,
//! named after it, at the project's rate. Choosing the folder is a native
//! dialog that a route cannot drive, so the route ticks the group, unticks
//! the mixdown and stops before "Export…". The stems all share one length, set
//! by the dialog's Tail row (to silence up to a cap, or fixed), saved in the
//! project. The route leaves the default (to silence), so its cost is the
//! same as before the setting existed.

use std::path::Path;

use egui::Modifiers;
use noodle_core::{NodeId, Position, Project};

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

/// A Drum Rack group on its own: the thing a stem is made of.
fn rack(dir: &Path) -> App {
    let mut session = virtual_session(dir);
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
    let (_, add) =
        template
            .1
            .instantiate(&template.0, None, Position { x: 300.0, y: 300.0 }, &mut ids);
    session.edit([Edit::Apply(add)]);
    finished(session, dir)
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("open the export dialog");
    let file = h.find("File");
    h.click(file, Modifiers::NONE);
    h.probe().settle();
    let export = h.find("Export Audio…");
    h.click(export, Modifiers::NONE);
    h.probe().settle();
    h.step("write the group as its own file, not a mixdown");
    // These labels are the checkboxes': the step cannot go on without them.
    let mixdown = h.find("Mixdown");
    h.click(mixdown, Modifiers::NONE);
    let group = h.find("Drum Rack");
    h.click(group, Modifiers::NONE);
    h.probe().settle();
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let groups = |p: &Project| {
        p.graph()
            .nodes()
            .filter(|(_, n)| n.parent.is_none() && n.type_id == noodle_core::group::GROUP)
            .count()
    };
    if groups(p.project()) == 0 {
        return Err("the project has no group to export".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S24",
        tag: Tag::Default,
        name: "set up a per-group export",
        fixture: rack,
        size: None,
        steps,
        done,
        late: &[],
    }
}
