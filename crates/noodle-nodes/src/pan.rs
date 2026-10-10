//! The Pan node.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Shape, Skip,
};

/// Places a signal in the stereo field with an equal-power pan law. A mono
/// input becomes stereo; a stereo input is balanced (each channel keeps its
/// side and is scaled by that side's gain). `pan` runs from -1 (hard left)
/// through 0 (middle) to 1 (hard right). The middle is unity in each ear, and
/// the sides are +3 dB, so the total power is the same everywhere.
///
/// It works per voice, and `pan` may be a polyphonic signal, so a Math node
/// can fan voices or unison copies across the field.
pub struct Pan;

pub const PAN_ID: &str = "noodle.util.pan";

#[derive(Ports)]
struct PanPorts {
    #[input("in", "In")]
    input: (),
    #[param("pan", "Pan", ParamInfo::new(-1.0, 1.0, 0.0))]
    pan: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = PanPorts::INPUT;
const PAN: usize = PanPorts::PAN;
const OUT: usize = PanPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: PAN_ID,
    version: 1,
    name: "Pan",
    category: "Utilities",
};

impl NodeType for Pan {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(PanPorts::layout())
    }

    /// Always stereo, with the voices of the inputs.
    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let shape = Shape::broadcast_all(inputs.iter().copied())?;
        if shape.channels > 2 {
            return Err(NodeError::config(
                "Pan takes a mono or stereo signal".to_string(),
            ));
        }
        Ok(vec![Shape::new(shape.voices, 2)])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(PanKernel, setup)))
    }
}

struct PanKernel;

/// The gains for one ear. `right` picks the ear.
fn gain(pan: f32, right: bool) -> f32 {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    std::f32::consts::SQRT_2 * if right { angle.sin() } else { angle.cos() }
}

impl LaneKernel for PanKernel {
    type State = ();

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[IN])
    }

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let right = lane.channel == 1;
        let input = lane.inputs.get(IN);
        let out = lane.outputs.get_mut(OUT);
        match lane.inputs.constant(PAN) {
            Some(pan) => {
                let k = gain(pan, right);
                for (o, x) in out.iter_mut().zip(input) {
                    *o = x * k;
                }
            }
            None => {
                for ((o, x), pan) in out.iter_mut().zip(input).zip(lane.inputs.get(PAN)) {
                    *o = x * gain(*pan, right);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    fn run(pan: f32) -> (f32, f32) {
        let mut h = Harness::new(&Pan, &Config::new(), &[(IN, Shape::MONO)], 48_000.0, 4).unwrap();
        h.input(IN, 4).fill(1.0);
        h.set(PAN, pan);
        h.run(4).unwrap();
        let out = h.output(OUT);
        assert_eq!(out.shape(), Shape::STEREO);
        (out.lane(0, 0)[0], out.lane(0, 1)[0])
    }

    #[test]
    fn the_middle_is_unity_and_the_edges_are_one_sided() {
        let (l, r) = run(0.0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
        let (l, r) = run(-1.0);
        assert!((l - std::f32::consts::SQRT_2).abs() < 1e-6 && r.abs() < 1e-6);
        let (l, r) = run(1.0);
        assert!(l.abs() < 1e-6 && (r - std::f32::consts::SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn power_is_constant_across_the_field() {
        for pan in [-0.9, -0.5, -0.1, 0.3, 0.8] {
            let (l, r) = run(pan);
            assert!((l * l + r * r - 2.0).abs() < 1e-5, "{pan}");
        }
    }

    #[test]
    fn each_voice_has_its_own_pan_and_stereo_input_is_balanced() {
        let poly = Shape::new(2, 1);
        let connected = [(IN, Shape::STEREO), (PAN, poly)];
        let mut h = Harness::new(&Pan, &Config::new(), &connected, 48_000.0, 2).unwrap();
        h.input(IN, 2).fill(1.0);
        let mut pan = h.input(PAN, 2);
        pan.lane_mut(0, 0).fill(-1.0);
        pan.lane_mut(1, 0).fill(1.0);
        h.run(2).unwrap();
        let out = h.output(OUT);
        assert_eq!(out.shape(), Shape::new(2, 2));
        assert!(out.lane(0, 0)[0] > 1.0 && out.lane(0, 1)[0].abs() < 1e-6);
        assert!(out.lane(1, 0)[0].abs() < 1e-6 && out.lane(1, 1)[0] > 1.0);
    }

    #[test]
    fn rejects_more_than_two_channels() {
        let layout = Pan.layout(&Config::new()).unwrap();
        assert!(
            Pan.output_shapes(&Config::new(), &layout, &[Shape::new(1, 6), Shape::MONO])
                .is_err()
        );
    }

    #[test]
    fn a_silent_voice_is_skipped_and_flagged() {
        let poly = Shape::new(2, 1);
        let mut h = Harness::new(&Pan, &Config::new(), &[(IN, poly)], 48_000.0, 4).unwrap();
        let mut input = h.input(IN, 4);
        input.lane_mut(0, 0).fill(1.0);
        input.silence(1, 0);
        h.run(4).unwrap();
        let out = h.output(OUT);
        assert!(!out.is_silent(0, 0));
        assert!(out.is_silent(1, 0) && out.is_silent(1, 1));
    }
}
