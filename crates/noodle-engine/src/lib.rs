//! Compiles the project graph into a `RenderPlan` and runs it, either from the
//! real-time audio callback or from the offline renderer. See
//! `docs/ARCHITECTURE.md` for the real-time rules this crate must follow.
//!
//! The node API, for writing nodes, starts at [`NodeType`].

mod builtin;
mod compile;
mod event;
mod lane;
mod node;
mod offline;
mod param;
mod plan;
mod registry;
mod render;
mod runtime;
mod signal;
pub mod testing;

pub use builtin::{OUTPUT_ID, Output};
pub use compile::{
    BufferId, Diagnostic, EventBufferId, InputSource, Location, Problem, Schedule, ScheduledNode,
    compile,
};
pub use event::{Event, EventKind, EventsOut, Expression, NoteId};
pub use lane::{Lane, LaneInputs, LaneKernel, LaneOutputs, PerLane};
pub use node::{
    ConfigInfo, Context, InputKind, InputPort, Instance, Io, Layout, Mode, Node, NodeError,
    NodeInfo, NodeType, Port, Setup, Transport,
};
pub use noodle_core::{Config, Value};
pub use offline::{Cancelled, OfflineNode, Progress};
pub use param::{ParamInfo, ParamKind, Taper, Unit};
pub use registry::Registry;
pub use render::{Render, RenderError, render};
pub use runtime::{Controller, Processor, Settings, SettingsError, engine};
pub use signal::{Shape, ShapeError, SignalBuffer, SignalIn, SignalOut};
