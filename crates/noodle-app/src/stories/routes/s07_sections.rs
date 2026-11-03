//! S07: restructure an arrangement by section.
//!
//! Blocked. There are no named arrangement sections (#329).
//!
//! A tripwire (see `tripwire.rs`): the route looks in the menus for arrangement sections
//! and passes while there is none.

use std::path::Path;

use super::super::{Route, Tag};
use super::empty_project;
use super::tripwire::{look, nothing_found};
use crate::app::App;

fn fixture(dir: &Path) -> App {
    empty_project(dir)
}

pub fn route() -> Route {
    Route {
        story: "S07",
        tag: Tag::Default,
        name: "look for arrangement sections",
        fixture,
        size: None,
        steps: |h, _| look(h, &["Arrangement section", "Sections"]),
        done: |p, _| nothing_found(p, "S07"),
        late: &[],
    }
}
