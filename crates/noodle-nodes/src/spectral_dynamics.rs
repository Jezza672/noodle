//! Spectral Compress and Spectral Denoise: dynamics that work on the levels
//! of the bins instead of the level of the whole signal.

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports,
    Setup, Unit, fft_bins, fft_hop,
};

use crate::level::db_to_gain;
use crate::spectral::{band_edges, map_frames, param, spectral_shape};

const CATEGORY: &str = "Spectral";

/// Below this (-140 dB) a level is silence: it is never boosted.
const SILENCE: f32 = 1e-7;

fn to_db(x: f32) -> f32 {
    20.0 * (x + 1e-12).log10()
}

// ---------------------------------------------------------------------------
// Spectral Compress
// ---------------------------------------------------------------------------

/// A compressor for each frequency band at once. Where an ordinary compressor
/// turns the whole signal down when it is loud, this turns each band down when
/// that band is loud, so a harsh resonance or one dominant partial is
/// tamed without dulling the rest, and a bass-heavy sound is evened out
/// without pumping its highs.
///
/// - `threshold` is the level a band must pass, in decibels (a sine of
///   amplitude 1 reads 0 dB on its bin; noise reads far lower on each bin, so
///   thresholds are often between -60 and -30).
/// - `ratio` is how much of the excess is kept: 4 turns 8 dB over into 2 dB
///   over.
/// - `attack` and `release` are how fast the level a band is judged by rises
///   and falls, in seconds (one frame is the quickest it can follow).
/// - `knee` softens the corner, in decibels.
/// - `makeup` is added after, in decibels.
/// - `bands` smooths the levels into that many log-spaced bands (2 to 256), so
///   the gain doesn't flutter from bin to bin; below 2 each bin is on its
///   own.
/// - `mode` *Upward* lifts quiet bands up towards the threshold instead
///   (never by more than 30 dB, and never a silent bin), which fills out a
///   thin sound and brings up detail.
pub struct SpectralCompress;

pub const SPECTRAL_COMPRESS_ID: &str = "noodle.spectral.compress";

/// The most a band is lifted in Upward mode, in decibels.
const MAX_BOOST_DB: f32 = 30.0;
/// The most bands Spectral Compress can smooth into.
const MAX_BANDS: usize = 256;

#[derive(Ports)]
struct CompressPorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[param(
        "threshold",
        "Threshold",
        ParamInfo::new(-120.0, 0.0, -50.0)
            .unit(Unit::Decibels)
            .offset()
    )]
    threshold: (),
    #[param("ratio", "Ratio", ParamInfo::new(1.0, 20.0, 4.0).log())]
    ratio: (),
    #[param(
        "attack",
        "Attack",
        ParamInfo::new(0.001, 0.5, 0.02)
            .log()
            .unit(Unit::Seconds)
    )]
    attack: (),
    #[param(
        "release",
        "Release",
        ParamInfo::new(0.005, 2.0, 0.2)
            .log()
            .unit(Unit::Seconds)
    )]
    release: (),
    #[param("knee", "Knee", ParamInfo::new(0.0, 24.0, 6.0).unit(Unit::Decibels))]
    knee: (),
    #[param(
        "makeup",
        "Makeup",
        ParamInfo::new(-12.0, 24.0, 0.0).unit(Unit::Decibels)
    )]
    makeup: (),
    #[param("bands", "Bands", ParamInfo::new(0.0, 256.0, 32.0))]
    bands: (),
    #[param("mode", "Mode", ParamInfo::choice(["Downward", "Upward"]))]
    mode: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static COMPRESS_INFO: NodeInfo = NodeInfo {
    id: SPECTRAL_COMPRESS_ID,
    version: 1,
    name: "Spectral Compress",
    category: CATEGORY,
};

impl NodeType for SpectralCompress {
    fn info(&self) -> &NodeInfo {
        &COMPRESS_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(CompressPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let lanes = spectral_shape(setup, CompressPorts::OUTPUTS).lanes();
        let bins = fft_bins(setup.spectral_size);
        Ok(Instance::realtime(CompressNode {
            bins,
            frame_seconds: fft_hop(setup.spectral_size) as f32 / setup.sample_rate,
            // One level per bin; with bands, the first `bands` are used.
            levels: vec![vec![f32::NEG_INFINITY; bins]; lanes],
            edges: vec![0; MAX_BANDS + 1],
            edges_for: 0,
        }))
    }
}

struct CompressNode {
    bins: usize,
    frame_seconds: f32,
    /// The level each band (or bin) is judged by, in decibels, per lane.
    levels: Vec<Vec<f32>>,
    edges: Vec<usize>,
    edges_for: usize,
}

/// The gain, in decibels, a compressor with `threshold`, `ratio` and `knee`
/// gives a band at `level`; negative when it turns the band down. Upward
/// compression mirrors it: it lifts a band below the threshold.
fn gain_db(level: f32, threshold: f32, ratio: f32, knee: f32, upward: bool) -> f32 {
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    if upward {
        // How far under the threshold.
        let under = threshold - level;
        let lift = if 2.0 * under < -knee {
            0.0
        } else if knee > 0.0 && 2.0 * under.abs() <= knee {
            slope * (under + knee / 2.0).powi(2) / (2.0 * knee)
        } else {
            slope * under
        };
        lift.min(MAX_BOOST_DB)
    } else {
        let over = level - threshold;
        if 2.0 * over < -knee {
            0.0
        } else if knee > 0.0 && 2.0 * over.abs() <= knee {
            -slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
        } else {
            -slope * over
        }
    }
}

impl Node for CompressNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let inputs = io.inputs;
        let bins = self.bins;
        let seconds = self.frame_seconds;
        let Self {
            levels,
            edges,
            edges_for,
            ..
        } = self;
        map_frames(io, |time, voice, lane, input, out| {
            let threshold = param(inputs, CompressPorts::THRESHOLD, voice, time);
            let ratio = param(inputs, CompressPorts::RATIO, voice, time).max(1.0);
            let attack = param(inputs, CompressPorts::ATTACK, voice, time).max(1e-4);
            let release = param(inputs, CompressPorts::RELEASE, voice, time).max(1e-4);
            let knee = param(inputs, CompressPorts::KNEE, voice, time).max(0.0);
            let makeup = param(inputs, CompressPorts::MAKEUP, voice, time);
            let upward = param(inputs, CompressPorts::MODE, voice, time) >= 0.5;
            let bands = param(inputs, CompressPorts::BANDS, voice, time)
                .round()
                .clamp(0.0, MAX_BANDS as f32) as usize;
            let bands = if bands < 2 { 0 } else { bands };
            let up = (-seconds / attack).exp();
            let down = (-seconds / release).exp();
            let level = &mut levels[lane];

            // One group of bins per band, or one per bin.
            let groups = if bands == 0 { bins } else { bands };
            if bands != 0 && bands != *edges_for {
                band_edges(bands, bins, edges);
                *edges_for = bands;
            }
            let range = |g: usize| {
                if bands == 0 {
                    g..g + 1
                } else {
                    edges[g]..edges[g + 1]
                }
            };
            for g in 0..groups {
                let r = range(g);
                let mean = input[r].iter().map(|b| b.power()).sum::<f32>() / range(g).len() as f32;
                let now = to_db(mean.sqrt());
                let held = &mut level[g];
                if !held.is_finite() {
                    *held = now;
                } else {
                    let keep = if now > *held { up } else { down };
                    *held = keep * *held + (1.0 - keep) * now;
                }
                let silent = mean.sqrt() < SILENCE;
                let gain = if silent && upward {
                    1.0
                } else {
                    db_to_gain(gain_db(*held, threshold, ratio, knee, upward) + makeup)
                };
                for (o, b) in out[range(g)].iter_mut().zip(&input[range(g)]) {
                    *o = b.scaled(gain);
                }
            }
        });
    }

    fn reset(&mut self) {
        for lane in &mut self.levels {
            lane.fill(f32::NEG_INFINITY);
        }
    }
}

// ---------------------------------------------------------------------------
// Spectral Denoise
// ---------------------------------------------------------------------------

/// Takes steady noise out of a sound, once it has heard what the noise is.
/// Turn `learn` on over a stretch of just the noise (a second of room tone,
/// hiss or hum before the music starts; you can automate it) and the node
/// remembers the average level of every bin. With `learn` off, bins that
/// aren't louder than that profile are turned down, and bins well above it
/// are left alone, so what is left sounds like the sound without the noise.
///
/// - `learn` *On* measures the noise (the sound passes through while it
///   does). Turning it on starts a new profile.
/// - `reduction` is the most a bin is turned down, in decibels.
/// - `threshold` is how far above the noise a bin must stand to be kept, in
///   decibels. Raise it if the noise still shows through; lower it if the
///   sound turns thin.
/// - `smooth` slows how quickly a bin's gain falls (0 to 1), which hides the
///   warbling "musical noise" that this kind of reduction can leave. A bin
///   that rises above the noise opens at once, so attacks keep their edge.
///
/// **A profile from a wire.** `learn` measures the noise inside the sound
/// being cleaned, and a profile learned in a pass is gone when the pass is
/// started again from somewhere else (a seek starts every node fresh). To
/// have it at any start, wire a spectrum of the noise alone (a clip of room
/// tone, looped, through its own Analyze) into `noise`: the node then keeps
/// its profile from that, a few seconds' average of it, whatever `learn` says.
///
/// With nothing learned the sound passes untouched.
pub struct SpectralDenoise;

pub const SPECTRAL_DENOISE_ID: &str = "noodle.spectral.denoise";

#[derive(Ports)]
struct DenoisePorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[spectral_input("noise", "Noise")]
    noise: (),
    #[param("learn", "Learn", ParamInfo::choice(["Off", "On"]))]
    learn: (),
    #[param(
        "reduction",
        "Reduction",
        ParamInfo::new(0.0, 60.0, 20.0).unit(Unit::Decibels)
    )]
    reduction: (),
    #[param(
        "threshold",
        "Threshold",
        ParamInfo::new(0.0, 30.0, 6.0).unit(Unit::Decibels)
    )]
    threshold: (),
    #[param("smooth", "Smooth", ParamInfo::new(0.0, 0.95, 0.5))]
    smooth: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static DENOISE_INFO: NodeInfo = NodeInfo {
    id: SPECTRAL_DENOISE_ID,
    version: 1,
    name: "Spectral Denoise",
    category: CATEGORY,
};

impl NodeType for SpectralDenoise {
    fn info(&self) -> &NodeInfo {
        &DENOISE_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(DenoisePorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let lanes = spectral_shape(setup, DenoisePorts::OUTPUTS).lanes();
        let bins = fft_bins(setup.spectral_size);
        let frames_per_second = setup.sample_rate / fft_hop(setup.spectral_size) as f32;
        Ok(Instance::realtime(DenoiseNode {
            bins,
            wire_frames: (WIRE_SECONDS * frames_per_second).max(8.0) as u32,
            lanes: (0..lanes).map(|_| DenoiseLane::new(bins)).collect(),
        }))
    }
}

/// How many seconds of a `noise` wire the profile averages over.
const WIRE_SECONDS: f32 = 4.0;

struct DenoiseLane {
    /// The mean power of each bin over the frames that were learned.
    profile: Vec<f32>,
    learned: u32,
    learning: bool,
    /// Whether the profile is following the `noise` wire.
    from_wire: bool,
    /// The gain each bin had in the last frame.
    gain: Vec<f32>,
    /// Scratch: the gains of this frame before smoothing across bins.
    raw: Vec<f32>,
}

impl DenoiseLane {
    fn new(bins: usize) -> Self {
        Self {
            profile: vec![0.0; bins],
            learned: 0,
            learning: false,
            from_wire: false,
            gain: vec![1.0; bins],
            raw: vec![1.0; bins],
        }
    }

    /// Folds one frame of noise into the profile, as a running mean over at
    /// most `window` frames.
    fn hear(&mut self, noise: &[noodle_engine::Bin], window: u32) {
        self.learned = (self.learned + 1).min(window);
        let n = self.learned as f32;
        for (mean, bin) in self.profile.iter_mut().zip(noise) {
            *mean += (bin.power() - *mean) / n;
        }
    }
}

struct DenoiseNode {
    bins: usize,
    wire_frames: u32,
    lanes: Vec<DenoiseLane>,
}

impl Node for DenoiseNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            spectral_inputs,
            spectral_outputs,
            ..
        } = io;
        let (signal, noise) = (&spectral_inputs[0], &spectral_inputs[1]);
        let out = &mut spectral_outputs[0];
        let shape = out.shape();
        let wire_frames = self.wire_frames;
        let mut next = 0;
        for frame in 0..signal.frames() {
            let time = signal.time(frame);
            // Bring the profile up to this frame from the noise wire.
            while next < noise.frames() && noise.time(next) <= time {
                self.hear_wire(noise, next, shape, wire_frames);
                next += 1;
            }
            let Some(at) = out.push(time) else { break };
            for voice in 0..shape.voices {
                for channel in 0..shape.channels {
                    let lane = voice * shape.channels + channel;
                    let state = &mut self.lanes[lane];
                    let input = signal.frame(frame, voice, channel);
                    let output = out.frame_mut(at, voice, channel);
                    let learn = param(inputs, DenoisePorts::LEARN, voice, time) >= 0.5;
                    if learn && !state.from_wire {
                        if !state.learning {
                            state.profile.fill(0.0);
                            state.learned = 0;
                        }
                        state.hear(input, u32::MAX);
                    }
                    state.learning = learn;
                    if (learn && !state.from_wire) || state.learned == 0 {
                        output.copy_from_slice(input);
                        state.gain.fill(1.0);
                        continue;
                    }
                    let reduction = param(inputs, DenoisePorts::REDUCTION, voice, time).max(0.0);
                    let threshold = param(inputs, DenoisePorts::THRESHOLD, voice, time).max(0.0);
                    let smooth = param(inputs, DenoisePorts::SMOOTH, voice, time).clamp(0.0, 0.95);
                    denoise(state, input, output, reduction, threshold, smooth);
                }
            }
        }
        while next < noise.frames() {
            self.hear_wire(noise, next, shape, wire_frames);
            next += 1;
        }
    }

    fn reset(&mut self) {
        for lane in &mut self.lanes {
            *lane = DenoiseLane::new(self.bins);
        }
    }
}

impl DenoiseNode {
    fn hear_wire(
        &mut self,
        noise: &noodle_engine::SpectralIn<'_>,
        frame: usize,
        shape: noodle_engine::Shape,
        window: u32,
    ) {
        for voice in 0..shape.voices {
            for channel in 0..shape.channels {
                let lane = &mut self.lanes[voice * shape.channels + channel];
                lane.from_wire = true;
                lane.hear(noise.frame(frame, voice, channel), window);
            }
        }
    }
}

/// Writes `input` to `output` with every bin turned down by how much of its
/// power the lane's learned noise accounts for.
fn denoise(
    state: &mut DenoiseLane,
    input: &[noodle_engine::Bin],
    output: &mut [noodle_engine::Bin],
    reduction: f32,
    threshold: f32,
    smooth: f32,
) {
    let floor = db_to_gain(-reduction);
    // Power the noise is taken to have, with the margin.
    let margin = 10.0f32.powf(threshold / 10.0);
    for ((raw, bin), noise) in state.raw.iter_mut().zip(input).zip(&state.profile) {
        let power = bin.power();
        let kept = if power > 1e-20 {
            (1.0 - margin * noise / power).max(0.0)
        } else {
            0.0
        };
        *raw = kept.sqrt().max(floor);
    }
    let bins = state.raw.len();
    for k in 0..bins {
        // Smooth across three bins, so one bin doesn't open alone.
        let lo = state.raw[k.saturating_sub(1)];
        let hi = state.raw[(k + 1).min(bins - 1)];
        let across = 0.25 * lo + 0.5 * state.raw[k] + 0.25 * hi;
        let target = across.min(1.0);
        // Open at once, close slowly.
        let g = if target >= state.gain[k] {
            target
        } else {
            smooth * state.gain[k] + (1.0 - smooth) * target
        };
        state.gain[k] = g;
        output[k] = input[k].scaled(g);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{Analyzer, FFT_SIZE, HOP, Shape, SignalIn, SpectralBuf};

    const RATE: f32 = 48_000.0;
    const FRAMES: usize = 60;

    /// `FRAMES` frames of white noise, from a seed.
    fn noise_frames(seed: u32) -> SpectralBuf {
        let mut state = seed;
        let input: Vec<f32> = (0..FRAMES * HOP + FFT_SIZE)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0) * 0.1
            })
            .collect();
        let mut buf = SpectralBuf::new(1, input.len());
        Analyzer::new(Shape::MONO).process(
            &SignalIn::new(&input, Shape::MONO, input.len()),
            &mut buf.as_out(Shape::MONO),
        );
        buf
    }

    fn feed(h: &mut Harness, frames: &SpectralBuf) -> Vec<f32> {
        let from = frames.as_in(Shape::MONO);
        let mut out = h.spectral_input(0);
        for f in 0..from.frames() {
            let at = out.push(from.time(f)).unwrap();
            out.frame_mut(at, 0, 0).copy_from_slice(from.frame(f, 0, 0));
        }
        h.run(from.time(from.frames() - 1) as usize + 1).unwrap();
        let result = h.spectral(0);
        (0..result.frames())
            .flat_map(|f| {
                result
                    .frame(f, 0, 0)
                    .iter()
                    .map(|b| b.power())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn harness() -> Harness {
        Harness::with_spectral(
            &SpectralDenoise,
            &Config::new(),
            &[],
            &[Shape::MONO, Shape::MONO],
            RATE,
            FRAMES * HOP + FFT_SIZE,
        )
        .unwrap()
    }

    #[test]
    fn learned_noise_is_taken_down() {
        let mut h = harness();
        // The second spectral input (the noise wire) is silent throughout.
        h.set(DenoisePorts::REDUCTION, 24.0);
        h.set(DenoisePorts::LEARN, 1.0);
        let heard = feed(&mut h, &noise_frames(1));
        let first = noise_frames(1);
        let first = first.as_in(Shape::MONO);
        let in_power: f32 = first
            .frame(first.frames() - 1, 0, 0)
            .iter()
            .map(|b| b.power())
            .sum();
        // Learning passes the sound.
        let passed: f32 = heard[heard.len() - 1025..].iter().sum();
        assert!((passed / in_power - 1.0).abs() < 1e-3);
        // Learn off: other noise of the same kind is far quieter.
        h.set(DenoisePorts::LEARN, 0.0);
        let cleaned = feed(&mut h, &noise_frames(2));
        let after: f32 = cleaned[cleaned.len() - 1025..].iter().sum();
        assert!(after < in_power * 0.1, "{after} vs {in_power}");
    }
}
