//! The Comparator node.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup,
};

/// Turns a signal into a gate: the output goes to 1 when the input rises
/// above `threshold` and back to 0 when it falls below it. `hysteresis` is a
/// dead band around the threshold (half above, half below) that the input
/// has to cross completely to flip the output, so a noisy or slowly moving
/// signal doesn't chatter.
///
/// Wire an [Envelope Follower](crate::EnvelopeFollower) in to trigger from
/// the loudness of a sound, an LFO to make a clock of your own, or a key's
/// velocity to split soft notes from hard. The output is a gate for the
/// ADSR, the Envelope or a Sample & Hold.
///
/// `out` starts low. Each voice has its own state.
pub struct Comparator;

pub const COMPARATOR_ID: &str = "noodle.mod.comparator";

#[derive(Ports)]
struct ComparatorPorts {
    #[input("in", "In")]
    input: (),
    #[param("threshold", "Threshold", ParamInfo::new(-1.0, 1.0, 0.0).offset())]
    threshold: (),
    #[param("hysteresis", "Hysteresis", ParamInfo::new(0.0, 1.0, 0.05))]
    hysteresis: (),
    #[control_output("out", "Out")]
    out: (),
}

const IN: usize = ComparatorPorts::INPUT;
const THRESHOLD: usize = ComparatorPorts::THRESHOLD;
const HYSTERESIS: usize = ComparatorPorts::HYSTERESIS;
const OUT: usize = ComparatorPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: COMPARATOR_ID,
    version: 1,
    name: "Comparator",
    category: "Modulation",
};

impl NodeType for Comparator {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ComparatorPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(ComparatorKernel, setup)))
    }
}

struct ComparatorKernel;

#[derive(Default)]
struct ComparatorState {
    high: bool,
}

impl LaneKernel for ComparatorKernel {
    type State = ComparatorState;

    fn process_lane(&mut self, state: &mut ComparatorState, _: &Context, mut lane: Lane<'_, '_>) {
        let input = lane.inputs.get(IN);
        let (threshold, hysteresis) = (lane.inputs.get(THRESHOLD), lane.inputs.get(HYSTERESIS));
        let out = lane.outputs.get_mut(OUT);
        for i in 0..out.len() {
            let half = 0.5 * hysteresis[i].max(0.0);
            if state.high {
                if input[i] < threshold[i] - half {
                    state.high = false;
                }
            } else if input[i] > threshold[i] + half {
                state.high = true;
            }
            out[i] = f32::from(state.high);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn run(input: &[f32], threshold: f32, hysteresis: f32) -> Vec<f32> {
        let mut h = Harness::new(
            &Comparator,
            &Config::new(),
            &[(IN, Shape::MONO)],
            48_000.0,
            input.len(),
        )
        .unwrap();
        h.set(THRESHOLD, threshold);
        h.set(HYSTERESIS, hysteresis);
        h.input(IN, input.len())
            .lane_mut(0, 0)
            .copy_from_slice(input);
        h.run(input.len()).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    #[test]
    fn goes_high_above_the_threshold_and_low_below_it() {
        let out = run(&[-0.5, 0.2, 0.6, 0.2, -0.5], 0.0, 0.0);
        assert_eq!(out, [0.0, 1.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn hysteresis_ignores_wobble_inside_the_dead_band() {
        // A dead band of 0.4 around 0.5 is 0.3 to 0.7.
        let input = [0.0, 0.6, 0.72, 0.6, 0.4, 0.6, 0.29, 0.5, 0.75];
        let out = run(&input, 0.5, 0.4);
        assert_eq!(out, [0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_noisy_ramp_chatters_without_hysteresis_and_not_with_it() {
        let ramp: Vec<f32> = (0..1_000)
            .map(|i| i as f32 / 1_000.0 + if i % 2 == 0 { 0.02 } else { -0.02 })
            .collect();
        let flips = |out: &[f32]| out.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(flips(&run(&ramp, 0.5, 0.0)) > 3);
        assert_eq!(flips(&run(&ramp, 0.5, 0.2)), 1);
    }
}
