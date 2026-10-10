//! The remapper.

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType,
    ParamInfo, PerLane, Ports, Setup,
};

use crate::curve::{Curve, lookup};

/// Maps a signal from one range to another along a curve you draw. The input
/// range (`in_min` to `in_max`) is scaled to 0 to 1, the curve gives a height
/// for that position, and the output range (`out_min` to `out_max`) scales it
/// back up. Inputs outside the input range are clamped to its ends. Swap the
/// output range to invert.
///
/// The curve is config (it is edited in the properties panel, and saved as
/// text). When it changes the node is rebuilt, which makes the lookup table
/// off the audio thread; the audio thread then only reads it.
pub struct Remap;

pub const REMAP_ID: &str = "noodle.util.remap";
/// The config key that holds the curve; see [`Curve::to_text`].
pub const REMAP_CURVE_KEY: &str = "curve";

const CURVE: ConfigInfo = ConfigInfo::text(REMAP_CURVE_KEY, "Curve");

#[derive(Ports)]
struct RemapPorts {
    #[input("in", "In")]
    input: (),
    #[param("in_min", "In min", ParamInfo::new(-20_000.0, 20_000.0, 0.0))]
    in_min: (),
    #[param("in_max", "In max", ParamInfo::new(-20_000.0, 20_000.0, 1.0))]
    in_max: (),
    #[param("out_min", "Out min", ParamInfo::new(-20_000.0, 20_000.0, 0.0))]
    out_min: (),
    #[param("out_max", "Out max", ParamInfo::new(-20_000.0, 20_000.0, 1.0))]
    out_max: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = RemapPorts::INPUT;
const IN_MIN: usize = RemapPorts::IN_MIN;
const IN_MAX: usize = RemapPorts::IN_MAX;
const OUT_MIN: usize = RemapPorts::OUT_MIN;
const OUT_MAX: usize = RemapPorts::OUT_MAX;
const OUT: usize = RemapPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: REMAP_ID,
    version: 1,
    name: "Remap",
    category: "Utilities",
};

static SETTINGS: [ConfigInfo; 1] = [CURVE];

impl NodeType for Remap {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &SETTINGS
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(RemapPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let curve = Curve::from_text(&CURVE.get_text(setup.config));
        Ok(Instance::realtime(PerLane::new(
            RemapKernel { lut: curve.lut() },
            setup,
        )))
    }
}

struct RemapKernel {
    lut: Box<[f32]>,
}

impl LaneKernel for RemapKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let input = lane.inputs.get(IN);
        let (in_min, in_max) = (lane.inputs.get(IN_MIN), lane.inputs.get(IN_MAX));
        let (out_min, out_max) = (lane.inputs.get(OUT_MIN), lane.inputs.get(OUT_MAX));
        let out = lane.outputs.get_mut(OUT);
        for i in 0..out.len() {
            let span = in_max[i] - in_min[i];
            let t = if span == 0.0 {
                0.0
            } else {
                (input[i] - in_min[i]) / span
            };
            out[i] = out_min[i] + lookup(&self.lut, t) * (out_max[i] - out_min[i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::Handle;
    use noodle_core::Value;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn run(config: &Config, params: &[(usize, f32)], input: &[f32]) -> Vec<f32> {
        let mut h =
            Harness::new(&Remap, config, &[(IN, Shape::MONO)], 48_000.0, input.len()).unwrap();
        h.input(IN, input.len())
            .lane_mut(0, 0)
            .copy_from_slice(input);
        for &(port, value) in params {
            h.set(port, value);
        }
        h.run(input.len()).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    fn close(a: &[f32], b: &[f32]) {
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() < 2e-3, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn the_default_curve_scales_one_range_to_another() {
        let out = run(
            &Config::new(),
            &[
                (IN_MIN, -1.0),
                (IN_MAX, 1.0),
                (OUT_MIN, 100.0),
                (OUT_MAX, 200.0),
            ],
            &[-1.0, 0.0, 1.0, 0.5],
        );
        close(&out, &[100.0, 150.0, 200.0, 175.0]);
    }

    #[test]
    fn inputs_outside_the_range_clamp() {
        let out = run(&Config::new(), &[], &[-3.0, 7.0, f32::NAN]);
        close(&out, &[0.0, 1.0, 0.0]);
    }

    #[test]
    fn a_swapped_output_range_inverts() {
        let out = run(
            &Config::new(),
            &[(OUT_MIN, 1.0), (OUT_MAX, 0.0)],
            &[0.0, 0.25],
        );
        close(&out, &[1.0, 0.75]);
    }

    #[test]
    fn an_empty_input_range_gives_the_start_of_the_curve() {
        let out = run(&Config::new(), &[(IN_MIN, 1.0), (IN_MAX, 1.0)], &[5.0]);
        close(&out, &[0.0]);
    }

    #[test]
    fn a_configured_curve_shapes_the_output() {
        let mut curve = Curve::linear();
        // An ease-in: slow start.
        curve.move_handle(0, Handle::Out, 0.8, 0.0);
        curve.move_handle(1, Handle::In, 1.0, 1.0);
        let config = Config::new().with("curve", Value::Text(curve.to_text()));
        let out = run(&config, &[], &[0.0, 0.25, 0.5, 1.0]);
        assert!(out[1] < 0.2 && out[2] < 0.45, "{out:?}");
        close(&[out[0], out[3]], &[0.0, 1.0]);
        close(&out[1..3], &[curve.eval(0.25), curve.eval(0.5)]);
    }

    #[test]
    fn a_damaged_curve_falls_back_to_linear() {
        let config = Config::new().with("curve", Value::Text("garbage".into()));
        close(&run(&config, &[], &[0.25]), &[0.25]);
    }
}
