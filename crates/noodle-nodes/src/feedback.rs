//! The Feedback node.

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Ports, Setup,
    Shape, SignalIn,
};

/// Delays its input by exactly one sample, and closes loops. Wire a signal
/// through it back to its own source (an oscillator's output into its own
/// frequency for feedback FM, a filter built from integrators) and the loop
/// is legal: the output of a sample is the input of the sample before.
///
/// A loop that passes through a Feedback node runs one sample at a time (see
/// [`Node::loop_chunk`]), so every node on the loop is called once per sample.
/// That costs a few percent of a core at 48 kHz for each node on it, so keep
/// such loops small. A loop that also has a [`Delay`](crate::Delay) in it runs
/// in chunks as long as the shortest of the two allows, which is one sample.
///
/// Outside a loop it is a plain one-sample delay. In a loop that can't run in
/// chunks (it has a plugin or a spectral node in it) it is held to a block,
/// like a Delay.
pub struct Feedback;

pub const FEEDBACK_ID: &str = "noodle.util.feedback";

#[derive(Ports)]
#[ports(bounded = 1)]
struct FeedbackPorts {
    #[input("in", "In")]
    input: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = FeedbackPorts::INPUT;

static INFO: NodeInfo = NodeInfo {
    id: FEEDBACK_ID,
    version: 1,
    name: "Feedback",
    category: "Utilities",
};

impl NodeType for Feedback {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(FeedbackPorts::layout())
    }

    fn loop_input(&self, _config: &Config) -> Option<&'static str> {
        Some("in")
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let shape = setup.output_shapes[0];
        Ok(Instance::realtime(FeedbackNode {
            shape,
            held: vec![0.0; shape.lanes()],
            block: (0..shape.lanes())
                .map(|_| vec![0.0; setup.max_frames].into_boxed_slice())
                .collect(),
            block_len: 0,
        }))
    }
}

struct FeedbackNode {
    shape: Shape,
    /// Per lane, the last sample of input: the next sample's output.
    held: Vec<f32>,
    /// Per lane, the input of the last call, for a loop that runs whole
    /// blocks: the output is then that block, a block late.
    block: Vec<Box<[f32]>>,
    block_len: usize,
}

impl FeedbackNode {
    fn lane_of(&self, lane: usize) -> (usize, usize) {
        (lane / self.shape.channels, lane % self.shape.channels)
    }

    fn remember(&mut self, input: &SignalIn<'_>, frames: usize) {
        for lane in 0..self.shape.lanes() {
            let (voice, channel) = self.lane_of(lane);
            let samples = input.lane(voice, channel);
            self.block[lane][..frames].copy_from_slice(samples);
            self.held[lane] = samples.last().copied().unwrap_or(0.0);
        }
        self.block_len = frames;
    }
}

impl Node for FeedbackNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let input = &io.inputs[IN];
        for lane in 0..self.shape.lanes() {
            let (voice, channel) = self.lane_of(lane);
            let samples = input.lane(voice, channel);
            let out = io.outputs[0].lane_mut(voice, channel);
            out[0] = self.held[lane];
            out[1..ctx.frames].copy_from_slice(&samples[..ctx.frames - 1]);
        }
        self.remember(input, ctx.frames);
    }

    fn process_output(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let out = &mut io.outputs[0];
        for lane in 0..self.shape.lanes() {
            let (voice, channel) = self.lane_of(lane);
            let frames = out.frames();
            let out = out.lane_mut(voice, channel);
            if frames == 1 {
                out[0] = self.held[lane];
            } else {
                // Held to whole blocks: the last block, a block late.
                for (k, sample) in out.iter_mut().enumerate() {
                    *sample = if k < self.block_len {
                        self.block[lane][k]
                    } else {
                        0.0
                    };
                }
            }
        }
    }

    fn process_input(&mut self, ctx: &Context, io: Io<'_, '_>) {
        self.remember(&io.inputs[IN], ctx.frames);
    }

    fn loop_chunk(&mut self, ctx: &Context, _inputs: &[SignalIn<'_>], held: bool) -> usize {
        if held { ctx.frames } else { 1 }
    }

    fn reset(&mut self) {
        self.held.fill(0.0);
        self.block_len = 0;
    }
}
