//! The Host Parameter node: one slot of the host's parameter bank on a wire.

use std::sync::Arc;

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Setup,
};
use noodle_io::{HOST_PARAMS, HostLink};

pub const HOST_PARAM_ID: &str = "noodle.io.host_param";

/// The config key of the slot a Host Parameter node reads: 1 to
/// [`HOST_PARAMS`].
pub const HOST_PARAM_SLOT_KEY: &str = "slot";

/// The slot a Host Parameter node reads, counting from 1.
pub const HOST_PARAM_SLOT: ConfigInfo = ConfigInfo::int(HOST_PARAM_SLOT_KEY, "Slot", 1);

/// The config key of the name the host shows the parameter by.
pub const HOST_PARAM_NAME_KEY: &str = "name";

/// The name the host shows for the slot. Empty gives "Param n".
pub const HOST_PARAM_NAME: ConfigInfo = ConfigInfo::text(HOST_PARAM_NAME_KEY, "Name");

static INFO: NodeInfo = NodeInfo {
    id: HOST_PARAM_ID,
    version: 1,
    name: "Host Param",
    category: "Input/Output",
};

/// A parameter of the host's, 0 to 1, as a signal: the host's knob or
/// automation lane for this plugin instance. A plugin instance gives its host
/// a fixed bank of [`HOST_PARAMS`] parameters, and this node says which one
/// it is (`slot`, counting from 1) and what to call it (`name`); wire its
/// output into what it should control. Nothing moves without a wire, so the
/// graph shows what the host's knobs do.
///
/// The value changes at the frame the host's automation says, as far as the
/// engine's blocks go (a change cuts the block it falls in). In the desktop
/// app, with no host, it stays at 0, or wherever its slot was last set.
///
/// Two nodes on one slot show the same value; the host sees one parameter,
/// named by the node with the lowest ID.
pub struct HostParam {
    link: Arc<HostLink>,
}

impl HostParam {
    pub fn new(link: &Arc<HostLink>) -> Self {
        Self {
            link: Arc::clone(link),
        }
    }

    fn slot(config: &Config) -> Result<usize, NodeError> {
        let slot = HOST_PARAM_SLOT.get_int(config);
        match usize::try_from(slot) {
            Ok(n @ 1..=HOST_PARAMS) => Ok(n - 1),
            _ => Err(NodeError::config(format!(
                "a host parameter's slot is 1 to {HOST_PARAMS}, not {slot}"
            ))),
        }
    }
}

impl NodeType for HostParam {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        static CONFIG: [ConfigInfo; 2] = [HOST_PARAM_SLOT, HOST_PARAM_NAME];
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        Self::slot(config)?;
        // The host moves it, not the timeline, so it is never cached.
        Ok(Layout::realtime()
            .control_output("value", "Value")
            .nondeterministic())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(HostParamNode {
            link: Arc::clone(&self.link),
            slot: Self::slot(setup.config)?,
        }))
    }
}

struct HostParamNode {
    link: Arc<HostLink>,
    slot: usize,
}

impl Node for HostParamNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let value = self.link.value(self.slot);
        for voice in 0..io.outputs[0].shape().voices {
            io.outputs[0].lane_mut(voice, 0).fill(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;

    fn run(link: &Arc<HostLink>, slot: i64) -> f32 {
        let mut config = Config::new();
        config.set(HOST_PARAM_SLOT_KEY, Value::Int(slot));
        let mut h = Harness::new(&HostParam::new(link), &config, &[], 48_000.0, 64).unwrap();
        h.run(64).unwrap();
        h.output(0).lane(0, 0)[63]
    }

    #[test]
    fn it_outputs_the_value_of_its_slot() {
        let link = HostLink::new();
        link.set_value(0, 0.25);
        link.set_value(63, 0.75);
        assert_eq!(run(&link, 1), 0.25);
        assert_eq!(run(&link, 64), 0.75);
        assert_eq!(run(&link, 2), 0.0);
    }

    #[test]
    fn a_slot_outside_the_bank_is_refused() {
        let link = HostLink::new();
        for slot in [0, 65, -1] {
            let mut config = Config::new();
            config.set(HOST_PARAM_SLOT_KEY, Value::Int(slot));
            assert!(Harness::new(&HostParam::new(&link), &config, &[], 48_000.0, 64).is_err());
        }
    }
}
