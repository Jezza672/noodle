//! The Chorus node.

use std::f32::consts::TAU;

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports,
    Setup, Shape, Unit,
};

/// A modulated delay that thickens a sound. A short delay (`delay`) is swept
/// up and down by an LFO (`rate`, `depth`) and blended with the dry signal
/// (`mix`), so the two drift in and out of tune and phase with each other.
///
/// With a small `delay` (a few milliseconds) and some `feedback` it is a
/// flanger: the comb filter's notches sweep and ring. With a larger `delay`
/// and no feedback it is a chorus.
///
/// The two channels of a stereo signal sweep a quarter of a cycle apart,
/// which spreads the movement across the stereo field. Every voice has its
/// own delay line and LFO, so a polyphonic input chorus per voice; voices
/// begin their sweep at the same point, since the node is a deterministic
/// function of its input.
///
/// A lane whose input has been silent for longer than the delay line is
/// flagged silent and costs almost nothing.
pub struct Chorus;

pub const CHORUS_ID: &str = "noodle.effect.chorus";

/// The longest delay the line holds, in seconds: `delay` plus `depth` at
/// their maxima.
const MAX_SECONDS: f32 = 0.06;
/// Below this a lane's input counts as silent, as in the Delay.
const INAUDIBLE: f32 = 1e-6;

#[derive(Ports)]
struct ChorusPorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "rate",
        "Rate",
        ParamInfo::new(0.05, 10.0, 0.6).log().unit(Unit::Hertz).offset()
    )]
    rate: (),
    #[param("depth", "Depth", ParamInfo::new(0.0, 15.0, 3.0).offset())]
    depth: (),
    #[param("delay", "Delay", ParamInfo::new(0.5, 40.0, 12.0).log())]
    delay: (),
    #[param("feedback", "Feedback", ParamInfo::new(-0.95, 0.95, 0.0))]
    feedback: (),
    #[param("mix", "Mix", ParamInfo::new(0.0, 1.0, 0.5))]
    mix: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = ChorusPorts::INPUT;
const RATE: usize = ChorusPorts::RATE;
const DEPTH: usize = ChorusPorts::DEPTH;
const DELAY: usize = ChorusPorts::DELAY;
const FEEDBACK: usize = ChorusPorts::FEEDBACK;
const MIX: usize = ChorusPorts::MIX;
const OUT: usize = ChorusPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: CHORUS_ID,
    version: 1,
    name: "Chorus",
    category: "Effects",
};

impl NodeType for Chorus {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ChorusPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let shape = setup.output_shapes[0];
        let len = ((MAX_SECONDS * setup.sample_rate).ceil() as usize + 4).next_power_of_two();
        Ok(Instance::realtime(ChorusNode {
            shape,
            sample_rate: setup.sample_rate,
            lines: (0..shape.lanes())
                .map(|_| vec![0.0; len].into_boxed_slice())
                .collect(),
            mask: len - 1,
            phases: vec![0.0; shape.lanes()],
            quiet: vec![len; shape.lanes()],
            written: 0,
        }))
    }
}

struct ChorusNode {
    shape: Shape,
    sample_rate: f32,
    lines: Vec<Box<[f32]>>,
    mask: usize,
    /// Each lane's LFO phase, in cycles.
    phases: Vec<f32>,
    /// Samples in a row each lane's input has been silent.
    quiet: Vec<usize>,
    written: usize,
}

impl Node for ChorusNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let start = self.written;
        let longest = (self.mask - 2) as f32;
        let channels = self.shape.channels;
        for lane in 0..self.shape.lanes() {
            let (voice, channel) = (lane / channels, lane % channels);
            let input = io.inputs[IN].lane(voice, channel);
            let silent = io.inputs[IN].is_silent(voice, channel)
                || input.iter().all(|x| x.abs() < INAUDIBLE);
            if silent && self.quiet[lane] > self.mask {
                io.outputs[OUT].silence(voice, channel);
                continue;
            }
            self.quiet[lane] = if silent {
                self.quiet[lane].saturating_add(ctx.frames)
            } else {
                0
            };
            let (rate, depth) = (
                io.inputs[RATE].lane(voice, channel),
                io.inputs[DEPTH].lane(voice, channel),
            );
            let (delay, feedback, mix) = (
                io.inputs[DELAY].lane(voice, channel),
                io.inputs[FEEDBACK].lane(voice, channel),
                io.inputs[MIX].lane(voice, channel),
            );
            let out = io.outputs[OUT].lane_mut(voice, channel);
            // The second channel sweeps a quarter cycle behind the first.
            let offset = 0.25 * channel as f32;
            let line = &mut self.lines[lane];
            let mut phase = self.phases[lane];
            for k in 0..out.len() {
                phase += rate[k] / self.sample_rate;
                phase -= phase.floor();
                let sweep = (TAU * (phase + offset)).sin();
                let ms = (delay[k] + depth[k] * sweep).max(0.0);
                let samples = (ms * 0.001 * self.sample_rate).clamp(1.0, longest);
                let at = start.wrapping_add(k);
                let whole = samples as usize;
                let frac = samples - whole as f32;
                let newer = line[at.wrapping_sub(whole) & self.mask];
                let older = line[at.wrapping_sub(whole + 1) & self.mask];
                let wet = newer + (older - newer) * frac;
                let wet = if wet.is_finite() { wet } else { 0.0 };
                line[at & self.mask] = input[k] + wet * feedback[k].clamp(-0.95, 0.95);
                let mix = mix[k].clamp(0.0, 1.0);
                out[k] = input[k] + (wet - input[k]) * mix;
            }
            self.phases[lane] = phase;
        }
        self.written = start.wrapping_add(ctx.frames);
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.fill(0.0);
        }
        self.phases.fill(0.0);
        self.quiet.fill(self.mask + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    const RATE_HZ: f32 = 48_000.0;

    fn harness(shape: Shape, frames: usize) -> Harness {
        Harness::new(&Chorus, &Config::new(), &[(IN, shape)], RATE_HZ, frames).unwrap()
    }

    #[test]
    fn with_no_depth_it_is_a_fixed_delay() {
        let frames = 2_000;
        let mut h = harness(Shape::MONO, frames);
        h.set(DEPTH, 0.0);
        h.set(DELAY, 10.0); // 480 samples
        h.set(MIX, 1.0);
        let mut impulse = vec![0.0; frames];
        impulse[100] = 1.0;
        h.input(IN, frames).lane_mut(0, 0).copy_from_slice(&impulse);
        h.run(frames).unwrap();
        let out = h.output(OUT).lane(0, 0).to_vec();
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        assert_eq!(peak.0, 580);
        assert!((peak.1 - 1.0).abs() < 1e-3, "{}", peak.1);
    }

    #[test]
    fn mix_zero_is_dry() {
        let frames = 500;
        let mut h = harness(Shape::MONO, frames);
        h.set(MIX, 0.0);
        let input: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.1).sin()).collect();
        h.input(IN, frames).lane_mut(0, 0).copy_from_slice(&input);
        h.run(frames).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &input[..]);
    }

    #[test]
    fn the_sweep_moves_the_delay() {
        // A steady sine through a wet-only chorus has a changing phase
        // against the input, so the output isn't a plain delayed copy.
        let frames = 24_000;
        let mut h = harness(Shape::MONO, frames);
        h.set(RATE, 2.0);
        h.set(DEPTH, 5.0);
        h.set(MIX, 1.0);
        let input: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.3).sin()).collect();
        h.input(IN, frames).lane_mut(0, 0).copy_from_slice(&input);
        h.run(frames).unwrap();
        let out = h.output(OUT).lane(0, 0).to_vec();
        assert!(out.iter().all(|x| x.is_finite() && x.abs() < 1.5));
        // Best-fitting fixed delay differs between the first and last half
        // second, since the delay is mid-sweep at different points.
        let best = |from: usize| {
            (400..800)
                .min_by(|&a, &b| {
                    let err = |d: usize| -> f32 {
                        (from..from + 2_000)
                            .map(|i| (out[i] - input[i - d]).powi(2))
                            .sum()
                    };
                    err(a).total_cmp(&err(b))
                })
                .unwrap()
        };
        assert_ne!(best(2_000), best(14_000));
    }

    #[test]
    fn stereo_channels_sweep_apart() {
        let frames = 4_800;
        let mut h = harness(Shape::new(1, 2), frames);
        h.set(DEPTH, 8.0);
        h.set(MIX, 1.0);
        let input: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.2).sin()).collect();
        {
            let mut inp = h.input(IN, frames);
            inp.lane_mut(0, 0).copy_from_slice(&input);
            inp.lane_mut(0, 1).copy_from_slice(&input);
        }
        h.run(frames).unwrap();
        let out = h.output(OUT);
        assert_ne!(out.lane(0, 0), out.lane(0, 1));
    }

    #[test]
    fn a_silent_lane_is_flagged_once_the_line_has_emptied() {
        let frames = 4_096;
        let mut h = harness(Shape::MONO, frames);
        h.input(IN, frames).fill(0.5);
        h.run(frames).unwrap();
        h.input(IN, frames).fill(0.0);
        for _ in 0..3 {
            h.run(frames).unwrap();
        }
        assert!(h.output(OUT).is_silent(0, 0));
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 0.0));
    }
}
