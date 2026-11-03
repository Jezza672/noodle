//! S04: tighten a multitrack drum recording.
//!
//! Blocked. Onsets are detected for warp markers, but nothing quantizes the transients of linked clips together at a strength with crossfades (#347).
//!
//! A tripwire (see `tripwire.rs`): the route looks in the menus for a group transient quantize
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
        story: "S04",
        tag: Tag::Default,
        name: "look for a group transient quantize",
        fixture,
        size: None,
        steps: |h, _| look(h, &["Transient", "Tighten"]),
        done: |p, _| nothing_found(p, "S04"),
        late: &[],
    }
}
