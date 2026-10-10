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

    /// When a lane may be skipped. By default never, which is right for
    /// anything that makes sound from silent inputs (a noise source, or a
    /// reverb tail). See [`Skip`].
    fn skip(&self) -> Skip {
        Skip::Never
    }

    /// Whether a lane's state has nothing left to ring out, so that silent
    /// inputs would give silent output. A lane is only skipped once this is
    /// true, so filter and envelope tails finish first. Only asked when
    /// [`skip`](Self::skip) says the inputs are silent.
    fn is_idle(&self, _state: &Self::State) -> bool {
        true
    }
}

/// Which silent inputs let [`PerLane`] skip a lane: it writes zeros to the
/// lane's outputs and flags them silent (see [`SignalOut::set_silent`]), so
/// the saving cascades to the nodes downstream.
///
/// Ports are positions in the node's [`Layout`] inputs. An input counts as
/// silent when [`SignalIn::is_silent`] says so, which includes an unconnected
/// input that holds 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Never,
    /// Skip when any of these inputs is silent, as for a multiplier (a VCA's
    /// input or level) or a generator driven by a pitch (no pitch, no sound).
    AnySilent(&'static [usize]),
    /// Skip when all of these inputs are silent, as for a mixer.
    AllSilent(&'static [usize]),
    /// Skip when every input from 0 up to this many is silent, for a node
    /// whose number of inputs is config (a mixer's).
    AllSilentBelow(usize),
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

    /// See [`SignalIn::is_silent`].
    pub fn is_silent(&self, port: usize) -> bool {
        self.signals[port].is_silent(self.voice, self.channel)
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
        let skip = self.kernel.skip();
        for (lane, state) in self.states.iter_mut().enumerate() {
            let (voice, channel) = (lane / channels, lane % channels);
            let silent = |port: &usize| io.inputs[*port].is_silent(voice, channel);
            let skippable = match skip {
                Skip::Never => false,
                Skip::AnySilent(ports) => ports.iter().any(silent),
                Skip::AllSilent(ports) => ports.iter().all(silent),
                Skip::AllSilentBelow(n) => (0..n).all(|port| silent(&port)),
            };
            if skippable && self.kernel.is_idle(state) {
                for output in io.outputs.iter_mut() {
                    output.silence(voice, channel);
                }
                continue;
            }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::testing::Harness;
    use crate::{Config, Instance, Layout, NodeError, NodeInfo, NodeType, Shape};

    /// Adds its two inputs, remembers the last sample as ringing state, and
    /// counts the lanes it runs.
    struct Adder {
        skip: Skip,
        runs: Arc<AtomicUsize>,
    }

    static INFO: NodeInfo = NodeInfo {
        id: "test.adder",
        version: 1,
        name: "Adder",
        category: "Test",
    };

    impl NodeType for Adder {
        fn info(&self) -> &NodeInfo {
            &INFO
        }

        fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
            Ok(Layout::realtime()
                .input("a", "A")
                .input("b", "B")
                .output("out", "Out")
                .output("copy", "Copy"))
        }

        fn instantiate(&self, setup: &crate::Setup<'_>) -> Result<Instance, NodeError> {
            Ok(Instance::realtime(PerLane::new(
                AdderKernel {
                    skip: self.skip,
                    runs: Arc::clone(&self.runs),
                },
                setup,
            )))
        }
    }

    struct AdderKernel {
        skip: Skip,
        runs: Arc<AtomicUsize>,
    }

    impl LaneKernel for AdderKernel {
        /// The last output, which counts as ringing while it is nonzero.
        type State = f32;

        fn skip(&self) -> Skip {
            self.skip
        }

        fn is_idle(&self, state: &f32) -> bool {
            *state == 0.0
        }

        fn process_lane(&mut self, state: &mut f32, _: &Context, mut lane: Lane<'_, '_>) {
            self.runs.fetch_add(1, Ordering::Relaxed);
            let (a, b) = (lane.inputs.get(0), lane.inputs.get(1));
            let [out, copy] = lane.outputs.get_disjoint_mut([0, 1]);
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                out[i] = a + b;
                copy[i] = a + b;
            }
            *state = *out.last().unwrap();
        }
    }

    const POLY: Shape = Shape::new(3, 1);

    fn harness(skip: Skip) -> (Harness, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let adder = Adder {
            skip,
            runs: Arc::clone(&runs),
        };
        let h = Harness::new(&adder, &Config::new(), &[(0, POLY), (1, POLY)], 48_000.0, 4).unwrap();
        (h, runs)
    }

    /// Lane 0 of both inputs sound, lane 1 is flagged silent, lane 2 holds
    /// zeros that nobody flagged.
    fn feed(h: &mut Harness, flag_b: bool) {
        for port in 0..2 {
            let mut input = h.input(port, 4);
            input.lane_mut(0, 0).fill(1.0);
            input.lane_mut(1, 0).fill(5.0);
            input.lane_mut(2, 0).fill(0.0);
            if port == 0 || flag_b {
                input.set_silent(1, 0);
            }
        }
    }

    #[test]
    fn any_silent_skips_a_lane_and_flags_its_outputs() {
        let (mut h, runs) = harness(Skip::AnySilent(&[0, 1]));
        feed(&mut h, false);
        h.run(4).unwrap();
        // Lane 0 sounds; lane 1 has one flagged input (stale data of 5.0 must
        // not leak through); lane 2 is zero but unflagged, so it still runs.
        assert_eq!(runs.load(Ordering::Relaxed), 2);
        for port in 0..2 {
            let out = h.output(port);
            assert_eq!(out.lane(0, 0), &[2.0; 4]);
            assert!(!out.is_silent(0, 0));
            assert_eq!(out.lane(1, 0), &[0.0; 4]);
            assert!(out.is_silent(1, 0));
            assert!(!out.is_silent(2, 0));
        }
    }

    #[test]
    fn all_silent_needs_every_input_silent() {
        let (mut h, runs) = harness(Skip::AllSilent(&[0, 1]));
        feed(&mut h, false);
        h.run(4).unwrap();
        assert_eq!(
            runs.load(Ordering::Relaxed),
            3,
            "only one of lane 1's inputs is"
        );

        let (mut h, runs) = harness(Skip::AllSilent(&[0, 1]));
        feed(&mut h, true);
        h.run(4).unwrap();
        assert_eq!(runs.load(Ordering::Relaxed), 2, "lane 1 has both flagged");
        assert!(h.output(0).is_silent(1, 0));
    }

    #[test]
    fn a_lane_is_not_skipped_until_it_has_nothing_left_to_ring() {
        let (mut h, runs) = harness(Skip::AnySilent(&[0]));
        feed(&mut h, true);
        h.run(4).unwrap();
        // Lane 0 ended at 2.0, so it's ringing. Silence its input: it still
        // runs once, then is skipped.
        feed(&mut h, true);
        let mut a = h.input(0, 4);
        a.lane_mut(0, 0).fill(0.0);
        a.set_silent(0, 0);
        a.set_silent(1, 0);
        h.input(1, 4).lane_mut(0, 0).fill(0.0);
        runs.store(0, Ordering::Relaxed);
        h.run(4).unwrap();
        assert_eq!(runs.load(Ordering::Relaxed), 2, "lanes 0 (ringing) and 2");
        assert!(!h.output(0).is_silent(0, 0));
        assert_eq!(h.output(0).lane(0, 0), &[0.0; 4]);
        runs.store(0, Ordering::Relaxed);
        h.run(4).unwrap();
        assert_eq!(runs.load(Ordering::Relaxed), 1, "lane 0 is now skipped");
        assert!(h.output(0).is_silent(0, 0));
    }

    #[test]
    fn unconnected_inputs_holding_zero_count_as_silent() {
        let runs = Arc::new(AtomicUsize::new(0));
        let adder = Adder {
            skip: Skip::AnySilent(&[0]),
            runs: Arc::clone(&runs),
        };
        let mut h = Harness::new(&adder, &Config::new(), &[], 48_000.0, 4).unwrap();
        h.run(4).unwrap();
        assert_eq!(runs.load(Ordering::Relaxed), 0);
        assert!(h.output(0).is_silent(0, 0));
        h.set(0, 0.5);
        h.run(4).unwrap();
        assert_eq!(runs.load(Ordering::Relaxed), 1);
        assert!(!h.output(0).is_silent(0, 0));
    }

    #[test]
    fn never_skips_by_default() {
        let (mut h, runs) = harness(Skip::Never);
        feed(&mut h, true);
        h.run(4).unwrap();
        assert_eq!(runs.load(Ordering::Relaxed), 3);
        assert!(!h.output(0).is_silent(1, 0));
    }
}
