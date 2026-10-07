use noodle_engine::{
    Cancelled, Config, Context, Instance, Io, Layout, NodeError, NodeInfo, NodeType, OfflineNode,
    Progress, Setup,
};

/// Plays its input backwards. It needs the end of its input before it can
/// produce the start of its output, so it's an offline node and its output is
/// always a cached render.
pub struct Reverse;

const IN: usize = 0;
const OUT: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: "noodle.offline.reverse",
    version: 1,
    name: "Reverse",
    category: "Offline",
};

impl NodeType for Reverse {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::offline().input("in", "In").output("out", "Out"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::offline(ReverseNode))
    }
}

struct ReverseNode;

impl OfflineNode for ReverseNode {
    fn render(
        &mut self,
        _ctx: &Context,
        io: Io<'_, '_>,
        progress: &Progress,
    ) -> Result<(), Cancelled> {
        let input = io.inputs[IN];
        let out = &mut io.outputs[OUT];
        let shape = out.shape();
        for lane in 0..shape.lanes() {
            let (voice, channel) = (lane / shape.channels, lane % shape.channels);
            let samples = out.lane_mut(voice, channel);
            samples.copy_from_slice(input.lane(voice, channel));
            samples.reverse();
            progress.report((lane + 1) as f32 / shape.lanes() as f32)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    #[test]
    fn reverses_the_whole_range() {
        let mut h =
            Harness::new(&Reverse, &Config::new(), &[(IN, Shape::MONO)], 48_000.0, 4).unwrap();
        h.input(IN, 4)
            .lane_mut(0, 0)
            .copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        h.run(4).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[4.0, 3.0, 2.0, 1.0]);
    }
}
