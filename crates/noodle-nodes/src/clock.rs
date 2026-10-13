//! The Clock node.

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports,
    Setup,
};

/// A tempo-synced pulse: the output goes high at the start of every division
/// of the beat and stays high for `width` of it. Wire it into an Envelope or
/// an ADSR as the trigger or gate, into a Sample & Hold for steps that change
/// on the beat, or through a Math node to cut a held sound into rhythm.
///
/// It reads the timeline's position, so it stays in time through tempo
/// changes, seeks and loops (a tempo change reaches it at the next block, as
/// for the Metronome), and it is silent while the transport is stopped.
/// Divisions are of the whole note (four beats, so `1/1` is a bar of 4/4): `1/4` is a beat, `1/8` an eighth
/// note, and a `T` is a triplet, three to the length of two.
///
/// Pulses start on the grid from the beginning of the timeline, so the clock
/// and a pattern of notes on the same grid agree.
pub struct Clock;

pub const CLOCK_ID: &str = "noodle.generator.clock";

/// The divisions, in the order of the `rate` choice.
pub const DIVISIONS: [&str; 10] = [
    "1/1", "1/2", "1/4", "1/8", "1/16", "1/32", "1/2T", "1/4T", "1/8T", "1/16T",
];
/// Each division's length in ticks (960 to a quarter note), for 4/4.
pub const DIVISION_TICKS: [f64; 10] = [
    3840.0, 1920.0, 960.0, 480.0, 240.0, 120.0, 1280.0, 640.0, 320.0, 160.0,
];

#[derive(Ports)]
struct ClockPorts {
    #[param(
        "rate",
        "Rate",
        ParamInfo {
            default: 3.0,
            ..ParamInfo::choice(DIVISIONS)
        }
    )]
    rate: (),
    #[param("width", "Width", ParamInfo::new(0.01, 1.0, 0.5))]
    width: (),
    #[output("out", "Out")]
    out: (),
}

const RATE: usize = ClockPorts::RATE;
const WIDTH: usize = ClockPorts::WIDTH;
const OUT: usize = ClockPorts::OUT;

/// Ticks to a quarter note; see `noodle_core::Tick`.
const TICKS_PER_QUARTER: f64 = 960.0;

static INFO: NodeInfo = NodeInfo {
    id: CLOCK_ID,
    version: 1,
    name: "Clock",
    category: "Generators",
};

impl NodeType for Clock {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ClockPorts::layout())
    }

    /// Reads only the transport, so a rebuilt copy picks up where the old
    /// one was.
    fn stateless(&self) -> bool {
        true
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(ClockNode))
    }
}

struct ClockNode;

/// The pulse at `tick`, for a division `ticks` long.
#[inline]
fn pulse(tick: f64, ticks: f64, width: f32) -> f32 {
    let along = (tick / ticks).rem_euclid(1.0);
    f32::from(along < f64::from(width))
}

impl Node for ClockNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let out = io.outputs[OUT].lane_mut(0, 0);
        let transport = &ctx.transport;
        if !transport.playing {
            out.fill(0.0);
            return;
        }
        let (rate, width) = (io.inputs[RATE].lane(0, 0), io.inputs[WIDTH].lane(0, 0));
        let ticks_per_sample =
            transport.bpm / 60.0 * TICKS_PER_QUARTER / f64::from(ctx.sample_rate);
        for (i, sample) in out.iter_mut().enumerate() {
            let division = (rate[i].round().max(0.0) as usize).min(DIVISIONS.len() - 1);
            let tick = transport.tick + i as f64 * ticks_per_sample;
            *sample = pulse(tick, DIVISION_TICKS[division], width[i].clamp(0.01, 1.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    /// At 120 bpm a quarter note is 0.5 s: 24,000 samples at 48 kHz.
    fn edges(rate: f32, width: f32, frames: usize) -> Vec<usize> {
        let mut h = Harness::new(&Clock, &Config::new(), &[], 48_000.0, frames).unwrap();
        h.set(RATE, rate);
        h.set(WIDTH, width);
        h.run(frames).unwrap();
        let out = h.output(OUT).lane(0, 0).to_vec();
        (0..frames)
            .filter(|&i| out[i] >= 0.5 && (i == 0 || out[i - 1] < 0.5))
            .collect()
    }

    #[test]
    fn pulses_every_division_from_the_start() {
        let quarters = edges(2.0, 0.5, 96_000);
        assert_eq!(quarters, [0, 24_000, 48_000, 72_000]);
        let eighths = edges(3.0, 0.5, 48_000);
        assert_eq!(eighths, [0, 12_000, 24_000, 36_000]);
    }

    #[test]
    fn a_triplet_is_three_in_the_time_of_two() {
        // Eighth triplets: three to a quarter, 8,000 samples apart.
        let triplets = edges(8.0, 0.5, 24_000);
        assert_eq!(triplets.len(), 3);
        assert!((triplets[1] as i64 - 8_000).abs() <= 1);
        assert!((triplets[2] as i64 - 16_000).abs() <= 1);
    }

    #[test]
    fn width_sets_how_long_the_pulse_stays_high() {
        let mut h = Harness::new(&Clock, &Config::new(), &[], 48_000.0, 24_000).unwrap();
        h.set(RATE, 2.0);
        h.set(WIDTH, 0.25);
        h.run(24_000).unwrap();
        let high = h
            .output(OUT)
            .lane(0, 0)
            .iter()
            .filter(|&&x| x >= 0.5)
            .count();
        assert!((high as i64 - 6_000).abs() <= 1, "{high}");
    }

    #[test]
    fn a_whole_note_is_four_beats() {
        assert_eq!(edges(0.0, 0.1, 192_000), [0, 96_000]);
    }
}
