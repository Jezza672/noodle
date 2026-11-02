//! The routes. One file per story, named `sNN_what.rs`; the framework's own
//! check is `framework_smoke`.

mod framework_smoke;

use std::path::Path;

use noodle_core::Project;

use super::Route;
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
