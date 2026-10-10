//! Compiles the project graph into a `RenderPlan` and runs it, either from the
//! real-time audio callback or from the offline renderer. See
//! `docs/ARCHITECTURE.md` for the real-time rules this crate must follow.
//!
//! The node API, for writing nodes, starts at [`NodeType`].

// Lets `#[derive(Ports)]`, which names `::noodle_engine`, work in this crate.
extern crate self as noodle_engine;

mod automation;
mod builtin;
mod cache;
mod compile;
mod denormals;
mod event;
mod flatten;
mod job;
mod lane;
mod node;
mod offline;
mod param;
mod plan;
mod registry;
mod render;
mod replace;
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
pub use cache::{
    Analysis, CacheEnv, InputOrigin, NodeAnalysis, Target, TargetKind, Uncacheable, analyze,
    output_key,
};
pub use compile::{
    BufferId, Diagnostic, EventBufferId, InputSource, Location, Phase, Problem, Schedule,
    ScheduledNode, compile, compile_replacing, compile_with_lanes,
};
pub use event::{Event, EventKind, EventsOut, Expression, NoteId};
pub use flatten::flatten;
pub use job::{Job, JobPanicked};
pub use lane::{Lane, LaneInputs, LaneKernel, LaneOutputs, PerLane, Skip};
pub use node::{
    ConfigInfo, Context, InputKind, InputPort, Instance, Io, Layout, Mode, Node, NodeError,
    NodeInfo, NodeType, Port, Setup, Transport,
};
pub use noodle_core::{Config, NodeId, Value};
pub use noodle_macros::Ports;
pub use offline::{
    Cancelled, OfflineError, OfflineInput, OfflineNode, OfflineOutput, Progress, apply_offset,
    render_offline_node,
};
pub use param::{Modulation, ParamInfo, ParamKind, Taper, Unit};
pub use registry::Registry;
pub use render::{
    Render, RenderError, StreamError, Tap, TapSink, render, render_project,
    render_project_streaming, render_project_streaming_replacing, render_taps,
};
pub use replace::{Replacement, Replacements, TapSpec};
pub use runtime::{Bus, Controller, Processor, Settings, SettingsError, engine};
pub use signal::{Shape, ShapeError, SignalBuffer, SignalIn, SignalOut};
pub use telemetry::{
    Level, MeterReader, MeterWriter, ParamReading, ParamWriter, ScopeView, ScopeWriter, Telemetry,
};
pub use tempo::TempoTable;
pub use transport::TransportControl;
