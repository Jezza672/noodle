//! The parameter fields on nodes: [`crate::widgets::ParamField`] for each
//! parameter input that has no wire.

use egui::{Id, Ui, Vec2};
use noodle_core::{Endpoint, NodeId};

use super::Frame_;
use super::draw::MIN_TEXT_ZOOM;
use super::layout::{NodeGeom, PortKind, Side};
use crate::session::Edit;
use crate::widgets::ParamField;
use crate::widgets::param::Gesture;

/// The fields of every node, shown one node at a time as the nodes are
/// painted, so nodes in front cover them.
pub struct Fields {
    ui: Ui,
    /// The node whose fields take the pointer: the topmost one under it.
    /// Fields of other nodes ignore the pointer, so a node in front (or a
    /// socket sticking out of it) gets the clicks instead of a field behind.
    /// A field already being dragged keeps the pointer either way.
    front: Option<NodeId>,
    /// Whether a field was being dragged last frame, and this frame.
    was_dragging: bool,
    dragging: bool,
    /// Whether one let go this frame.
    released: bool,
}

impl Fields {
    /// `front` is the topmost node under the pointer, if any.
    pub fn new(parent: &mut Ui, canvas: egui::Rect, front: Option<NodeId>) -> Self {
        let mut ui = parent.new_child(
            egui::UiBuilder::new()
                .id_salt("noodle-editor-fields")
                .max_rect(canvas),
        );
        ui.set_clip_rect(canvas);
        let was_dragging = ui.data(|d| d.get_temp::<bool>(drag_id())).unwrap_or(false);
        Self {
            ui,
            front,
            was_dragging,
            dragging: false,
            released: false,
        }
    }

    /// Shows `node`'s fields. Call it straight after painting the node.
    pub fn show(&mut self, f: &Frame_<'_>, node: &NodeGeom, edits: &mut Vec<Edit>) {
        let z = f.t.zoom;
        if node.reroute || z < MIN_TEXT_ZOOM {
            return;
        }
        let graph = f.project.graph();
        let Some(values) = graph.node(node.id).map(|n| &n.params) else {
            return;
        };
        let covered = self.front.is_some_and(|front| front != node.id);
        let was_dragging = self.was_dragging;
        let (dragging, released) = (&mut self.dragging, &mut self.released);
        // A covered field still looks the same, it just doesn't respond.
        self.ui.scope(|ui| {
            if covered {
                ui.disable();
                ui.visuals_mut().disabled_alpha = 1.0;
            }
            for port in &node.ports {
                let PortKind::Param(info) = &port.kind else {
                    continue;
                };
                if port.side != Side::Input
                    || graph
                        .source(&Endpoint::new(node.id, port.key.clone()))
                        .is_some()
                {
                    continue;
                }
                let value = values.get(&port.key).copied().unwrap_or(info.default);
                let rect =
                    f.t.rect_to_screen(port.row)
                        .shrink2(Vec2::new(10.0 * z, 2.0 * z));
                let out = ParamField::new(&port.name, info, value)
                    .id_salt((node.id, &port.key))
                    .compact(true)
                    .zoom(z)
                    .show_at(ui, rect);
                *dragging |= out.gesture == Gesture::Dragging;
                *released |= out.gesture == Gesture::Released;
                // A drag the canvas took over (see `pointer`) stops for the
                // field without having started, so it has no undo step to end.
                let unstarted = out.gesture == Gesture::Released && !was_dragging;
                edits.extend(
                    out.edits(node.id, &port.key)
                        .into_iter()
                        .filter(|e| !(unstarted && matches!(e, Edit::EndDrag))),
                );
            }
        });
    }

    /// Closes the undo step of a field drag that stopped without the field
    /// seeing it let go: the field was hidden mid-drag, by zooming out or
    /// deleting its node.
    pub fn finish(self, edits: &mut Vec<Edit>) {
        if self.was_dragging && !self.dragging && !self.released {
            edits.push(Edit::EndDrag);
        }
        self.ui
            .data_mut(|d| d.insert_temp(drag_id(), self.dragging));
    }
}

fn drag_id() -> Id {
    Id::new("noodle-editor-field-drag")
}
