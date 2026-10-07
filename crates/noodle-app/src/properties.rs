//! The properties panel: the active node's parameters and config.
//!
//! A placeholder until the real panel and widgets land.

use noodle_core::NodeId;

use crate::session::{Edit, Session};

pub fn show(ui: &mut egui::Ui, session: &Session, active: Option<NodeId>) -> Vec<Edit> {
    let Some(node) = active.and_then(|id| session.project().graph().node(id)) else {
        ui.weak("Select a node to see its properties.");
        return Vec::new();
    };
    ui.heading(&node.type_id);
    for (key, value) in &node.params {
        ui.label(format!("{key}: {value}"));
    }
    Vec::new()
}
