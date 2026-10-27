//! The Phaser node.

use std::f32::consts::{PI, TAU};

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Skip, Unit,
};

/// A chain of allpass filters whose corner frequency is swept by an LFO,
/// blended with the dry signal. The allpasses leave the level alone but turn
/// the phase, so where the turn reaches half a cycle the wet signal cancels
/// the dry one and a notch appears; sweeping the corner sweeps the notches.
/// Two stages make one notch, four make two, and so on (the notch lands where
/// the stages together have turned the phase half a cycle, which for two
/// stages is at the corner itself).
///
/// `centre` is the frequency the sweep is centred on, `depth` how far it
/// swings (at 1, two octaves each way), `rate` how fast. `feedback` returns
/// the chain's output to its input, which sharpens the notches into peaks
/// (a negative value moves the notches between them). `stages` picks how
/// many allpasses there are.
///
/// The two channels of a stereo signal sweep a quarter cycle apart. Each
/// voice has its own filter memory.
pub struct Phaser;

pub const PHASER_ID: &str = "noodle.effect.phaser";

/// The stage counts, in the order of the `stages` choice.
pub const STAGES: [usize; 5] = [2, 4, 6, 8, 12];
const MAX_STAGES: usize = 12;

#[derive(Ports)]
#[ports(cost = 40)]
struct PhaserPorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "rate",
        "Rate",
        ParamInfo::new(0.02, 10.0, 0.4).log().unit(Unit::Hertz).offset().featured()
    )]
    rate: (),
    #[param("depth", "Depth", ParamInfo::new(0.0, 1.0, 0.7).offset().featured())]
    depth: (),
    #[param(
        "centre",
        "Centre",
        ParamInfo::new(100.0, 4_000.0, 600.0)
            .log()
            .unit(Unit::Hertz)
            .offset().featured()
    )]
    centre: (),
    #[param(
        "stages",
        "Stages",
        ParamInfo {
            default: 1.0,
            ..ParamInfo::choice(["2", "4", "6", "8", "12"])
        }
    )]
    stages: (),
    #[param("feedback", "Feedback", ParamInfo::new(-0.95, 0.95, 0.3))]
    feedback: (),
    #[param("mix", "Mix", ParamInfo::new(0.0, 1.0, 0.5).featured())]
    mix: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = PhaserPorts::INPUT;
const RATE: usize = PhaserPorts::RATE;
const DEPTH: usize = PhaserPorts::DEPTH;
const CENTRE: usize = PhaserPorts::CENTRE;
const STAGE_COUNT: usize = PhaserPorts::STAGES;
const FEEDBACK: usize = PhaserPorts::FEEDBACK;
const MIX: usize = PhaserPorts::MIX;
const OUT: usize = PhaserPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: PHASER_ID,
    version: 1,
    name: "Phaser",
    category: "Effects",
};

impl NodeType for Phaser {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(PhaserPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(PhaserKernel, setup)))
    }
}

struct PhaserKernel;

#[derive(Default)]
struct PhaserState {
    /// Each allpass's memory.
    memory: [f32; MAX_STAGES],
    /// The last output of the chain, for the feedback.
    last: f32,
    /// The LFO's phase, in cycles.
    phase: f32,
}

/// The coefficient of a first-order allpass with its corner at `hz`.
fn coefficient(hz: f32, sample_rate: f32) -> f32 {
    let t = (PI * hz.clamp(10.0, 0.45 * sample_rate) / sample_rate).tan();
    (t - 1.0) / (t + 1.0)
}

impl LaneKernel for PhaserKernel {
    type State = PhaserState;

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[IN])
    }

    fn is_idle(&self, state: &PhaserState) -> bool {
        state.last.abs() < 1e-9 && state.memory.iter().all(|z| z.abs() < 1e-9)
    }

    fn process_lane(&mut self, state: &mut PhaserState, ctx: &Context, mut lane: Lane<'_, '_>) {
        let input = lane.inputs.get(IN);
        let (rate, depth) = (lane.inputs.get(RATE), lane.inputs.get(DEPTH));
        let (centre, stages) = (lane.inputs.get(CENTRE), lane.inputs.get(STAGE_COUNT));
        let (feedback, mix) = (lane.inputs.get(FEEDBACK), lane.inputs.get(MIX));
        let out = lane.outputs.get_mut(OUT);
        let offset = 0.25 * lane.channel as f32;
        for i in 0..out.len() {
            state.phase += rate[i] / ctx.sample_rate;
            state.phase -= state.phase.floor();
            let sweep = (TAU * (state.phase + offset)).sin();
            let hz = centre[i] * (2.0 * depth[i].clamp(0.0, 1.0) * sweep).exp2();
            let a = coefficient(hz, ctx.sample_rate);
            let count = STAGES[(stages[i].round().max(0.0) as usize).min(STAGES.len() - 1)];

            let mut x = input[i] + state.last * feedback[i].clamp(-0.95, 0.95);
            for z in &mut state.memory[..count] {
                let y = a * x + *z;
                *z = x - a * y;
                x = y;
            }
            if !x.is_finite() {
                *state = PhaserState::default();
                x = 0.0;
            }
            state.last = x;
            let mix = mix[i].clamp(0.0, 1.0);
            out[i] = input[i] + (x - input[i]) * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    const RATE_HZ: f32 = 48_000.0;

    fn run(input: &[f32], params: &[(usize, f32)]) -> Vec<f32> {
        let mut h = Harness::new(
            &Phaser,
            &Config::new(),
            &[(IN, Shape::MONO)],
            RATE_HZ,
            input.len(),
        )
        .unwrap();
        h.input(IN, input.len())
            .lane_mut(0, 0)
            .copy_from_slice(input);
        for &(port, value) in params {
            h.set(port, value);
        }
        h.run(input.len()).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    fn sine(hz: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (TAU * hz * i as f32 / RATE_HZ).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn the_wet_signal_alone_keeps_its_level() {
        // Allpass filters change phase, not level.
        let out = run(
            &sine(500.0, 24_000),
            &[(MIX, 1.0), (FEEDBACK, 0.0), (DEPTH, 0.0)],
        );
        let (input, output) = (rms(&sine(500.0, 24_000)[12_000..]), rms(&out[12_000..]));
        assert!((input - output).abs() < 0.01, "{input} vs {output}");
    }

    #[test]
    fn mixed_with_the_dry_signal_it_notches_a_tone_near_the_corner() {
        // Two allpasses with their corner at 600 Hz turn a 600 Hz tone half
        // a cycle (a quarter each), so the wet and dry halves cancel.
        let tone = sine(600.0, 24_000);
        let out = run(
            &tone,
            &[
                (MIX, 0.5),
                (FEEDBACK, 0.0),
                (DEPTH, 0.0),
                (CENTRE, 600.0),
                (STAGE_COUNT, 0.0),
            ],
        );
        assert!(rms(&out[12_000..]) < 0.05 * rms(&tone[12_000..]));
        // A tone far from the corner passes at nearly full level.
        let far = sine(40.0, 24_000);
        let out = run(
            &far,
            &[
                (MIX, 0.5),
                (FEEDBACK, 0.0),
                (DEPTH, 0.0),
                (CENTRE, 3_000.0),
                (STAGE_COUNT, 0.0),
            ],
        );
        assert!(rms(&out[12_000..]) > 0.9 * rms(&far[12_000..]));
    }

    #[test]
    fn the_sweep_changes_the_sound_over_time() {
        let tone = sine(600.0, 48_000);
        let out = run(
            &tone,
            &[(RATE, 1.0), (DEPTH, 1.0), (MIX, 0.5), (FEEDBACK, 0.0)],
        );
        let level = |from: usize| rms(&out[from..from + 2_400]);
        let levels: Vec<f32> = (0..8).map(|k| level(2_400 + k * 5_000)).collect();
        let (low, high) = levels
            .iter()
            .fold((f32::MAX, 0.0f32), |(l, h), &x| (l.min(x), h.max(x)));
        assert!(high > 2.0 * low, "{levels:?}");
    }

    #[test]
    fn feedback_stays_stable_at_its_limits() {
        let noise: Vec<f32> = (0..20_000)
            .map(|i| ((i * 7919 % 2003) as f32 / 1001.5) - 1.0)
            .collect();
        for fb in [-0.95, 0.95] {
            let out = run(&noise, &[(FEEDBACK, fb), (STAGE_COUNT, 4.0), (MIX, 1.0)]);
            assert!(out.iter().all(|x| x.is_finite() && x.abs() < 100.0), "{fb}");
        }
    }

    #[test]
    fn a_tail_rings_out_and_then_the_lane_is_skipped() {
        let frames = 4_096;
        let mut h = Harness::new(
            &Phaser,
            &Config::new(),
            &[(IN, Shape::MONO)],
            RATE_HZ,
            frames,
        )
        .unwrap();
        h.set(FEEDBACK, 0.0);
        let mut impulse = vec![0.0; frames];
        impulse[0] = 1.0;
        h.input(IN, frames).lane_mut(0, 0).copy_from_slice(&impulse);
        h.run(frames).unwrap();
        assert!(
            h.output(OUT).lane(0, 0)[1..]
                .iter()
                .any(|&x| x.abs() > 1e-4)
        );
        let mut quiet = h.input(IN, frames);
        quiet.fill(0.0);
        quiet.set_silent(0, 0);
        for _ in 0..20 {
            h.run(frames).unwrap();
        }
        assert!(h.output(OUT).is_silent(0, 0));
    }
}
