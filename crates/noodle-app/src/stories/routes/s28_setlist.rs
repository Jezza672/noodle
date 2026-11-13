//! S28: run backing tracks on stage.
//!
//! Blocked. There is no setlist of songs with their own tempo, click, cues and outputs.
//!
//! A tripwire (see `tripwire.rs`): the route looks in the menus for a setlist
//! and, while there is none, fails as blocked: the goal is not reached, so the
//! scorecard must not count it as done.

use std::path::Path;

use super::super::{Route, Tag};
use super::empty_project;
use super::tripwire::{blocked, look};
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
        done: |p, _| blocked(p, "S28"),
        late: &[],
    }
}
