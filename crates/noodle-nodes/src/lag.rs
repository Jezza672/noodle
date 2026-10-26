//! The Lag node.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Unit,
};

/// Smooths a control signal: the output chases the input, taking `rise` to
/// get most of the way up and `fall` most of the way down. With both at 0 it
/// follows the input exactly. Use it for portamento on a pitch, to round off
/// a stepped signal (a Sample & Hold's staircase, a Comparator's gate), or
/// to give a switch a soft edge.
///
/// A time is how long the output takes to cover all but a thousandth of the
/// distance, as for the [ADSR](crate::Adsr)'s decay: the curve is
/// exponential, quick at first and slower as it arrives. The first sample
/// the node sees is taken as it is, so a pitch doesn't glide up from 0 when
/// the patch starts.
pub struct Lag;

pub const LAG_ID: &str = "noodle.mod.lag";

#[derive(Ports)]
#[ports(bounded = 70000)]
struct LagPorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "rise",
        "Rise",
        ParamInfo::new(0.0, 10.0, 0.1).unit(Unit::Seconds).offset()
    )]
    rise: (),
    #[param(
        "fall",
        "Fall",
        ParamInfo::new(0.0, 10.0, 0.1).unit(Unit::Seconds).offset()
    )]
    fall: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = LagPorts::INPUT;
const RISE: usize = LagPorts::RISE;
const FALL: usize = LagPorts::FALL;
const OUT: usize = LagPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: LAG_ID,
    version: 1,
    name: "Lag",
    category: "Modulation",
};

impl NodeType for Lag {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(LagPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(LagKernel, setup)))
    }
}

/// -ln(0.001): the exponent at which a decay has covered 99.9% of its distance.
const SIXTY_DB: f32 = 6.907_755;

struct LagKernel;

#[derive(Default)]
struct LagState {
    level: f32,
    started: bool,
}

impl LaneKernel for LagKernel {
    type State = LagState;

    fn process_lane(&mut self, state: &mut LagState, ctx: &Context, mut lane: Lane<'_, '_>) {
        let input = lane.inputs.get(IN);
        let (rise, fall) = (lane.inputs.get(RISE), lane.inputs.get(FALL));
        let out = lane.outputs.get_mut(OUT);
        for i in 0..out.len() {
            let x = if input[i].is_finite() { input[i] } else { 0.0 };
            if !state.started {
                state.started = true;
                state.level = x;
            }
            let seconds = if x > state.level { rise[i] } else { fall[i] };
            let samples = seconds.max(0.0) * ctx.sample_rate;
            state.level = if samples < 1.0 {
                x
            } else {
                x + (state.level - x) * (-SIXTY_DB / samples).exp()
            };
            out[i] = state.level;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn run(input: &[f32], rise: f32, fall: f32) -> Vec<f32> {
        let mut h = Harness::new(
            &Lag,
            &Config::new(),
            &[(IN, Shape::MONO)],
            48_000.0,
            input.len(),
        )
        .unwrap();
        h.set(RISE, rise);
        h.set(FALL, fall);
        h.input(IN, input.len())
            .lane_mut(0, 0)
            .copy_from_slice(input);
        h.run(input.len()).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    #[test]
    fn zero_times_follow_the_input() {
        let input: Vec<f32> = (0..100).map(|i| (i as f32 * 0.3).sin()).collect();
        assert_eq!(run(&input, 0.0, 0.0), input);
    }

    #[test]
    fn rises_and_falls_at_their_own_times() {
        let mut input = vec![0.0; 100];
        input.extend(vec![1.0; 9_600]);
        input.extend(vec![0.0; 9_600]);
        let out = run(&input, 0.05, 0.1);
        // The rise covers 99.9% of the way in 0.05 s = 2400 samples.
        assert!((out[100 + 2_400] - 0.999).abs() < 0.002, "{}", out[2_500]);
        assert!(out[100 + 1_200] < 0.99 && out[100 + 1_200] > 0.9);
        // The fall takes 0.1 s to cover 99.9%, so it is half way after
        // ln 2 / 6.9 of that (481 samples).
        let fall_at = 100 + 9_600;
        assert!(
            (out[fall_at + 481] - 0.5).abs() < 0.02,
            "{}",
            out[fall_at + 481]
        );
        assert!(out[fall_at + 4_800] < 0.002);
    }

    #[test]
    fn the_first_sample_is_taken_as_it_is() {
        let out = run(&vec![440.0; 200], 1.0, 1.0);
        assert!(out.iter().all(|&x| x == 440.0));
    }

    #[test]
    fn a_nan_input_does_not_stick() {
        let mut input = vec![1.0; 50];
        input[10] = f32::NAN;
        let out = run(&input, 0.001, 0.001);
        assert!(out.iter().all(|x| x.is_finite()));
    }
}
