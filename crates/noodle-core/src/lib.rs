//! Project document model: the graph, tracks and clips, edit commands with
//! undo/redo, and serialization. Knows nothing about DSP or the UI; node types
//! are referred to by ID and resolved by the engine's registry.

mod automation;
mod cache_key;
mod clip;
mod config;
mod edit;
mod frame;
mod graph;
pub mod group;
pub mod library;
mod project;
pub mod spare;
mod timeline;
mod warp;

pub use automation::{AutomationLane, AutomationPoint, Curve, LaneId};
pub use cache_key::{CacheKey, KeyBuilder};
pub use clip::{
    AudioClip, BUTT_SECONDS, Clip, ClipContent, ClipId, Crossfade, GAIN_POINT_MAX_DB,
    GAIN_POINT_MIN_DB, GainPoint, MIN_LOOP_LEN, MidiClip, MidiNote,
};
pub use config::{Config, Value};
pub use edit::{Command, EditError, History};
pub use frame::{Frame, FrameId};
pub use graph::{Connection, Endpoint, Graph, Node, NodeId, Position};
pub use project::{LoadError, Project, TransportLoop};
pub use timeline::{
    MusicalPosition, SignatureChange, TICKS_PER_QUARTER, TempoChange, TempoMap, TempoMapError,
    Tick, TimeSignature,
};
pub use warp::{
    ClipWarp, ENGINE_MAX_SPEED, GROOVE_SLOT, GROOVE_SLOTS, Groove, LoopRegion, MAX_CLIP_BPM,
    MAX_GRID_OFFSET, MAX_WARP_SPEED, MIN_CLIP_BPM, MIN_LOOP_BEATS, MIN_LOOP_SECONDS,
    MIN_WARP_SPEED, PiecewiseLinear, QUANTISE_GRIDS, TempoCurve, TempoPoint, WarpAxis, WarpCurve,
    WarpId, WarpMarker, WarpMode, WarpQuantise,
};
