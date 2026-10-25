//! The Beat Length node.

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports,
    Setup,
};

use crate::clock::DIVISIONS;
use crate::tempo::seconds;

/// A tempo in beats per minute as a length of time: a division of the beat at
/// that tempo, as `time` in seconds and `rate` in hertz. It is the Tempo node
/// for any tempo, not just the project's: wire a tempo from a Math node, a
/// Lag or a control into `bpm`, and the outputs follow it.
///
/// `bpm` is a parameter that may be wired, and a wire sets its value (as for
/// a Math input), so the outputs follow it sample by sample. The outputs are
/// the values themselves, so for a Delay's `time` or an LFO's `rate`, whose
/// sliders are offsets, put a Math node between, as the Tempo Echo and Tempo
/// Tremolo templates do. Divisions are as for Tempo: of the whole note, `1/4`
/// a beat, `D` dotted and `T` a triplet.
pub struct BeatLength;

pub const BEAT_LENGTH_ID: &str = "noodle.mod.beat_length";

#[derive(Ports)]
struct BeatLengthPorts {
    #[param("bpm", "BPM", ParamInfo::new(10.0, 999.0, 120.0))]
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

const BPM: usize = BeatLengthPorts::BPM;
const DIVISION: usize = BeatLengthPorts::DIVISION;
const TIME: usize = BeatLengthPorts::TIME;
const RATE: usize = BeatLengthPorts::RATE;

static INFO: NodeInfo = NodeInfo {
    id: BEAT_LENGTH_ID,
    version: 1,
    name: "Beat Length",
    category: "Modulation",
};

impl NodeType for BeatLength {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(BeatLengthPorts::layout())
    }

    fn stateless(&self) -> bool {
        true
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(BeatLengthNode))
    }
}

struct BeatLengthNode;

impl Node for BeatLengthNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let bpm = io.inputs[BPM].lane(0, 0);
        let division = io.inputs[DIVISION].lane(0, 0);
        let (first, second) = io.outputs.split_at_mut(RATE);
        let (time, rate) = (first[TIME].lane_mut(0, 0), second[0].lane_mut(0, 0));
        for (((time, rate), &division), &bpm) in
            time.iter_mut().zip(rate.iter_mut()).zip(division).zip(bpm)
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
        let mut h = Harness::new(&BeatLength, &Config::new(), &[], 48_000.0, 64).unwrap();
        h.set(BPM, bpm);
        h.set(DIVISION, division);
        h.run(64).unwrap();
        (h.output(TIME).lane(0, 0)[63], h.output(RATE).lane(0, 0)[63])
    }

    #[test]
    fn a_quarter_at_120_is_half_a_second() {
        assert_eq!(run(120.0, 2.0), (0.5, 2.0));
    }

    #[test]
    fn the_length_follows_the_bpm_input() {
        for bpm in [60.0, 90.0, 140.0, 200.0] {
            let (time, rate) = run(bpm, 2.0);
            assert!((time - 60.0 / bpm).abs() < 1e-6, "{bpm}: {time}");
            assert!((rate - bpm / 60.0).abs() < 1e-4, "{bpm}: {rate}");
        }
    }

    #[test]
    fn dotted_and_triplet_divisions_scale_the_eighth() {
        let eighth = run(100.0, 3.0).0;
        assert!((run(100.0, 14.0).0 - eighth * 1.5).abs() < 1e-6);
        assert!((run(100.0, 8.0).0 - eighth * 2.0 / 3.0).abs() < 1e-6);
    }
}
