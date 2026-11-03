//! S11: turn an audio phrase into MIDI.
//!
//! Blocked. There is no audio-to-MIDI conversion (#352).
//!
//! A tripwire (see `tripwire.rs`): the route looks in the menus for audio to MIDI
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
        story: "S11",
        tag: Tag::Default,
        name: "look for audio to MIDI",
        fixture,
        size: None,
        steps: |h, _| look(h, &["Audio to MIDI", "Convert to MIDI"]),
        done: |p, _| nothing_found(p, "S11"),
        late: &[],
    }
}
