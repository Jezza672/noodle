//! Per-lane processing: the easy way to write a node whose lanes are
//! independent.

use crate::{Context, Io, Node, Setup, Shape, SignalIn, SignalOut};

/// A node whose (voice, channel) lanes are processed independently:
/// oscillators, filters, gains, envelopes and most effects.
///
/// Wrap it in [`PerLane`] to get a [`Node`]. Each lane gets its own `State`,
/// and inputs broadcast, so a kernel never has to deal with how many voices
/// or channels there are.
pub trait LaneKernel: Send + 'static {
    /// Per-lane state, such as oscillator phase or filter memory.
    type State: Default + Send + 'static;

    /// Same real-time rules as [`Node::process`].
    fn process_lane(&mut self, state: &mut Self::State, ctx: &Context, lane: Lane<'_, '_>);
}

/// One lane's view of a node's inputs and outputs for one block. The two
/// halves are separate fields, so a kernel can hold an output slice while it
/// reads inputs.
pub struct Lane<'a, 'b> {
    pub voice: usize,
    pub channel: usize,
    pub inputs: LaneInputs<'a>,
    pub outputs: LaneOutputs<'a, 'b>,
}

#[derive(Clone, Copy)]
pub struct LaneInputs<'a> {
    signals: &'a [SignalIn<'a>],
    voice: usize,
    channel: usize,
}

impl<'a> LaneInputs<'a> {
    pub fn get(&self, port: usize) -> &'a [f32] {
        self.signals[port].lane(self.voice, self.channel)
    }

    /// See [`SignalIn::constant`].
    pub fn constant(&self, port: usize) -> Option<f32> {
        self.signals[port].constant()
    }
}

pub struct LaneOutputs<'a, 'b> {
    signals: &'a mut [SignalOut<'b>],
    voice: usize,
    channel: usize,
}

impl LaneOutputs<'_, '_> {
    pub fn get_mut(&mut self, port: usize) -> &mut [f32] {
        self.signals[port].lane_mut(self.voice, self.channel)
    }

    /// Several outputs at once. Panics if a port repeats or is out of range.
    pub fn get_disjoint_mut<const N: usize>(&mut self, ports: [usize; N]) -> [&mut [f32]; N] {
        let (voice, channel) = (self.voice, self.channel);
        self.signals
            .get_disjoint_mut(ports)
            .expect("output ports must be distinct and in range")
            .map(|signal| signal.lane_mut(voice, channel))
    }
}

/// Runs a [`LaneKernel`] over every lane. All outputs must share one shape,
/// which decides the lanes. Inputs broadcast to it.
pub struct PerLane<K: LaneKernel> {
    kernel: K,
    shape: Shape,
    states: Vec<K::State>,
}

impl<K: LaneKernel> PerLane<K> {
    pub fn new(kernel: K, setup: &Setup<'_>) -> Self {
        let shape = setup.output_shapes.first().copied().unwrap_or(Shape::MONO);
        assert!(
            setup.output_shapes.iter().all(|&s| s == shape),
            "PerLane needs all outputs to have the same shape"
        );
        let states = (0..shape.lanes()).map(|_| K::State::default()).collect();
        Self {
            kernel,
            shape,
            states,
        }
    }
}

impl<K: LaneKernel> Node for PerLane<K> {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let channels = self.shape.channels;
        for (lane, state) in self.states.iter_mut().enumerate() {
            let (voice, channel) = (lane / channels, lane % channels);
            let lane = Lane {
                voice,
                channel,
                inputs: LaneInputs {
                    signals: io.inputs,
                    voice,
                    channel,
                },
                outputs: LaneOutputs {
                    signals: &mut *io.outputs,
                    voice,
                    channel,
                },
            };
            self.kernel.process_lane(state, ctx, lane);
        }
    }

    fn reset(&mut self) {
        self.states.fill_with(K::State::default);
    }
}
