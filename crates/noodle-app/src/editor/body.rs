//! Custom drawing inside a node, below its ports, for nodes that show
//! something other than parameters, such as meters and scopes.
//!
//! To give a node type a body: return its height from [`height`], and draw it
//! in [`paint`]. The editor reserves the space, and calls `paint` after
//! drawing the rest of the node.

use egui::{Painter, Rect};
use noodle_core::NodeId;

/// Extra space below a node's ports, in graph units. Zero for most nodes.
pub fn height(type_id: &str) -> f32 {
    let _ = type_id;
    0.0
}

/// Draws a node's body into `rect`, the space [`height`] reserved, in screen
/// points. Scale anything drawn by `zoom`.
pub fn paint(painter: &Painter, rect: Rect, zoom: f32, node: NodeId, type_id: &str) {
    let _ = (painter, rect, zoom, node, type_id);
}
