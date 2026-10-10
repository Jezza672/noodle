use std::f32::consts::PI;

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Skip, Unit,
};

/// A state-variable filter with low-pass, band-pass and high-pass outputs all
/// at once. Wire up whichever ones you need.
pub struct Svf;

#[derive(Ports)]
struct SvfPorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "cutoff",
        "Cutoff",
        ParamInfo::new(20.0, 20_000.0, 1_000.0)
            .log()
            .unit(Unit::Hertz)
            .offset()
    )]
    cutoff: (),
    #[param("resonance", "Resonance", ParamInfo::new(0.0, 1.0, 0.0).offset())]
    resonance: (),
    #[output("low", "Low")]
    low: (),
    #[output("band", "Band")]
    band: (),
    #[output("high", "High")]
    high: (),
}

const IN: usize = SvfPorts::INPUT;
const CUTOFF: usize = SvfPorts::CUTOFF;
const RESONANCE: usize = SvfPorts::RESONANCE;

const LOW: usize = SvfPorts::LOW;
const BAND: usize = SvfPorts::BAND;
const HIGH: usize = SvfPorts::HIGH;

static INFO: NodeInfo = NodeInfo {
    id: "noodle.filter.svf",
    version: 1,
    name: "State Variable Filter",
    category: "Filters",
};

impl NodeType for Svf {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(SvfPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(SvfKernel, setup)))
    }
}

struct SvfKernel;

impl LaneKernel for SvfKernel {
    type State = SvfState;

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[IN])
    }

    fn is_idle(&self, state: &SvfState) -> bool {
        state.ic1.abs() < IDLE && state.ic2.abs() < IDLE
    }

    fn process_lane(&mut self, state: &mut SvfState, ctx: &Context, lane: Lane<'_, '_>) {
        let Lane {
            inputs,
            mut outputs,
            ..
        } = lane;
        let [low, band, high] = outputs.get_disjoint_mut([LOW, BAND, HIGH]);
        let input = inputs.get(IN);

        // Coefficients cost a tan(), so when cutoff and resonance aren't
        // modulated, compute them once per block instead of every sample.
        let fixed = match (inputs.constant(CUTOFF), inputs.constant(RESONANCE)) {
            (Some(cutoff), Some(resonance)) => {
                Some(Coefficients::new(cutoff, resonance, ctx.sample_rate))
            }
            _ => None,
        };
        let (cutoff, resonance) = (inputs.get(CUTOFF), inputs.get(RESONANCE));

        for (i, &x) in input.iter().enumerate() {
            let c = fixed
                .unwrap_or_else(|| Coefficients::new(cutoff[i], resonance[i], ctx.sample_rate));
            (low[i], band[i], high[i]) = state.tick(x, &c);
        }
        state.tidy();
    }
}

#[derive(Clone, Copy)]
struct Coefficients {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl Coefficients {
    fn new(cutoff: f32, resonance: f32, sample_rate: f32) -> Self {
        let cutoff = cutoff.clamp(10.0, sample_rate * 0.49);
        let g = (PI * cutoff / sample_rate).tan();
        let k = 2.0 * (1.0 - 0.99 * resonance.clamp(0.0, 1.0));
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Self {
            k,
            a1,
            a2,
            a3: g * a2,
        }
    }
}

#[derive(Default)]
struct SvfState {
    ic1: f32,
    ic2: f32,
}

impl SvfState {
    /// One sample through Andrew Simper's trapezoidal SVF. Returns
    /// (low, band, high).
    fn tick(&mut self, x: f32, c: &Coefficients) -> (f32, f32, f32) {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (v2, v1, x - c.k * v1 - v2)
    }

    /// Once per block: resets state that an infinite or NaN input made
    /// non-finite, which would otherwise stay that way for good, and flushes
    /// state too small to hear to zero, so it doesn't decay into the slow
    /// subnormal range even where the engine can't flush them.
    fn tidy(&mut self) {
        if !(self.ic1.is_finite() && self.ic2.is_finite()) {
            *self = Self::default();
        }
        for x in [&mut self.ic1, &mut self.ic2] {
            if x.abs() < TINY {
                *x = 0.0;
            }
        }
    }
}

/// A filter whose memory is below this (about -140 dB) has nothing left to
/// ring out, and may be skipped while its input is silent.
const IDLE: f32 = 1e-7;

/// About -600 dB: far below anything audible, and far above the subnormals.
const TINY: f32 = 1e-30;

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn last(h: &Harness, port: usize) -> f32 {
        *h.output(port).lane(0, 0).last().unwrap()
    }

    #[test]
    fn dc_goes_to_the_low_output() {
        let mut h = Harness::new(&Svf, &Config::new(), &[], 48_000.0, 480).unwrap();
        h.set(IN, 1.0);
        for _ in 0..10 {
            h.run(480).unwrap();
        }
        assert!((last(&h, LOW) - 1.0).abs() < 1e-4);
        assert!(last(&h, BAND).abs() < 1e-4);
        assert!(last(&h, HIGH).abs() < 1e-4);
    }

    #[test]
    fn nyquist_goes_to_the_high_output() {
        let mut h =
            Harness::new(&Svf, &Config::new(), &[(IN, Shape::MONO)], 48_000.0, 480).unwrap();
        h.set(CUTOFF, 100.0);
        for (i, x) in h.input(IN, 480).lane_mut(0, 0).iter_mut().enumerate() {
            *x = if i % 2 == 0 { 1.0 } else { -1.0 };
        }
        h.run(480).unwrap();
        let tail = |port| {
            h.output(port).lane(0, 0)[240..]
                .iter()
                .fold(0f32, |m, x| m.max(x.abs()))
        };
        assert!(tail(LOW) < 1e-3);
        assert!(tail(HIGH) > 0.99);
    }

    /// Without the engine's flush-to-zero, as the harness runs it.
    #[test]
    fn silence_decays_to_zero_not_to_subnormals() {
        let mut h =
            Harness::new(&Svf, &Config::new(), &[(IN, Shape::MONO)], 48_000.0, 512).unwrap();
        h.set(CUTOFF, 200.0);
        h.input(IN, 512).lane_mut(0, 0).fill(1.0);
        h.run(512).unwrap();
        h.input(IN, 512).lane_mut(0, 0).fill(0.0);
        // 20 s of silence.
        for _ in 0..1875 {
            h.run(512).unwrap();
        }
        for port in [LOW, BAND, HIGH] {
            let out = h.output(port).lane(0, 0);
            assert!(out.iter().all(|&x| x == 0.0), "{:e}", out[511]);
        }
    }

    #[test]
    fn recovers_from_an_infinite_input() {
        let mut h = Harness::new(&Svf, &Config::new(), &[(IN, Shape::MONO)], 48_000.0, 64).unwrap();
        h.input(IN, 64).lane_mut(0, 0)[10] = f32::INFINITY;
        h.run(64).unwrap();
        h.input(IN, 64).fill(1.0);
        h.run(64).unwrap();
        for port in [LOW, BAND, HIGH] {
            assert!(h.output(port).lane(0, 0).iter().all(|x| x.is_finite()));
        }
    }

    #[test]
    fn modulated_cutoff_matches_constant_cutoff() {
        let run = |connected: &[(usize, Shape)]| {
            let mut h = Harness::new(&Svf, &Config::new(), connected, 48_000.0, 64).unwrap();
            h.set(IN, 1.0);
            if connected.is_empty() {
                h.set(CUTOFF, 500.0);
            } else {
                h.input(CUTOFF, 64).fill(500.0);
            }
            h.run(64).unwrap();
            h.output(LOW).lane(0, 0).to_vec()
        };
        assert_eq!(run(&[]), run(&[(CUTOFF, Shape::MONO)]));
    }
}
