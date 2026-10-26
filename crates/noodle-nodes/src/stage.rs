use noodle_engine::{
    Config, Context, Instance, Io, Lane, LaneKernel, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, PerLane, Ports, Setup, Skip, Telemetry, Unit,
};

use crate::meter::{LevelProbe, voice_sum_out};

/// What a group's boundary node becomes when its gain or mute is in use, and
/// always at a group's outputs. The compiler swaps it in (see `flatten`);
/// users don't add it themselves. It reports its output's level through
/// [`Telemetry`], for the meters on the group and its output node.
pub struct GroupStage {
    telemetry: Telemetry,
}

impl GroupStage {
    pub fn new(telemetry: &Telemetry) -> Self {
        Self {
            telemetry: telemetry.clone(),
        }
    }
}

#[derive(Ports)]
#[ports(memoryless)]
struct StagePorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "gain",
        "Gain",
        ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels)
    )]
    gain: (),
    // Smoothed like any continuous parameter, so muting ramps down instead of
    // clicking.
    #[param("mute", "Mute", ParamInfo::new(0.0, 1.0, 0.0))]
    mute: (),
    // Another track's solo. Separate from mute so a lane driving the mute
    // still can't make a soloed-out track audible.
    #[param("solo_mute", "Solo mute", ParamInfo::new(0.0, 1.0, 0.0))]
    solo_mute: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = StagePorts::INPUT;
const GAIN: usize = StagePorts::GAIN;
const MUTE: usize = StagePorts::MUTE;
const SOLO_MUTE: usize = StagePorts::SOLO_MUTE;
const OUT: usize = StagePorts::OUT;

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
        Ok(StagePorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let channels = setup.output_shapes[OUT].channels;
        Ok(Instance::realtime(StageNode {
            stage: PerLane::new(StageKernel, setup),
            probe: LevelProbe::new(&self.telemetry, setup.node, channels, setup.sample_rate),
        }))
    }
}

struct StageNode {
    stage: PerLane<StageKernel>,
    probe: LevelProbe,
}

impl Node for StageNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            outputs,
            event_inputs,
            event_outputs,
            spectral_inputs,
            spectral_outputs,
        } = io;
        self.stage.process(
            ctx,
            Io {
                inputs,
                outputs: &mut *outputs,
                event_inputs,
                event_outputs: &mut *event_outputs,
                spectral_inputs,
                spectral_outputs: &mut *spectral_outputs,
            },
        );
        let out = &mut outputs[OUT];
        for channel in 0..self.probe.slots() {
            self.probe.measure(channel, ctx.frames, |frame| {
                let x = voice_sum_out(out, channel, frame);
                (x * x, x.abs())
            });
        }
    }

    fn reset(&mut self) {
        self.probe.reset();
        self.stage.reset();
    }

    fn stopped(&mut self) {
        self.probe.silence();
    }
}

struct StageKernel;

impl LaneKernel for StageKernel {
    type State = ();

    /// Gain times silence is silence, so a group's stage passes the flags on.
    fn skip(&self) -> Skip {
        Skip::AnySilent(&[IN])
    }

    /// Muted for good (the mute has finished ramping up), the stage writes
    /// zeros, so nothing that only feeds it needs to run.
    fn silenced_by(&self, constants: &[Option<f32>]) -> bool {
        [MUTE, SOLO_MUTE]
            .iter()
            .any(|&port| constants[port].is_some_and(|value| value >= 1.0))
    }

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
            &GroupStage::new(&Telemetry::new()),
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
        let layout = GroupStage::new(&Telemetry::new())
            .layout(&Config::new())
            .unwrap();
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
            &GroupStage::new(&Telemetry::new()),
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

    #[test]
    fn silent_lanes_stay_silent_through_the_stage() {
        let poly = Shape::new(2, 1);
        let mut h = Harness::new(
            &GroupStage::new(&Telemetry::new()),
            &Config::new(),
            &[(IN, poly)],
            48_000.0,
            4,
        )
        .unwrap();
        let mut input = h.input(IN, 4);
        input.lane_mut(0, 0).fill(1.0);
        input.silence(1, 0);
        h.run(4).unwrap();
        assert!(!h.output(OUT).is_silent(0, 0));
        assert!(h.output(OUT).is_silent(1, 0));
    }
}
