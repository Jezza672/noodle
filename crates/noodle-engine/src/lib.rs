//! Compiles the project graph into a `RenderPlan` and runs it, either from the
//! real-time audio callback or from the offline renderer. See
//! `docs/ARCHITECTURE.md` for the real-time rules this crate must follow.
//!
//! The node API, for writing nodes, starts at [`NodeType`].

mod automation;
mod builtin;
mod compile;
mod denormals;
mod event;
mod flatten;
mod lane;
mod node;
mod offline;
mod param;
mod plan;
mod registry;
mod render;
mod runtime;
mod signal;
mod telemetry;
mod tempo;
pub mod testing;
mod transport;

pub use automation::{AUTOMATION_ID, INTERNAL_CATEGORY, Lanes};
pub use builtin::{
    INPUT_CHANNELS, INPUT_ID, Input, MAX_INPUT_CHANNELS, OUTPUT_DEVICE, OUTPUT_DEVICE_KEY,
    OUTPUT_ID, Output, output_devices,
};
pub use compile::{
    BufferId, Diagnostic, EventBufferId, InputSource, Location, Problem, Schedule, ScheduledNode,
    compile, compile_with_lanes,
};
pub use event::{Event, EventKind, EventsOut, Expression, NoteId};
pub use flatten::flatten;
pub use lane::{Lane, LaneInputs, LaneKernel, LaneOutputs, PerLane};
pub use node::{
    ConfigInfo, Context, InputKind, InputPort, Instance, Io, Layout, Mode, Node, NodeError,
    NodeInfo, NodeType, Port, Setup, Transport,
};
pub use noodle_core::{Config, NodeId, Value};
pub use offline::{Cancelled, OfflineNode, Progress};
pub use param::{ParamInfo, ParamKind, Taper, Unit};
pub use registry::Registry;
pub use render::{Render, RenderError, render, render_project};
pub use runtime::{Bus, Controller, Processor, Settings, SettingsError, engine};
pub use signal::{Shape, ShapeError, SignalBuffer, SignalIn, SignalOut};
pub use telemetry::{
    Level, MeterReader, MeterWriter, ParamReading, ParamWriter, ScopeView, ScopeWriter, Telemetry,
};
pub use tempo::TempoTable;
pub use transport::TransportControl;
