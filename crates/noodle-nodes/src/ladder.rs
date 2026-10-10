//! The ladder filter.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Skip, Unit,
};

/// A four-pole (24 dB per octave) low-pass filter in the style of a Moog
/// ladder, with a saturating stage that makes it warm when pushed.
///
/// `resonance` feeds the last stage back to the input. At 1 it rings by
/// itself (the saturation keeps it bounded) and sings at the cutoff; as
/// resonance rises the passband gets quieter, as on the hardware. `drive`
/// scales the input before the saturation, so more of it distorts.
///
/// The feedback comes from the previous sample, so at cutoffs near the top
/// of the spectrum the resonance peak sits a little below the dial.
pub struct Ladder;

#[derive(Ports)]
struct LadderPorts {
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
    #[param("drive", "Drive", ParamInfo::new(0.1, 10.0, 1.0).log())]
    drive: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = LadderPorts::INPUT;
const CUTOFF: usize = LadderPorts::CUTOFF;
const RESONANCE: usize = LadderPorts::RESONANCE;
const DRIVE: usize = LadderPorts::DRIVE;
const OUT: usize = LadderPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: "noodle.filter.ladder",
    version: 1,
    name: "Ladder Filter",
    category: "Filters",
};

impl NodeType for Ladder {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(LadderPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(LadderKernel, setup)))
    }
}

struct LadderKernel;

impl LaneKernel for LadderKernel {
    type State = LadderState;

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[IN])
    }

    fn is_idle(&self, state: &LadderState) -> bool {
        state.memory.iter().all(|m| m.abs() < IDLE)
    }

    fn process_lane(&mut self, state: &mut LadderState, ctx: &Context, mut lane: Lane<'_, '_>) {
        let input = lane.inputs.get(IN);
        let out = lane.outputs.get_mut(OUT);

        // The coefficient costs an exp(), so when cutoff isn't modulated
        // compute it once per block.
        let fixed = lane
            .inputs
            .constant(CUTOFF)
            .map(|cutoff| coefficient(cutoff, ctx.sample_rate));
        let (cutoff, resonance, drive) = (
            lane.inputs.get(CUTOFF),
            lane.inputs.get(RESONANCE),
            lane.inputs.get(DRIVE),
        );

        for (i, (o, &x)) in out.iter_mut().zip(input).enumerate() {
            let g = fixed.unwrap_or_else(|| coefficient(cutoff[i], ctx.sample_rate));
            *o = state.tick(x * drive[i], g, resonance[i].clamp(0.0, 1.0) * FEEDBACK);
        }
        state.tidy();
    }
}

/// The feedback gain that makes the ladder ring by itself: 4 would just hold
/// a tone, and the saturation takes a little off the top.
const FEEDBACK: f32 = 4.3;

/// The one-pole coefficient for a cutoff, `g / (1 + g)` for the pre-warped
/// `g`, as a trapezoidal (zero-delay) one-pole uses it.
fn coefficient(cutoff: f32, sample_rate: f32) -> f32 {
    let cutoff = cutoff.clamp(10.0, sample_rate * 0.45);
    let g = (std::f32::consts::PI * cutoff / sample_rate).tan();
    g / (1.0 + g)
}

/// A cheap tanh: exact slope at 0, and ±1 past ±3.
fn saturate(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

#[derive(Default)]
struct LadderState {
    /// The four one-poles' memories.
    memory: [f32; 4],
}

impl LadderState {
    /// One sample. The four one-poles are trapezoidal, and the feedback is
    /// solved within the sample rather than delayed by one, so the cutoff and
    /// the point where it rings are right at every pitch. The saturation sits
    /// at the input of the stages, after the feedback is subtracted.
    fn tick(&mut self, x: f32, big_g: f32, k: f32) -> f32 {
        let g = big_g;
        let rest = 1.0 - g;
        let g2 = g * g;
        let g4 = g2 * g2;
        // What the memories contribute to the last stage's output.
        let [m0, m1, m2, m3] = self.memory;
        let from_memory = rest * (g2 * g * m0 + g2 * m1 + g * m2 + m3);
        let y4 = (g4 * x + from_memory) / (1.0 + k * g4);

        let mut stage_in = saturate(x - k * y4);
        for m in &mut self.memory {
            let out = g * stage_in + rest * *m;
            *m = 2.0 * out - *m;
            stage_in = out;
        }
        stage_in
    }

    /// Once per block: resets state that an infinite or NaN input made
    /// non-finite, and flushes state too small to hear to zero, so it doesn't
    /// decay into the slow subnormal range.
    fn tidy(&mut self) {
        if !self.memory.iter().all(|m| m.is_finite()) {
            *self = Self::default();
        }
        for m in &mut self.memory {
            if m.abs() < TINY {
                *m = 0.0;
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
    use std::f32::consts::TAU;

    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;

    /// Runs a sine through the filter (after letting it settle) and returns
    /// the output's peak.
    fn gain_at(hz: f32, cutoff: f32, resonance: f32) -> f32 {
        let mut h =
            Harness::new(&Ladder, &Config::new(), &[(IN, Shape::MONO)], RATE, 4800).unwrap();
        h.set(CUTOFF, cutoff);
        h.set(RESONANCE, resonance);
        let mut peak = 0f32;
        for block in 0..4 {
            for (i, x) in h.input(IN, 4800).lane_mut(0, 0).iter_mut().enumerate() {
                let n = (block * 4800 + i) as f32;
                *x = 0.1 * (TAU * hz * n / RATE).sin();
            }
            h.run(4800).unwrap();
            if block == 3 {
                peak = h
                    .output(OUT)
                    .lane(0, 0)
                    .iter()
                    .fold(0f32, |m, x| m.max(x.abs()));
            }
        }
        peak / 0.1
    }

    #[test]
    fn passes_dc_unchanged_without_resonance() {
        let mut h = Harness::new(&Ladder, &Config::new(), &[], RATE, 480).unwrap();
        h.set(IN, 0.05);
        for _ in 0..20 {
            h.run(480).unwrap();
        }
        let last = *h.output(OUT).lane(0, 0).last().unwrap();
        assert!((last - 0.05).abs() < 1e-3, "{last}");
    }

    #[test]
    fn rolls_off_at_24_db_per_octave() {
        let pass = gain_at(100.0, 2_000.0, 0.0);
        assert!((pass - 1.0).abs() < 0.05, "{pass}");
        // Two octaves above the cutoff: 48 dB down from the passband, less the
        // gentler slope near the corner.
        let stop = gain_at(8_000.0, 2_000.0, 0.0);
        assert!(stop < 0.01, "{stop}");
        // One further octave: another ~24 dB.
        let octave = gain_at(16_000.0, 2_000.0, 0.0);
        assert!(octave < stop / 4.0, "{octave} vs {stop}");
    }

    #[test]
    fn resonance_peaks_at_the_cutoff_and_thins_the_passband() {
        let flat = gain_at(1_000.0, 1_000.0, 0.0);
        let peaked = gain_at(1_000.0, 1_000.0, 0.9);
        assert!(peaked > flat * 2.0, "{peaked} vs {flat}");
        let low = gain_at(50.0, 1_000.0, 0.9);
        assert!(low < 0.5, "the passband loses level: {low}");
    }

    #[test]
    fn full_resonance_rings_by_itself_at_any_cutoff_but_stays_bounded() {
        for cutoff in [200.0, 1_000.0, 5_000.0] {
            let mut h =
                Harness::new(&Ladder, &Config::new(), &[(IN, Shape::MONO)], RATE, 480).unwrap();
            h.set(CUTOFF, cutoff);
            h.set(RESONANCE, 1.0);
            // A single click, then silence.
            h.input(IN, 480).lane_mut(0, 0)[0] = 1.0;
            h.run(480).unwrap();
            let mut late = 0f32;
            for _ in 0..200 {
                h.run(480).unwrap();
                let out = h.output(OUT).lane(0, 0);
                late = out.iter().fold(0f32, |m, x| m.max(x.abs()));
                assert!(out.iter().all(|x| x.is_finite() && x.abs() < 2.0));
            }
            assert!(late > 0.05, "{cutoff} Hz: still ringing after 2 s: {late}");
        }
    }

    #[test]
    fn silence_decays_to_exact_zero() {
        let mut h = Harness::new(&Ladder, &Config::new(), &[(IN, Shape::MONO)], RATE, 512).unwrap();
        h.set(CUTOFF, 200.0);
        h.input(IN, 512).lane_mut(0, 0).fill(1.0);
        h.run(512).unwrap();
        h.input(IN, 512).lane_mut(0, 0).fill(0.0);
        for _ in 0..3750 {
            h.run(512).unwrap();
        }
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn recovers_from_an_infinite_input() {
        let mut h = Harness::new(&Ladder, &Config::new(), &[(IN, Shape::MONO)], RATE, 64).unwrap();
        h.input(IN, 64).lane_mut(0, 0)[10] = f32::INFINITY;
        h.run(64).unwrap();
        h.input(IN, 64).fill(0.5);
        h.run(64).unwrap();
        h.run(64).unwrap();
        assert!(h.output(OUT).lane(0, 0).iter().all(|x| x.is_finite()));
    }

    #[test]
    fn a_silent_lane_is_skipped_once_it_has_rung_out() {
        let mut h = Harness::new(&Ladder, &Config::new(), &[(IN, Shape::MONO)], RATE, 256).unwrap();
        h.set(CUTOFF, 4_000.0);
        h.input(IN, 256).lane_mut(0, 0).fill(1.0);
        h.run(256).unwrap();
        assert!(!h.output(OUT).is_silent(0, 0));
        // Input goes silent but is flagged: the tail still rings first.
        h.input(IN, 256).fill(0.0);
        h.input(IN, 256).set_silent(0, 0);
        h.run(256).unwrap();
        assert!(!h.output(OUT).is_silent(0, 0), "the tail is not cut");
        assert!(h.output(OUT).lane(0, 0).iter().any(|&x| x != 0.0));
        for _ in 0..64 {
            h.run(256).unwrap();
        }
        assert!(h.output(OUT).is_silent(0, 0));
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 0.0));
    }
}
