//! The Sample node: a sample for other nodes to play.

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Ports,
    Setup,
};

/// Holds a sample, or a window of one, and puts it on its `sample` output, a
/// wire that carries a reference to the audio rather than the audio. Wire it
/// to a Sampler's sample input, in the same group or across a group's
/// boundary, to choose what the Sampler plays.
///
/// Dragging a clip from the timeline into the graph makes one, holding the
/// part of the clip's file the clip plays. It is a snapshot (the audio's
/// content hash and the window), not a link: the clip can move or change
/// afterwards and the node keeps the sound it took.
///
/// The `sample` option is a [[`SampleRef`]: the hash of the audio, or
/// `hash:start:end` with the window as fractions of it. It makes no sound by
/// itself and does no work while the engine runs; the compiler reads it.
pub struct SampleInput;

pub const SAMPLE_INPUT_ID: &str = "noodle.instrument.sample";

static CONFIG: [ConfigInfo; 1] = [ConfigInfo::text(crate::SAMPLE_KEY, "Sample")];

#[derive(Ports)]
struct SampleInputPorts {
    #[sample_output("sample", "Sample")]
    sample: (),
}

static INFO: NodeInfo = NodeInfo {
    id: SAMPLE_INPUT_ID,
    version: 1,
    name: "Sample",
    category: "Instruments",
};

impl NodeType for SampleInput {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(SampleInputPorts::layout())
    }

    fn sample_output(&self, config: &Config, _key: &str) -> Option<String> {
        let text = CONFIG[0].get_text(config);
        (!text.is_empty()).then_some(text)
    }

    fn stateless(&self) -> bool {
        true
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(SampleInputNode))
    }
}

struct SampleInputNode;

impl Node for SampleInputNode {
    fn process(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_puts_its_option_on_the_wire_and_nothing_when_empty() {
        let config = Config::new().with(
            crate::SAMPLE_KEY,
            noodle_core::Value::Text("abc:0.25:0.5".into()),
        );
        assert_eq!(
            SampleInput.sample_output(&config, "sample").as_deref(),
            Some("abc:0.25:0.5")
        );
        assert_eq!(SampleInput.sample_output(&Config::new(), "sample"), None);
    }
}
