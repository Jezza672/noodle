//! The routes. One file per story, named `sNN_what.rs`; the framework's own
//! check is `framework_smoke`.

mod framework_smoke;

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
    vec![framework_smoke::route()]
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
