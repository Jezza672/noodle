//! The Convolver node: a sound run through a recorded impulse response.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports,
    Setup, Shape, ShapeError, Unit,
};

use crate::level::db_to_gain;
use crate::sample::{Sample, SampleRef, SampleStore};

/// Convolves its input with an impulse response, the recording of a room (or
/// a plate, a spring, a speaker cabinet) answering a click: the result is the
/// input as if it had been played in that space.
///
/// The impulse response is a sample input, `ir`, so it comes from a Sample
/// node (the Convolution Reverb group has one inside). It is resampled to the
/// engine's rate when the node is built, cut at 10 seconds, and scaled so the
/// louder of its two channels has unit energy: a noise-like sound comes
/// out as loud as it went in, whatever the level of the file, and `gain`
/// adjusts from there. With no impulse response the output is silent.
///
/// The input is mono or stereo, one voice. The output is stereo: a mono input
/// is convolved with the impulse response's left channel for the left output
/// and its right for the right, so a stereo recording of a room gives a wide
/// reverb from a mono source; a stereo input is convolved channel by channel
/// (left with left, right with right).
///
/// It is exact (uniform partitioned convolution: 256-sample blocks, a
/// frequency-domain delay line, one multiply-add per partition per block), so
/// the sound is the true convolution, tail and all. The price is a delay of one
/// block, 256 samples (5 ms at 48 kHz), reported to the engine so a dry path
/// beside it is lined up. The work grows with the length of the response:
/// about 0.15 partitions-times-bins multiply-adds per sample per second of it.
pub struct Convolver {
    samples: SampleStore,
}

impl Convolver {
    pub fn new(samples: &SampleStore) -> Self {
        Self {
            samples: samples.clone(),
        }
    }
}

pub const CONVOLVER_ID: &str = "noodle.effect.convolver";

/// The config key the compiler writes the `ir` sample input to.
pub const IR_KEY: &str = "ir";

#[derive(Ports)]
struct ConvolverPorts {
    #[input("in", "In")]
    input: (),
    #[sample_input("ir", "Impulse response")]
    ir: (),
    #[param(
        "gain",
        "Gain",
        ParamInfo::new(-24.0, 24.0, 0.0).unit(Unit::Decibels)
    )]
    gain: (),
    #[output("out", "Out")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: CONVOLVER_ID,
    version: 1,
    name: "Convolver",
    category: "Effects",
};

/// Samples per partition: also the node's delay.
pub const BLOCK: usize = 256;
const FFT: usize = 2 * BLOCK;
const BINS: usize = BLOCK + 1;
/// The longest impulse response used, in seconds.
pub const MAX_SECONDS: f32 = 10.0;

impl NodeType for Convolver {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let layout = ConvolverPorts::layout();
        // The past dies away within the response, and the block's delay.
        Ok(
            match self
                .reference(config)
                .and_then(|r| self.samples.get(&r.hash).map(|s| (r, s)))
            {
                Some((reference, sample)) if sample.frames() > 0 => {
                    let seconds = (sample.window(reference.start, reference.end).frames() as f32
                        / sample.sample_rate as f32)
                        .min(MAX_SECONDS);
                    layout.bounded((1000.0 * seconds).ceil() as u32 + 20)
                }
                _ => layout.memoryless(),
            },
        )
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let input = inputs[ConvolverPorts::INPUT];
        if input.voices != 1 {
            return Err(ShapeError(Shape::STEREO, input).into());
        }
        Ok(vec![Shape::STEREO])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let channels = setup.input_shapes[ConvolverPorts::INPUT].channels.min(2);
        let ir = self.response(setup.config, setup.sample_rate);
        Ok(Instance::realtime(ConvolverNode::new(ir, channels)))
    }
}

impl Convolver {
    /// The sample `config` names as the response.
    fn reference(&self, config: &Config) -> Option<SampleRef> {
        let text = config.get(IR_KEY).and_then(|v| match v {
            noodle_core::Value::Text(t) => Some(t.as_str()),
            _ => None,
        })?;
        (!text.is_empty()).then(|| SampleRef::parse(text))
    }

    /// The response `config` names, at `rate`, ready to convolve with.
    fn response(&self, config: &Config, rate: f32) -> Option<Response> {
        let reference = self.reference(config)?;
        let sample = self
            .samples
            .get(&reference.hash)?
            .window(reference.start, reference.end);
        Response::new(&sample, rate)
    }
}

/// An impulse response at the engine's rate: two channels of samples.
struct Response {
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Response {
    fn new(sample: &Sample, rate: f32) -> Option<Self> {
        if sample.frames() == 0 {
            return None;
        }
        let ratio = f64::from(sample.sample_rate) / f64::from(rate);
        let frames = ((sample.frames() as f64 / ratio) as usize)
            .min((MAX_SECONDS * rate) as usize)
            .max(1);
        let resample = |channel: &[f32]| -> Vec<f32> {
            (0..frames)
                .map(|i| {
                    // Linear interpolation: a response is broadband noise, so
                    // the interpolation's small high-frequency loss is
                    // inaudible in it.
                    let at = i as f64 * ratio;
                    let a = at.floor() as usize;
                    let t = (at - a as f64) as f32;
                    let x0 = channel.get(a).copied().unwrap_or(0.0);
                    let x1 = channel.get(a + 1).copied().unwrap_or(0.0);
                    x0 + (x1 - x0) * t
                })
                .collect()
        };
        let mut left = resample(&sample.left);
        let mut right = resample(&sample.right);
        // Fade the last twentieth, so cutting a long response doesn't click.
        let fade = (frames / 20).max(1);
        for channel in [&mut left, &mut right] {
            for i in 0..fade {
                channel[frames - 1 - i] *= i as f32 / fade as f32;
            }
        }
        let energy = |c: &[f32]| c.iter().map(|x| x * x).sum::<f32>();
        let loudest = energy(&left).max(energy(&right));
        if loudest <= 0.0 {
            return None;
        }
        let scale = loudest.sqrt().recip();
        for x in left.iter_mut().chain(&mut right) {
            *x *= scale;
        }
        Some(Self { left, right })
    }
}

struct ConvolverNode {
    channels: usize,
    partitions: usize,
    /// The response's spectrum, per output channel: `partitions` blocks of
    /// `BINS` bins.
    response: [Vec<Complex<f32>>; 2],
    /// The spectra of the last `partitions` input blocks, per input channel,
    /// as a ring.
    history: [Vec<Complex<f32>>; 2],
    /// The slot in the ring that holds the newest block.
    newest: usize,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    /// The block being collected, per input channel, and how much of it is.
    collecting: [Vec<f32>; 2],
    fill: usize,
    /// The finished block that is playing, per output channel.
    playing: [Vec<f32>; 2],
    /// The second half of the last inverse transform, still to add.
    tail: [Vec<f32>; 2],
    time: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    accumulator: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Blocks in a row that were all zero: once the history is all zero, the
    /// answer is too.
    quiet: usize,
}

impl ConvolverNode {
    fn new(response: Option<Response>, channels: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(FFT);
        let inverse = planner.plan_fft_inverse(FFT);
        let mut time = forward.make_input_vec();
        let mut spectrum = forward.make_output_vec();
        let mut scratch = forward.make_scratch_vec();
        let longest = scratch.len().max(inverse.make_scratch_vec().len());
        scratch.resize(longest, Complex::new(0.0, 0.0));
        let partitions = response
            .as_ref()
            .map_or(0, |r| r.left.len().div_ceil(BLOCK));
        let mut spectra = [Vec::new(), Vec::new()];
        if let Some(response) = &response {
            for (spectra, channel) in spectra.iter_mut().zip([&response.left, &response.right]) {
                spectra.reserve(partitions * BINS);
                for part in channel.chunks(BLOCK) {
                    time.fill(0.0);
                    time[..part.len()].copy_from_slice(part);
                    let _ = forward.process_with_scratch(&mut time, &mut spectrum, &mut scratch);
                    spectra.extend_from_slice(&spectrum);
                }
            }
        }
        let zeros = |n: usize| vec![0.0; n];
        Self {
            channels,
            partitions,
            response: spectra,
            history: [
                vec![Complex::new(0.0, 0.0); partitions * BINS],
                vec![Complex::new(0.0, 0.0); partitions * BINS],
            ],
            newest: 0,
            forward,
            inverse,
            collecting: [zeros(BLOCK), zeros(BLOCK)],
            fill: 0,
            playing: [zeros(BLOCK), zeros(BLOCK)],
            tail: [zeros(BLOCK), zeros(BLOCK)],
            time,
            spectrum,
            accumulator: vec![Complex::new(0.0, 0.0); BINS],
            scratch,
            quiet: 0,
        }
    }

    /// A block of input has been collected: transform it, and make the next
    /// block of output.
    fn step(&mut self) {
        let silent = self.collecting[..self.channels]
            .iter()
            .all(|c| c.iter().all(|&x| x == 0.0));
        self.quiet = if silent { self.quiet + 1 } else { 0 };
        if self.partitions == 0 {
            return;
        }
        // With a whole response of silence behind it, the answer is silence.
        let dead = self.quiet > self.partitions + 1;
        self.newest = (self.newest + 1) % self.partitions;
        for c in 0..self.channels {
            let slot = &mut self.history[c][self.newest * BINS..(self.newest + 1) * BINS];
            if dead {
                slot.fill(Complex::new(0.0, 0.0));
                continue;
            }
            self.time[..BLOCK].copy_from_slice(&self.collecting[c]);
            self.time[BLOCK..].fill(0.0);
            let _ = self
                .forward
                .process_with_scratch(&mut self.time, slot, &mut self.scratch);
        }
        for ch in 0..2 {
            if dead {
                self.playing[ch].fill(0.0);
                self.tail[ch].fill(0.0);
                continue;
            }
            let input = if self.channels == 1 { 0 } else { ch };
            self.accumulator.fill(Complex::new(0.0, 0.0));
            // Partition `p` of the response meets the block `p` blocks ago.
            for p in 0..self.partitions {
                let slot = (self.newest + self.partitions - p) % self.partitions;
                let x = &self.history[input][slot * BINS..(slot + 1) * BINS];
                let h = &self.response[ch][p * BINS..(p + 1) * BINS];
                for ((acc, x), h) in self.accumulator.iter_mut().zip(x).zip(h) {
                    *acc += x * h;
                }
            }
            // The transform wants real DC and Nyquist bins.
            self.accumulator[0].im = 0.0;
            self.accumulator[BINS - 1].im = 0.0;
            self.spectrum.copy_from_slice(&self.accumulator);
            let _ = self.inverse.process_with_scratch(
                &mut self.spectrum,
                &mut self.time,
                &mut self.scratch,
            );
            let scale = 1.0 / FFT as f32;
            for i in 0..BLOCK {
                self.playing[ch][i] = (self.time[i] + self.tail[ch][i]) * scale;
                self.tail[ch][i] = self.time[BLOCK + i];
            }
        }
    }
}

impl Node for ConvolverNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let input = &io.inputs[ConvolverPorts::INPUT];
        let gain = &io.inputs[ConvolverPorts::GAIN];
        let frames = input.frames();
        let gains = gain.lane(0, 0);
        let last_gain = gains.last().copied().unwrap_or(0.0);
        for t in 0..frames {
            for c in 0..self.channels {
                self.collecting[c][self.fill] = input.lane(0, c)[t];
            }
            let g = db_to_gain(gains.get(t).copied().unwrap_or(last_gain));
            for (ch, play) in self.playing.iter().enumerate() {
                io.outputs[ConvolverPorts::OUT].lane_mut(0, ch)[t] = play[self.fill] * g;
            }
            self.fill += 1;
            if self.fill == BLOCK {
                self.fill = 0;
                self.step();
            }
        }
    }

    fn latency(&self) -> u32 {
        BLOCK as u32
    }

    fn reset(&mut self) {
        for h in &mut self.history {
            h.fill(Complex::new(0.0, 0.0));
        }
        for v in self
            .collecting
            .iter_mut()
            .chain(&mut self.playing)
            .chain(&mut self.tail)
        {
            v.fill(0.0);
        }
        self.fill = 0;
        self.newest = 0;
        self.quiet = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;
    use noodle_io::Audio;

    const RATE: f32 = 48_000.0;

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    /// A stereo response (interleaved) in a store, and the config naming it.
    fn response(left: &[f32], right: &[f32]) -> (SampleStore, Config) {
        let store = SampleStore::new();
        let samples = left.iter().zip(right).flat_map(|(&l, &r)| [l, r]).collect();
        let hash = store.insert(&Audio {
            samples,
            channels: 2,
            sample_rate: RATE as u32,
        });
        let mut config = Config::new();
        config.set(IR_KEY, Value::Text(hash));
        (store, config)
    }

    fn naive(input: &[f32], h: &[f32]) -> Vec<f32> {
        (0..input.len())
            .map(|n| {
                (0..h.len().min(n + 1))
                    .map(|k| h[k] * input[n - k])
                    .sum::<f32>()
            })
            .collect()
    }

    /// Runs `input` (mono) through the node in blocks of `block`; returns the
    /// two outputs.
    fn run(store: &SampleStore, config: &Config, input: &[f32], block: usize) -> [Vec<f32>; 2] {
        let mut h = Harness::new(
            &Convolver::new(store),
            config,
            &[(ConvolverPorts::INPUT, Shape::MONO)],
            RATE,
            block,
        )
        .unwrap();
        let mut out = [Vec::new(), Vec::new()];
        for chunk in input.chunks(block) {
            h.input(ConvolverPorts::INPUT, chunk.len())
                .lane_mut(0, 0)
                .copy_from_slice(chunk);
            h.run(chunk.len()).unwrap();
            for (ch, out) in out.iter_mut().enumerate() {
                out.extend_from_slice(h.output(ConvolverPorts::OUT).lane(0, ch));
            }
        }
        out
    }

    #[test]
    fn it_is_the_true_convolution_across_many_partitions_and_block_sizes() {
        // A response of 1 500 samples spans six 256-sample partitions.
        let left = noise(1_500, 1);
        let right = noise(1_500, 2);
        let (store, config) = response(&left, &right);
        let input = noise(5_000, 3);
        let energy = |c: &[f32]| c.iter().map(|x| x * x).sum::<f32>();
        let scale = energy(&left).max(energy(&right)).sqrt().recip();
        // The last twentieth of the response is faded out when it's built.
        let fade = |c: &[f32]| {
            let mut c: Vec<f32> = c.iter().map(|x| x * scale).collect();
            let n = c.len();
            let f = (n / 20).max(1);
            for i in 0..f {
                c[n - 1 - i] *= i as f32 / f as f32;
            }
            c
        };
        let (want_left, want_right) = (naive(&input, &fade(&left)), naive(&input, &fade(&right)));
        for block in [256, 37, 1024] {
            let out = run(&store, &config, &input, block);
            for (out, want) in out.iter().zip([&want_left, &want_right]) {
                // The fade changes the energy a little, and so the scale.
                let latency = BLOCK;
                let mut worst = 0.0f32;
                for n in 0..input.len() - latency {
                    worst =
                        worst.max((out[n + latency] - want[n] * fade_gain(&left, &right)).abs());
                }
                assert!(worst < 2e-3, "block {block}: worst {worst}");
            }
        }
    }

    /// The scale the node applies on top of this test's, which scales by the
    /// energy before the fade: the node scales after it.
    fn fade_gain(left: &[f32], right: &[f32]) -> f32 {
        let faded = |c: &[f32]| {
            let mut c = c.to_vec();
            let n = c.len();
            let f = (n / 20).max(1);
            for i in 0..f {
                c[n - 1 - i] *= i as f32 / f as f32;
            }
            c.iter().map(|x| x * x).sum::<f32>()
        };
        let energy = |c: &[f32]| c.iter().map(|x| x * x).sum::<f32>();
        let before = energy(left).max(energy(right));
        let after = faded(left).max(faded(right));
        (before / after).sqrt()
    }

    #[test]
    fn a_single_click_response_is_a_delay() {
        let mut left = vec![0.0; 600];
        left[100] = 0.7;
        let (store, config) = response(&left, &left);
        let input = noise(3_000, 4);
        let out = run(&store, &config, &input, 128);
        // Normalised to unit energy, so the click is 1: a pure delay of 100
        // samples, plus the node's own block.
        for n in 0..2_000 {
            assert!(
                (out[0][n + 100 + BLOCK] - input[n]).abs() < 1e-4,
                "sample {n}"
            );
            assert!((out[1][n + 100 + BLOCK] - input[n]).abs() < 1e-4);
        }
    }

    #[test]
    fn without_a_response_it_is_silent_and_reports_its_delay() {
        let store = SampleStore::new();
        let out = run(&store, &Config::new(), &noise(1_000, 5), 128);
        assert!(out[0].iter().chain(&out[1]).all(|&x| x == 0.0));
        let node = ConvolverNode::new(None, 1);
        assert_eq!(node.latency(), BLOCK as u32);
    }

    #[test]
    fn a_response_at_another_rate_is_resampled() {
        // A click 0.01 s in, recorded at 24 kHz, lands 480 samples in at
        // 48 kHz.
        let store = SampleStore::new();
        let mut samples = vec![0.0; 600];
        samples[240] = 1.0;
        let hash = store.insert(&Audio {
            samples,
            channels: 1,
            sample_rate: 24_000,
        });
        let mut config = Config::new();
        config.set(IR_KEY, Value::Text(hash));
        let mut input = vec![0.0; 3_000];
        input[10] = 1.0;
        let out = run(&store, &config, &input, 256);
        let peak = out[0]
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0;
        // 10 + 480 + the block; a linear resample spreads it over a sample.
        assert!(
            (peak as i64 - (10 + 480 + BLOCK) as i64).abs() <= 1,
            "peak at {peak}"
        );
    }
}
