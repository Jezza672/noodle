//! The Tempo node.

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports,
    Setup,
};

use crate::clock::{DIVISION_TICKS, DIVISIONS};

/// The project's tempo as a length of time: a division of the beat, as
/// `time` in seconds and `rate` in hertz, for a synced echo or a tremolo
/// locked to the beat.
///
/// The outputs are the values themselves. A wire into a parameter whose
/// slider is an offset (Delay's `time`, the LFO's `rate`) moves the slider
/// along its travel instead of setting it, so for those put a Math node
/// between: the Tempo Echo and Tempo Tremolo templates show it. A parameter
/// that is set by its wire (a Math input) takes them as they are; a Lag's
/// `rise` and `fall` are offset sliders too and need the same conversion.
///
/// It reads the tempo at the start of the block, so a tempo change reaches
/// it at the next block, as for Clock and the Metronome. The outputs move in
/// steps then; put a Lag after `time` if a tempo ramp should not click a
/// delay line. It works with the transport stopped (the tempo is still the
/// tempo). Divisions are of the whole note (four beats): `1/4` is a beat, a
/// `D` is dotted (one and a half times as long) and a `T` is a triplet
/// (three to the length of two). `4/1` and `2/1` are four and two bars of 4/4.
pub struct Tempo;

pub const TEMPO_ID: &str = "noodle.mod.tempo";

/// Ticks to a quarter note; see `noodle_core::Tick`.
const TICKS_PER_QUARTER: f64 = 960.0;

#[derive(Ports)]
struct TempoPorts {
    #[param(
        "division",
        "Division",
        ParamInfo {
            default: 2.0,
            ..ParamInfo::choice(DIVISIONS)
        }
    )]
    division: (),
    #[control_output("time", "Time")]
    time: (),
    #[control_output("rate", "Rate")]
    rate: (),
}

const DIVISION: usize = TempoPorts::DIVISION;
const TIME: usize = TempoPorts::TIME;
const RATE: usize = TempoPorts::RATE;

static INFO: NodeInfo = NodeInfo {
    id: TEMPO_ID,
    version: 1,
    name: "Tempo",
    category: "Modulation",
};

impl NodeType for Tempo {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(TempoPorts::layout())
    }

    /// Reads only the transport, so a rebuilt copy picks up where the old
    /// one was.
    fn stateless(&self) -> bool {
        true
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(TempoNode))
    }
}

struct TempoNode;

/// The length of `division` at `bpm`, in seconds.
#[inline]
pub(crate) fn seconds(division: f32, bpm: f64) -> f32 {
    let division = (division.round().max(0.0) as usize).min(DIVISIONS.len() - 1);
    (DIVISION_TICKS[division] / TICKS_PER_QUARTER * 60.0 / bpm.max(1.0)) as f32
}

impl Node for TempoNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let division = io.inputs[DIVISION].lane(0, 0);
        let bpm = ctx.transport.bpm;
        let (first, second) = io.outputs.split_at_mut(RATE);
        let (time, rate) = (first[TIME].lane_mut(0, 0), second[0].lane_mut(0, 0));
        for ((time, rate), &division) in time.iter_mut().zip(rate.iter_mut()).zip(division) {
            *time = seconds(division, bpm);
            *rate = time.recip();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    fn run(division: f32) -> (f32, f32) {
        let mut h = Harness::new(&Tempo, &Config::new(), &[], 48_000.0, 64).unwrap();
        h.set(DIVISION, division);
        h.run(64).unwrap();
        let (time, rate) = (h.output(TIME).lane(0, 0)[63], h.output(RATE).lane(0, 0)[63]);
        (time, rate)
    }

    #[test]
    fn a_quarter_is_half_a_second_at_120() {
        let (time, rate) = run(2.0);
        assert_eq!((time, rate), (0.5, 2.0));
    }

    #[test]
    fn dotted_and_triplet_divisions_scale_the_eighth() {
        let eighth = run(3.0).0;
        // 1/8D and 1/8T.
        assert!((run(14.0).0 - eighth * 1.5).abs() < 1e-6);
        assert!((run(8.0).0 - eighth * 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn the_division_scales_with_the_tempo() {
        // A quarter note is a beat: 60 / bpm.
        for bpm in [60.0, 90.0, 140.0, 200.0] {
            assert!((seconds(2.0, bpm) - (60.0 / bpm) as f32).abs() < 1e-6);
        }
    }

    #[test]
    fn every_division_has_a_name_and_a_length() {
        assert_eq!(DIVISIONS.len(), DIVISION_TICKS.len());
        // 4/1 is four whole notes.
        assert_eq!(seconds(10.0, 120.0), 8.0);
    }
}
