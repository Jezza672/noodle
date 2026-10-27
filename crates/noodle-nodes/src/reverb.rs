//! The Reverb node.

use std::f32::consts::TAU;

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports,
    Setup, Shape, ShapeError, Unit,
};

/// A stereo reverb: a feedback delay network of eight delay lines, mixed by a
/// Householder matrix so every line feeds every other, behind two allpass
/// stages that smear the input, with a slow wobble on the line lengths so the
/// tail doesn't ring at fixed pitches.
///
/// - `size` scales the delay lengths (0.5 to 1.5 times the nominal 28 to 58 ms),
///   so a bigger room has sparser early reflections.
/// - `decay` is the time for the tail to fall 60 dB, in seconds.
/// - `damping` is how quickly high frequencies die in the tail: 0 is bright,
///   1 is dull.
/// - `predelay` holds the reverb back, in seconds.
/// - `width` is the stereo spread of the wet signal: 0 is mono, 1 is full.
/// - `mix` is the wet share: the output is `dry · (1 - mix) + wet · mix`.
///
/// The input is mono or stereo, one voice (a polyphonic signal goes through a
/// Voice Mix first). The output is stereo. It is deterministic: the same input
/// always gives the same tail.
pub struct Reverb;

pub const REVERB_ID: &str = "noodle.effect.reverb";

#[derive(Ports)]
#[ports(bounded = 30200, cost = 120)]
struct ReverbPorts {
    #[input("in", "In")]
    input: (),
    #[param("size", "Size", ParamInfo::new(0.5, 1.5, 1.0).featured())]
    size: (),
    #[param(
        "decay",
        "Decay",
        ParamInfo::new(0.1, 20.0, 2.0).log().unit(Unit::Seconds).featured()
    )]
    decay: (),
    #[param("damping", "Damping", ParamInfo::new(0.0, 1.0, 0.4).featured())]
    damping: (),
    #[param(
        "predelay",
        "Pre-delay",
        ParamInfo::new(0.0, MAX_PREDELAY, 0.01).unit(Unit::Seconds)
    )]
    predelay: (),
    #[param("width", "Width", ParamInfo::new(0.0, 1.0, 1.0))]
    width: (),
    #[param("mix", "Mix", ParamInfo::new(0.0, 1.0, 0.3).featured())]
    mix: (),
    #[output("out", "Out")]
    out: (),
}

/// The longest pre-delay, in seconds.
pub const MAX_PREDELAY: f32 = 0.2;

static INFO: NodeInfo = NodeInfo {
    id: REVERB_ID,
    version: 2,
    name: "Reverb",
    category: "Effects",
};

/// The nominal line lengths in samples at 48 kHz.
const LINES: [f32; 8] = [
    1327.0, 1523.0, 1721.0, 1931.0, 2129.0, 2347.0, 2549.0, 2767.0,
];
/// The allpass delays at 48 kHz, for the left and right input.
const ALLPASS: [[f32; 2]; 2] = [[142.0, 107.0], [379.0, 277.0]];
const ALLPASS_GAIN: f32 = 0.6;
/// Depth of the length wobble, in samples at 48 kHz, and its speed in hertz.
/// Below this (about -120 dB) the input or the tail counts as silent.
const INAUDIBLE: f32 = 1e-6;
/// How many silent samples in a row, with the tail gone, before the node
/// stops processing. More than the longest pre-delay, so nothing is still in
/// flight.
const IDLE_AFTER: usize = 48_000;
const WOBBLE: f32 = 6.0;
const WOBBLE_RATE: f32 = 0.31;
/// What the input is scaled by going in and the wet by coming out, so a
/// medium room returns about the level it was fed.
const GAIN_IN: f32 = 0.5;
const GAIN_OUT: f32 = 0.5;

impl NodeType for Reverb {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ReverbPorts::layout())
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let input = inputs[ReverbPorts::INPUT];
        if input.voices != 1 {
            return Err(ShapeError(Shape::STEREO, input).into());
        }
        Ok(vec![Shape::STEREO])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let scale = setup.sample_rate / 48_000.0;
        let line_len =
            |n: f32| ((n * scale * 1.5 + WOBBLE * scale + 8.0) as usize).next_power_of_two();
        let predelay = ((MAX_PREDELAY * setup.sample_rate) as usize + setup.max_frames + 4)
            .next_power_of_two();
        Ok(Instance::realtime(ReverbNode {
            sample_rate: setup.sample_rate,
            lines: LINES.map(|n| Ring::new(line_len(n))),
            damp: [0.0; 8],
            predelay: [Ring::new(predelay), Ring::new(predelay)],
            allpass: [
                [
                    Ring::new(((ALLPASS[0][0] * scale) as usize + 2).next_power_of_two()),
                    Ring::new(((ALLPASS[1][0] * scale) as usize + 2).next_power_of_two()),
                ],
                [
                    Ring::new(((ALLPASS[0][1] * scale) as usize + 2).next_power_of_two()),
                    Ring::new(((ALLPASS[1][1] * scale) as usize + 2).next_power_of_two()),
                ],
            ],
            phase: 0.0,
            left: vec![0.0; setup.max_frames].into_boxed_slice(),
            right: vec![0.0; setup.max_frames].into_boxed_slice(),
            quiet: 0,
        }))
    }
}

/// A ring buffer addressed by how long ago a sample was written.
struct Ring {
    data: Box<[f32]>,
    mask: usize,
    at: usize,
}

impl Ring {
    fn new(len: usize) -> Self {
        let len = len.next_power_of_two();
        Self {
            data: vec![0.0; len].into_boxed_slice(),
            mask: len - 1,
            at: 0,
        }
    }

    fn push(&mut self, x: f32) {
        self.data[self.at & self.mask] = x;
        self.at = self.at.wrapping_add(1);
    }

    /// The sample written `delay` samples before the latest (0 is the
    /// latest), linearly interpolated.
    fn read(&self, delay: f32) -> f32 {
        let whole = delay as usize;
        let frac = delay - whole as f32;
        let newer = self.data[self.at.wrapping_sub(1 + whole) & self.mask];
        let older = self.data[self.at.wrapping_sub(2 + whole) & self.mask];
        newer + (older - newer) * frac
    }

    fn clear(&mut self) {
        self.data.fill(0.0);
    }
}

struct ReverbNode {
    sample_rate: f32,
    lines: [Ring; 8],
    /// The low-pass memory of each line.
    damp: [f32; 8],
    predelay: [Ring; 2],
    /// Two allpass stages for each of the left and right.
    allpass: [[Ring; 2]; 2],
    /// Phase of the wobble, in cycles.
    phase: f32,
    left: Box<[f32]>,
    right: Box<[f32]>,
    /// How many samples in a row the input and the whole tail have been
    /// below [`INAUDIBLE`].
    quiet: usize,
}

impl ReverbNode {
    fn allpass(&mut self, stage: usize, side: usize, x: f32) -> f32 {
        let scale = self.sample_rate / 48_000.0;
        let delay = ALLPASS[stage][side] * scale;
        let ring = &mut self.allpass[stage][side];
        let delayed = ring.read(delay - 1.0);
        let v = x - ALLPASS_GAIN * delayed;
        ring.push(v);
        delayed + ALLPASS_GAIN * v
    }
}

impl Node for ReverbNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs, outputs, ..
        } = io;
        let input = &inputs[ReverbPorts::INPUT];
        let out = &mut outputs[ReverbPorts::OUT];
        let params: [_; 6] = [
            ReverbPorts::SIZE,
            ReverbPorts::DECAY,
            ReverbPorts::DAMPING,
            ReverbPorts::PREDELAY,
            ReverbPorts::WIDTH,
            ReverbPorts::MIX,
        ]
        .map(|p| inputs[p].lane(0, 0));
        let [size, decay, damping, predelay, width, mix] = params;
        let (in_l, in_r) = (input.lane(0, 0), input.lane(0, 1));
        // Once the input has been silent and the tail has died away, the
        // state is cleared and the node costs nothing until sound comes in.
        let input_quiet = (input.is_silent(0, 0) && input.is_silent(0, 1))
            || in_l[..ctx.frames]
                .iter()
                .chain(&in_r[..ctx.frames])
                .all(|x| x.abs() < INAUDIBLE);
        if self.quiet >= IDLE_AFTER && input_quiet {
            out.silence(0, 0);
            out.silence(0, 1);
            return;
        }
        let mut left = std::mem::take(&mut self.left);
        let mut right = std::mem::take(&mut self.right);
        let scale = self.sample_rate / 48_000.0;

        let tail_samples = self.lines.iter().map(|l| l.mask).max().unwrap_or(0) as f32;
        // The feedback gains: each line loses 60 dB in `rt60`. The few
        // samples of wobble in a line's length change this far below what
        // anyone can hear, so it is worked out once for the block.
        let room = size[0].clamp(0.5, 1.5);
        let rt60 = decay[0].clamp(0.1, 20.0);
        let gains: [f32; 8] = std::array::from_fn(|n| {
            let delay = (LINES[n] * scale * room).clamp(2.0, tail_samples - 4.0);
            10f32.powf(-3.0 * delay / (rt60 * self.sample_rate))
        });
        let mut tail = 0.0f32;
        for i in 0..ctx.frames {
            let (dry_l, dry_r) = (in_l[i], in_r[i]);
            // Pre-delay, then the allpass smear.
            let pd = (predelay[i].clamp(0.0, MAX_PREDELAY) * self.sample_rate).max(0.0);
            self.predelay[0].push(dry_l);
            self.predelay[1].push(dry_r);
            let (pl, pr) = (self.predelay[0].read(pd), self.predelay[1].read(pd));
            let mut inj = [pl, pr];
            for stage in 0..2 {
                inj = [
                    self.allpass(stage, 0, inj[0]),
                    self.allpass(stage, 1, inj[1]),
                ];
            }

            let room = size[i].clamp(0.5, 1.5);
            // Per sample, so the sound doesn't depend on how it is cut into
            // blocks (the playback cache renders in blocks of its own).
            let phase = self.phase + WOBBLE_RATE * i as f32 / self.sample_rate;
            let damp = damping[i].clamp(0.0, 1.0) * 0.9;
            let mut y = [0.0f32; 8];
            for (n, ring) in self.lines.iter().enumerate() {
                let wobble = WOBBLE * scale * (TAU * (phase + n as f32 / 8.0)).sin();
                let delay = (LINES[n] * scale * room + wobble).clamp(2.0, tail_samples - 4.0);
                let gain = gains[n];
                let raw = ring.read(delay - 1.0);
                self.damp[n] += (1.0 - damp) * (raw - self.damp[n]);
                y[n] = self.damp[n] * gain;
            }
            // Householder: every line gets the sum of all, less itself twice.
            tail = y.iter().fold(tail, |m, v| m.max(v.abs()));
            let sum: f32 = y.iter().sum::<f32>() * 0.25;
            for (n, &y) in y.iter().enumerate() {
                let source = if n % 2 == 0 { inj[0] } else { inj[1] };
                self.lines[n].push(y - sum + GAIN_IN * source);
            }

            // Left from the even lines, right from the odd, in alternating
            // signs so they are not all in phase.
            let (mut wet_l, mut wet_r) = (0.0, 0.0);
            for n in 0..4 {
                let sign = if n % 2 == 0 { 1.0 } else { -1.0 };
                wet_l += sign * y[2 * n];
                wet_r += sign * y[2 * n + 1];
            }
            let (wet_l, wet_r) = (wet_l * GAIN_OUT, wet_r * GAIN_OUT);
            let mid = 0.5 * (wet_l + wet_r);
            let w = width[i].clamp(0.0, 1.0);
            let (wet_l, wet_r) = (mid + w * (wet_l - mid), mid + w * (wet_r - mid));
            let m = mix[i].clamp(0.0, 1.0);
            left[i] = dry_l * (1.0 - m) + wet_l * m;
            right[i] = dry_r * (1.0 - m) + wet_r * m;
        }
        self.quiet = if input_quiet && tail < INAUDIBLE {
            self.quiet.saturating_add(ctx.frames)
        } else {
            0
        };
        if self.quiet >= IDLE_AFTER {
            self.reset();
            self.quiet = IDLE_AFTER;
            left.fill(0.0);
            right.fill(0.0);
        }
        self.phase = (self.phase + WOBBLE_RATE * ctx.frames as f32 / self.sample_rate).fract();

        // Memory an infinite input made non-finite would never clear.
        if !self.damp.iter().all(|x| x.is_finite())
            || !left[..ctx.frames]
                .iter()
                .chain(&right[..ctx.frames])
                .all(|x| x.is_finite())
        {
            self.reset();
            left.fill(0.0);
            right.fill(0.0);
        }
        out.lane_mut(0, 0).copy_from_slice(&left[..ctx.frames]);
        out.lane_mut(0, 1).copy_from_slice(&right[..ctx.frames]);
        self.left = left;
        self.right = right;
        for d in &mut self.damp {
            if d.abs() < 1e-30 {
                *d = 0.0;
            }
        }
    }

    fn reset(&mut self) {
        for ring in self
            .lines
            .iter_mut()
            .chain(self.predelay.iter_mut())
            .chain(self.allpass.iter_mut().flatten())
        {
            ring.clear();
        }
        self.damp = [0.0; 8];
        self.quiet = 0;
        self.phase = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;
    const IN: usize = ReverbPorts::INPUT;
    const OUT: usize = ReverbPorts::OUT;

    fn harness(shape: Shape) -> Harness {
        Harness::new(&Reverb, &Config::new(), &[(IN, shape)], RATE, 4_800).unwrap()
    }

    fn impulse(h: &mut Harness, blocks: usize) -> Vec<(Vec<f32>, Vec<f32>)> {
        let mut out = Vec::new();
        for block in 0..blocks {
            let mut input = h.input(IN, 4_800);
            input.fill(0.0);
            if block == 0 {
                input.lane_mut(0, 0)[0] = 1.0;
            }
            h.run(4_800).unwrap();
            out.push((
                h.output(OUT).lane(0, 0).to_vec(),
                h.output(OUT).lane(0, 1).to_vec(),
            ));
        }
        out
    }

    #[test]
    fn the_sound_does_not_depend_on_the_block_size() {
        let play = |block: usize| {
            let mut h =
                Harness::new(&Reverb, &Config::new(), &[(IN, Shape::MONO)], RATE, block).unwrap();
            h.set(ReverbPorts::MIX, 1.0);
            let (mut l, mut r) = (Vec::new(), Vec::new());
            for start in (0..9_600).step_by(block) {
                for (i, x) in h.input(IN, block).lane_mut(0, 0).iter_mut().enumerate() {
                    *x = ((start + i) as f32 * 0.03).sin();
                }
                h.run(block).unwrap();
                l.extend_from_slice(h.output(OUT).lane(0, 0));
                r.extend_from_slice(h.output(OUT).lane(0, 1));
            }
            (l, r)
        };
        let (whole, cut) = (play(4_800), play(96));
        // Equal but for rounding in the phase; cutting the wobble by block
        // used to differ by about 5e-3.
        for (a, b) in [(&whole.0, &cut.0), (&whole.1, &cut.1)] {
            let worst = a
                .iter()
                .zip(b)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0, f32::max);
            assert!(worst < 1e-4, "differs by {worst}");
        }
        assert!(rms(&whole.0) > 0.01, "the reverb sounds");
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn a_dry_mix_passes_the_input() {
        let mut h = harness(Shape::MONO);
        h.set(ReverbPorts::MIX, 0.0);
        for (i, x) in h.input(IN, 4_800).lane_mut(0, 0).iter_mut().enumerate() {
            *x = (i as f32 * 0.05).sin();
        }
        h.run(4_800).unwrap();
        let (l, r) = (
            h.output(OUT).lane(0, 0).to_vec(),
            h.output(OUT).lane(0, 1).to_vec(),
        );
        assert_eq!(l, r);
        assert!((l[100] - (100.0f32 * 0.05).sin()).abs() < 1e-6);
    }

    #[test]
    fn an_impulse_rings_out_and_decays_at_the_set_time() {
        let mut h = harness(Shape::MONO);
        h.set(ReverbPorts::MIX, 1.0);
        h.set(ReverbPorts::DECAY, 1.0);
        h.set(ReverbPorts::DAMPING, 0.0);
        let blocks = impulse(&mut h, 30);
        // Level (dB) of a 0.1 s block at 0.3 s, and at 1.3 s: 1 s later, so
        // 60 dB down.
        let level = |b: usize| 20.0 * rms(&blocks[b].0).log10();
        assert!(level(3) > -90.0, "there is a tail: {}", level(3));
        let drop = level(3) - level(3 + 10);
        assert!((drop - 60.0).abs() < 12.0, "fell {drop} dB in a second");
        assert!(
            blocks[0].0[..40].iter().all(|x| x.abs() < 1e-3),
            "after the pre-delay"
        );
    }

    #[test]
    fn a_dead_tail_goes_idle_and_the_next_sound_wakes_it() {
        let mut h = harness(Shape::MONO);
        h.set(ReverbPorts::MIX, 1.0);
        h.set(ReverbPorts::DECAY, 0.3);
        let blocks = impulse(&mut h, 50);
        assert!(rms(&blocks[1].0) > 1e-5, "there is a tail first");
        assert!(
            h.output(OUT).is_silent(0, 0),
            "idle flags its output silent"
        );
        assert!(h.output(OUT).is_silent(0, 1));
        // A new sound rings again.
        let mut input = h.input(IN, 4_800);
        input.fill(0.0);
        input.lane_mut(0, 0)[0] = 1.0;
        h.run(4_800).unwrap();
        h.run(4_800).unwrap();
        assert!(!h.output(OUT).is_silent(0, 0));
        assert!(rms(h.output(OUT).lane(0, 0)) > 1e-4);
    }

    #[test]
    fn a_longer_decay_lasts_longer() {
        let tail = |decay| {
            let mut h = harness(Shape::MONO);
            h.set(ReverbPorts::MIX, 1.0);
            h.set(ReverbPorts::DECAY, decay);
            let blocks = impulse(&mut h, 20);
            rms(&blocks[19].0)
        };
        assert!(tail(4.0) > 5.0 * tail(1.0));
    }

    #[test]
    fn damping_takes_the_highs_out_of_the_tail() {
        let brightness = |damping| {
            let mut h = harness(Shape::MONO);
            h.set(ReverbPorts::MIX, 1.0);
            h.set(ReverbPorts::DAMPING, damping);
            h.set(ReverbPorts::DECAY, 3.0);
            let blocks = impulse(&mut h, 6);
            // The energy in the difference signal against the signal.
            let x = &blocks[4].0;
            let diff: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
            rms(&diff) / rms(x)
        };
        assert!(brightness(0.9) < 0.6 * brightness(0.0));
    }

    #[test]
    fn width_zero_is_mono() {
        let mut h = harness(Shape::MONO);
        h.set(ReverbPorts::MIX, 1.0);
        h.set(ReverbPorts::WIDTH, 0.0);
        let blocks = impulse(&mut h, 3);
        assert_eq!(blocks[2].0, blocks[2].1);
        let mut h = harness(Shape::MONO);
        h.set(ReverbPorts::MIX, 1.0);
        let blocks = impulse(&mut h, 3);
        assert_ne!(blocks[2].0, blocks[2].1, "full width differs");
    }

    #[test]
    fn white_noise_comes_back_at_about_the_level_it_went_in() {
        let mut h = harness(Shape::MONO);
        h.set(ReverbPorts::MIX, 1.0);
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        };
        let mut out = Vec::new();
        for block in 0..10 {
            for x in h.input(IN, 4_800).lane_mut(0, 0) {
                *x = 0.5 * noise();
            }
            h.run(4_800).unwrap();
            if block >= 5 {
                out.extend_from_slice(h.output(OUT).lane(0, 0));
            }
        }
        let ratio = rms(&out) / (0.5 / 3f32.sqrt());
        assert!((0.3..1.5).contains(&ratio), "gain {ratio}");
    }

    #[test]
    fn it_is_deterministic_and_recovers_from_infinity() {
        let run = || {
            let mut h = harness(Shape::STEREO);
            impulse(&mut h, 2)
        };
        assert_eq!(run(), run());

        let mut h = harness(Shape::MONO);
        h.input(IN, 4_800).lane_mut(0, 0)[5] = f32::INFINITY;
        h.run(4_800).unwrap();
        h.input(IN, 4_800).fill(0.0);
        h.run(4_800).unwrap();
        assert!(h.output(OUT).lane(0, 1).iter().all(|x| x.is_finite()));
    }

    #[test]
    fn a_polyphonic_input_is_refused() {
        let layout = Reverb.layout(&Config::new()).unwrap();
        let mut inputs = vec![Shape::MONO; layout.inputs.len()];
        inputs[IN] = Shape::new(4, 1);
        assert!(
            Reverb
                .output_shapes(&Config::new(), &layout, &inputs)
                .is_err()
        );
    }
}
