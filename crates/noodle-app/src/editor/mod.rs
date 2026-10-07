//! The node editor: the canvas in the middle of the window.
//!
//! A placeholder until the real editor lands. It lists the nodes so the
//! shell has something to select and show.

use std::collections::BTreeSet;

use noodle_core::NodeId;

use crate::session::{Edit, Session};

/// What the editor remembers between frames.
#[derive(Default)]
pub struct EditorState {
    pub selected: BTreeSet<NodeId>,
    /// The node the properties panel shows. Always one of `selected`.
    pub active: Option<NodeId>,
}

impl EditorState {
    /// Forgets nodes that no longer exist, e.g. after an undo.
    pub fn retain_existing(&mut self, session: &Session) {
        let graph = session.project().graph();
        self.selected.retain(|&id| graph.node(id).is_some());
        if self.active.is_some_and(|id| !self.selected.contains(&id)) {
            self.active = None;
        }
    }
}

/// Draws the editor and returns the edits the user made.
pub fn show(ui: &mut egui::Ui, state: &mut EditorState, session: &Session) -> Vec<Edit> {
    let graph = session.project().graph();
    if graph.nodes().next().is_none() {
        ui.centered_and_justified(|ui| ui.weak("No nodes yet"));
        return Vec::new();
    }
    for (id, node) in graph.nodes() {
        let name = session
            .registry()
            .get(&node.type_id)
            .map_or(node.type_id.as_str(), |t| t.info().name);
        let selected = state.selected.contains(&id);
        if ui
            .selectable_label(selected, format!("{name} ({id})"))
            .clicked()
        {
            state.selected = BTreeSet::from([id]);
            state.active = Some(id);
        }
    }
    Vec::new()
}
