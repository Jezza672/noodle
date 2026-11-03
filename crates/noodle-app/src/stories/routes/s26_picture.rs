//! S26: score and mix to picture.
//!
//! Blocked. There is no video import, SMPTE offset or frame-locked hit point (#363).
//!
//! A tripwire (see `tripwire.rs`): the route looks in the menus for video or timecode
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
        story: "S26",
        tag: Tag::Default,
        name: "look for video or timecode",
        fixture,
        size: None,
        steps: |h, _| look(h, &["Video", "Timecode", "SMPTE"]),
        done: |p, _| nothing_found(p, "S26"),
        late: &[],
    }
}
