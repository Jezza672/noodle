//! The BPM to Length node.

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports,
    Setup,
};

use crate::clock::DIVISIONS;
use crate::tempo::seconds;

/// Turns a tempo in beats per minute into the length of a division of the
/// beat, as `time` in seconds and `rate` in hertz: the Tempo node for any
/// BPM, not just the project's. Wire a Math node's result or a slider into
/// `bpm`, and the outputs into a Delay's `time` or an LFO's `rate` (see Tempo
/// for how offset sliders treat a wire). Divisions are of the whole note, as
/// for Tempo: `1/4` is a beat.
pub struct BpmLength;

pub const BPM_LENGTH_ID: &str = "noodle.mod.bpm_length";

#[derive(Ports)]
struct BpmLengthPorts {
    #[param("bpm", "BPM", ParamInfo::new(20.0, 300.0, 120.0))]
    bpm: (),
    #[param(
        "division",
        "Division",
        ParamInfo {
            default: 2.0,
            ..ParamInfo::choice(DIVISIONS)
        }
    )]
    division: (),
    #[output("time", "Time")]
    time: (),
    #[output("rate", "Rate")]
    rate: (),
}

const BPM: usize = BpmLengthPorts::BPM;
const DIVISION: usize = BpmLengthPorts::DIVISION;
const TIME: usize = BpmLengthPorts::TIME;
const RATE: usize = BpmLengthPorts::RATE;

static INFO: NodeInfo = NodeInfo {
    id: BPM_LENGTH_ID,
    version: 1,
    name: "BPM to Length",
    category: "Modulation",
};

impl NodeType for BpmLength {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(BpmLengthPorts::layout())
    }

    fn stateless(&self) -> bool {
        true
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(BpmLengthNode))
    }
}

struct BpmLengthNode;

impl Node for BpmLengthNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let bpm = io.inputs[BPM].lane(0, 0);
        let division = io.inputs[DIVISION].lane(0, 0);
        let (first, second) = io.outputs.split_at_mut(RATE);
        let (time, rate) = (first[TIME].lane_mut(0, 0), second[0].lane_mut(0, 0));
        for (((time, rate), &bpm), &division) in
            time.iter_mut().zip(rate.iter_mut()).zip(bpm).zip(division)
        {
            *time = seconds(division, f64::from(bpm));
            *rate = time.recip();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    fn run(bpm: f32, division: f32) -> (f32, f32) {
        let mut h = Harness::new(&BpmLength, &Config::new(), &[], 48_000.0, 64).unwrap();
        h.set(BPM, bpm);
        h.set(DIVISION, division);
        h.run(64).unwrap();
        (h.output(TIME).lane(0, 0)[63], h.output(RATE).lane(0, 0)[63])
    }

    #[test]
    fn a_beat_is_sixty_over_the_bpm() {
        assert_eq!(run(120.0, 2.0), (0.5, 2.0));
        assert_eq!(run(60.0, 2.0), (1.0, 1.0));
    }

    #[test]
    fn the_division_scales_the_length() {
        let beat = run(100.0, 2.0).0;
        // 1/8 is half a beat.
        assert!((run(100.0, 3.0).0 - beat / 2.0).abs() < 1e-6);
    }
}
