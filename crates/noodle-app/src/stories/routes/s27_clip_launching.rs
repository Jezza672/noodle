//! S27: perform a set live with clips and scenes.
//!
//! Blocked. There is no clip launcher or scene (#364), and no transport tap tempo, MIDI clock in or Link (#365).
//!
//! A tripwire (see `tripwire.rs`): the route looks in the menus for a launcher, scenes or tempo sync
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
        story: "S27",
        tag: Tag::Default,
        name: "look for a launcher, scenes or tempo sync",
        fixture,
        size: None,
        steps: |h, _| look(h, &["Launcher", "Scene", "Tap tempo", "Link"]),
        done: |p, _| nothing_found(p, "S27"),
        late: &[],
    }
}
