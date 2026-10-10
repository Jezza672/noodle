//! The unison saw oscillator.

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Ports, Setup, Shape, Unit,
};

/// Several band-limited saws at slightly different pitches, spread across
/// the stereo field: the "supersaw" sound.
///
/// The `unison` config is how many saws play (1 to 16). They are detuned
/// evenly across `detune` cents either side of the pitch, and panned evenly
/// across `spread`, with the lowest on the left. At a `spread` of 0 every saw
/// is in the middle, and at 1 the outer two are hard left and right. The level
/// is scaled by the square root of the count, so adding saws thickens the
/// sound without raising it much.
///
/// The output is stereo, and polyphonic if the frequency is, with its own set
/// of saws for each voice. Detune and spread are read once per block. A
/// voice whose frequency is silent (see [`Voices`](crate::Voices)) is skipped.
pub struct UnisonSaw;

pub const UNISON_SAW_ID: &str = "noodle.osc.unison_saw";

#[derive(Ports)]
struct UnisonPorts {
    #[param(
        "frequency",
        "Frequency",
        ParamInfo::new(0.01, 20_000.0, 440.0)
            .log()
            .unit(Unit::Hertz)
    )]
    frequency: (),
    #[param("detune", "Detune", ParamInfo::new(0.0, 100.0, 15.0).unit(Unit::Cents))]
    detune: (),
    #[param("spread", "Spread", ParamInfo::new(0.0, 1.0, 0.5))]
    spread: (),
    #[output("out", "Out")]
    out: (),
}

const FREQUENCY: usize = UnisonPorts::FREQUENCY;
const DETUNE: usize = UnisonPorts::DETUNE;
const SPREAD: usize = UnisonPorts::SPREAD;
const OUT: usize = UnisonPorts::OUT;

const UNISON: ConfigInfo = ConfigInfo::int("unison", "Unison", 5);
const MAX_UNISON: usize = 16;

static CONFIG: [ConfigInfo; 1] = [UNISON];

static INFO: NodeInfo = NodeInfo {
    id: UNISON_SAW_ID,
    version: 1,
    name: "Unison Saw",
    category: "Generators",
};

impl UnisonSaw {
    fn count(config: &Config) -> Result<usize, NodeError> {
        let count = UNISON.get_int(config);
        match usize::try_from(count) {
            Ok(n @ 1..=MAX_UNISON) => Ok(n),
            _ => Err(NodeError::config(format!(
                "Unison Saw needs between 1 and {MAX_UNISON} saws, not {count}"
            ))),
        }
    }
}

impl NodeType for UnisonSaw {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        Self::count(config)?;
        Ok(UnisonPorts::layout())
    }

    /// One stereo output, with a voice for each voice of the frequency.
    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let shape = Shape::broadcast_all(inputs.iter().copied())?;
        Ok(vec![Shape::new(shape.voices, 2)])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let saws = Self::count(setup.config)?;
        let voices = setup.output_shapes[OUT].voices;
        let mut node = UnisonNode {
            saws,
            phases: vec![0.0; voices * saws],
            ratios: vec![1.0; saws],
            gains: vec![(0.0, 0.0); saws],
            left: vec![0.0; setup.max_frames],
            right: vec![0.0; setup.max_frames],
        };
        node.reset();
        Ok(Instance::realtime(node))
    }
}

struct UnisonNode {
    saws: usize,
    /// Each voice's saws' phases, in cycles, voice-major.
    phases: Vec<f32>,
    /// Per block: each saw's frequency relative to the pitch.
    ratios: Vec<f32>,
    /// Per block: each saw's gain to the left and right.
    gains: Vec<(f32, f32)>,
    /// A voice's mix before it is copied out, because a lane borrows the
    /// whole output.
    left: Vec<f32>,
    right: Vec<f32>,
}

impl UnisonNode {
    /// Evenly across -1 to 1, with a single saw in the middle.
    fn position(&self, i: usize) -> f32 {
        if self.saws == 1 {
            0.0
        } else {
            2.0 * i as f32 / (self.saws - 1) as f32 - 1.0
        }
    }

    fn set_block(&mut self, detune_cents: f32, spread: f32) {
        let level = (2.0 / self.saws as f32).sqrt();
        for i in 0..self.saws {
            let position = self.position(i);
            self.ratios[i] = (detune_cents.clamp(0.0, 100.0) * position / 1200.0).exp2();
            // Equal power, with the middle at 1 in each ear.
            let angle = (spread.clamp(0.0, 1.0) * position + 1.0) * std::f32::consts::FRAC_PI_4;
            self.gains[i] = (angle.cos() * level, angle.sin() * level);
        }
    }
}

impl Node for UnisonNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let (detune, spread) = (
            io.inputs[DETUNE].lane(0, 0).first().copied().unwrap_or(0.0),
            io.inputs[SPREAD].lane(0, 0).first().copied().unwrap_or(0.0),
        );
        self.set_block(detune, spread);
        let seconds_per_sample = 1.0 / ctx.sample_rate;
        let frames = ctx.frames;
        let out = &mut io.outputs[OUT];

        for voice in 0..out.shape().voices {
            if io.inputs[FREQUENCY].is_silent(voice, 0) {
                out.silence(voice, 0);
                out.silence(voice, 1);
                continue;
            }
            let frequency = io.inputs[FREQUENCY].lane(voice, 0);
            let (left, right) = (&mut self.left[..frames], &mut self.right[..frames]);
            left.fill(0.0);
            right.fill(0.0);
            let phases = &mut self.phases[voice * self.saws..(voice + 1) * self.saws];
            for ((phase, &ratio), &(gl, gr)) in phases.iter_mut().zip(&self.ratios).zip(&self.gains)
            {
                for ((l, r), &f) in left.iter_mut().zip(right.iter_mut()).zip(frequency) {
                    let step = f * ratio * seconds_per_sample;
                    let sample = 2.0 * *phase - 1.0 - poly_blep(*phase, step.abs());
                    *l += gl * sample;
                    *r += gr * sample;
                    *phase = (*phase + step).rem_euclid(1.0);
                }
                if !phase.is_finite() {
                    *phase = 0.0;
                }
            }
            out.lane_mut(voice, 0).copy_from_slice(left);
            out.lane_mut(voice, 1).copy_from_slice(right);
        }
    }

    /// The saws start spread through the cycle, so they don't all peak
    /// together.
    fn reset(&mut self) {
        for (i, phase) in self.phases.iter_mut().enumerate() {
            *phase = ((i % self.saws) as f32 * 0.618_034).fract();
        }
    }
}

/// See the saw in `osc.rs`.
fn poly_blep(phase: f32, step: f32) -> f32 {
    if step <= 0.0 {
        0.0
    } else if phase < step {
        let t = phase / step;
        2.0 * t - t * t - 1.0
    } else if phase > 1.0 - step {
        let t = (phase - 1.0) / step;
        t * t + 2.0 * t + 1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;

    fn unison(count: i64) -> Config {
        let mut config = Config::new();
        config.set("unison", Value::Int(count));
        config
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn run(count: i64, detune: f32, spread: f32, frames: usize) -> Harness {
        let mut h = Harness::new(&UnisonSaw, &unison(count), &[], 48_000.0, frames).unwrap();
        h.set(FREQUENCY, 220.0);
        h.set(DETUNE, detune);
        h.set(SPREAD, spread);
        h.run(frames).unwrap();
        h
    }

    #[test]
    fn the_output_is_stereo_and_polyphonic_with_the_frequency() {
        let poly = Shape::new(3, 1);
        let h = Harness::new(&UnisonSaw, &unison(4), &[(FREQUENCY, poly)], 48_000.0, 8).unwrap();
        assert_eq!(h.output(OUT).shape(), Shape::new(3, 2));
    }

    #[test]
    fn rejects_a_bad_count() {
        for count in [0, 17, -2] {
            assert!(UnisonSaw.layout(&unison(count)).is_err(), "{count}");
        }
    }

    #[test]
    fn a_single_saw_without_spread_is_the_same_in_both_ears() {
        let h = run(1, 30.0, 1.0, 480);
        let out = h.output(OUT);
        assert_eq!(out.lane(0, 0), out.lane(0, 1));
        assert!(rms(out.lane(0, 0)) > 0.3);
    }

    #[test]
    fn no_spread_means_mono_and_full_spread_means_different_ears() {
        let mono = run(5, 20.0, 0.0, 4800);
        let out = mono.output(OUT);
        assert_eq!(out.lane(0, 0), out.lane(0, 1));

        let wide = run(5, 20.0, 1.0, 4800);
        let out = wide.output(OUT);
        let diff: Vec<f32> = out
            .lane(0, 0)
            .iter()
            .zip(out.lane(0, 1))
            .map(|(l, r)| l - r)
            .collect();
        assert!(rms(&diff) > 0.1, "{}", rms(&diff));
    }

    #[test]
    fn more_saws_do_not_get_much_louder() {
        let one = rms(run(1, 0.0, 0.0, 4800).output(OUT).lane(0, 0));
        let seven = rms(run(7, 0.0, 0.0, 4800).output(OUT).lane(0, 0));
        // Detuned saws add as uncorrelated sources at full detune, but with
        // none they stack: the level scaling keeps that bounded.
        assert!(seven < one * 3.0, "{seven} vs {one}");
        let seven_detuned = rms(run(7, 40.0, 0.0, 48_000).output(OUT).lane(0, 0));
        assert!(
            (seven_detuned / one - 1.0).abs() < 0.4,
            "{seven_detuned} vs {one}"
        );
    }

    #[test]
    fn detune_beats_slowly() {
        // Two saws 20 cents apart at 220 Hz beat about 5 times a second. Over
        // a second the envelope of the output swings, which a lone saw's does
        // not.
        let h = run(2, 10.0, 0.0, 48_000);
        let out = h.output(OUT).lane(0, 0);
        let windows: Vec<f32> = out.chunks(960).map(rms).collect();
        let (lo, hi) = windows
            .iter()
            .fold((f32::MAX, 0f32), |(lo, hi), &w| (lo.min(w), hi.max(w)));
        assert!(hi > lo * 1.5, "{lo}..{hi}");
    }

    #[test]
    fn a_silent_frequency_voice_is_skipped_and_flagged() {
        let poly = Shape::new(2, 1);
        let mut h =
            Harness::new(&UnisonSaw, &unison(3), &[(FREQUENCY, poly)], 48_000.0, 64).unwrap();
        let mut frequency = h.input(FREQUENCY, 64);
        frequency.lane_mut(0, 0).fill(440.0);
        frequency.silence(1, 0);
        h.run(64).unwrap();
        let out = h.output(OUT);
        assert!(!out.is_silent(0, 0));
        assert!(rms(out.lane(0, 0)) > 0.1);
        assert!(out.is_silent(1, 0) && out.is_silent(1, 1));
        assert!(
            out.lane(1, 0)
                .iter()
                .chain(out.lane(1, 1))
                .all(|&x| x == 0.0)
        );
    }

    #[test]
    fn recovers_from_an_infinite_frequency() {
        let mut h = Harness::new(
            &UnisonSaw,
            &unison(3),
            &[(FREQUENCY, Shape::MONO)],
            48_000.0,
            64,
        )
        .unwrap();
        let mut frequency = h.input(FREQUENCY, 64);
        frequency.fill(440.0);
        frequency.lane_mut(0, 0)[10] = f32::INFINITY;
        h.run(64).unwrap();
        h.input(FREQUENCY, 64).fill(440.0);
        h.run(64).unwrap();
        h.run(64).unwrap();
        let out = h.output(OUT);
        assert!(
            out.lane(0, 0)
                .iter()
                .chain(out.lane(0, 1))
                .all(|x| x.is_finite())
        );
    }
}
