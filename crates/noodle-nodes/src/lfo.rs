//! The low-frequency oscillator.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Unit,
};

/// A control-rate oscillator for modulating parameters: sine, triangle, saw,
/// square, or a random value held for each cycle. It outputs
/// `offset + depth × wave`, where the wave swings between -1 and 1, so at the
/// defaults it is a plain bipolar wave, and a depth of 0.5 with an offset of
/// 0.5 swings between 0 and 1.
///
/// Each voice has its own phase. `phase` shifts where in the cycle a wave
/// starts, which makes two LFOs a quarter cycle apart for stereo movement.
/// The random shape draws from a generator seeded by the node's seed, so a
/// render repeats exactly.
pub struct Lfo;

pub const LFO_ID: &str = "noodle.mod.lfo";

#[derive(Ports)]
struct LfoPorts {
    #[param(
        "rate",
        "Rate",
        ParamInfo::new(0.01, 100.0, 1.0)
            .log()
            .unit(Unit::Hertz)
            .offset()
    )]
    rate: (),
    #[param("depth", "Depth", ParamInfo::new(0.0, 1.0, 1.0).offset())]
    depth: (),
    #[param("offset", "Offset", ParamInfo::new(-1.0, 1.0, 0.0))]
    offset: (),
    #[param(
        "shape",
        "Shape",
        ParamInfo::choice(["Sine", "Triangle", "Saw", "Square", "Random"])
    )]
    shape: (),
    #[param("phase", "Phase", ParamInfo::new(0.0, 1.0, 0.0))]
    phase: (),
    #[output("out", "Out")]
    out: (),
}

const RATE: usize = LfoPorts::RATE;
const DEPTH: usize = LfoPorts::DEPTH;
const OFFSET: usize = LfoPorts::OFFSET;
const SHAPE: usize = LfoPorts::SHAPE;
const PHASE: usize = LfoPorts::PHASE;
const OUT: usize = LfoPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: LFO_ID,
    version: 1,
    name: "LFO",
    category: "Modulation",
};

impl NodeType for Lfo {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(LfoPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(
            LfoKernel { seed: setup.seed },
            setup,
        )))
    }
}

struct LfoKernel {
    seed: u64,
}

#[derive(Default)]
struct LfoState {
    /// In cycles, from 0 to 1.
    phase: f32,
    /// The random shape's held value, and its generator (0 until seeded).
    held: f32,
    rng: u64,
}

impl LfoState {
    /// A value from -1 to 1 (xorshift64*).
    fn next_random(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let bits = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as u32; // 24 bits
        bits as f32 / (1 << 23) as f32 - 1.0
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Wave {
    Sine,
    Triangle,
    Saw,
    Square,
    Random,
}

impl Wave {
    fn from_param(value: f32) -> Self {
        match value.round() as i32 {
            1 => Wave::Triangle,
            2 => Wave::Saw,
            3 => Wave::Square,
            4 => Wave::Random,
            _ => Wave::Sine,
        }
    }
}

impl LaneKernel for LfoKernel {
    type State = LfoState;

    fn process_lane(&mut self, state: &mut LfoState, ctx: &Context, mut lane: Lane<'_, '_>) {
        if state.rng == 0 {
            // Every lane draws differently, and the same way on every run.
            let lane_id = (lane.voice * 64 + lane.channel) as u64;
            state.rng = (self.seed ^ lane_id.wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1;
            state.held = state.next_random();
        }
        let (rate, depth) = (lane.inputs.get(RATE), lane.inputs.get(DEPTH));
        let (offset, shape) = (lane.inputs.get(OFFSET), lane.inputs.get(SHAPE));
        let phase_shift = lane.inputs.get(PHASE);
        let seconds_per_sample = 1.0 / ctx.sample_rate;

        for (i, out) in lane.outputs.get_mut(OUT).iter_mut().enumerate() {
            let wave = match Wave::from_param(shape[i]) {
                Wave::Random => state.held,
                other => wave((state.phase + phase_shift[i]).rem_euclid(1.0), other),
            };
            *out = offset[i] + depth[i] * wave;
            let next = state.phase + rate[i] * seconds_per_sample;
            if next >= 1.0 {
                state.held = state.next_random();
            }
            state.phase = next.rem_euclid(1.0);
        }
        // A non-finite rate would otherwise poison the phase for good.
        if !state.phase.is_finite() {
            state.phase = 0.0;
        }
    }
}

fn wave(phase: f32, shape: Wave) -> f32 {
    match shape {
        Wave::Sine => (phase * std::f32::consts::TAU).sin(),
        Wave::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
        Wave::Saw => 2.0 * phase - 1.0,
        Wave::Square => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        Wave::Random => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    const RATE_HZ: f32 = 48_000.0;

    /// One second of the LFO at 1 Hz with the given shape.
    fn one_cycle(shape: f32) -> Vec<f32> {
        let mut h = Harness::new(&Lfo, &Config::new(), &[], RATE_HZ, 48_000).unwrap();
        h.set(SHAPE, shape);
        h.run(48_000).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    #[test]
    fn sine_swings_between_minus_one_and_one() {
        let out = one_cycle(0.0);
        assert!(out[0].abs() < 1e-3);
        assert!((out[12_000] - 1.0).abs() < 1e-3);
        assert!((out[36_000] + 1.0).abs() < 1e-3);
    }

    #[test]
    fn shapes_have_their_waveforms() {
        let tri = one_cycle(1.0);
        assert!((tri[0] + 1.0).abs() < 1e-3);
        assert!((tri[24_000] - 1.0).abs() < 1e-3);
        let saw = one_cycle(2.0);
        assert!((saw[0] + 1.0).abs() < 1e-3);
        assert!((saw[47_000] - 0.958).abs() < 0.01);
        let square = one_cycle(3.0);
        assert_eq!((square[100], square[30_000]), (1.0, -1.0));
    }

    #[test]
    fn depth_and_offset_scale_the_wave() {
        let mut h = Harness::new(&Lfo, &Config::new(), &[], RATE_HZ, 48_000).unwrap();
        h.set(DEPTH, 0.5);
        h.set(OFFSET, 0.5);
        h.set(SHAPE, 3.0);
        h.run(48_000).unwrap();
        let out = h.output(OUT).lane(0, 0);
        assert_eq!((out[100], out[30_000]), (1.0, 0.0));
    }

    #[test]
    fn phase_shifts_the_wave() {
        let mut h = Harness::new(&Lfo, &Config::new(), &[], RATE_HZ, 48_000).unwrap();
        h.set(PHASE, 0.25);
        h.run(48_000).unwrap();
        assert!((h.output(OUT).lane(0, 0)[0] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn random_holds_a_value_for_each_cycle() {
        let mut h = Harness::with_seed(&Lfo, &Config::new(), &[], RATE_HZ, 96_000, 7).unwrap();
        h.set(SHAPE, 4.0);
        h.run(96_000).unwrap();
        let out = h.output(OUT).lane(0, 0).to_vec();
        assert!(out[..47_900].iter().all(|&x| x == out[0]));
        assert!(out[48_100..95_900].iter().all(|&x| x == out[48_100]));
        assert_ne!(out[0], out[48_100]);
        assert!(out.iter().all(|x| (-1.0..=1.0).contains(x)));

        // Same seed, same render.
        let mut again = Harness::with_seed(&Lfo, &Config::new(), &[], RATE_HZ, 96_000, 7).unwrap();
        again.set(SHAPE, 4.0);
        again.run(96_000).unwrap();
        assert_eq!(again.output(OUT).lane(0, 0), &out[..]);
    }

    #[test]
    fn a_non_finite_rate_does_not_stick() {
        let mut h = Harness::new(
            &Lfo,
            &Config::new(),
            &[(RATE, noodle_engine::Shape::MONO)],
            RATE_HZ,
            8,
        )
        .unwrap();
        h.input(RATE, 8).fill(f32::INFINITY);
        h.run(8).unwrap();
        h.input(RATE, 8).fill(1.0);
        h.run(8).unwrap();
        assert!(h.output(OUT).lane(0, 0).iter().all(|x| x.is_finite()));
    }
}
