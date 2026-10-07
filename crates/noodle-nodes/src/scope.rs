use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ScopeWriter, Setup,
    Telemetry,
};

use crate::meter::voice_sum;

/// Sends its input's waveform, per channel, to the UI through [`Telemetry`].
/// Voices are summed first, as the Output node does.
pub struct Scope {
    telemetry: Telemetry,
}

impl Scope {
    pub fn new(telemetry: &Telemetry) -> Self {
        Self {
            telemetry: telemetry.clone(),
        }
    }
}

const IN: usize = 0;

/// How much the scope buffers for the UI, which reads it every frame.
const BUFFER_SECONDS: f32 = 1.0;

static INFO: NodeInfo = NodeInfo {
    id: "noodle.view.scope",
    version: 1,
    name: "Scope",
    category: "Views",
};

impl NodeType for Scope {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let capacity = ((BUFFER_SECONDS * setup.sample_rate) as usize).max(setup.max_frames);
        let channels = setup.input_shapes[IN].channels;
        Ok(Instance::realtime(ScopeNode {
            writer: self.telemetry.open_scope(setup.node, channels, capacity),
        }))
    }
}

struct ScopeNode {
    writer: ScopeWriter,
}

impl Node for ScopeNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let input = &io.inputs[IN];
        self.writer.write(ctx.frames, |frame, channel| {
            voice_sum(input, channel, frame)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{NodeId, ScopeView, Shape};

    #[test]
    fn sends_frames_with_voices_summed() {
        let telemetry = Telemetry::new();
        let mut h = Harness::new(
            &Scope::new(&telemetry),
            &Config::new(),
            &[(IN, Shape::new(2, 2))],
            48_000.0,
            3,
        )
        .unwrap();
        let mut input = h.input(IN, 3);
        input.lane_mut(0, 0).copy_from_slice(&[1.0, 2.0, 3.0]);
        input.lane_mut(1, 0).copy_from_slice(&[10.0, 20.0, 30.0]);
        input.lane_mut(1, 1).copy_from_slice(&[-1.0, -2.0, -3.0]);
        h.run(3).unwrap();

        let mut view = ScopeView::default();
        assert!(telemetry.read_scope(NodeId(0), &mut view));
        assert_eq!(view.samples(), [11.0, -1.0, 22.0, -2.0, 33.0, -3.0]);
    }
}
