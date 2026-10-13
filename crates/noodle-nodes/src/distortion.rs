//! The Distortion node.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Skip, Unit,
};

/// A waveshaper: `drive` pushes the signal into a curve that bends it, the
/// way an overdriven amplifier or a clipping stage does, and `mix` blends the
/// result with the dry signal.
///
/// - **Soft** is a tanh curve: it rounds off peaks and adds odd harmonics
///   that grow gently with the drive.
/// - **Hard** clips flat at ±1, which is harsher and brighter.
/// - **Fold** reflects the signal back from ±1 instead of flattening it,
///   so a hard-driven sine keeps ringing with new overtones (a wavefolder).
/// - **Asymmetric** bends the positive half of the wave more than the
///   negative, which adds even harmonics, as a tube stage does.
///
/// The wet signal is scaled by `level` (in decibels), which is how to bring
/// the volume back down after turning the drive up. Every shape maps 0 to 0
/// and stays between -1 and 1, so silence stays silent (and a silent lane
/// costs nothing) and nothing runs away.
pub struct Distortion;

pub const DISTORTION_ID: &str = "noodle.effect.distortion";

/// The shapes, in the order of the `shape` choice.
pub const SHAPES: [&str; 4] = ["Soft", "Hard", "Fold", "Asymmetric"];

#[derive(Ports)]
struct DistortionPorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "drive",
        "Drive",
        ParamInfo::new(0.0, 48.0, 12.0).unit(Unit::Decibels).offset()
    )]
    drive: (),
    #[param("shape", "Shape", ParamInfo::choice(SHAPES))]
    shape: (),
    #[param(
        "level",
        "Level",
        ParamInfo::new(-48.0, 12.0, -6.0).unit(Unit::Decibels)
    )]
    level: (),
    #[param("mix", "Mix", ParamInfo::new(0.0, 1.0, 1.0))]
    mix: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = DistortionPorts::INPUT;
const DRIVE: usize = DistortionPorts::DRIVE;
const SHAPE: usize = DistortionPorts::SHAPE;
const LEVEL: usize = DistortionPorts::LEVEL;
const MIX: usize = DistortionPorts::MIX;
const OUT: usize = DistortionPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: DISTORTION_ID,
    version: 1,
    name: "Distortion",
    category: "Effects",
};

impl NodeType for Distortion {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(DistortionPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(DistortionKernel, setup)))
    }
}

/// Runs `x` (already driven) through the curve for `shape`.
pub fn shaper(shape: usize, x: f32) -> f32 {
    match shape {
        0 => x.tanh(),
        1 => x.clamp(-1.0, 1.0),
        2 => fold(x),
        // A tanh biased off centre, moved and scaled to pass through 0 and
        // stay between -1 and 1: the positive half saturates sooner than the
        // negative, which adds even harmonics.
        _ => ((x + ASYMMETRY).tanh() - ASYMMETRY.tanh()) / (1.0 + ASYMMETRY.tanh()),
    }
}

/// How far off centre the asymmetric curve sits.
const ASYMMETRY: f32 = 0.4;

/// A triangle-wave fold: x for |x| <= 1, then reflected at ±1, forever.
fn fold(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    // Period 4: rising 0..1, falling 1..-1, rising -1..0.
    let t = (x + 1.0).rem_euclid(4.0);
    if t < 2.0 { t - 1.0 } else { 3.0 - t }
}

struct DistortionKernel;

impl LaneKernel for DistortionKernel {
    type State = ();

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[IN])
    }

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let (input, drive) = (lane.inputs.get(IN), lane.inputs.get(DRIVE));
        let (shape, level) = (lane.inputs.get(SHAPE), lane.inputs.get(LEVEL));
        let mix = lane.inputs.get(MIX);
        let out = lane.outputs.get_mut(OUT);
        for i in 0..out.len() {
            let gain = 10f32.powf(drive[i] / 20.0);
            let wet = shaper(shape[i].round().max(0.0) as usize, input[i] * gain)
                * 10f32.powf(level[i] / 20.0);
            let mix = mix[i].clamp(0.0, 1.0);
            out[i] = input[i] + (wet - input[i]) * mix;
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
            &Distortion,
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

    fn sine(frames: usize, amplitude: f32) -> Vec<f32> {
        (0..frames)
            .map(|i| amplitude * (i as f32 * 0.05).sin())
            .collect()
    }

    #[test]
    fn every_shape_keeps_silence_silent_and_stays_bounded() {
        for shape in 0..4 {
            assert_eq!(shaper(shape, 0.0), 0.0, "shape {shape}");
            for x in [-100.0, -3.0, -0.5, 0.5, 3.0, 100.0, f32::INFINITY] {
                let y = shaper(shape, x);
                assert!(y.abs() <= 1.0 + 1e-6, "shape {shape} at {x} gave {y}");
            }
        }
    }

    #[test]
    fn hard_clipping_flattens_peaks_at_the_level() {
        // +12 dB of drive on a full-scale sine, hard clipped, 0 dB level.
        let out = run(
            &sine(2_000, 1.0),
            &[(DRIVE, 12.0), (SHAPE, 1.0), (LEVEL, 0.0)],
        );
        let flat = out
            .iter()
            .filter(|&&x| (x.abs() - 1.0).abs() < 1e-6)
            .count();
        assert!(flat > 500, "only {flat} samples are on the rails");
        assert!(out.iter().all(|x| x.abs() <= 1.0));
    }

    #[test]
    fn soft_clipping_is_near_linear_at_low_drive() {
        let input = sine(1_000, 0.01);
        let out = run(&input, &[(DRIVE, 0.0), (SHAPE, 0.0), (LEVEL, 0.0)]);
        for (x, y) in input.iter().zip(&out) {
            assert!((x - y).abs() < 1e-5);
        }
    }

    #[test]
    fn folding_reflects_instead_of_clipping() {
        assert!((fold(1.5) - 0.5).abs() < 1e-6);
        assert!((fold(-1.5) + 0.5).abs() < 1e-6);
        assert!((fold(2.0) - 0.0).abs() < 1e-6);
        assert!((fold(3.0) + 1.0).abs() < 1e-6);
        assert!((fold(0.3) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn the_asymmetric_shape_adds_a_dc_offset_a_symmetric_one_does_not() {
        let input = sine(4_000, 0.8);
        let mean = |out: &[f32]| out.iter().sum::<f32>() / out.len() as f32;
        let soft = run(&input, &[(DRIVE, 12.0), (SHAPE, 0.0), (LEVEL, 0.0)]);
        let asym = run(&input, &[(DRIVE, 12.0), (SHAPE, 3.0), (LEVEL, 0.0)]);
        assert!(mean(&soft).abs() < 0.01, "{}", mean(&soft));
        assert!(mean(&asym).abs() > 0.02, "{}", mean(&asym));
    }

    #[test]
    fn mix_blends_the_dry_signal_in() {
        let input = sine(1_000, 0.5);
        let dry = run(&input, &[(MIX, 0.0)]);
        assert_eq!(dry, input);
        let wet = run(&input, &[(MIX, 1.0)]);
        let half = run(&input, &[(MIX, 0.5)]);
        for i in 0..input.len() {
            assert!((half[i] - 0.5 * (wet[i] + input[i])).abs() < 1e-5);
        }
    }
}
