//! The voltage-controlled amplifier.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Skip,
};

/// Multiplies its input by a linear level. Wire an envelope into `level` to
/// shape a note's loudness; unlike [`Gain`](crate::Gain), whose gain is in dB,
/// a level of 0 is silence.
pub struct Vca;

pub const VCA_ID: &str = "noodle.util.vca";

#[derive(Ports)]
struct VcaPorts {
    #[input("in", "In")]
    input: (),
    #[param("level", "Level", ParamInfo::new(0.0, 1.0, 1.0))]
    level: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = VcaPorts::INPUT;
const LEVEL: usize = VcaPorts::LEVEL;
const OUT: usize = VcaPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: VCA_ID,
    version: 1,
    name: "VCA",
    category: "Utilities",
};

impl NodeType for Vca {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(VcaPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(VcaKernel, setup)))
    }
}

struct VcaKernel;

impl LaneKernel for VcaKernel {
    type State = ();

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[IN, LEVEL])
    }

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let (input, level) = (lane.inputs.get(IN), lane.inputs.get(LEVEL));
        for ((o, x), l) in lane.outputs.get_mut(OUT).iter_mut().zip(input).zip(level) {
            *o = x * l;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    #[test]
    fn scales_by_the_level() {
        let mut h = Harness::new(&Vca, &Config::new(), &[(IN, Shape::MONO)], 48_000.0, 4).unwrap();
        h.input(IN, 4).fill(2.0);
        h.set(LEVEL, 0.25);
        h.run(4).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[0.5; 4]);
    }

    #[test]
    fn a_mono_envelope_shapes_every_voice() {
        let connected = [(IN, Shape::new(3, 1)), (LEVEL, Shape::MONO)];
        let mut h = Harness::new(&Vca, &Config::new(), &connected, 48_000.0, 2).unwrap();
        h.input(IN, 2).fill(1.0);
        h.input(LEVEL, 2).fill(0.5);
        h.run(2).unwrap();
        assert_eq!(h.output(OUT).shape(), Shape::new(3, 1));
        assert_eq!(h.output(OUT).lane(2, 0), &[0.5, 0.5]);
    }

    #[test]
    fn a_silent_input_or_level_gives_a_silent_flagged_lane() {
        let connected = [(IN, Shape::new(3, 1)), (LEVEL, Shape::new(3, 1))];
        let mut h = Harness::new(&Vca, &Config::new(), &connected, 48_000.0, 4).unwrap();
        let mut input = h.input(IN, 4);
        input.lane_mut(0, 0).fill(1.0);
        input.lane_mut(1, 0).fill(1.0);
        input.silence(2, 0);
        let mut level = h.input(LEVEL, 4);
        level.lane_mut(0, 0).fill(1.0);
        level.silence(1, 0);
        level.lane_mut(2, 0).fill(1.0);
        h.run(4).unwrap();
        let out = h.output(OUT);
        assert!(!out.is_silent(0, 0));
        assert_eq!(out.lane(0, 0), &[1.0; 4]);
        assert!(out.is_silent(1, 0) && out.is_silent(2, 0));
        assert!(
            out.lane(1, 0)
                .iter()
                .chain(out.lane(2, 0))
                .all(|&x| x == 0.0)
        );
    }
}
