//! The Bitcrusher node.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Skip, Unit,
};

/// A lo-fi effect that throws resolution away two ways: `bits` rounds each
/// sample to that many bits of amplitude (16 is clean, 1 is a square-ish
/// buzz), and `rate` holds each sample for as long as a converter running at
/// that many hertz would, which folds high frequencies back down as aliasing.
/// `mix` blends the crushed signal with the dry one.
///
/// `bits` is a number of bits and may be fractional, which sets the step
/// size to `2^(1 - bits)` and so sweeps smoothly between the whole values.
/// `rate` at or above half the sample rate passes the signal through.
pub struct Bitcrusher;

pub const BITCRUSHER_ID: &str = "noodle.effect.bitcrusher";

#[derive(Ports)]
struct BitcrusherPorts {
    #[input("in", "In")]
    input: (),
    #[param("bits", "Bits", ParamInfo::new(1.0, 16.0, 8.0).offset())]
    bits: (),
    #[param(
        "rate",
        "Rate",
        ParamInfo::new(100.0, 48_000.0, 12_000.0)
            .log()
            .unit(Unit::Hertz)
            .offset()
    )]
    rate: (),
    #[param("mix", "Mix", ParamInfo::new(0.0, 1.0, 1.0))]
    mix: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = BitcrusherPorts::INPUT;
const BITS: usize = BitcrusherPorts::BITS;
const RATE: usize = BitcrusherPorts::RATE;
const MIX: usize = BitcrusherPorts::MIX;
const OUT: usize = BitcrusherPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: BITCRUSHER_ID,
    version: 1,
    name: "Bitcrusher",
    category: "Effects",
};

impl NodeType for Bitcrusher {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(BitcrusherPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(BitcrusherKernel, setup)))
    }
}

/// `x` rounded to a grid `bits` bits fine (the grid spans -1 to 1).
pub fn quantise(x: f32, bits: f32) -> f32 {
    let step = 2f32.powf(1.0 - bits.clamp(1.0, 24.0));
    (x / step).round() * step
}

struct BitcrusherKernel;

#[derive(Default)]
struct CrushState {
    held: f32,
    /// Samples left in the current hold.
    counter: f32,
}

impl LaneKernel for BitcrusherKernel {
    type State = CrushState;

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[IN])
    }

    fn is_idle(&self, state: &CrushState) -> bool {
        state.held == 0.0
    }

    fn process_lane(&mut self, state: &mut CrushState, ctx: &Context, mut lane: Lane<'_, '_>) {
        let (input, bits) = (lane.inputs.get(IN), lane.inputs.get(BITS));
        let (rate, mix) = (lane.inputs.get(RATE), lane.inputs.get(MIX));
        let out = lane.outputs.get_mut(OUT);
        for i in 0..out.len() {
            // A new sample is taken whenever a hold of `period` samples is up.
            let period = if rate[i].is_finite() {
                (ctx.sample_rate / rate[i].max(1.0)).max(1.0)
            } else {
                1.0
            };
            state.counter -= 1.0;
            if state.counter < 0.0 {
                state.counter += period;
                state.held = quantise(input[i], bits[i]);
            }
            if !state.held.is_finite() {
                state.held = 0.0;
            }
            let mix = mix[i].clamp(0.0, 1.0);
            out[i] = input[i] + (state.held - input[i]) * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn run(input: &[f32], params: &[(usize, f32)]) -> Vec<f32> {
        let mut h = Harness::new(
            &Bitcrusher,
            &Config::new(),
            &[(IN, Shape::MONO)],
            48_000.0,
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

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| i as f32 / frames as f32 * 2.0 - 1.0)
            .collect()
    }

    #[test]
    fn rounds_to_the_requested_number_of_bits() {
        let out = run(&ramp(1_000), &[(BITS, 3.0), (RATE, 48_000.0)]);
        // 3 bits: a step of 2^-2 = 0.25, so 9 distinct levels from -1 to 1.
        let mut levels: Vec<i32> = out.iter().map(|x| (x * 4.0).round() as i32).collect();
        levels.dedup();
        assert_eq!(levels.len(), 9);
        for x in &out {
            assert!((x * 4.0 - (x * 4.0).round()).abs() < 1e-5, "{x}");
        }
    }

    #[test]
    fn sixteen_bits_is_nearly_transparent() {
        let input = ramp(1_000);
        let out = run(&input, &[(BITS, 16.0), (RATE, 48_000.0)]);
        for (x, y) in input.iter().zip(&out) {
            assert!((x - y).abs() < 1e-4);
        }
    }

    #[test]
    fn a_lower_rate_holds_samples() {
        // 12 kHz at 48 kHz holds each sample for four.
        let out = run(&ramp(400), &[(BITS, 16.0), (RATE, 12_000.0)]);
        for group in out.chunks_exact(4).skip(1) {
            assert!(group.iter().all(|&x| x == group[0]), "{group:?}");
        }
        assert_ne!(out[8], out[12]);
    }

    #[test]
    fn mix_zero_is_the_dry_signal() {
        let input = ramp(200);
        assert_eq!(run(&input, &[(BITS, 2.0), (MIX, 0.0)]), input);
    }

    #[test]
    fn quantise_stays_finite_and_in_range() {
        assert_eq!(quantise(0.0, 1.0), 0.0);
        assert_eq!(quantise(1.0, 1.0), 1.0);
    }
}
