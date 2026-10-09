use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Setup, Shape,
    Telemetry,
};

use crate::meter::{LevelProbe, voice_sum_out};

/// Sums the voices of a polyphonic signal, keeping its channels. It works
/// across lanes, so it implements [`Node`] directly instead of using a lane
/// kernel. It reports its output's level through [`Telemetry`], for the meter
/// drawn on the node.
pub struct VoiceMix {
    telemetry: Telemetry,
}

impl VoiceMix {
    pub fn new(telemetry: &Telemetry) -> Self {
        Self {
            telemetry: telemetry.clone(),
        }
    }
}

/// The Voice Mix's type ID.
pub const VOICE_MIX_ID: &str = "noodle.poly.voice_mix";

const IN: usize = 0;
const OUT: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: VOICE_MIX_ID,
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

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let channels = setup.output_shapes[OUT].channels;
        Ok(Instance::realtime(VoiceMixNode {
            probe: LevelProbe::new(&self.telemetry, setup.node, channels, setup.sample_rate),
        }))
    }
}

struct VoiceMixNode {
    probe: LevelProbe,
}

impl Node for VoiceMixNode {
    fn reset(&mut self) {
        self.probe.reset();
    }

    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
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
        for channel in 0..self.probe.slots() {
            self.probe.measure(channel, ctx.frames, |frame| {
                let x = voice_sum_out(out, channel, frame);
                (x * x, x.abs())
            });
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
        let mut h = Harness::new(
            &VoiceMix::new(&Telemetry::new()),
            &Config::new(),
            &[(IN, shape)],
            48_000.0,
            4,
        )
        .unwrap();
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
