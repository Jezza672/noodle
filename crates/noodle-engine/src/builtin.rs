//! Node types the engine itself provides, because the executor treats them
//! specially.

use noodle_core::Config;

use crate::{
    ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Registry,
    Setup, Shape,
};

pub const OUTPUT_ID: &str = "noodle.io.output";
pub const INPUT_ID: &str = "noodle.io.input";

/// Sends its input to the audio device. Every Output node's input is mixed
/// into the device output: a mono signal goes to every channel, and voices are
/// summed.
pub struct Output;

static OUTPUT: NodeInfo = NodeInfo {
    id: OUTPUT_ID,
    version: 1,
    name: "Output",
    category: "Input/Output",
};

impl NodeType for Output {
    fn info(&self) -> &NodeInfo {
        &OUTPUT
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(Passive))
    }
}

/// Delivers audio from the input device. Its output has `channels` channels
/// (config, default 2): channel n is the device's channel n, a mono device
/// feeds every channel, and channels the device doesn't have are silent.
/// With no input device, as when rendering offline, it's silent.
pub struct Input;

static INPUT: NodeInfo = NodeInfo {
    id: INPUT_ID,
    version: 1,
    name: "Input",
    category: "Input/Output",
};

/// Channels on an Input node's output. Config, since it's the output's shape.
pub const INPUT_CHANNELS: ConfigInfo = ConfigInfo::int("channels", "Channels", 2);

/// The most channels an Input node can have.
pub const MAX_INPUT_CHANNELS: usize = 64;

impl Input {
    fn channels(config: &Config) -> Result<usize, NodeError> {
        let channels = INPUT_CHANNELS.get_int(config);
        match usize::try_from(channels) {
            Ok(n @ 1..=MAX_INPUT_CHANNELS) => Ok(n),
            _ => Err(NodeError::config(format!(
                "an input needs 1 to {MAX_INPUT_CHANNELS} channels, not {channels}"
            ))),
        }
    }
}

impl NodeType for Input {
    fn info(&self) -> &NodeInfo {
        &INPUT
    }

    fn config(&self) -> &[ConfigInfo] {
        static CONFIG: [ConfigInfo; 1] = [INPUT_CHANNELS];
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        Self::channels(config)?;
        // Live input isn't a function of the timeline, so it's never cached.
        Ok(Layout::realtime().output("out", "Out").nondeterministic())
    }

    fn output_shapes(
        &self,
        config: &Config,
        _layout: &Layout,
        _inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(vec![Shape::new(1, Self::channels(config)?)])
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(Passive))
    }
}

/// Does nothing. The executor reads the Output node's input, and writes the
/// Input node's output, itself.
struct Passive;

impl Node for Passive {
    fn process(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}
}

impl Registry {
    /// A registry holding the engine's own node types.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(Output);
        registry.register(Input);
        registry
    }
}
