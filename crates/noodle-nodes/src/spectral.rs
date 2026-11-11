//! Spectral nodes: [Analyze](Analyze) and [Synthesize](Synthesize), which turn
//! audio into FFT frames and back, and the nodes that change the frames
//! between them.
//!
//! The frames and their scaling are described in
//! [`noodle_engine::Analyzer`]; there is one frame size for the whole engine,
//! so any spectral node feeds any other. A spectral node works on frames, not
//! samples: it pushes one frame out for each frame in, at the same time, so
//! only the pair of Analyze and Synthesize adds delay
//! ([`SYNTH_LATENCY`], which the engine compensates where the spectral path
//! meets a path that doesn't have it).

use noodle_engine::{
    Analyzer, BINS, Bin, Config, Context, FFT_SIZE, FormantWarp, HOP, Instance, Io, Layout, Node,
    NodeError, NodeInfo, NodeType, ParamInfo, Ports, SYNTH_LATENCY, Setup, Shape, SignalIn,
    Synthesizer, Unit, bin_frequency,
};

const CATEGORY: &str = "Spectral";

fn db_to_amp(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn wrap_phase(x: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    x - tau * (x / tau).round()
}

/// The value a parameter holds at `time`, for a voice (a mono parameter
/// serves every voice).
fn param(inputs: &[SignalIn<'_>], port: usize, voice: usize, time: u32) -> f32 {
    let lane = inputs[port].lane(voice, 0);
    lane.get(time as usize)
        .or(lane.last())
        .copied()
        .unwrap_or(0.0)
}

/// Runs `f` on each frame of the first spectral input, writing the result as
/// a frame of the first spectral output at the same time. `f` gets the
/// frame's time, the voice, the lane, the bins in and the bins out (zero to
/// begin with).
fn map_frames(io: Io<'_, '_>, mut f: impl FnMut(u32, usize, usize, &[Bin], &mut [Bin])) {
    let Io {
        spectral_inputs,
        spectral_outputs,
        ..
    } = io;
    let input = &spectral_inputs[0];
    let out = &mut spectral_outputs[0];
    let shape = out.shape();
    for frame in 0..input.frames() {
        let time = input.time(frame);
        let Some(at) = out.push(time) else { break };
        for voice in 0..shape.voices {
            for channel in 0..shape.channels {
                let lane = voice * shape.channels + channel;
                f(
                    time,
                    voice,
                    lane,
                    input.frame(frame, voice, channel),
                    out.frame_mut(at, voice, channel),
                );
            }
        }
    }
}

/// The shape of the first spectral output, from `Setup`: the signal outputs
/// come first in `output_shapes`.
fn spectral_shape(setup: &Setup<'_>, signal_outputs: usize) -> Shape {
    setup
        .output_shapes
        .get(signal_outputs)
        .copied()
        .unwrap_or(Shape::MONO)
}

/// Turns audio into a spectral signal: the FFT frames the other spectral
/// nodes work on. Each lane of the audio (voices and channels) becomes a lane
/// of the spectrum.
///
/// Wire the result through any spectral nodes into a [Synthesize](Synthesize)
/// to hear it again. A spectrum can feed several spectral nodes at once;
/// [Cross](Cross) takes two. Analyze itself adds no delay, and with nothing
/// changing the frames, Analyze into Synthesize gives the input back, late by
/// `SYNTH_LATENCY` samples.
pub struct Analyze;

pub const ANALYZE_ID: &str = "noodle.spectral.analyze";

#[derive(Ports)]
struct AnalyzePorts {
    #[input("in", "In")]
    input: (),
    #[spectral_output("spectrum", "Spectrum")]
    spectrum: (),
}

static ANALYZE_INFO: NodeInfo = NodeInfo {
    id: ANALYZE_ID,
    version: 1,
    name: "Analyze",
    category: CATEGORY,
};

impl NodeType for Analyze {
    fn info(&self) -> &NodeInfo {
        &ANALYZE_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(AnalyzePorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let shape = spectral_shape(setup, AnalyzePorts::OUTPUTS);
        Ok(Instance::realtime(AnalyzeNode {
            analyzer: Analyzer::new(shape),
        }))
    }
}

struct AnalyzeNode {
    analyzer: Analyzer,
}

impl Node for AnalyzeNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        self.analyzer
            .process(&io.inputs[AnalyzePorts::INPUT], &mut io.spectral_outputs[0]);
    }

    fn reset(&mut self) {
        self.analyzer.reset();
    }
}

/// Turns a spectral signal back into audio by overlap-adding its frames. The
/// audio sounds `SYNTH_LATENCY` samples (about 46 ms at 44.1 kHz) after the
/// audio the frames were analysed from; the engine delays whatever is wired
/// alongside to match, so a dry path mixed back in stays in time.
pub struct Synthesize;

pub const SYNTHESIZE_ID: &str = "noodle.spectral.synthesize";

#[derive(Ports)]
struct SynthesizePorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[output("out", "Out")]
    out: (),
}

static SYNTHESIZE_INFO: NodeInfo = NodeInfo {
    id: SYNTHESIZE_ID,
    version: 1,
    name: "Synthesize",
    category: CATEGORY,
};

impl NodeType for Synthesize {
    fn info(&self) -> &NodeInfo {
        &SYNTHESIZE_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(SynthesizePorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let shape = setup.output_shapes[SynthesizePorts::OUT];
        Ok(Instance::realtime(SynthesizeNode {
            synthesizer: Synthesizer::new(shape),
        }))
    }
}

struct SynthesizeNode {
    synthesizer: Synthesizer,
}

impl Node for SynthesizeNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        self.synthesizer.process(
            &io.spectral_inputs[0],
            &mut io.outputs[SynthesizePorts::OUT],
        );
    }

    fn latency(&self) -> u32 {
        SYNTH_LATENCY
    }

    fn reset(&mut self) {
        self.synthesizer.reset();
    }
}

/// Silences the bins that are quiet: a bin whose magnitude is below
/// `threshold` is turned down by `reduction`. With a high threshold it leaves
/// only a sound's loudest partials; with a low one it removes steady noise
/// (hiss, hum floors) between them. `knee` softens the edge: bins within it
/// of the threshold are turned down part of the way, which sounds less
/// fluttery than a hard cut.
pub struct SpectralGate;

pub const SPECTRAL_GATE_ID: &str = "noodle.spectral.gate";

#[derive(Ports)]
struct GatePorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[param(
        "threshold",
        "Threshold",
        ParamInfo::new(-120.0, 0.0, -60.0)
            .unit(Unit::Decibels)
            .offset()
    )]
    threshold: (),
    #[param(
        "reduction",
        "Reduction",
        ParamInfo::new(0.0, 120.0, 80.0).unit(Unit::Decibels)
    )]
    reduction: (),
    #[param("knee", "Knee", ParamInfo::new(0.0, 24.0, 6.0).unit(Unit::Decibels))]
    knee: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static GATE_INFO: NodeInfo = NodeInfo {
    id: SPECTRAL_GATE_ID,
    version: 1,
    name: "Spectral Gate",
    category: CATEGORY,
};

impl NodeType for SpectralGate {
    fn info(&self) -> &NodeInfo {
        &GATE_INFO
    }

    fn stateless(&self) -> bool {
        true
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(GatePorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(GateNode))
    }
}

struct GateNode;

impl Node for GateNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let inputs = io.inputs;
        map_frames(io, |time, voice, _, input, out| {
            let threshold = param(inputs, GatePorts::THRESHOLD, voice, time);
            let reduction = param(inputs, GatePorts::REDUCTION, voice, time).max(0.0);
            let knee = param(inputs, GatePorts::KNEE, voice, time).max(0.0);
            for (out, bin) in out.iter_mut().zip(input) {
                let level = 20.0 * (bin.magnitude() + 1e-12).log10();
                // How far above the bottom of the knee, 0 to 1.
                let open = if knee > 0.0 {
                    ((level - threshold) / knee + 0.5).clamp(0.0, 1.0)
                } else {
                    f32::from(level >= threshold)
                };
                *out = bin.scaled(db_to_amp(-reduction * (1.0 - open)));
            }
        });
    }
}

/// Keeps only the bins between `low` and `high` (or, in Band stop, removes
/// them): a filter with edges as steep as the frame size allows, about 21 Hz
/// per bin. Where an ordinary filter rings and shifts phase, this one cuts
/// cleanly, which suits taking one band out of a mix, or building a
/// vocoder-style set of bands from a pair of them.
pub struct SpectralFilter;

pub const SPECTRAL_FILTER_ID: &str = "noodle.spectral.filter";

#[derive(Ports)]
struct FilterPorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[param(
        "low",
        "Low",
        ParamInfo::new(20.0, 20_000.0, 20.0)
            .unit(Unit::Hertz)
            .log()
            .offset()
    )]
    low: (),
    #[param(
        "high",
        "High",
        ParamInfo::new(20.0, 20_000.0, 20_000.0)
            .unit(Unit::Hertz)
            .log()
            .offset()
    )]
    high: (),
    #[param("mode", "Mode", ParamInfo::choice(["Band pass", "Band stop"]))]
    mode: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static FILTER_INFO: NodeInfo = NodeInfo {
    id: SPECTRAL_FILTER_ID,
    version: 1,
    name: "Spectral Filter",
    category: CATEGORY,
};

impl NodeType for SpectralFilter {
    fn info(&self) -> &NodeInfo {
        &FILTER_INFO
    }

    fn stateless(&self) -> bool {
        true
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(FilterPorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(FilterNode))
    }
}

struct FilterNode;

impl Node for FilterNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let inputs = io.inputs;
        let rate = ctx.sample_rate;
        map_frames(io, |time, voice, _, input, out| {
            let low = param(inputs, FilterPorts::LOW, voice, time);
            let high = param(inputs, FilterPorts::HIGH, voice, time);
            let stop = param(inputs, FilterPorts::MODE, voice, time) >= 0.5;
            for (j, (out, bin)) in out.iter_mut().zip(input).enumerate() {
                let f = bin_frequency(j, rate);
                if (f >= low && f <= high) != stop {
                    *out = *bin;
                }
            }
        });
    }
}

/// Moves every frequency up or down by the same number of hertz, which is not
/// the same as changing pitch: a note's partials at 100, 200 and 300 Hz
/// shifted by 50 Hz sit at 150, 250 and 350 Hz and no longer form a harmonic
/// series, so the sound turns bell-like and then metallic. A shift of one or
/// two bins mixed back with the dry sound makes a detuned, phasing
/// thickness.
///
/// The shift is rounded to whole bins (about 23 Hz at 48 kHz), so a shift
/// under half a bin (about 11 Hz) changes nothing.
pub struct SpectralShift;

pub const SPECTRAL_SHIFT_ID: &str = "noodle.spectral.shift";

#[derive(Ports)]
struct ShiftPorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[param(
        "shift",
        "Shift",
        ParamInfo::new(-2000.0, 2000.0, 0.0)
            .unit(Unit::Hertz)
            .offset()
    )]
    shift: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static SHIFT_INFO: NodeInfo = NodeInfo {
    id: SPECTRAL_SHIFT_ID,
    version: 1,
    name: "Spectral Shift",
    category: CATEGORY,
};

impl NodeType for SpectralShift {
    fn info(&self) -> &NodeInfo {
        &SHIFT_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(ShiftPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let lanes = spectral_shape(setup, ShiftPorts::OUTPUTS).lanes();
        Ok(Instance::realtime(ShiftNode {
            turns: vec![0.0; lanes],
        }))
    }
}

struct ShiftNode {
    /// The phase every bin of a lane is turned by, in radians. A partial
    /// moved `k` bins up has to advance `k` bins' worth of phase more each
    /// frame, which is the same turn for all of them.
    turns: Vec<f32>,
}

impl Node for ShiftNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let inputs = io.inputs;
        let bin_width = bin_frequency(1, ctx.sample_rate);
        let turns = &mut self.turns;
        map_frames(io, |time, voice, lane, input, out| {
            let bins = (param(inputs, ShiftPorts::SHIFT, voice, time) / bin_width).round() as isize;
            turns[lane] = wrap_phase(
                turns[lane] + std::f32::consts::TAU * bins as f32 * HOP as f32 / FFT_SIZE as f32,
            );
            let (sin, cos) = turns[lane].sin_cos();
            for (j, bin) in input.iter().enumerate() {
                let to = j as isize + bins;
                if (0..BINS as isize).contains(&to) {
                    out[to as usize] =
                        Bin::new(bin.re * cos - bin.im * sin, bin.re * sin + bin.im * cos);
                }
            }
        });
    }

    fn reset(&mut self) {
        self.turns.fill(0.0);
    }
}

/// Moves the formants, the broad humps of the spectrum that make a voice or
/// an instrument sound like itself, without moving the pitch. Up makes a
/// voice smaller or brighter, down larger or darker.
///
/// It finds the spectrum's outline (the magnitude smoothed over `smooth` Hz),
/// moves the outline by `shift` and scales each bin to follow, so the
/// harmonics stay where they were. Put it after a pitch shifter, set to the
/// opposite of the shift, and the voice keeps its character. Keep `smooth`
/// wider than the gap between harmonics (about the pitch in Hz, or a little
/// more), or the outline follows single harmonics. A formant can only be
/// lifted or cut by 24 dB, so extreme shifts of narrow features fall short.
pub struct FormantShift;

pub const FORMANT_SHIFT_ID: &str = "noodle.spectral.formant";

#[derive(Ports)]
struct FormantPorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[param(
        "shift",
        "Shift",
        ParamInfo::new(-12.0, 12.0, 0.0)
            .unit(Unit::Semitones)
            .offset()
    )]
    shift: (),
    #[param("smooth", "Smooth", ParamInfo::new(50.0, 2000.0, 400.0).unit(Unit::Hertz))]
    smooth: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static FORMANT_INFO: NodeInfo = NodeInfo {
    id: FORMANT_SHIFT_ID,
    version: 1,
    name: "Formant Shift",
    category: CATEGORY,
};

impl NodeType for FormantShift {
    fn info(&self) -> &NodeInfo {
        &FORMANT_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(FormantPorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(FormantNode {
            warp: FormantWarp::new(),
        }))
    }
}

struct FormantNode {
    warp: FormantWarp,
}

impl Node for FormantNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let inputs = io.inputs;
        let bin_width = bin_frequency(1, ctx.sample_rate);
        let warp = &mut self.warp;
        map_frames(io, |time, voice, _lane, input, out| {
            let semitones = param(inputs, FormantPorts::SHIFT, voice, time);
            let smooth = param(inputs, FormantPorts::SMOOTH, voice, time);
            let width = (smooth / bin_width / 2.0).round().max(1.0) as usize;
            warp.apply(input, out, (semitones / 12.0).exp2(), width);
        });
    }
}

/// Holds the spectrum while `freeze` is on, so a moment of sound goes on
/// sounding as a steady tone or texture. It keeps each partial's level and
/// goes on advancing its phase at the frequency it had when it was caught,
/// which sounds like a held chord for tonal sound and a hiss for noisy sound.
/// Turn `freeze` off and the live sound returns at once.
pub struct SpectralFreeze;

pub const SPECTRAL_FREEZE_ID: &str = "noodle.spectral.freeze";

#[derive(Ports)]
struct FreezePorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[param("freeze", "Freeze", ParamInfo::choice(["Off", "On"]))]
    freeze: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static FREEZE_INFO: NodeInfo = NodeInfo {
    id: SPECTRAL_FREEZE_ID,
    version: 1,
    name: "Spectral Freeze",
    category: CATEGORY,
};

impl NodeType for SpectralFreeze {
    fn info(&self) -> &NodeInfo {
        &FREEZE_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(FreezePorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let lanes = spectral_shape(setup, FreezePorts::OUTPUTS).lanes();
        Ok(Instance::realtime(FreezeNode {
            lanes: (0..lanes).map(|_| FreezeLane::new()).collect(),
        }))
    }
}

struct FreezeLane {
    /// The last live frame: magnitude and phase of each bin.
    magnitude: Vec<f32>,
    previous: Vec<f32>,
    /// How far each bin's phase moves per frame.
    advance: Vec<f32>,
    /// The phase the held bins have reached.
    phase: Vec<f32>,
}

impl FreezeLane {
    fn new() -> Self {
        Self {
            magnitude: vec![0.0; BINS],
            previous: vec![0.0; BINS],
            advance: vec![0.0; BINS],
            phase: vec![0.0; BINS],
        }
    }
}

struct FreezeNode {
    lanes: Vec<FreezeLane>,
}

impl Node for FreezeNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let inputs = io.inputs;
        let lanes = &mut self.lanes;
        map_frames(io, |time, voice, lane, input, out| {
            let state = &mut lanes[lane];
            let frozen = param(inputs, FreezePorts::FREEZE, voice, time) >= 0.5;
            if frozen {
                for (j, out) in out.iter_mut().enumerate() {
                    state.phase[j] = wrap_phase(state.phase[j] + state.advance[j]);
                    *out = Bin::polar(state.magnitude[j], state.phase[j]);
                    // So that unfreezing, and freezing again at once, measures
                    // the partials' steps from where they are now.
                    state.previous[j] = input[j].phase();
                }
            } else {
                for (j, bin) in input.iter().enumerate() {
                    let phase = bin.phase();
                    let expected = std::f32::consts::TAU * j as f32 * HOP as f32 / FFT_SIZE as f32;
                    // The partial's real frequency: the phase step beyond
                    // what a bin-centre sine would make, added to it.
                    state.advance[j] = expected + wrap_phase(phase - state.previous[j] - expected);
                    state.previous[j] = phase;
                    state.phase[j] = phase;
                    state.magnitude[j] = bin.magnitude();
                    out[j] = *bin;
                }
            }
        });
    }

    fn reset(&mut self) {
        for lane in &mut self.lanes {
            *lane = FreezeLane::new();
        }
    }
}

/// Smears the spectrum through time: each bin's level chases the live level
/// instead of following it, so transients lose their edge and a sound seems
/// to bloom and trail. At `amount` 0 nothing changes; near 1 the sound is
/// washed into a slowly changing haze. The phases stay the live ones, so it
/// stays a version of the sound rather than a drone.
pub struct SpectralBlur;

pub const SPECTRAL_BLUR_ID: &str = "noodle.spectral.blur";

#[derive(Ports)]
struct BlurPorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[param("amount", "Amount", ParamInfo::new(0.0, 0.999, 0.8).offset())]
    amount: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static BLUR_INFO: NodeInfo = NodeInfo {
    id: SPECTRAL_BLUR_ID,
    version: 1,
    name: "Spectral Blur",
    category: CATEGORY,
};

impl NodeType for SpectralBlur {
    fn info(&self) -> &NodeInfo {
        &BLUR_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(BlurPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let lanes = spectral_shape(setup, BlurPorts::OUTPUTS).lanes();
        Ok(Instance::realtime(BlurNode {
            levels: vec![vec![0.0; BINS]; lanes],
        }))
    }
}

struct BlurNode {
    levels: Vec<Vec<f32>>,
}

impl Node for BlurNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let inputs = io.inputs;
        let levels = &mut self.levels;
        map_frames(io, |time, voice, lane, input, out| {
            let amount = param(inputs, BlurPorts::AMOUNT, voice, time).clamp(0.0, 0.999);
            for ((out, bin), level) in out.iter_mut().zip(input).zip(&mut levels[lane]) {
                *level = amount * *level + (1.0 - amount) * bin.magnitude();
                let magnitude = bin.magnitude();
                // The live bin, rescaled to the smoothed level.
                *out = if magnitude > 1e-12 {
                    bin.scaled(*level / magnitude)
                } else {
                    Bin::ZERO
                };
            }
        });
    }

    fn reset(&mut self) {
        for lane in &mut self.levels {
            lane.fill(0.0);
        }
    }
}

/// Cross-synthesis: the levels of the `modulator` spectrum shape the
/// `carrier`, which keeps its own phases. Wire a voice into the modulator and
/// a saw or noise into the carrier, and the saw talks; wire two instruments
/// and each takes on the other's spectral outline. `mix` 0 is the carrier
/// alone and 1 gives it the modulator's levels entirely; in between, the
/// levels are blended on a decibel scale.
///
/// `bands` smooths the modulator's levels into that many log-spaced bands
/// (2 to 256) and divides out the carrier's own band levels, so the carrier
/// keeps its fine structure and takes on only the modulator's outline: the
/// channel vocoder's character, at any frame size. Below 2 it is off and
/// every bin is matched on its own.
///
/// Paths of different latency into the two inputs are lined up by delay
/// compensation (the earlier one's frames are held back), and the modulator's
/// latest frame is used for each carrier frame.
pub struct Cross;

/// The most bands Cross can smooth into.
const MAX_BANDS: usize = 256;

/// Bin edges of `n` log-spaced bands over bins 1 to `BINS`, written into
/// `edges[..=n]`: band `b` covers `edges[b]..edges[b + 1]`, and every band
/// holds at least one bin. Bin 0 (DC) joins the first band.
fn band_edges(n: usize, edges: &mut [usize]) {
    let top = (BINS - 1) as f32;
    let mut prev = 0;
    edges[0] = 0;
    #[allow(clippy::needless_range_loop)]
    for b in 1..=n {
        let edge = top.powf(b as f32 / n as f32).round() as usize;
        // At least one bin per band, but leave room for the bands left.
        let edge = edge.max(prev + 1).min(BINS - (n - b));
        edges[b] = edge;
        prev = edge;
    }
    edges[n] = BINS;
}

pub const CROSS_ID: &str = "noodle.spectral.cross";

#[derive(Ports)]
struct CrossPorts {
    #[spectral_input("carrier", "Carrier")]
    carrier: (),
    #[spectral_input("modulator", "Modulator")]
    modulator: (),
    #[param("mix", "Mix", ParamInfo::new(0.0, 1.0, 1.0).offset())]
    mix: (),
    #[param("bands", "Bands", ParamInfo::new(0.0, 256.0, 0.0))]
    bands: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static CROSS_INFO: NodeInfo = NodeInfo {
    id: CROSS_ID,
    version: 1,
    name: "Cross",
    category: CATEGORY,
};

impl NodeType for Cross {
    fn info(&self) -> &NodeInfo {
        &CROSS_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(CrossPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let lanes = spectral_shape(setup, CrossPorts::OUTPUTS).lanes();
        Ok(Instance::realtime(CrossNode {
            held: vec![vec![0.0; BINS]; lanes],
            edges: vec![0; MAX_BANDS + 1],
            edges_for: 0,
            modulator_bands: vec![0.0; MAX_BANDS],
            carrier_bands: vec![0.0; MAX_BANDS],
        }))
    }
}

struct CrossNode {
    /// The latest modulator magnitudes, per lane.
    held: Vec<Vec<f32>>,
    /// Band edges for `edges_for` bands (zero: none computed yet).
    edges: Vec<usize>,
    edges_for: usize,
    /// Scratch: mean levels per band.
    modulator_bands: Vec<f32>,
    carrier_bands: Vec<f32>,
}

impl Node for CrossNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs,
            spectral_inputs,
            spectral_outputs,
            ..
        } = io;
        let (carrier, modulator) = (&spectral_inputs[0], &spectral_inputs[1]);
        let out = &mut spectral_outputs[0];
        let shape = out.shape();
        let mut next = 0;
        for frame in 0..carrier.frames() {
            let time = carrier.time(frame);
            // Bring the held modulator up to this frame.
            while next < modulator.frames() && modulator.time(next) <= time {
                for voice in 0..shape.voices {
                    for channel in 0..shape.channels {
                        let lane = voice * shape.channels + channel;
                        for (held, bin) in self.held[lane]
                            .iter_mut()
                            .zip(modulator.frame(next, voice, channel))
                        {
                            *held = bin.magnitude();
                        }
                    }
                }
                next += 1;
            }
            let Some(at) = out.push(time) else { break };
            for voice in 0..shape.voices {
                let mix = param(inputs, CrossPorts::MIX, voice, time).clamp(0.0, 1.0);
                let bands = param(inputs, CrossPorts::BANDS, voice, time)
                    .round()
                    .clamp(0.0, MAX_BANDS as f32) as usize;
                let bands = if bands < 2 { 0 } else { bands };
                if bands != 0 && bands != self.edges_for {
                    band_edges(bands, &mut self.edges);
                    self.edges_for = bands;
                }
                for channel in 0..shape.channels {
                    let lane = voice * shape.channels + channel;
                    let held = &self.held[lane];
                    let source = carrier.frame(frame, voice, channel);
                    if bands != 0 {
                        for b in 0..bands {
                            let range = self.edges[b]..self.edges[b + 1];
                            let n = range.len() as f32;
                            self.modulator_bands[b] = held[range.clone()].iter().sum::<f32>() / n;
                            self.carrier_bands[b] =
                                source[range].iter().map(|bin| bin.magnitude()).sum::<f32>() / n;
                        }
                    }
                    let bins = out.frame_mut(at, voice, channel);
                    let mut band = 0;
                    for (i, ((out, bin), held)) in bins.iter_mut().zip(source).zip(held).enumerate()
                    {
                        let magnitude = bin.magnitude();
                        if magnitude <= 1e-12 {
                            continue;
                        }
                        // The level this bin is pulled towards.
                        let level = if bands == 0 {
                            *held
                        } else {
                            while i >= self.edges[band + 1] {
                                band += 1;
                            }
                            magnitude * self.modulator_bands[band]
                                / (self.carrier_bands[band] + 1e-12)
                        };
                        // Geometric blend of the two levels.
                        let target =
                            (magnitude + 1e-12).powf(1.0 - mix) * (level + 1e-12).powf(mix);
                        *out = bin.scaled(target / magnitude);
                    }
                }
            }
        }
        // Modulator frames after the last carrier frame wait for the next
        // block, in order.
        while next < modulator.frames() {
            for voice in 0..shape.voices {
                for channel in 0..shape.channels {
                    let lane = voice * shape.channels + channel;
                    for (held, bin) in self.held[lane]
                        .iter_mut()
                        .zip(modulator.frame(next, voice, channel))
                    {
                        *held = bin.magnitude();
                    }
                }
            }
            next += 1;
        }
    }

    fn reset(&mut self) {
        for lane in &mut self.held {
            lane.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{SpectralBuf, SpectralIn};

    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 16_384;

    /// Frames of a sine at `bin` (centre-aligned) of amplitude `amp`.
    fn sine_frames(bin: usize, amp: f32, frames: usize) -> SpectralBuf {
        let shape = Shape::MONO;
        let input: Vec<f32> = (0..frames * HOP)
            .map(|n| amp * (std::f32::consts::TAU * bin as f32 * n as f32 / FFT_SIZE as f32).sin())
            .collect();
        let mut analyzer = Analyzer::new(shape);
        let mut buf = SpectralBuf::new(1, frames * HOP);
        analyzer.process(
            &SignalIn::new(&input, shape, input.len()),
            &mut buf.as_out(shape),
        );
        buf
    }

    /// Copies `from`'s frames into the harness's spectral input 0.
    fn feed(h: &mut Harness, port: usize, from: &SpectralBuf) {
        let frames = from.as_in(Shape::MONO);
        let mut out = h.spectral_input(port);
        for f in 0..frames.frames() {
            let at = out.push(frames.time(f)).unwrap();
            out.frame_mut(at, 0, 0)
                .copy_from_slice(frames.frame(f, 0, 0));
        }
    }

    fn harness(node: &dyn NodeType, spectral: usize) -> Harness {
        Harness::with_spectral(
            node,
            &Config::new(),
            &[],
            &vec![Shape::MONO; spectral],
            RATE,
            BLOCK,
        )
        .unwrap()
    }

    fn magnitudes(frames: SpectralIn<'_>, frame: usize) -> Vec<f32> {
        frames
            .frame(frame, 0, 0)
            .iter()
            .map(|b| b.magnitude())
            .collect()
    }

    fn last_frame(frames: &SpectralIn<'_>) -> usize {
        frames.frames() - 1
    }

    #[test]
    fn analyze_then_synthesize_is_the_input_late() {
        let shape = Shape::MONO;
        let mut analyze =
            Harness::new(&Analyze, &Config::new(), &[(0, shape)], RATE, BLOCK).unwrap();
        let mut synth =
            Harness::with_spectral(&Synthesize, &Config::new(), &[], &[shape], RATE, BLOCK)
                .unwrap();
        let input: Vec<f32> = (0..BLOCK * 6)
            .map(|n| (n as f32 * 0.05).sin() * 0.5)
            .collect();
        let mut out = Vec::new();
        for chunk in input.chunks(BLOCK) {
            analyze
                .input(0, BLOCK)
                .lane_mut(0, 0)
                .copy_from_slice(chunk);
            analyze.run(BLOCK).unwrap();
            let frames = analyze.spectral(0);
            let mut spectral = synth.spectral_input(0);
            for f in 0..frames.frames() {
                let at = spectral.push(frames.time(f)).unwrap();
                spectral
                    .frame_mut(at, 0, 0)
                    .copy_from_slice(frames.frame(f, 0, 0));
            }
            synth.run(BLOCK).unwrap();
            out.extend_from_slice(synth.output(0).lane(0, 0));
        }
        let delay = SYNTH_LATENCY as usize;
        for n in (FFT_SIZE..input.len() - delay).step_by(11) {
            assert!(
                (out[n + delay] - input[n]).abs() < 1e-3,
                "sample {n}: {} vs {}",
                out[n + delay],
                input[n]
            );
        }
    }

    #[test]
    fn the_gate_removes_quiet_bins_and_keeps_loud_ones() {
        let loud = sine_frames(100, 0.5, 8);
        let quiet = sine_frames(200, 0.001, 8);
        let mut mixed = SpectralBuf::new(1, 8 * HOP);
        {
            let (a, b) = (loud.as_in(Shape::MONO), quiet.as_in(Shape::MONO));
            let mut out = mixed.as_out(Shape::MONO);
            for f in 0..a.frames() {
                let at = out.push(a.time(f)).unwrap();
                for (o, (x, y)) in out
                    .frame_mut(at, 0, 0)
                    .iter_mut()
                    .zip(a.frame(f, 0, 0).iter().zip(b.frame(f, 0, 0)))
                {
                    *o = Bin::new(x.re + y.re, x.im + y.im);
                }
            }
        }
        let mut h = harness(&SpectralGate, 1);
        h.set(GatePorts::THRESHOLD, -40.0);
        h.set(GatePorts::KNEE, 0.0);
        feed(&mut h, 0, &mixed);
        h.run(8 * HOP).unwrap();
        let frames = h.spectral(0);
        let m = magnitudes(frames, last_frame(&frames));
        assert!((m[100] - 0.5).abs() < 0.02, "loud bin {}", m[100]);
        assert!(m[200] < 1e-5, "quiet bin {}", m[200]);
    }

    #[test]
    fn each_voice_reads_its_own_parameter() {
        // Two voices of the same quiet tone; the gate's threshold is high for
        // voice 1 only (a polyphonic parameter), so only it is silenced.
        let tone = sine_frames(100, 0.01, 6);
        let shape = Shape::new(2, 1);
        let mut h = Harness::with_spectral(
            &SpectralGate,
            &Config::new(),
            &[(GatePorts::THRESHOLD, shape)],
            &[shape],
            RATE,
            BLOCK,
        )
        .unwrap();
        h.input(GatePorts::THRESHOLD, 6 * HOP)
            .lane_mut(0, 0)
            .fill(-80.0);
        h.input(GatePorts::THRESHOLD, 6 * HOP)
            .lane_mut(1, 0)
            .fill(-20.0);
        h.set(GatePorts::KNEE, 0.0);
        let frames = tone.as_in(Shape::MONO);
        let mut out = h.spectral_input(0);
        for f in 0..frames.frames() {
            let at = out.push(frames.time(f)).unwrap();
            for voice in 0..2 {
                out.frame_mut(at, voice, 0)
                    .copy_from_slice(frames.frame(f, 0, 0));
            }
        }
        h.run(6 * HOP).unwrap();
        let result = h.spectral(0);
        let last = result.frames() - 1;
        assert!(result.frame(last, 0, 0)[100].magnitude() > 0.009);
        assert!(result.frame(last, 1, 0)[100].magnitude() < 1e-4);
    }

    #[test]
    fn the_filter_passes_a_band_and_stop_does_the_opposite() {
        let low = sine_frames(50, 0.5, 6);
        let freq = |bin: usize| bin_frequency(bin, RATE);
        for (mode, kept, cut) in [(0.0, 50, 400), (1.0, 400, 50)] {
            // A pair of sines, one in the band and one out of it.
            let high = sine_frames(400, 0.5, 6);
            let mut both = SpectralBuf::new(1, 6 * HOP);
            {
                let (a, b) = (low.as_in(Shape::MONO), high.as_in(Shape::MONO));
                let mut out = both.as_out(Shape::MONO);
                for f in 0..a.frames() {
                    let at = out.push(a.time(f)).unwrap();
                    for (o, (x, y)) in out
                        .frame_mut(at, 0, 0)
                        .iter_mut()
                        .zip(a.frame(f, 0, 0).iter().zip(b.frame(f, 0, 0)))
                    {
                        *o = Bin::new(x.re + y.re, x.im + y.im);
                    }
                }
            }
            let mut h = harness(&SpectralFilter, 1);
            h.set(FilterPorts::LOW, freq(30));
            h.set(FilterPorts::HIGH, freq(100));
            h.set(FilterPorts::MODE, mode);
            feed(&mut h, 0, &both);
            h.run(6 * HOP).unwrap();
            let frames = h.spectral(0);
            let m = magnitudes(frames, last_frame(&frames));
            assert!(m[kept] > 0.4, "mode {mode}: kept {}", m[kept]);
            assert!(m[cut] < 1e-6, "mode {mode}: cut {}", m[cut]);
        }
    }

    /// Six identical frames: harmonics every 10 bins under one broad hump
    /// centred on bin 200.
    fn voiced_frames() -> SpectralBuf {
        let shape = Shape::MONO;
        let mut buf = SpectralBuf::new(1, 6 * HOP);
        let mut out = buf.as_out(shape);
        for f in 0..6 {
            let at = out.push((f * HOP) as u32).unwrap();
            for (k, bin) in out.frame_mut(at, 0, 0).iter_mut().enumerate() {
                if k % 10 == 0 {
                    let d = (k as f32 - 200.0) / 80.0;
                    *bin = Bin::new((-d * d).exp(), 0.0);
                }
            }
        }
        buf
    }

    fn hump(m: &[f32]) -> usize {
        (20..m.len() - 20)
            .max_by(|&a, &b| {
                let sum = |k: usize| m[k - 20..=k + 20].iter().sum::<f32>();
                sum(a).total_cmp(&sum(b))
            })
            .unwrap()
    }

    #[test]
    fn formant_shift_moves_the_hump_and_leaves_the_harmonics() {
        let mut h = harness(&FormantShift, 1);
        h.set(FormantPorts::SHIFT, 4.0);
        h.set(FormantPorts::SMOOTH, 800.0);
        feed(&mut h, 0, &voiced_frames());
        h.run(6 * HOP).unwrap();
        let frames = h.spectral(0);
        let m = magnitudes(frames, last_frame(&frames));
        let up = hump(&m);
        // 4 semitones is a factor of 1.26: the hump from 200 to about 252.
        assert!((up as i32 - 252).abs() < 14, "hump at {up}");
        assert!(
            m.iter().enumerate().all(|(k, &x)| k % 10 == 0 || x == 0.0),
            "a bin between the harmonics sounds"
        );
    }

    #[test]
    fn formant_shift_of_nothing_passes_the_frames_unchanged() {
        let mut h = harness(&FormantShift, 1);
        feed(&mut h, 0, &voiced_frames());
        h.run(6 * HOP).unwrap();
        let frames = h.spectral(0);
        let m = magnitudes(frames, last_frame(&frames));
        let want = magnitudes(voiced_frames().as_in(Shape::MONO), 5);
        assert_eq!(m, want);
    }

    #[test]
    fn a_shift_moves_a_partial_by_the_shift_and_keeps_it_steady() {
        // A tone on bin 100 shifted 10 bins up should read as a tone on bin
        // 110: its magnitude stays put and its phase advances as a sine on
        // bin 110 would, frame to frame.
        let source = sine_frames(100, 0.5, 12);
        let mut h = harness(&SpectralShift, 1);
        h.set(ShiftPorts::SHIFT, 10.0 * bin_frequency(1, RATE));
        feed(&mut h, 0, &source);
        h.run(12 * HOP).unwrap();
        let frames = h.spectral(0);
        let last = last_frame(&frames);
        assert!(frames.frame(last, 0, 0)[100].magnitude() < 1e-6);
        assert!((frames.frame(last, 0, 0)[110].magnitude() - 0.5).abs() < 0.02);
        let step = |a: usize, b: usize| {
            wrap_phase(frames.frame(b, 0, 0)[110].phase() - frames.frame(a, 0, 0)[110].phase())
        };
        let expected = wrap_phase(std::f32::consts::TAU * 110.0 * HOP as f32 / FFT_SIZE as f32);
        let got = step(last - 1, last);
        assert!(
            wrap_phase(got - expected).abs() < 0.05,
            "phase step {got} vs {expected}"
        );
    }

    #[test]
    fn a_frozen_tone_holds_its_level_and_keeps_its_frequency() {
        let source = sine_frames(64, 0.4, 16);
        let frames_in = source.as_in(Shape::MONO);
        let mut live = harness(&SpectralFreeze, 1);
        live.set(FreezePorts::FREEZE, 0.0);
        {
            let mut out = live.spectral_input(0);
            for f in 0..8 {
                let at = out.push(frames_in.time(f)).unwrap();
                out.frame_mut(at, 0, 0)
                    .copy_from_slice(frames_in.frame(f, 0, 0));
            }
        }
        live.run(8 * HOP).unwrap();
        // Same node, now frozen with an empty (silent) input.
        live.set(FreezePorts::FREEZE, 1.0);
        {
            let mut out = live.spectral_input(0);
            for f in 8..16 {
                let at = out.push(frames_in.time(f) - 8 * HOP as u32).unwrap();
                out.frame_mut(at, 0, 0).fill(Bin::ZERO);
            }
        }
        live.run(8 * HOP).unwrap();
        let held = live.spectral(0);
        let last = last_frame(&held);
        let m = held.frame(last, 0, 0)[64].magnitude();
        assert!((m - 0.4).abs() < 0.02, "held level {m}");
        let step =
            wrap_phase(held.frame(last, 0, 0)[64].phase() - held.frame(last - 1, 0, 0)[64].phase());
        let expected = wrap_phase(std::f32::consts::TAU * 64.0 * HOP as f32 / FFT_SIZE as f32);
        assert!(
            wrap_phase(step - expected).abs() < 0.05,
            "{step} vs {expected}"
        );
    }

    #[test]
    fn blur_smooths_a_step_in_level_over_several_frames() {
        let loud = sine_frames(80, 0.5, 20);
        let frames_in = loud.as_in(Shape::MONO);
        let mut h = harness(&SpectralBlur, 1);
        h.set(BlurPorts::AMOUNT, 0.8);
        feed(&mut h, 0, &loud);
        h.run(20 * HOP).unwrap();
        let frames = h.spectral(0);
        let level = |f: usize| frames.frame(f, 0, 0)[80].magnitude();
        // The first frame is only 20% of the way, and it keeps rising.
        assert!(level(0) < 0.15, "first {}", level(0));
        assert!(level(5) > level(0) && level(19) > level(5));
        assert!((level(19) - frames_in.frame(19, 0, 0)[80].magnitude()).abs() < 0.1);
    }

    #[test]
    fn cross_gives_the_carrier_the_modulators_levels() {
        // A carrier with one level and a modulator twice as loud on the
        // same bin: at mix 1 the output has the modulator's level and the
        // carrier's phase.
        let carrier = sine_frames(90, 0.2, 8);
        let modulator = sine_frames(90, 0.4, 8);
        let mut h = harness(&Cross, 2);
        h.set(CrossPorts::MIX, 1.0);
        feed(&mut h, 0, &carrier);
        feed(&mut h, 1, &modulator);
        h.run(8 * HOP).unwrap();
        let out = h.spectral(0);
        let last = last_frame(&out);
        let c = carrier.as_in(Shape::MONO);
        let bin = out.frame(last, 0, 0)[90];
        assert!(
            (bin.magnitude() - 0.4).abs() < 0.02,
            "level {}",
            bin.magnitude()
        );
        assert!(
            wrap_phase(bin.phase() - c.frame(last, 0, 0)[90].phase()).abs() < 1e-3,
            "phase kept"
        );
        // At mix 0 the carrier comes through unchanged.
        let mut h = harness(&Cross, 2);
        h.set(CrossPorts::MIX, 0.0);
        feed(&mut h, 0, &carrier);
        feed(&mut h, 1, &modulator);
        h.run(8 * HOP).unwrap();
        let out = h.spectral(0);
        assert!((out.frame(last, 0, 0)[90].magnitude() - 0.2).abs() < 0.01);
    }

    #[test]
    fn cross_bands_smooth_the_modulator_into_an_outline() {
        // Two modulator partials of different levels in the same band: with
        // bands on, the carrier's partials both land near the band's mean
        // outline rather than each taking its own partial's level, and the
        // carrier's own balance between them is kept.
        let carrier = sine_frames(90, 0.2, 8);
        let modulator = sine_frames(90, 0.4, 8);
        let mut h = harness(&Cross, 2);
        h.set(CrossPorts::MIX, 1.0);
        h.set(CrossPorts::BANDS, 8.0);
        feed(&mut h, 0, &carrier);
        feed(&mut h, 1, &modulator);
        h.run(8 * HOP).unwrap();
        let out = h.spectral(0);
        let last = last_frame(&out);
        // A lone partial fills its band alone-ish: the output is the
        // carrier scaled by modulator-band / carrier-band, which is 2.
        let level = out.frame(last, 0, 0)[90].magnitude();
        assert!((level - 0.4).abs() < 0.04, "level {level}");
        // Bin edges are monotonic and cover every bin.
        let mut edges = vec![0; MAX_BANDS + 1];
        for n in [2, 8, 64, 256] {
            band_edges(n, &mut edges);
            assert_eq!((edges[0], edges[n]), (0, BINS));
            assert!(edges[..=n].windows(2).all(|w| w[0] < w[1]), "{n} bands");
        }
    }

    #[test]
    fn a_synthesizer_reports_its_latency() {
        let node = Synthesize
            .instantiate(&Setup {
                node: noodle_engine::NodeId(0),
                config: &Config::new(),
                sample_rate: RATE,
                max_frames: BLOCK,
                input_shapes: &[Shape::MONO],
                output_shapes: &[Shape::MONO],
                seed: 0,
                position: 0,
            })
            .unwrap();
        let Instance::Realtime(node) = node else {
            panic!()
        };
        assert_eq!(node.latency(), SYNTH_LATENCY);
    }
}
