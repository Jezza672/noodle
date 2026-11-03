//! The routes. One file per story, named `sNN_what.rs`; the framework's own
//! check is `framework_smoke`.

mod framework_smoke;
mod s01_comp;
mod s09_drum_pattern;
mod s18_plugins;
mod s19_drum_bus;
mod s20_automation;

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{NodeId, Project};

use super::Route;
use super::handle::Handle;
use crate::app::App;
use crate::session::{Nodes, Session};
use crate::testkit::virtual_audio;

/// Every route, in story order.
pub fn all() -> Vec<Route> {
    vec![
        framework_smoke::route(),
        s01_comp::route(),
        s09_drum_pattern::route(),
        s18_plugins::route(),
        s19_drum_bus::route(),
        s20_automation::route(),
    ]
}

/// An empty project that saves to a file in `dir`, playing on the virtual
/// output.
pub fn empty_project(dir: &Path) -> App {
    let path = dir.join("project.ron");
    std::fs::write(&path, Project::new().to_ron()).expect("the project file is written");
    let mut session = Session::open(Nodes::all(), &path).expect("the project opens");
    session.set_audio_config(virtual_audio());
    App::new(session)
}

/// A new, saved session for fixtures that build a project by hand.
pub fn virtual_session(dir: &Path) -> Session {
    let mut session = Session::new(Nodes::all());
    assert!(
        session.save_as(&dir.join("project.ron")),
        "the project saves"
    );
    session
}

/// The fixture's session saved and opened again on the virtual output, so
/// that building it leaves nothing on the undo stack for a route to undo.
pub fn finished(mut session: Session, dir: &Path) -> App {
    session.save();
    let mut session =
        Session::open(Nodes::all(), &dir.join("project.ron")).expect("the project opens");
    session.set_audio_config(virtual_audio());
    App::new(session)
}

/// Shift+A at `at`, types `query`, Enter on the first result.
pub fn add_node(h: &mut Handle, at: Pos2, query: &str) -> NodeId {
    let before = h.probe().nodes();
    h.press(at, Modifiers::SHIFT, Key::A);
    h.type_text(at, query);
    h.press(at, Modifiers::NONE, Key::Enter);
    let after = h.probe().nodes();
    *after
        .iter()
        .find(|id| !before.contains(id))
        .unwrap_or_else(|| panic!("searching for {query:?} added no node"))
}
