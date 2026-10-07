//! Node types the engine itself provides, because the executor treats them
//! specially.

use noodle_core::Config;

use crate::{Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Registry, Setup};

pub const OUTPUT_ID: &str = "noodle.io.output";

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

/// Does nothing. The executor reads the Output node's input itself.
struct Passive;

impl Node for Passive {
    fn process(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}
}

impl Registry {
    /// A registry holding the engine's own node types.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(Output);
        registry
    }
}
