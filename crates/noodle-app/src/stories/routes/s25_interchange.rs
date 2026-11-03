//! S25: exchange a session via AAF or OMF.
//!
//! Blocked. There is no AAF or OMF import or export (#362).
//!
//! A tripwire (see `tripwire.rs`): the route looks in the menus for an interchange import or export
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
        story: "S25",
        tag: Tag::Default,
        name: "look for an interchange import or export",
        fixture,
        size: None,
        steps: |h, _| look(h, &["AAF", "OMF"]),
        done: |p, _| nothing_found(p, "S25"),
        late: &[],
    }
}
