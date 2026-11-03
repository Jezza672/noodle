//! S21: recall alternate mix versions.
//!
//! Blocked. There are no named mix versions, no A/B while playing and no batch export of every version (#358).
//!
//! A tripwire (see `tripwire.rs`): the route looks in the menus for mix snapshots or versions
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
        story: "S21",
        tag: Tag::Default,
        name: "look for mix snapshots or versions",
        fixture,
        size: None,
        steps: |h, _| look(h, &["Snapshot", "Version"]),
        done: |p, _| nothing_found(p, "S21"),
        late: &[],
    }
}
