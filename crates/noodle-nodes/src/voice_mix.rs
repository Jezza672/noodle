use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Setup, Shape,
};

/// Sums the voices of a polyphonic signal, keeping its channels. It works
/// across lanes, so it implements [`Node`] directly instead of using a lane
/// kernel.
pub struct VoiceMix;

const IN: usize = 0;
const OUT: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: "noodle.poly.voice_mix",
    version: 1,
    name: "Voice Mix",
    category: "Polyphony",
};

impl NodeType for VoiceMix {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In").output("out", "Out"))
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(vec![Shape::new(1, inputs[IN].channels)])
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(VoiceMixNode))
    }
}

struct VoiceMixNode;

impl Node for VoiceMixNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let input = io.inputs[IN];
        let out = &mut io.outputs[OUT];
        for channel in 0..out.shape().channels {
            let lane = out.lane_mut(0, channel);
            lane.fill(0.0);
            for voice in 0..input.shape().voices {
                for (o, x) in lane.iter_mut().zip(input.lane(voice, channel)) {
                    *o += x;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    #[test]
    fn sums_voices_and_keeps_channels() {
        let shape = Shape::new(3, 2);
        let mut h = Harness::new(&VoiceMix, &Config::new(), &[(IN, shape)], 48_000.0, 4).unwrap();
        let mut input = h.input(IN, 4);
        for voice in 0..3 {
            input.lane_mut(voice, 0).fill(1.0);
            input.lane_mut(voice, 1).fill(voice as f32);
        }
        h.run(4).unwrap();

        let out = h.output(OUT);
        assert_eq!(out.shape(), Shape::STEREO);
        assert_eq!(out.lane(0, 0), &[3.0; 4]);
        assert_eq!(out.lane(0, 1), &[3.0; 4]); // 0 + 1 + 2
    }
}
