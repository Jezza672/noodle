//! The Envelope Follower node.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Skip, Unit,
};

/// Turns the level of a sound into a control signal: it rises towards the
/// input's level at the `attack` rate and falls away at the `release` rate,
/// so a drum loop becomes a stream of bumps that can open a filter or duck a
/// pad (wire it into a parameter, or through a [Comparator](crate::Comparator)
/// for a gate).
///
/// `Peak` follows the rectified signal, which is quick and tracks every
/// transient. `RMS` follows the signal's power, which tracks loudness the way
/// the ear does and ignores single-sample spikes. Times are the time constant
/// of the smoothing, in seconds: the follower covers about two thirds of a
/// jump in that long. A sine wave of amplitude 1 reads about 0.64 in `Peak`
/// mode once the follower settles on its average, and 0.71 (1/√2) in `RMS`;
/// scale it up with a Gain or Math node if the full 0 to 1 is wanted.
pub struct EnvelopeFollower;

pub const FOLLOWER_ID: &str = "noodle.mod.follower";

#[derive(Ports)]
struct FollowerPorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "attack",
        "Attack",
        ParamInfo::new(0.0001, 1.0, 0.005).log().unit(Unit::Seconds)
    )]
    attack: (),
    #[param(
        "release",
        "Release",
        ParamInfo::new(0.001, 5.0, 0.1).log().unit(Unit::Seconds)
    )]
    release: (),
    #[param("mode", "Mode", ParamInfo::choice(["Peak", "RMS"]))]
    mode: (),
    #[control_output("out", "Out")]
    out: (),
}

const IN: usize = FollowerPorts::INPUT;
const ATTACK: usize = FollowerPorts::ATTACK;
const RELEASE: usize = FollowerPorts::RELEASE;
const MODE: usize = FollowerPorts::MODE;
const OUT: usize = FollowerPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: FOLLOWER_ID,
    version: 1,
    name: "Envelope Follower",
    category: "Modulation",
};

impl NodeType for EnvelopeFollower {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(FollowerPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(FollowerKernel, setup)))
    }
}

struct FollowerKernel;

#[derive(Default)]
struct FollowerState {
    /// The smoothed level (peak mode) or power (RMS mode).
    level: f32,
}

/// What a smoothing of `seconds` multiplies the old value by per sample.
#[inline]
fn coefficient(seconds: f32, sample_rate: f32) -> f32 {
    (-1.0 / (seconds.max(1e-5) * sample_rate)).exp()
}

impl LaneKernel for FollowerKernel {
    type State = FollowerState;

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[IN])
    }

    fn is_idle(&self, state: &FollowerState) -> bool {
        state.level < 1e-9
    }

    fn process_lane(&mut self, state: &mut FollowerState, ctx: &Context, mut lane: Lane<'_, '_>) {
        let input = lane.inputs.get(IN);
        let (attack, release) = (lane.inputs.get(ATTACK), lane.inputs.get(RELEASE));
        let mode = lane.inputs.get(MODE);
        let out = lane.outputs.get_mut(OUT);
        for i in 0..out.len() {
            let rms = mode[i] >= 0.5;
            let x = if rms {
                input[i] * input[i]
            } else {
                input[i].abs()
            };
            let x = if x.is_finite() { x } else { 0.0 };
            let seconds = if x > state.level {
                attack[i]
            } else {
                release[i]
            };
            let a = coefficient(seconds, ctx.sample_rate);
            state.level = x + (state.level - x) * a;
            out[i] = if rms { state.level.sqrt() } else { state.level };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    const RATE_HZ: f32 = 48_000.0;

    fn run(input: &[f32], params: &[(usize, f32)]) -> Vec<f32> {
        let mut h = Harness::new(
            &EnvelopeFollower,
            &Config::new(),
            &[(IN, Shape::MONO)],
            RATE_HZ,
            input.len(),
        )
        .unwrap();
        h.input(IN, input.len())
            .lane_mut(0, 0)
            .copy_from_slice(input);
        for &(port, value) in params {
            h.set(port, value);
        }
        h.run(input.len()).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    #[test]
    fn rises_to_a_steady_level_and_falls_back() {
        let mut input = vec![0.8; 4_800];
        input.extend(vec![0.0; 24_000]);
        let out = run(&input, &[(ATTACK, 0.002), (RELEASE, 0.02)]);
        assert!((out[4_799] - 0.8).abs() < 0.01, "{}", out[4_799]);
        assert!(out[4_800..].windows(2).all(|w| w[1] <= w[0]));
        assert!(out[28_799] < 1e-3);
    }

    #[test]
    fn a_negative_input_reads_as_its_magnitude() {
        let out = run(&vec![-0.5; 4_800], &[(ATTACK, 0.002)]);
        assert!((out[4_799] - 0.5).abs() < 0.01);
    }

    #[test]
    fn attack_and_release_times_set_the_speed() {
        let mut input = vec![1.0; 2_400];
        input.extend(vec![0.0; 2_400]);
        let fast = run(&input, &[(ATTACK, 0.0005), (RELEASE, 0.005)]);
        let slow = run(&input, &[(ATTACK, 0.02), (RELEASE, 0.2)]);
        assert!(fast[100] > slow[100]);
        assert!(fast[2_400 + 500] < slow[2_400 + 500]);
        // One time constant (0.02 s = 960 samples) reaches 1 - 1/e.
        assert!((slow[960] - 0.632).abs() < 0.02, "{}", slow[960]);
    }

    #[test]
    fn rms_reads_a_sine_as_its_root_mean_square() {
        let input: Vec<f32> = (0..24_000)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / RATE_HZ).sin())
            .collect();
        let rms = run(&input, &[(MODE, 1.0), (ATTACK, 0.05), (RELEASE, 0.05)]);
        assert!(
            (rms[23_999] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.03,
            "{}",
            rms[23_999]
        );
    }

    #[test]
    fn a_tail_finishes_before_the_lane_is_skipped() {
        let frames = 4_800;
        let mut h = Harness::new(
            &EnvelopeFollower,
            &Config::new(),
            &[(IN, Shape::MONO)],
            RATE_HZ,
            frames,
        )
        .unwrap();
        h.set(RELEASE, 0.05);
        h.input(IN, frames).fill(1.0);
        h.run(frames).unwrap();
        let mut quiet = h.input(IN, frames);
        quiet.fill(0.0);
        quiet.set_silent(0, 0);
        h.run(frames).unwrap();
        // Still falling, so not skipped.
        assert!(!h.output(OUT).is_silent(0, 0));
        for _ in 0..30 {
            h.run(frames).unwrap();
        }
        assert!(h.output(OUT).is_silent(0, 0));
    }
}
