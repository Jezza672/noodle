//! S28: run backing tracks on stage.
//!
//! Blocked. There is no setlist of songs with their own tempo, click, cues and outputs (#366).
//!
//! A tripwire (see `tripwire.rs`): the route looks in the menus for a setlist
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
        story: "S28",
        tag: Tag::Default,
        name: "look for a setlist",
        fixture,
        size: None,
        steps: |h, _| look(h, &["Setlist"]),
        done: |p, _| nothing_found(p, "S28"),
        late: &[],
    }
}
