//! The parameter fields on nodes: [`crate::widgets::ParamField`] for each
//! parameter input that has no wire.

use egui::{Pos2, Ui, Vec2};
use noodle_core::Endpoint;

use super::Frame_;
use super::draw::MIN_TEXT_ZOOM;
use super::layout::{NodeGeom, PortKind, Side};
use crate::session::Edit;
use crate::widgets::ParamField;

/// Shows `node`'s fields, which must be done straight after painting it so
/// nodes in front cover them. A node in front of this one under the pointer
/// hides the fields from it, so they don't take its clicks.
pub fn show(
    ui: &mut Ui,
    f: &Frame_<'_>,
    node: &NodeGeom,
    pointer: Option<Pos2>,
    edits: &mut Vec<Edit>,
) {
    let z = f.t.zoom;
    if node.reroute || z < MIN_TEXT_ZOOM {
        return;
    }
    let graph = f.project.graph();
    let Some(values) = graph.node(node.id).map(|n| &n.params) else {
        return;
    };
    let covered = pointer.is_some_and(|p| covered(f, node, p));
    // A covered field still looks the same, it just doesn't respond.
    ui.scope(|ui| {
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
            edits.extend(out.edits(node.id, &port.key));
        }
    });
}

/// Whether a node drawn after `node` is under `pointer`.
fn covered(f: &Frame_<'_>, node: &NodeGeom, pointer: Pos2) -> bool {
    f.order
        .iter()
        .map(|&i| &f.scene.nodes[i])
        .skip_while(|other| other.id != node.id)
        .skip(1)
        .any(|other| f.t.rect_to_screen(other.rect).contains(pointer))
}
