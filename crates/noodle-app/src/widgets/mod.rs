//! Widgets for editing parameters, shared by the properties panel and the
//! node editor.
//!
//! [`ParamField`] covers every [`ParamInfo`](noodle_engine::ParamInfo): linear
//! and log tapers, units, and stepped choices. [`ParamField::compact`] is the
//! smaller version drawn on node bodies. [`ConfigField`] edits one config
//! setting. [`GraphEditor`] draws what a node does (an envelope, a filter's
//! response) with points to drag. The widgets never change the project themselves: they report an
//! edit, and the caller turns it into a command.

pub mod config;
pub mod curve;
pub mod format;
pub mod graph;
pub mod mixing_graph;
pub mod param;
pub mod taper;

#[cfg(test)]
mod tests;

pub use config::ConfigField;
pub use curve::CurveEditor;
pub use graph::{GraphEditor, Kind as GraphKind};
pub use param::{Live, ParamField};
