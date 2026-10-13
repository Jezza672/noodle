//! The drawn envelope generator.

use std::fmt::Write;

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Lane, LaneKernel, Layout, Node, NodeError, NodeInfo,
    NodeType, ParamInfo, PerLane, Ports, Setup, Shape, Skip, Unit,
};

use crate::curve::{LUT_STEPS, lookup};

/// An envelope you draw: a curve that plays out, over `length` seconds, each
/// time a trigger arrives. It outputs `depth × curve`, where the curve runs
/// from 0 to 1 in height. Before the first trigger, and after the curve has
/// played, the output rests at the curve's last height (0 for the default
/// shape and most drawn ones).
///
/// A trigger is a rising edge through 0.5 on `trigger`: a gate, a key's
/// gate, or a clock. A new trigger while the envelope is still playing
/// restarts it from the beginning. With `loop` on, the curve repeats for as
/// long as the trigger is held high, and a release lets the cycle in hand
/// finish.
///
/// The curve is config (it is drawn in the properties panel and saved as
/// text; see [`EnvelopeCurve::to_text`]). So is `single`, because it decides
/// the output's shape:
///
/// - **Multi-voice** (`single` 0, the default): every voice of the trigger has
///   its own envelope, so a polyphonic gate gives a polyphonic envelope.
/// - **Single voice** (`single` 1): one envelope serves the whole patch. A
///   trigger on any voice (re)starts it, and the output is mono. This is the
///   shape for a shared modulator, such as one filter sweep for a chord.
///
/// `active` is 1 while the curve is playing, for a
/// [`Voices`](crate::Voices) node's `busy` input.
///
/// Editing the curve rebuilds the node, which restarts an envelope that is
/// playing.
pub struct Envelope;

pub const ENVELOPE_ID: &str = "noodle.mod.envelope";
/// The config key that holds the curve; see [`EnvelopeCurve::to_text`].
pub const ENVELOPE_CURVE_KEY: &str = "curve";
/// The config key for single-voice mode: 0 for one envelope per voice, 1 for
/// one shared envelope.
pub const ENVELOPE_SINGLE_KEY: &str = "single";

const CURVE: ConfigInfo = ConfigInfo::text(ENVELOPE_CURVE_KEY, "Curve");
const SINGLE: ConfigInfo = ConfigInfo::int(ENVELOPE_SINGLE_KEY, "Single voice", 0);
static SETTINGS: [ConfigInfo; 2] = [CURVE, SINGLE];

#[derive(Ports)]
struct EnvelopePorts {
    #[input("trigger", "Trigger")]
    trigger: (),
    #[param(
        "length",
        "Length",
        ParamInfo::new(0.005, 60.0, 1.0).log().unit(Unit::Seconds)
    )]
    length: (),
    #[param("depth", "Depth", ParamInfo::new(0.0, 1.0, 1.0).offset())]
    depth: (),
    #[param("loop", "Loop", ParamInfo::choice(["Off", "On"]))]
    looping: (),
    #[output("out", "Out")]
    out: (),
    #[output("active", "Active")]
    active: (),
}

const TRIGGER: usize = EnvelopePorts::TRIGGER;
const LENGTH: usize = EnvelopePorts::LENGTH;
const DEPTH: usize = EnvelopePorts::DEPTH;
const LOOP: usize = EnvelopePorts::LOOPING;
const OUT: usize = EnvelopePorts::OUT;
const ACTIVE: usize = EnvelopePorts::ACTIVE;

static INFO: NodeInfo = NodeInfo {
    id: ENVELOPE_ID,
    version: 1,
    name: "Envelope",
    category: "Modulation",
};

/// Whether the config asks for the shared, single-voice envelope.
pub fn is_single(config: &Config) -> bool {
    SINGLE.get_int(config) != 0
}

impl NodeType for Envelope {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &SETTINGS
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(EnvelopePorts::layout())
    }

    fn output_shapes(
        &self,
        config: &Config,
        layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let shape = if is_single(config) {
            Shape::MONO
        } else {
            Shape::broadcast_all(inputs.iter().copied())?
        };
        Ok(vec![shape; layout.outputs.len()])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let curve = EnvelopeCurve::from_text(&CURVE.get_text(setup.config));
        let lut = curve.lut();
        let rest = lut[lut.len() - 1];
        if is_single(setup.config) {
            let voices = setup.input_shapes[TRIGGER].voices;
            Ok(Instance::realtime(SingleEnvelope {
                lut,
                rest,
                runner: Runner::default(),
                gates: vec![false; voices],
            }))
        } else {
            Ok(Instance::realtime(PerLane::new(
                EnvelopeKernel { lut, rest },
                setup,
            )))
        }
    }
}

/// Where one envelope is in its curve.
#[derive(Clone, Copy, Default)]
struct Runner {
    /// How far along the curve, 0 to 1.
    phase: f32,
    running: bool,
}

impl Runner {
    /// Advances one sample, returning the curve's height. `inc` is the
    /// fraction of the curve one sample covers.
    fn tick(
        &mut self,
        lut: &[f32],
        rest: f32,
        start: bool,
        held: bool,
        inc: f32,
        looping: bool,
    ) -> f32 {
        if start {
            self.phase = 0.0;
            self.running = true;
        }
        if !self.running {
            return rest;
        }
        let height = lookup(lut, self.phase);
        self.phase += inc;
        if self.phase >= 1.0 || !self.phase.is_finite() {
            if looping && held && self.phase.is_finite() {
                self.phase %= 1.0;
            } else {
                self.phase = 0.0;
                self.running = false;
            }
        }
        height
    }
}

/// The curve fraction one sample covers for a `length` in seconds.
#[inline]
fn increment(length: f32, sample_rate: f32) -> f32 {
    let length = if length.is_finite() {
        length.max(0.001)
    } else {
        1.0
    };
    1.0 / (length * sample_rate)
}

struct EnvelopeKernel {
    lut: Box<[f32]>,
    rest: f32,
}

#[derive(Default)]
struct EnvelopeState {
    runner: Runner,
    gate: bool,
}

impl LaneKernel for EnvelopeKernel {
    type State = EnvelopeState;

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[TRIGGER])
    }

    /// Resting at 0 with nothing playing and the gate low (a skipped lane
    /// never sees the gate fall, so a stale high gate would swallow the next
    /// note's rising edge); a curve that rests above 0 is
    /// never skipped, since its output is not silence.
    fn is_idle(&self, state: &EnvelopeState) -> bool {
        !state.runner.running && !state.gate && self.rest == 0.0
    }

    fn process_lane(&mut self, state: &mut EnvelopeState, ctx: &Context, mut lane: Lane<'_, '_>) {
        let trigger = lane.inputs.get(TRIGGER);
        let (length, depth) = (lane.inputs.get(LENGTH), lane.inputs.get(DEPTH));
        let looping = lane.inputs.get(LOOP);
        let [out, active] = lane.outputs.get_disjoint_mut([OUT, ACTIVE]);
        for i in 0..out.len() {
            let held = trigger[i] >= 0.5;
            let start = held && !state.gate;
            state.gate = held;
            let inc = increment(length[i], ctx.sample_rate);
            let height =
                state
                    .runner
                    .tick(&self.lut, self.rest, start, held, inc, looping[i] >= 0.5);
            out[i] = height * depth[i];
            active[i] = f32::from(state.runner.running);
        }
    }
}

/// One envelope for every voice.
struct SingleEnvelope {
    lut: Box<[f32]>,
    rest: f32,
    runner: Runner,
    /// Each voice's gate as of the last sample.
    gates: Vec<bool>,
}

impl Node for SingleEnvelope {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let trigger = &io.inputs[TRIGGER];
        let (length, depth) = (io.inputs[LENGTH].lane(0, 0), io.inputs[DEPTH].lane(0, 0));
        let looping = io.inputs[LOOP].lane(0, 0);
        let [out, active] = io
            .outputs
            .get_disjoint_mut([OUT, ACTIVE])
            .expect("the outputs are distinct");
        let (out, active) = (out.lane_mut(0, 0), active.lane_mut(0, 0));
        for i in 0..out.len() {
            let mut start = false;
            let mut held = false;
            for (voice, gate) in self.gates.iter_mut().enumerate() {
                let high = trigger.lane(voice, 0)[i] >= 0.5;
                start |= high && !*gate;
                held |= high;
                *gate = high;
            }
            let inc = increment(length[i], ctx.sample_rate);
            let height =
                self.runner
                    .tick(&self.lut, self.rest, start, held, inc, looping[i] >= 0.5);
            out[i] = height * depth[i];
            active[i] = f32::from(self.runner.running);
        }
    }

    fn reset(&mut self) {
        self.runner = Runner::default();
        self.gates.fill(false);
    }
}

/// One anchor of an [`EnvelopeCurve`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvelopePoint {
    /// Along the curve, 0 to 1. The first point is at 0 and the last at 1.
    pub x: f32,
    /// Height, 0 to 1.
    pub y: f32,
    /// How the segment from this point to the next bends, from -1 to 1. 0 is
    /// a straight line, negative moves fast first and settles slowly (the
    /// shape of an exponential decay), positive waits and then moves fast.
    /// Unused on the last point.
    pub bend: f32,
}

/// The shape of an [`Envelope`]: anchors joined by segments that can each
/// bend. Unlike a [`Curve`](crate::Curve), its ends can be at any height, so
/// an envelope can start and finish where it likes.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvelopeCurve {
    points: Vec<EnvelopePoint>,
}

/// The closest two anchors may come in x.
const MIN_GAP: f32 = 1e-3;

impl Default for EnvelopeCurve {
    /// A quick rise and an exponential-looking fall back to 0.
    fn default() -> Self {
        Self {
            points: vec![
                EnvelopePoint {
                    x: 0.0,
                    y: 0.0,
                    bend: -0.5,
                },
                EnvelopePoint {
                    x: 0.08,
                    y: 1.0,
                    bend: -0.7,
                },
                EnvelopePoint {
                    x: 1.0,
                    y: 0.0,
                    bend: 0.0,
                },
            ],
        }
    }
}

impl EnvelopeCurve {
    pub fn points(&self) -> &[EnvelopePoint] {
        &self.points
    }

    /// Reads the text form. Anything unreadable gives the default shape, so
    /// a damaged project still loads.
    pub fn from_text(text: &str) -> Self {
        let parsed: Option<Vec<EnvelopePoint>> = text
            .split(';')
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                let v: Vec<f32> = p
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .ok()?;
                (v.len() == 3 && v.iter().all(|f| f.is_finite())).then(|| EnvelopePoint {
                    x: v[0],
                    y: v[1],
                    bend: v[2],
                })
            })
            .collect();
        match parsed {
            Some(points) if points.len() >= 2 => {
                let mut curve = Self { points };
                curve.sanitize();
                curve
            }
            _ => Self::default(),
        }
    }

    /// One point per `;`, each "x y bend". The default shape is the empty
    /// string, the config default.
    pub fn to_text(&self) -> String {
        if *self == Self::default() {
            return String::new();
        }
        let mut text = String::new();
        for p in &self.points {
            if !text.is_empty() {
                text.push(';');
            }
            let _ = write!(text, "{} {} {}", p.x, p.y, p.bend);
        }
        text
    }

    /// Puts everything back in range: ends at 0 and 1 in x, anchors in
    /// order, heights and bends inside their limits.
    fn sanitize(&mut self) {
        let n = self.points.len();
        self.points[0].x = 0.0;
        self.points[n - 1].x = 1.0;
        self.points[n - 1].bend = 0.0;
        for i in 1..n - 1 {
            let lo = self.points[i - 1].x + MIN_GAP;
            let hi = 1.0 - MIN_GAP * (n - 1 - i) as f32;
            self.points[i].x = self.points[i].x.clamp(lo, hi.max(lo));
        }
        for p in &mut self.points {
            p.y = p.y.clamp(0.0, 1.0);
            p.bend = p.bend.clamp(-1.0, 1.0);
        }
    }

    /// Moves anchor `index` to (x, y). The end points only move in height,
    /// and the others stay between their neighbours.
    pub fn move_point(&mut self, index: usize, x: f32, y: f32) {
        let n = self.points.len();
        if index >= n {
            return;
        }
        let x = if index == 0 {
            0.0
        } else if index + 1 == n {
            1.0
        } else {
            x.clamp(
                self.points[index - 1].x + MIN_GAP,
                self.points[index + 1].x - MIN_GAP,
            )
        };
        self.points[index].x = x;
        self.points[index].y = y.clamp(0.0, 1.0);
    }

    /// Sets how the segment after anchor `index` bends.
    pub fn set_bend(&mut self, index: usize, bend: f32) {
        if index + 1 < self.points.len() {
            self.points[index].bend = bend.clamp(-1.0, 1.0);
        }
    }

    /// The bend that makes the segment after anchor `index` pass through
    /// height `y` at its middle, for dragging a segment. 0 if the segment is
    /// flat or `y` is outside the heights it joins.
    pub fn bend_for_middle(&self, index: usize, y: f32) -> f32 {
        let (Some(a), Some(b)) = (self.points.get(index), self.points.get(index + 1)) else {
            return 0.0;
        };
        let span = b.y - a.y;
        if span.abs() < 1e-4 {
            return 0.0;
        }
        // Halfway along in x, the shaped progress is 0.5^p.
        let progress = ((y - a.y) / span).clamp(0.02, 0.98);
        let exponent = progress.ln() / 0.5f32.ln();
        (exponent.ln() / 4f32.ln()).clamp(-1.0, 1.0)
    }

    /// Adds an anchor on the curve at `x` and returns its index. If `x` is
    /// too close to an anchor to fit one, that anchor's index.
    pub fn insert_at(&mut self, x: f32) -> usize {
        let x = x.clamp(0.0, 1.0);
        let after = self.points.partition_point(|p| p.x <= x);
        let (before, next) = (after - 1, after.min(self.points.len() - 1));
        if x - self.points[before].x < MIN_GAP {
            return before;
        }
        if self.points[next].x - x < MIN_GAP {
            return next;
        }
        let y = self.eval(x);
        let bend = self.points[before].bend;
        self.points.insert(after, EnvelopePoint { x, y, bend });
        after
    }

    /// Removes an interior anchor. Returns false for an end, which stays.
    pub fn remove(&mut self, index: usize) -> bool {
        if index == 0 || index + 1 >= self.points.len() {
            return false;
        }
        self.points.remove(index);
        true
    }

    /// The curve's height at `x` (0 to 1).
    pub fn eval(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let i = self
            .points
            .windows(2)
            .position(|w| x <= w[1].x)
            .unwrap_or(self.points.len() - 2);
        let (a, b) = (self.points[i], self.points[i + 1]);
        let t = ((x - a.x) / (b.x - a.x)).clamp(0.0, 1.0);
        let progress = t.powf(4f32.powf(a.bend));
        (a.y + (b.y - a.y) * progress).clamp(0.0, 1.0)
    }

    /// The curve sampled at `LUT_STEPS + 1` evenly spaced x values from 0
    /// to 1, for [`lookup`]. Allocates, so build it off the audio thread.
    pub fn lut(&self) -> Box<[f32]> {
        (0..=LUT_STEPS)
            .map(|i| self.eval(i as f32 / LUT_STEPS as f32))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;

    fn run(h: &mut Harness, gate: &[f32]) -> (Vec<f32>, Vec<f32>) {
        h.input(TRIGGER, gate.len())
            .lane_mut(0, 0)
            .copy_from_slice(gate);
        h.run(gate.len()).unwrap();
        (
            h.output(OUT).lane(0, 0).to_vec(),
            h.output(ACTIVE).lane(0, 0).to_vec(),
        )
    }

    fn config(curve: &EnvelopeCurve, single: bool) -> Config {
        Config::new()
            .with(ENVELOPE_CURVE_KEY, Value::Text(curve.to_text()))
            .with(ENVELOPE_SINGLE_KEY, Value::Int(i64::from(single)))
    }

    /// A ramp from 0 up to 1 and a fall back to 0, straight lines.
    fn triangle() -> EnvelopeCurve {
        EnvelopeCurve::from_text("0 0 0;0.5 1 0;1 0 0")
    }

    fn gate(frames: usize, high: std::ops::Range<usize>) -> Vec<f32> {
        (0..frames).map(|i| f32::from(high.contains(&i))).collect()
    }

    #[test]
    fn plays_the_curve_over_its_length_when_triggered() {
        let frames = 6_000;
        let mut h = Harness::new(
            &Envelope,
            &config(&triangle(), false),
            &[(TRIGGER, Shape::MONO)],
            RATE,
            frames,
        )
        .unwrap();
        h.set(LENGTH, 0.1);
        let (out, active) = run(&mut h, &gate(frames, 10..20));
        // Quiet until the trigger.
        assert!(out[..10].iter().all(|&x| x == 0.0));
        // A 0.1 s triangle peaks 0.05 s (2400 samples) after the trigger.
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        assert!(
            (2_400 + 10 - peak.0 as i64).abs() < 20,
            "peak at {}",
            peak.0
        );
        assert!((peak.1 - 1.0).abs() < 1e-2, "peak {}", peak.1);
        // The trigger was only 10 samples long, and the curve still played.
        assert_eq!(active[1_000], 1.0);
        assert!(out[frames - 1] < 1e-2 && active[frames - 1] == 0.0);
    }

    #[test]
    fn a_gate_released_on_a_block_edge_does_not_swallow_the_next_note() {
        let frames = 512;
        let mut h = Harness::new(
            &Envelope,
            &config(&triangle(), false),
            &[(TRIGGER, Shape::MONO)],
            RATE,
            frames,
        )
        .unwrap();
        h.set(LENGTH, 0.005);
        // Held for a whole block, longer than the curve: it finishes with the
        // gate still high.
        run(&mut h, &vec![1.0; frames]);
        // Released exactly on the block edge, with the block flagged silent.
        let mut quiet = h.input(TRIGGER, frames);
        quiet.fill(0.0);
        quiet.set_silent(0, 0);
        h.run(frames).unwrap();
        // The next note still starts.
        let (out, _) = run(&mut h, &vec![1.0; frames]);
        assert!(out.iter().any(|&x| x > 0.9), "the envelope never started");
    }

    #[test]
    fn depth_scales_the_output() {
        let frames = 2_400;
        let mut h = Harness::new(
            &Envelope,
            &config(&triangle(), false),
            &[(TRIGGER, Shape::MONO)],
            RATE,
            frames,
        )
        .unwrap();
        h.set(LENGTH, 0.1);
        h.set(DEPTH, 0.25);
        let (out, _) = run(&mut h, &gate(frames, 0..10));
        let peak = out.iter().copied().fold(0.0, f32::max);
        assert!((peak - 0.25).abs() < 5e-3, "peak {peak}");
    }

    #[test]
    fn a_new_trigger_restarts_the_curve() {
        let frames = 7_200;
        let mut h = Harness::new(
            &Envelope,
            &config(&triangle(), false),
            &[(TRIGGER, Shape::MONO)],
            RATE,
            frames,
        )
        .unwrap();
        h.set(LENGTH, 0.1);
        // Triggers at 0 and at 3840 (past the peak, on the way down).
        let mut trig = vec![0.0; frames];
        trig[0] = 1.0;
        trig[3_840] = 1.0;
        let (out, _) = run(&mut h, &trig);
        assert!(out[3_830] < 0.5, "falling before the restart");
        assert!(out[3_840 + 100] < 0.1, "climbing from the start again");
        assert!(out[3_840 + 2_400] > 0.95, "peaks 0.05 s later");
    }

    #[test]
    fn looping_repeats_while_held_and_finishes_the_cycle_after() {
        let frames = 9_600;
        let mut h = Harness::new(
            &Envelope,
            &config(&triangle(), false),
            &[(TRIGGER, Shape::MONO)],
            RATE,
            frames,
        )
        .unwrap();
        h.set(LENGTH, 0.05); // 2400 samples a cycle
        h.set(LOOP, 1.0);
        // Held for 1.5 cycles.
        let (out, active) = run(&mut h, &gate(frames, 0..3_600));
        let peaks = out
            .windows(3)
            .filter(|w| w[1] > 0.99 && w[1] >= w[0] && w[1] > w[2])
            .count();
        assert_eq!(peaks, 2, "peaks at the middle of the first two cycles");
        // The cycle in hand finished at 4800 and the rest is quiet.
        assert!(out[5_000..].iter().all(|&x| x < 1e-2));
        assert_eq!(active[frames - 1], 0.0);
    }

    #[test]
    fn multi_voice_gives_each_voice_its_own_envelope() {
        let frames = 4_800;
        let voices = Shape::new(2, 1);
        let mut h = Harness::new(
            &Envelope,
            &config(&triangle(), false),
            &[(TRIGGER, voices)],
            RATE,
            frames,
        )
        .unwrap();
        h.set(LENGTH, 0.1);
        {
            let mut input = h.input(TRIGGER, frames);
            input.lane_mut(0, 0).copy_from_slice(&gate(frames, 0..10));
            input
                .lane_mut(1, 0)
                .copy_from_slice(&gate(frames, 1_200..1_210));
        }
        h.run(frames).unwrap();
        let out = h.output(OUT);
        assert_eq!(out.shape(), voices);
        let first_peak = out.lane(0, 0).iter().position(|&x| x > 0.99).unwrap();
        let second_peak = out.lane(1, 0).iter().position(|&x| x > 0.99).unwrap();
        assert!((second_peak as i64 - first_peak as i64 - 1_200).abs() < 30);
    }

    #[test]
    fn single_voice_is_mono_and_any_voice_restarts_it() {
        let frames = 7_200;
        let voices = Shape::new(2, 1);
        let mut h = Harness::new(
            &Envelope,
            &config(&triangle(), true),
            &[(TRIGGER, voices)],
            RATE,
            frames,
        )
        .unwrap();
        h.set(LENGTH, 0.1);
        {
            let mut input = h.input(TRIGGER, frames);
            input.lane_mut(0, 0).copy_from_slice(&gate(frames, 0..10));
            // The second voice triggers while the first envelope plays.
            input
                .lane_mut(1, 0)
                .copy_from_slice(&gate(frames, 3_840..3_850));
        }
        h.run(frames).unwrap();
        let out = h.output(OUT);
        assert_eq!(out.shape(), Shape::MONO);
        let out = out.lane(0, 0);
        assert!(out[3_830] < 0.5);
        assert!(out[3_840 + 2_400] > 0.95, "restarted by the second voice");
    }

    #[test]
    fn single_voice_with_a_held_gate_does_not_retrigger() {
        let frames = 7_200;
        let mut h = Harness::new(
            &Envelope,
            &config(&triangle(), true),
            &[(TRIGGER, Shape::new(2, 1))],
            RATE,
            frames,
        )
        .unwrap();
        h.set(LENGTH, 0.1);
        {
            let mut input = h.input(TRIGGER, frames);
            input.lane_mut(0, 0).fill(1.0);
            input
                .lane_mut(1, 0)
                .copy_from_slice(&gate(frames, 100..frames));
        }
        h.run(frames).unwrap();
        let out = h.output(OUT);
        let out = out.lane(0, 0);
        // Only the rising edges at 0 and 100 restart it, so it still peaks.
        assert!(out.iter().any(|&x| x > 0.99));
        assert!(out[frames - 1] < 1e-2, "ran to its end and stayed there");
    }

    #[test]
    fn a_curve_that_rests_above_zero_holds_its_last_height() {
        let rising = EnvelopeCurve::from_text("0 0.2 0;1 0.8 0");
        let frames = 4_800;
        let mut h = Harness::new(
            &Envelope,
            &config(&rising, false),
            &[(TRIGGER, Shape::MONO)],
            RATE,
            frames,
        )
        .unwrap();
        h.set(LENGTH, 0.05);
        let (out, _) = run(&mut h, &gate(frames, 0..1));
        assert!((out[frames - 1] - 0.8).abs() < 1e-3);
        // Before any trigger it rests there too, so it is never skipped.
        let mut h = Harness::new(
            &Envelope,
            &config(&rising, false),
            &[(TRIGGER, Shape::MONO)],
            RATE,
            frames,
        )
        .unwrap();
        let (out, _) = run(&mut h, &vec![0.0; frames]);
        assert!((out[0] - 0.8).abs() < 1e-3);
    }

    #[test]
    fn the_text_form_round_trips_and_unreadable_text_gives_the_default() {
        let mut curve = EnvelopeCurve::default();
        curve.move_point(1, 0.3, 0.9);
        curve.set_bend(1, 0.4);
        let back = EnvelopeCurve::from_text(&curve.to_text());
        assert_eq!(back, curve);
        assert_eq!(EnvelopeCurve::default().to_text(), "");
        assert_eq!(EnvelopeCurve::from_text(""), EnvelopeCurve::default());
        assert_eq!(EnvelopeCurve::from_text("junk"), EnvelopeCurve::default());
        assert_eq!(EnvelopeCurve::from_text("0 0 0"), EnvelopeCurve::default());
        assert_eq!(
            EnvelopeCurve::from_text("0 0 NaN;1 1 0"),
            EnvelopeCurve::default()
        );
    }

    #[test]
    fn editing_keeps_the_ends_and_the_order() {
        let mut curve = triangle();
        // The ends only move in height.
        curve.move_point(0, 0.5, 0.3);
        curve.move_point(2, 0.1, 0.6);
        assert_eq!((curve.points()[0].x, curve.points()[0].y), (0.0, 0.3));
        assert_eq!((curve.points()[2].x, curve.points()[2].y), (1.0, 0.6));
        // An inner anchor can't pass its neighbours.
        curve.move_point(1, 2.0, 5.0);
        assert!(curve.points()[1].x < 1.0 && curve.points()[1].y == 1.0);
        // Anchors can be added on the curve and removed, but not the ends.
        let at = curve.insert_at(0.25);
        assert_eq!(at, 1);
        assert_eq!(curve.points().len(), 4);
        assert!((curve.points()[1].y - curve.eval(0.25)).abs() < 1e-6);
        assert!(curve.remove(1));
        assert!(!curve.remove(0));
        assert!(!curve.remove(curve.points().len() - 1));
        // Too close to an anchor to fit one: that anchor.
        assert_eq!(curve.insert_at(0.0), 0);
    }

    #[test]
    fn bending_a_segment_moves_its_middle_where_it_is_dragged() {
        let mut curve = EnvelopeCurve::from_text("0 0 0;1 1 0");
        for target in [0.2, 0.5, 0.8] {
            let bend = curve.bend_for_middle(0, target);
            curve.set_bend(0, bend);
            assert!(
                (curve.eval(0.5) - target).abs() < 0.02,
                "{target}: {}",
                curve.eval(0.5)
            );
        }
        // A fast-first (negative) bend is above the straight line.
        curve.set_bend(0, -0.8);
        assert!(curve.eval(0.5) > 0.5);
        curve.set_bend(0, 0.8);
        assert!(curve.eval(0.5) < 0.5);
        // A flat segment has nothing to bend.
        let flat = EnvelopeCurve::from_text("0 0.5 0;1 0.5 0");
        assert_eq!(flat.bend_for_middle(0, 0.9), 0.0);
    }
}
