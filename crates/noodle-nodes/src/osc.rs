//! Oscillators.

use std::f32::consts::TAU;

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Setup, Unit,
};

/// A sine oscillator. Its frequency input runs at audio rate, so the same node
/// serves as an LFO and supports through-zero FM.
pub struct Sine;

const FREQUENCY: usize = 0;
const OUT: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: "noodle.osc.sine",
    version: 1,
    name: "Sine",
    category: "Generators",
};

impl NodeType for Sine {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .param(
                "frequency",
                "Frequency",
                ParamInfo::new(0.01, 20_000.0, 440.0)
                    .log()
                    .unit(Unit::Hertz),
            )
            .output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(SineKernel, setup)))
    }
}

struct SineKernel;

impl LaneKernel for SineKernel {
    /// Phase in cycles, from 0 to 1.
    type State = f32;

    fn process_lane(&mut self, phase: &mut f32, ctx: &Context, mut lane: Lane<'_, '_>) {
        let frequency = lane.inputs.get(FREQUENCY);
        let seconds_per_sample = 1.0 / ctx.sample_rate;
        for (out, f) in lane.outputs.get_mut(OUT).iter_mut().zip(frequency) {
            *out = (*phase * TAU).sin();
            *phase = (*phase + f * seconds_per_sample).rem_euclid(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    #[test]
    fn one_cycle() {
        let mut h = Harness::new(&Sine, &Config::new(), &[], 48_000.0, 48).unwrap();
        h.set(FREQUENCY, 1_000.0);
        h.run(48).unwrap();
        let out = h.output(OUT).lane(0, 0);
        assert!(out[0].abs() < 1e-6);
        assert!((out[12] - 1.0).abs() < 1e-4);
        assert!((out[36] + 1.0).abs() < 1e-4);
    }

    #[test]
    fn polyphonic_frequency_gives_one_oscillator_per_voice() {
        let poly = Shape::new(2, 1);
        let mut h =
            Harness::new(&Sine, &Config::new(), &[(FREQUENCY, poly)], 48_000.0, 48).unwrap();
        let mut frequency = h.input(FREQUENCY, 48);
        frequency.lane_mut(0, 0).fill(1_000.0);
        frequency.lane_mut(1, 0).fill(2_000.0);
        h.run(48).unwrap();

        let out = h.output(OUT);
        assert_eq!(out.shape(), poly);
        // A quarter of a cycle: 12 samples at 1 kHz, 6 at 2 kHz.
        assert!((out.lane(0, 0)[12] - 1.0).abs() < 1e-4);
        assert!((out.lane(1, 0)[6] - 1.0).abs() < 1e-4);
    }
}
