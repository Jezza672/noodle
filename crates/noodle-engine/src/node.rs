//! The node API: how a node type describes itself to the compiler and the UI,
//! and the traits its instances implement.
//!
//! A node type comes in two halves:
//!
//! - A [`NodeType`]: one shared value per type, kept in the
//!   [`Registry`](crate::Registry). Given a node's [`Config`], it reports the
//!   node's ports ([`Layout`]) and output shapes, and creates instances.
//! - Instances: one per node in the graph, created off the audio thread. Most
//!   nodes implement [`LaneKernel`](crate::LaneKernel) and are wrapped in
//!   [`PerLane`](crate::PerLane). Nodes that work across voices or channels
//!   implement [`Node`] directly, and offline nodes implement
//!   [`OfflineNode`](crate::OfflineNode).
//!
//! Make every setting a parameter, so it can be modulated, unless it changes
//! the node's ports or output shapes. Only those settings are config (such as
//! Mix's number of inputs), and changing one rebuilds the node.
//!
//! Ports are referred to by position, so a node's port-index constants must
//! match the order its layout declares them in. `#[derive(Ports)]` generates
//! both from one declaration; nodes whose ports depend on their config build
//! a [`Layout`] by hand instead.

use std::borrow::Cow;
use std::fmt;

use noodle_core::{Config, NodeId, Value};

use crate::{Event, EventsOut, OfflineNode, ParamInfo, Shape, ShapeError, SignalIn, SignalOut};

/// Facts about a node type that never depend on config.
#[derive(Clone, Copy, Debug)]
pub struct NodeInfo {
    /// Saved in project files, so it must never change, e.g. `"noodle.osc.sine"`.
    pub id: &'static str,
    /// Bump this whenever the node's output changes for the same inputs, since
    /// it's part of cache keys.
    pub version: u32,
    pub name: &'static str,
    /// Where the node is listed in the add-node menu.
    pub category: &'static str,
}

pub trait NodeType: Send + Sync + 'static {
    fn info(&self) -> &NodeInfo;

    /// The config settings this node type reads, for the UI to show.
    fn config(&self) -> &[ConfigInfo] {
        &[]
    }

    /// True if the node keeps nothing from block to block, so replacing it
    /// with a rebuilt copy (after a config change that leaves its ports and
    /// shapes alone) changes only what it computes, like moving a parameter,
    /// and the swap needs no fade. A node with any state (a phase, a filter,
    /// a delay line) must leave this false.
    fn stateless(&self) -> bool {
        false
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError>;

    /// The key of a signal input that the node reads only *after* it has
    /// written its outputs, so a block's outputs don't depend on that
    /// block's value of this input. This is what lets a wire go from the
    /// node's outputs round to that input: a feedback loop.
    ///
    /// A node that returns a key here must implement
    /// [`Node::process_output`] and [`Node::process_input`]. The compiler
    /// splits it into those two steps only when it lies on a loop; otherwise
    /// it runs whole, through [`Node::process`], which must do the same as
    /// the two in a row. Every *other* input, and the event ports, belong to
    /// `process_output`, so wires into them can't close a loop.
    fn loop_input(&self, _config: &Config) -> Option<&'static str> {
        None
    }

    /// The shapes of the outputs, given the shapes of the signal inputs
    /// (unconnected inputs are [`Shape::MONO`]). By default every output gets
    /// the broadcast of all the inputs.
    fn output_shapes(
        &self,
        _config: &Config,
        layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let shape = Shape::broadcast_all(inputs.iter().copied())?;
        Ok(vec![shape; layout.outputs.len()])
    }

    /// Runs off the audio thread, so it may allocate.
    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError>;
}

/// A node's ports and processing mode for a particular config.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Audio and parameter inputs, in the order nodes index them.
    pub inputs: Vec<InputPort>,
    pub outputs: Vec<Port>,
    pub event_inputs: Vec<Port>,
    pub event_outputs: Vec<Port>,
    pub mode: Mode,
    /// False if identical inputs can give different output, e.g. unseeded
    /// randomness. Nondeterministic nodes are never cached.
    pub deterministic: bool,
}

/// Port keys are saved in project files to identify connections, so they must
/// stay stable. Names are only for display.
#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub key: Cow<'static, str>,
    pub name: Cow<'static, str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputPort {
    pub key: Cow<'static, str>,
    pub name: Cow<'static, str>,
    pub kind: InputKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    /// Silent when unconnected.
    Audio,
    Param(ParamInfo),
}

impl InputPort {
    /// The value the input holds when nothing is connected to it.
    pub fn default_value(&self) -> f32 {
        match &self.kind {
            InputKind::Audio => 0.0,
            InputKind::Param(param) => param.default,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Processes one block at a time on the audio thread.
    Realtime,
    /// Needs its whole input before it can produce output, so it only runs in
    /// the offline renderer and its output is always cached.
    Offline,
}

impl Layout {
    pub fn realtime() -> Self {
        Self::with_mode(Mode::Realtime)
    }

    pub fn offline() -> Self {
        Self::with_mode(Mode::Offline)
    }

    fn with_mode(mode: Mode) -> Self {
        Self {
            inputs: Vec::new(),
            outputs: Vec::new(),
            event_inputs: Vec::new(),
            event_outputs: Vec::new(),
            mode,
            deterministic: true,
        }
    }

    pub fn input(
        mut self,
        key: impl Into<Cow<'static, str>>,
        name: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.inputs.push(InputPort {
            key: key.into(),
            name: name.into(),
            kind: InputKind::Audio,
        });
        self
    }

    pub fn param(
        mut self,
        key: impl Into<Cow<'static, str>>,
        name: impl Into<Cow<'static, str>>,
        info: ParamInfo,
    ) -> Self {
        self.inputs.push(InputPort {
            key: key.into(),
            name: name.into(),
            kind: InputKind::Param(info),
        });
        self
    }

    pub fn output(
        mut self,
        key: impl Into<Cow<'static, str>>,
        name: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.outputs.push(port(key, name));
        self
    }

    pub fn event_input(
        mut self,
        key: impl Into<Cow<'static, str>>,
        name: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.event_inputs.push(port(key, name));
        self
    }

    pub fn event_output(
        mut self,
        key: impl Into<Cow<'static, str>>,
        name: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.event_outputs.push(port(key, name));
        self
    }

    pub fn nondeterministic(self) -> Self {
        Self {
            deterministic: false,
            ..self
        }
    }
}

fn port(key: impl Into<Cow<'static, str>>, name: impl Into<Cow<'static, str>>) -> Port {
    Port {
        key: key.into(),
        name: name.into(),
    }
}

/// Describes one config setting.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigInfo {
    pub key: &'static str,
    pub name: &'static str,
    pub default: Value,
}

impl ConfigInfo {
    pub const fn int(key: &'static str, name: &'static str, default: i64) -> Self {
        Self {
            key,
            name,
            default: Value::Int(default),
        }
    }

    /// A text setting that is empty by default.
    pub const fn text(key: &'static str, name: &'static str) -> Self {
        Self {
            key,
            name,
            default: Value::Text(String::new()),
        }
    }

    /// Reads this setting from `config` as text, falling back to the default
    /// if it's missing or has the wrong type.
    pub fn get_text(&self, config: &Config) -> String {
        match (config.get(self.key), &self.default) {
            (Some(Value::Text(text)), _) | (_, Value::Text(text)) => text.clone(),
            _ => String::new(),
        }
    }

    /// Reads this setting from `config`, falling back to the default if it's
    /// missing or has the wrong type.
    pub fn get_int(&self, config: &Config) -> i64 {
        config
            .get(self.key)
            .and_then(Value::as_int)
            .or(self.default.as_int())
            .unwrap_or_default()
    }
}

/// Everything known about a node when it's instantiated.
#[derive(Clone, Copy, Debug)]
pub struct Setup<'a> {
    /// The node's ID in the project graph, e.g. for keying its
    /// [`Telemetry`](crate::Telemetry) channels. A test harness uses
    /// `NodeId(0)`.
    pub node: NodeId,
    pub config: &'a Config,
    pub sample_rate: f32,
    /// The most frames any block will have.
    pub max_frames: usize,
    pub input_shapes: &'a [Shape],
    pub output_shapes: &'a [Shape],
    /// A seed for anything random. It's the same for this node on every run,
    /// so renders repeat exactly (and can be cached), but different for every
    /// node, so two noise sources aren't identical.
    pub seed: u64,
}

pub enum Instance {
    Realtime(Box<dyn Node>),
    Offline(Box<dyn OfflineNode>),
}

impl Instance {
    pub fn realtime(node: impl Node) -> Self {
        Self::Realtime(Box::new(node))
    }

    pub fn offline(node: impl OfflineNode) -> Self {
        Self::Offline(Box::new(node))
    }

    pub fn mode(&self) -> Mode {
        match self {
            Self::Realtime(_) => Mode::Realtime,
            Self::Offline(_) => Mode::Offline,
        }
    }
}

/// An instance of a real-time node.
///
/// Both methods run on the audio thread and must follow the real-time rules
/// in `docs/ARCHITECTURE.md`: no allocation, locks, I/O or unbounded loops.
pub trait Node: Send + 'static {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>);

    /// First half of [`process`](Self::process) for a node that names a
    /// [`NodeType::loop_input`], when the node is on a feedback loop. Writes
    /// every output. Every input but the loop input is real; the loop input
    /// is an empty placeholder, since nothing has produced it yet.
    fn process_output(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        for output in io.outputs {
            output.fill(0.0);
        }
    }

    /// Second half, run once the loop input has been produced. Only the loop
    /// input is real, and there are no outputs.
    fn process_input(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}

    /// Clears internal state such as filter memory, e.g. when the transport
    /// jumps.
    fn reset(&mut self) {}
}

#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub sample_rate: f32,
    /// Frames in this block. In an offline render, the length of the whole
    /// range.
    pub frames: usize,
    pub transport: Transport,
}

/// Where the timeline is at the start of a block. Without a timeline (a live
/// patch), `position` just counts samples and the musical fields stay at
/// their defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transport {
    /// Whether the transport is playing. A stopped transport holds its
    /// position, and the graph keeps rendering.
    pub playing: bool,
    /// Timeline position of the block's first frame, in samples.
    pub position: u64,
    /// The loop's start and end in samples, while looping is on (whether or
    /// not the transport is playing). A block never crosses the end: the position wraps to the start at the
    /// block boundary. Nodes that read ahead (clip streams) use it to have
    /// the audio at the loop's start ready before the wrap.
    pub loop_range: Option<(u64, u64)>,
    /// The same position in ticks, 960 to a quarter note, fractions included.
    pub tick: f64,
    /// The tempo at the block's start, in quarter notes per minute. A tempo
    /// change inside the block reaches nodes at the next block.
    pub bpm: f64,
    /// The time signature at the block's start.
    pub signature: noodle_core::TimeSignature,
}

impl Default for Transport {
    /// Stopped at the start, at 120 beats per minute in 4/4.
    fn default() -> Self {
        Self {
            playing: false,
            position: 0,
            loop_range: None,
            tick: 0.0,
            bpm: 120.0,
            signature: noodle_core::TimeSignature::COMMON,
        }
    }
}

/// A node's inputs and outputs for one block, indexed in [`Layout`] order.
///
/// Inputs broadcast (see [`SignalIn::lane`]). Outputs have exactly the shapes
/// from [`NodeType::output_shapes`]. Inputs and outputs never alias. Outputs
/// aren't cleared beforehand, so a node must write every sample of every
/// output.
pub struct Io<'a, 'b> {
    pub inputs: &'a [SignalIn<'a>],
    pub outputs: &'a mut [SignalOut<'b>],
    pub event_inputs: &'a [&'a [Event]],
    pub event_outputs: &'a mut [EventsOut<'b>],
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeError {
    Shape(ShapeError),
    Config(Cow<'static, str>),
}

impl NodeError {
    pub fn config(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Config(message.into())
    }
}

impl From<ShapeError> for NodeError {
    fn from(error: ShapeError) -> Self {
        Self::Shape(error)
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape(error) => error.fmt(f),
            Self::Config(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for NodeError {}
