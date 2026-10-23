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

use crate::{
    Event, EventsOut, OfflineNode, ParamInfo, Shape, ShapeError, SignalIn, SignalOut, SpectralIn,
    SpectralOut,
};

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

    /// The config keys that reach a running instance through
    /// [`retune`](Self::retune) instead of by rebuilding it. A change to only
    /// these leaves the instance in place, with its state (a sounding note,
    /// say), so a drag that sets one many times a second doesn't cut it off.
    fn live_config(&self) -> &[&'static str] {
        &[]
    }

    /// Tells the node's running instances the node's config, whenever a plan
    /// is built that keeps them. Runs off the audio thread. A node type that
    /// lists [`live_config`](Self::live_config) keys passes them on here, to
    /// the instances it has made for `node`; an instance made later reads its
    /// config when it is instantiated.
    fn retune(&self, _node: NodeId, _config: &Config) {}

    /// True if the node does something besides turn its inputs into its
    /// outputs, such as reporting to the UI, reading a device or hosting a
    /// plugin, so the engine runs it even when nothing downstream of it is
    /// listening. Every other node is only run while a path from it leads to
    /// an Output node (see "Silence skipping" in the architecture notes).
    fn always_runs(&self) -> bool {
        false
    }

    /// The config a newly added node of this type starts with, for settings
    /// whose first value isn't the same as leaving them out. Whatever adds a
    /// node from the UI (the add-node search, the plugin browser) applies it.
    fn initial_config(&self) -> Config {
        Config::new()
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

    /// Mixes into `key` whatever this node's output depends on besides its
    /// config, its inputs and the tempo map, which are keyed already. A track
    /// input adds the clips it plays, with the contents of their files. An
    /// error says the output can't be cached, e.g. a file that can't be read.
    fn cache_inputs(
        &self,
        _node: NodeId,
        _config: &Config,
        _env: &crate::CacheEnv<'_>,
        _key: &mut noodle_core::KeyBuilder,
    ) -> Result<(), crate::Uncacheable> {
        Ok(())
    }

    /// The sample a sample output port carries, as the text its receiver is
    /// given: a content hash, optionally followed by `:start:end`, a window
    /// as fractions of the sample (see `noodle_nodes::SampleRef`). The
    /// compiler writes it into the config of the node wired to the port, under
    /// the receiving sample input's key, in place of whatever was set there.
    /// `None` is no sample, which the receiver treats as silence.
    fn sample_output(&self, _config: &Config, _key: &str) -> Option<String> {
        None
    }

    /// The shapes of the outputs, given the shapes of the signal inputs
    /// (unconnected inputs are [`Shape::MONO`]). By default every output gets
    /// the broadcast of all the inputs.
    ///
    /// A node with spectral ports gets the shapes of its spectral inputs
    /// after those of its signal inputs, and returns the shapes of its
    /// spectral outputs after those of its signal outputs. The default
    /// therefore gives a spectral output the shape of the audio it came
    /// from, and an audio output the shape of the spectrum it plays.
    fn output_shapes(
        &self,
        _config: &Config,
        layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let shape = Shape::broadcast_all(inputs.iter().copied())?;
        Ok(vec![
            shape;
            layout.outputs.len() + layout.spectral_outputs.len()
        ])
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
    /// Spectral (FFT frame) ports; see [`crate::spectral`](crate::Analyzer).
    pub spectral_inputs: Vec<Port>,
    pub spectral_outputs: Vec<Port>,
    /// Sample ports carry a reference to a sample, not signal. The compiler
    /// resolves them before scheduling (see [`NodeType::sample_output`]), so
    /// they have no buffers and no place in the shapes.
    pub sample_inputs: Vec<Port>,
    pub sample_outputs: Vec<Port>,
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
    /// The most parameters a node shows on itself by default once it has
    /// more than [`Self::MANY_PORTS`] inputs or outputs.
    pub const DEFAULT_SHOWN: usize = 4;
    /// A node with more inputs or more outputs than this shows only some of
    /// its parameters by default.
    pub const MANY_PORTS: usize = 4;

    /// Whether the parameter `key` is drawn on the node in the graph, given
    /// the node's own choices (`chosen`, by key). A parameter nobody chose
    /// for shows by default on a node with few ports; on one with many it
    /// shows if the type marks it [`featured`](ParamInfo::featured), or, when
    /// the type marks none, if it is one of the first few. Audio and event
    /// ports always show.
    pub fn param_shown(
        &self,
        chosen: &std::collections::BTreeMap<String, bool>,
        key: &str,
    ) -> bool {
        if let Some(&shown) = chosen.get(key) {
            return shown;
        }
        let many = self.inputs.len() + self.event_inputs.len() + self.spectral_inputs.len()
            > Self::MANY_PORTS
            || self.outputs.len() + self.event_outputs.len() + self.spectral_outputs.len()
                > Self::MANY_PORTS;
        if !many {
            return true;
        }
        let params = || {
            self.inputs.iter().filter_map(|input| match &input.kind {
                InputKind::Param(info) => Some((input.key.as_ref(), info)),
                InputKind::Audio => None,
            })
        };
        if params().any(|(_, info)| info.featured) {
            return params().any(|(k, info)| k == key && info.featured);
        }
        params().take(Self::DEFAULT_SHOWN).any(|(k, _)| k == key)
    }

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
            spectral_inputs: Vec::new(),
            spectral_outputs: Vec::new(),
            sample_inputs: Vec::new(),
            sample_outputs: Vec::new(),
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

    pub fn spectral_input(
        mut self,
        key: impl Into<Cow<'static, str>>,
        name: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.spectral_inputs.push(port(key, name));
        self
    }

    pub fn spectral_output(
        mut self,
        key: impl Into<Cow<'static, str>>,
        name: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.spectral_outputs.push(port(key, name));
        self
    }

    pub fn sample_input(
        mut self,
        key: impl Into<Cow<'static, str>>,
        name: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.sample_inputs.push(port(key, name));
        self
    }

    pub fn sample_output(
        mut self,
        key: impl Into<Cow<'static, str>>,
        name: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.sample_outputs.push(port(key, name));
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
    /// The shapes of the signal inputs, then those of the spectral inputs.
    pub input_shapes: &'a [Shape],
    /// The shapes of the signal outputs, then those of the spectral outputs.
    pub output_shapes: &'a [Shape],
    /// A seed for anything random. It's the same for this node on every run,
    /// so renders repeat exactly (and can be cached), but different for every
    /// node, so two noise sources aren't identical.
    pub seed: u64,
    /// Where the transport's playhead is, in samples, as of the compile. A
    /// node built mid-playback (a cached render swapped in, say) can use it
    /// to have the audio at the playhead ready before its first block. It is
    /// 0 for offline renders, which run from the start.
    pub position: u64,
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

    /// How many samples later than its inputs the node's outputs sound, as
    /// reported by a plugin or caused by lookahead. The engine delays the
    /// node's siblings to match (plugin delay compensation), so parallel
    /// paths stay aligned where they meet. Read once, when the plan is
    /// built, right after the node is created; limited to [`MAX_LATENCY`].
    fn latency(&self) -> u32 {
        0
    }

    /// Clears internal state such as filter memory, e.g. when the transport
    /// jumps, or when the node starts running again after the engine stopped
    /// it because nothing downstream was listening.
    fn reset(&mut self) {}

    /// Called once when the engine stops running the node because nothing
    /// downstream is listening (see "Silence skipping" in the architecture
    /// notes). A node that reports a level to the UI writes zero here, so its
    /// meter doesn't hold the last reading.
    fn stopped(&mut self) {}

    /// Whether the node's outputs are exactly zero for this block whatever
    /// its connected inputs hold, given the inputs that are the same for the
    /// whole block (`constants[i]` is `Some` for input `i` if it is
    /// unconnected, and has settled). The engine then doesn't run the nodes
    /// that only feed it. Must be cheap and real-time safe; false is always
    /// safe.
    fn silenced_by(&self, _constants: &[Option<f32>]) -> bool {
        false
    }

    /// The value signal output `output` holds for the whole block, if it is
    /// known from the inputs that are constant for the block (`constants`, as
    /// for [`silenced_by`](Self::silenced_by)) without running the node. A
    /// node wired from it can then be [silenced](Self::silenced_by) by it
    /// as if the value were its own parameter. `None` is always safe.
    fn constant_output(&self, _output: usize, _constants: &[Option<f32>]) -> Option<f32> {
        None
    }
}

/// The most latency, in samples, the engine will compensate for a single
/// node (two seconds at 96 kHz), so a plugin reporting nonsense can't cost
/// gigabytes of delay line.
pub const MAX_LATENCY: u32 = 192_000;

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
    /// The tick where that time signature began (0 until the first change),
    /// so bars and beats count from it.
    pub signature_start: f64,
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
            signature_start: 0.0,
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
    pub spectral_inputs: &'a [SpectralIn<'a>],
    pub spectral_outputs: &'a mut [SpectralOut<'b>],
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

#[cfg(test)]
mod layout_tests {
    use std::collections::BTreeMap;

    use super::*;

    fn params(keys: &[(&str, bool)]) -> Layout {
        let mut layout = Layout::realtime().input("in", "In");
        for &(key, featured) in keys {
            let info = ParamInfo::new(0.0, 1.0, 0.0);
            layout = layout.param(
                key.to_owned(),
                key.to_owned(),
                if featured { info.featured() } else { info },
            );
        }
        layout
    }

    #[test]
    fn a_node_with_few_ports_shows_every_parameter() {
        let layout = params(&[("a", false), ("b", false), ("c", false), ("d", false)]);
        assert!(
            ["a", "b", "c", "d"]
                .iter()
                .all(|k| layout.param_shown(&BTreeMap::new(), k))
        );
    }

    #[test]
    fn a_node_with_many_ports_shows_its_featured_parameters() {
        let layout = params(&[
            ("a", false),
            ("b", true),
            ("c", false),
            ("d", true),
            ("e", false),
        ]);
        let shown = |k| layout.param_shown(&BTreeMap::new(), k);
        assert_eq!(
            [shown("a"), shown("b"), shown("c"), shown("d"), shown("e")],
            [false, true, false, true, false]
        );
    }

    #[test]
    fn with_none_featured_the_first_four_show() {
        let layout = params(&[
            ("a", false),
            ("b", false),
            ("c", false),
            ("d", false),
            ("e", false),
        ]);
        let shown = |k| layout.param_shown(&BTreeMap::new(), k);
        assert_eq!([shown("a"), shown("d"), shown("e")], [true, true, false]);
    }

    #[test]
    fn the_users_choice_beats_the_default() {
        let layout = params(&[
            ("a", false),
            ("b", false),
            ("c", false),
            ("d", false),
            ("e", false),
        ]);
        let chosen = BTreeMap::from([("a".to_owned(), false), ("e".to_owned(), true)]);
        assert!(!layout.param_shown(&chosen, "a"));
        assert!(layout.param_shown(&chosen, "e"));
    }
}
