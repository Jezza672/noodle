use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Setup, Unit,
};

/// What a group's boundary node becomes when its gain or mute is in use. The
/// compiler swaps it in (see `flatten`); users don't add it themselves.
pub struct GroupStage;

const IN: usize = 0;
const GAIN: usize = 1;
const MUTE: usize = 2;
const SOLO_MUTE: usize = 3;
const OUT: usize = 0;

static INFO: NodeInfo = NodeInfo {
    id: "noodle.group.stage",
    version: 1,
    name: "Group stage",
    category: noodle_engine::INTERNAL_CATEGORY,
};

impl NodeType for GroupStage {
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
            // Smoothed like any continuous parameter, so muting ramps down
            // instead of clicking.
            .param("mute", "Mute", ParamInfo::new(0.0, 1.0, 0.0))
            // Another track's solo. Separate from mute so a lane driving the
            // mute still can't make a soloed-out track audible.
            .param("solo_mute", "Solo mute", ParamInfo::new(0.0, 1.0, 0.0))
            .output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(StageKernel, setup)))
    }
}

struct StageKernel;

impl LaneKernel for StageKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let input = lane.inputs.get(IN);
        let gain = lane.inputs.get(GAIN);
        let mute = lane.inputs.get(MUTE);
        let solo_mute = lane.inputs.get(SOLO_MUTE);
        let out = lane.outputs.get_mut(OUT);
        for ((((o, x), db), mute), solo_mute) in
            out.iter_mut().zip(input).zip(gain).zip(mute).zip(solo_mute)
        {
            let muted = mute.max(*solo_mute).clamp(0.0, 1.0);
            *o = x * 10f32.powf(db / 20.0) * (1.0 - muted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn run(gain_db: f32, mute: f32) -> Vec<f32> {
        let mut h = Harness::new(
            &GroupStage,
            &Config::new(),
            &[(IN, Shape::MONO)],
            48_000.0,
            4,
        )
        .unwrap();
        h.input(IN, 4).fill(1.0);
        h.set(GAIN, gain_db);
        h.set(MUTE, mute);
        h.run(4).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    #[test]
    fn names_match_what_flatten_emits() {
        use noodle_core::group::{
            GAIN as GAIN_KEY, GROUP_STAGE, MUTE as MUTE_KEY, SOLO_MUTE as SOLO_KEY,
        };
        assert_eq!(INFO.id, GROUP_STAGE);
        let layout = GroupStage.layout(&Config::new()).unwrap();
        assert_eq!(layout.inputs[GAIN].key, GAIN_KEY);
        assert_eq!(layout.inputs[MUTE].key, MUTE_KEY);
        assert_eq!(layout.inputs[SOLO_MUTE].key, SOLO_KEY);
    }

    #[test]
    fn passes_the_signal_at_its_gain() {
        assert!(run(0.0, 0.0).iter().all(|&x| (x - 1.0).abs() < 1e-6));
        assert!(run(-6.0206, 0.0).iter().all(|&x| (x - 0.5).abs() < 1e-4));
    }

    #[test]
    fn mute_silences_it_whatever_the_gain() {
        assert!(run(0.0, 1.0).iter().all(|&x| x == 0.0));
        assert!(run(12.0, 1.0).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn a_solo_mute_silences_it_whatever_the_mute() {
        let mut h = Harness::new(
            &GroupStage,
            &Config::new(),
            &[(IN, Shape::MONO)],
            48_000.0,
            4,
        )
        .unwrap();
        h.input(IN, 4).fill(1.0);
        h.set(MUTE, 0.0);
        h.set(SOLO_MUTE, 1.0);
        h.run(4).unwrap();
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 0.0));
    }
}
