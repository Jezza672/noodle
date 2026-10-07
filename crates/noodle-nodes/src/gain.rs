use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Setup, Unit,
};

pub struct Gain;

const IN: usize = 0;
const GAIN: usize = 1;
const OUT: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: "noodle.util.gain",
    version: 1,
    name: "Gain",
    category: "Utilities",
};

impl NodeType for Gain {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .input("in", "In")
            .param(
                "gain",
                "Gain",
                ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels),
            )
            .output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(GainKernel, setup)))
    }
}

struct GainKernel;

impl LaneKernel for GainKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let input = lane.inputs.get(IN);
        let out = lane.outputs.get_mut(OUT);
        // The gain is rarely modulated, so usually convert from dB once per block.
        if let Some(db) = lane.inputs.constant(GAIN) {
            let gain = db_to_amplitude(db);
            for (o, x) in out.iter_mut().zip(input) {
                *o = x * gain;
            }
        } else {
            let gain = lane.inputs.get(GAIN);
            for ((o, x), db) in out.iter_mut().zip(input).zip(gain) {
                *o = x * db_to_amplitude(*db);
            }
        }
    }
}

fn db_to_amplitude(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    const MINUS_6_DB: f32 = -6.0206;

    #[test]
    fn halves_at_minus_six_db() {
        let mut h =
            Harness::new(&Gain, &Config::new(), &[(IN, Shape::STEREO)], 48_000.0, 4).unwrap();
        h.input(IN, 4).lane_mut(0, 1).fill(1.0);
        h.set(GAIN, MINUS_6_DB);
        h.run(4).unwrap();

        let out = h.output(OUT);
        assert_eq!(out.shape(), Shape::STEREO);
        assert!(out.lane(0, 0).iter().all(|&x| x == 0.0));
        assert!(out.lane(0, 1).iter().all(|&x| (x - 0.5).abs() < 1e-4));
    }

    #[test]
    fn modulated_gain_matches_constant_gain() {
        let connected = [(IN, Shape::MONO), (GAIN, Shape::MONO)];
        let mut h = Harness::new(&Gain, &Config::new(), &connected, 48_000.0, 4).unwrap();
        h.input(IN, 4).fill(1.0);
        h.input(GAIN, 4).fill(MINUS_6_DB);
        h.run(4).unwrap();
        assert!(
            h.output(OUT)
                .lane(0, 0)
                .iter()
                .all(|&x| (x - 0.5).abs() < 1e-4)
        );
    }
}
