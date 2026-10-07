//! Project document model: the graph, tracks and clips, edit commands with
//! undo/redo, and serialization. Knows nothing about DSP or the UI; node types
//! are referred to by ID and resolved by the engine's registry.

mod config;
mod edit;
mod frame;
mod graph;
pub mod group;
mod project;

pub use config::{Config, Value};
pub use edit::{Command, EditError, History};
pub use frame::{Frame, FrameId};
pub use graph::{Connection, Endpoint, Graph, Node, NodeId, Position};
pub use project::{LoadError, Project};
