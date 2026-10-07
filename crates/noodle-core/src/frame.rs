//! Frames: labelled boxes drawn behind nodes in the editor, for organising a
//! patch. They're pure layout and have no effect on the sound.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Position;

/// Identifies a frame within a project. Like node IDs, frame IDs aren't
/// reused within a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FrameId(pub u64);

impl fmt::Display for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "frame #{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub label: String,
    /// The top-left corner, in the same coordinates as node positions.
    pub position: Position,
    pub width: f32,
    pub height: f32,
}
