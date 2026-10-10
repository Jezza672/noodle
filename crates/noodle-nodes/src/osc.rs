//! Oscillators.

use std::f32::consts::TAU;

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Skip, Unit,
};

/// A sine oscillator. Its frequency input runs at audio rate, so the same node
/// serves as an LFO and supports through-zero FM.
pub struct Sine;

/// A band-limited sawtooth oscillator, ramping from -1 up to 1. Like
/// [`Sine`], its frequency runs at audio rate.
pub struct Saw;

/// A band-limited pulse wave (PolyBLEP on both edges). Its `width` is the
/// fraction of each cycle spent high; at 0.5 it is a square. A width other
/// than 0.5 adds a DC offset, as any pulse wave does.
pub struct Square;

/// A band-limited triangle wave (PolyBLAMP at its two corners), starting at 0
/// and rising.
pub struct Triangle;

#[derive(Ports)]
struct OscPorts {
    #[param(
        "frequency",
        "Frequency",
        ParamInfo::new(0.01, 20_000.0, 440.0)
            .log()
            .unit(Unit::Hertz)
    )]
    frequency: (),
    #[output("out", "Out")]
    out: (),
}

#[derive(Ports)]
struct PulsePorts {
    #[param(
        "frequency",
        "Frequency",
        ParamInfo::new(0.01, 20_000.0, 440.0)
            .log()
            .unit(Unit::Hertz)
    )]
    frequency: (),
    #[param("width", "Width", ParamInfo::new(0.01, 0.99, 0.5).offset())]
    width: (),
    #[output("out", "Out")]
    out: (),
}

const FREQUENCY: usize = OscPorts::FREQUENCY;
const OUT: usize = OscPorts::OUT;
const WIDTH: usize = PulsePorts::WIDTH;

static SINE: NodeInfo = NodeInfo {
    id: "noodle.osc.sine",
    version: 1,
    name: "Sine",
    category: "Generators",
};

static SQUARE: NodeInfo = NodeInfo {
    id: "noodle.osc.square",
    version: 1,
    name: "Square",
    category: "Generators",
};

static TRIANGLE: NodeInfo = NodeInfo {
    id: "noodle.osc.triangle",
    version: 1,
    name: "Triangle",
    category: "Generators",
};

static SAW: NodeInfo = NodeInfo {
    id: "noodle.osc.saw",
    version: 1,
    name: "Saw",
    category: "Generators",
};

impl NodeType for Sine {
    fn info(&self) -> &NodeInfo {
        &SINE
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(OscPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(SineKernel, setup)))
    }
}

struct SineKernel;

impl LaneKernel for SineKernel {
    /// Phase in cycles, from 0 to 1.
    type State = f32;

    /// No pitch, no sound: a voice the Voices node isn't using has a silent
    /// frequency.
    fn skip(&self) -> Skip {
        Skip::AnySilent(&[FREQUENCY])
    }

    fn process_lane(&mut self, phase: &mut f32, ctx: &Context, mut lane: Lane<'_, '_>) {
        let frequency = lane.inputs.get(FREQUENCY);
        let seconds_per_sample = 1.0 / ctx.sample_rate;
        for (out, f) in lane.outputs.get_mut(OUT).iter_mut().zip(frequency) {
            *out = (*phase * TAU).sin();
            *phase = (*phase + f * seconds_per_sample).rem_euclid(1.0);
        }
        recover(phase);
    }
}

impl NodeType for Saw {
    fn info(&self) -> &NodeInfo {
        &SAW
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(OscPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(SawKernel, setup)))
    }
}

struct SawKernel;

impl LaneKernel for SawKernel {
    /// Phase in cycles, from 0 to 1.
    type State = f32;

    /// No pitch, no sound: a voice the Voices node isn't using has a silent
    /// frequency.
    fn skip(&self) -> Skip {
        Skip::AnySilent(&[FREQUENCY])
    }

    fn process_lane(&mut self, phase: &mut f32, ctx: &Context, mut lane: Lane<'_, '_>) {
        let frequency = lane.inputs.get(FREQUENCY);
        let seconds_per_sample = 1.0 / ctx.sample_rate;
        for (out, f) in lane.outputs.get_mut(OUT).iter_mut().zip(frequency) {
            let step = f * seconds_per_sample;
            *out = 2.0 * *phase - 1.0 - poly_blep(*phase, step.abs());
            *phase = (*phase + step).rem_euclid(1.0);
        }
        recover(phase);
    }
}

impl NodeType for Square {
    fn info(&self) -> &NodeInfo {
        &SQUARE
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(PulsePorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(SquareKernel, setup)))
    }
}

struct SquareKernel;

impl LaneKernel for SquareKernel {
    /// Phase in cycles, from 0 to 1.
    type State = f32;

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[FREQUENCY])
    }

    fn process_lane(&mut self, phase: &mut f32, ctx: &Context, mut lane: Lane<'_, '_>) {
        let frequency = lane.inputs.get(FREQUENCY);
        let width = lane.inputs.get(WIDTH);
        let seconds_per_sample = 1.0 / ctx.sample_rate;
        for ((out, f), w) in lane
            .outputs
            .get_mut(OUT)
            .iter_mut()
            .zip(frequency)
            .zip(width)
        {
            let step = f * seconds_per_sample;
            let width = w.clamp(0.01, 0.99);
            let naive = if *phase < width { 1.0 } else { -1.0 };
            // Up at the start of the cycle, down at the width.
            let falling = (*phase - width).rem_euclid(1.0);
            *out = naive + poly_blep(*phase, step.abs()) - poly_blep(falling, step.abs());
            *phase = (*phase + step).rem_euclid(1.0);
        }
        recover(phase);
    }
}

impl NodeType for Triangle {
    fn info(&self) -> &NodeInfo {
        &TRIANGLE
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(OscPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(TriangleKernel, setup)))
    }
}

struct TriangleKernel;

impl LaneKernel for TriangleKernel {
    /// Phase in cycles, from 0 to 1.
    type State = f32;

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[FREQUENCY])
    }

    fn process_lane(&mut self, phase: &mut f32, ctx: &Context, mut lane: Lane<'_, '_>) {
        let frequency = lane.inputs.get(FREQUENCY);
        let seconds_per_sample = 1.0 / ctx.sample_rate;
        for (out, f) in lane.outputs.get_mut(OUT).iter_mut().zip(frequency) {
            let step = f * seconds_per_sample;
            let dt = step.abs().min(0.5);
            // 0 at phase 0, rising to 1 at a quarter, down to -1 at three
            // quarters. Its slope changes by -8 (per cycle) at the peak and
            // +8 at the trough.
            let naive = 1.0 - 4.0 * ((*phase + 0.25).rem_euclid(1.0) - 0.5).abs();
            *out =
                naive - 8.0 * poly_blamp(*phase - 0.25, dt) + 8.0 * poly_blamp(*phase - 0.75, dt);
            *phase = (*phase + step).rem_euclid(1.0);
        }
        recover(phase);
    }
}

/// Restarts a phase that an infinite or NaN frequency made NaN, which would
/// otherwise stay NaN for good. Once per block is enough.
fn recover(phase: &mut f32) {
    if !phase.is_finite() {
        *phase = 0.0;
    }
}

/// Rounds off a saw's jump where its phase wraps, which removes most of the
/// aliasing a naive saw has (PolyBLEP). `step` is the phase advance per
/// sample, in cycles.
fn poly_blep(phase: f32, step: f32) -> f32 {
    if step <= 0.0 {
        0.0
    } else if phase < step {
        let t = phase / step;
        2.0 * t - t * t - 1.0
    } else if phase > 1.0 - step {
        let t = (phase - 1.0) / step;
        t * t + 2.0 * t + 1.0
    } else {
        0.0
    }
}

/// Rounds off a corner (a jump in slope) where the phase is `distance`
/// cycles from it, which removes most of the aliasing a naive triangle has
/// (PolyBLAMP). Multiply by the slope change per cycle. `step` is the phase
/// advance per sample, in cycles.
fn poly_blamp(distance: f32, step: f32) -> f32 {
    if step <= 0.0 {
        return 0.0;
    }
    let wrapped = (distance + 0.5).rem_euclid(1.0) - 0.5;
    let u = wrapped.abs() / step;
    if u < 1.0 {
        let v = 1.0 - u;
        step * v * v * v / 6.0
    } else {
        0.0
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

    #[test]
    fn saw_ramps_up_and_wraps() {
        let mut h = Harness::new(&Saw, &Config::new(), &[], 48_000.0, 96).unwrap();
        h.set(FREQUENCY, 1_000.0);
        h.run(96).unwrap();
        let out = h.output(OUT).lane(0, 0);
        // Away from the wrap, it's the plain ramp: a quarter, half and three
        // quarters of the way through a 48-sample cycle.
        assert!((out[12] + 0.5).abs() < 1e-4);
        assert!(out[24].abs() < 1e-4);
        assert!((out[36] - 0.5).abs() < 1e-4);
        // At the wrap, PolyBLEP lands halfway instead of jumping.
        assert!(out[48].abs() < 1e-4);
    }

    #[test]
    fn oscillators_recover_from_an_infinite_frequency() {
        for node in [&Sine as &dyn NodeType, &Saw] {
            let connected = [(FREQUENCY, Shape::MONO)];
            let mut h = Harness::new(node, &Config::new(), &connected, 48_000.0, 64).unwrap();
            let mut frequency = h.input(FREQUENCY, 64);
            frequency.fill(440.0);
            frequency.lane_mut(0, 0)[10] = f32::INFINITY;
            h.run(64).unwrap();
            h.input(FREQUENCY, 64).fill(440.0);
            h.run(64).unwrap();
            let out = h.output(OUT).lane(0, 0);
            assert!(out.iter().all(|x| x.is_finite()), "{}", node.info().id);
        }
    }

    #[test]
    fn saw_has_no_dc_offset() {
        let mut h = Harness::new(&Saw, &Config::new(), &[], 48_000.0, 4800).unwrap();
        h.set(FREQUENCY, 1_000.0);
        h.run(4800).unwrap();
        let out = h.output(OUT).lane(0, 0);
        let mean = out.iter().sum::<f32>() / out.len() as f32;
        assert!(mean.abs() < 1e-3, "{mean}");
    }

    /// How much of `samples` is a sine at `hz` (Goertzel).
    fn strength(samples: &[f32], hz: f32) -> f32 {
        let w = TAU * hz / 48_000.0;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &x) in samples.iter().enumerate() {
            re += x * (w * i as f32).cos();
            im += x * (w * i as f32).sin();
        }
        2.0 * (re * re + im * im).sqrt() / samples.len() as f32
    }

    fn render(node: &dyn NodeType, hz: f32, frames: usize) -> Vec<f32> {
        let mut h = Harness::new(node, &Config::new(), &[], 48_000.0, frames).unwrap();
        h.set(FREQUENCY, hz);
        h.run(frames).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    #[test]
    fn square_is_high_then_low_and_width_moves_the_edge() {
        let mut h = Harness::new(&Square, &Config::new(), &[], 48_000.0, 96).unwrap();
        h.set(FREQUENCY, 1_000.0);
        h.run(96).unwrap();
        let out = h.output(OUT).lane(0, 0);
        // Away from the edges, 48 samples a cycle, half high.
        assert!((out[12] - 1.0).abs() < 1e-4 && (out[36] + 1.0).abs() < 1e-4);
        assert!((out[60] - 1.0).abs() < 1e-4);

        h.set(WIDTH, 0.25);
        h.run(96).unwrap();
        let out = h.output(OUT).lane(0, 0);
        let high = out.iter().filter(|&&x| x > 0.99).count();
        assert!((20..=28).contains(&high), "a quarter of 96 samples: {high}");
    }

    #[test]
    fn triangle_starts_at_zero_and_peaks_at_a_quarter() {
        let out = render(&Triangle, 1_000.0, 96);
        assert!(out[0].abs() < 0.03, "{}", out[0]);
        // Peaks and troughs are rounded off a little, by about a dt.
        assert!((out[12] - 1.0).abs() < 0.05, "{}", out[12]);
        assert!((out[36] + 1.0).abs() < 0.05, "{}", out[36]);
        // Straight lines between: halfway up is 0.5.
        assert!((out[6] - 0.5).abs() < 0.02, "{}", out[6]);
    }

    #[test]
    fn square_and_triangle_have_no_dc_offset_at_half_width() {
        for node in [&Square as &dyn NodeType, &Triangle] {
            let out = render(node, 1_000.0, 4800);
            let mean = out.iter().sum::<f32>() / out.len() as f32;
            assert!(mean.abs() < 1e-3, "{}: {mean}", node.info().id);
        }
    }

    /// A high note folds its harmonics back down. The band-limited waves
    /// leave far less at an alias than the naive ones do.
    #[test]
    fn band_limiting_cuts_aliasing() {
        let hz = 7_123.0;
        let frames = 9_600;
        // The 7th harmonic is 49 861 Hz, which folds to 1 861 Hz.
        let alias = 49_861.0 - 48_000.0;

        let naive_square: Vec<f32> = (0..frames)
            .map(|n| {
                if (n as f32 * hz / 48_000.0).fract() < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            })
            .collect();
        let square = render(&Square, hz, frames);
        let (naive, blep) = (strength(&naive_square, alias), strength(&square, alias));
        assert!(blep < naive / 3.0, "square: {blep} vs {naive}");

        let naive_saw: Vec<f32> = (0..frames)
            .map(|n| 2.0 * (n as f32 * hz / 48_000.0).fract() - 1.0)
            .collect();
        let saw = render(&Saw, hz, frames);
        let (naive, blep) = (strength(&naive_saw, alias), strength(&saw, alias));
        assert!(blep < naive / 3.0, "saw: {blep} vs {naive}");

        // The triangle's harmonics fall as 1/n², so check its 9th: 64 107 Hz
        // folds to 16 107 Hz.
        let alias = 64_107.0 - 48_000.0;
        let naive_triangle: Vec<f32> = (0..frames)
            .map(|n| 1.0 - 4.0 * ((n as f32 * hz / 48_000.0 + 0.25).fract() - 0.5).abs())
            .collect();
        let triangle = render(&Triangle, hz, frames);
        let (naive, blamp) = (strength(&naive_triangle, alias), strength(&triangle, alias));
        assert!(blamp < naive / 2.0, "triangle: {blamp} vs {naive}");
    }

    #[test]
    fn a_silent_frequency_lane_makes_a_silent_flagged_lane() {
        for node in [&Sine as &dyn NodeType, &Saw, &Square, &Triangle] {
            let poly = Shape::new(2, 1);
            let mut h =
                Harness::new(node, &Config::new(), &[(FREQUENCY, poly)], 48_000.0, 32).unwrap();
            let mut frequency = h.input(FREQUENCY, 32);
            frequency.lane_mut(0, 0).fill(1_000.0);
            frequency.silence(1, 0);
            h.run(32).unwrap();
            let out = h.output(OUT);
            let id = node.info().id;
            assert!(!out.is_silent(0, 0), "{id}");
            assert!(out.lane(0, 0).iter().any(|&x| x != 0.0), "{id}");
            assert!(out.is_silent(1, 0), "{id}");
            assert!(out.lane(1, 0).iter().all(|&x| x == 0.0), "{id}");
        }
    }
}
