//! Shared steps for routes that program a MIDI part in the piano roll.

use std::path::Path;

use egui::{Modifiers, Pos2};
use noodle_core::{ClipId, Command, Endpoint, Node, Project};
use noodle_nodes::TRACK_INPUT_ID;

use super::super::handle::Handle;
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

pub const BEAT: i64 = 960;

/// A saved project with one track, wired to an output.
pub fn one_track(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    let track = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: track,
        node: Node::new(TRACK_INPUT_ID).at(0.0, 300.0),
    })]);
    let out = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: out,
        node: Node::new(noodle_engine::OUTPUT_ID).at(700.0, 300.0),
    })]);
    session.edit([Edit::Apply(noodle_core::spare::wire(
        Endpoint::new(track, "midi"),
        Endpoint::new(out, "in"),
    ))]);
    finished(session, dir)
}

/// Right-clicks the track's lane for a new MIDI clip, which opens the roll.
pub fn new_clip(h: &mut Handle) {
    h.step("make a clip");
    h.probe().settle();
    h.right_click(Pos2::new(190.0, 100.0));
    let new = h.find("New MIDI clip");
    h.click(new, Modifiers::NONE);
}

/// The one MIDI clip.
pub fn clip_of(p: &Project) -> ClipId {
    p.clips()
        .find(|(_, c)| c.as_midi().is_some())
        .map(|(id, _)| id)
        .expect("the MIDI clip")
}

/// One note, drawn by dragging across its row from `at`.
pub fn hit(h: &mut Handle, at: i64, key: u8) {
    h.roll_point(at + 20, key);
    let to = h.roll_point(at + 240, key);
    let from = h.roll_point(at + 20, key);
    h.drag_playing(&[from, to]);
}

/// Selects the notes starting at the given ticks: a click on the first,
/// then Shift+click on each of the others.
pub fn select(h: &mut Handle, notes: &[(i64, u8)]) {
    for (i, (tick, key)) in notes.iter().enumerate() {
        let at = h.roll_point(tick + 100, *key);
        let modifiers = if i == 0 {
            Modifiers::NONE
        } else {
            Modifiers::SHIFT
        };
        h.click(at, modifiers);
    }
}
