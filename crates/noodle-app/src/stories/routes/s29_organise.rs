//! S29: organise a large session with templates.
//!
//! Partial. Groups and templates exist. Colour-coding tracks, folders,
//! hiding unused tracks and macros for repeated edits are not there
//! (#367), so the route looks for them in the menus and passes while there
//! is none (a tripwire; see `tripwire.rs`).

use super::super::{Route, Tag};
use super::empty_project;
use super::tripwire::{look, nothing_found};

pub fn route() -> Route {
    Route {
        story: "S29",
        tag: Tag::Default,
        name: "look for track colours, folders and macros",
        fixture: empty_project,
        size: None,
        steps: |h, _| {
            look(
                h,
                &[
                    "Track colour",
                    "Track color",
                    "Folder",
                    "Hide track",
                    "Macro",
                ],
            )
        },
        done: |p, _| nothing_found(p, "S29"),
        late: &[],
    }
}
