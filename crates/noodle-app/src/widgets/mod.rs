//! Widgets for editing parameters, shared by the properties panel and the
//! node editor.
//!
//! [`ParamField`] covers every [`ParamInfo`](noodle_engine::ParamInfo): linear
//! and log tapers, units, and stepped choices. [`ParamField::compact`] is the
//! smaller version drawn on node bodies. [`ConfigField`] edits one config
//! setting. The widgets never change the project themselves: they report an
//! edit, and the caller turns it into a command.

pub mod config;
pub mod format;
pub mod param;
pub mod taper;

#[cfg(test)]
mod tests;

pub use config::ConfigField;
pub use param::{Live, ParamField};
