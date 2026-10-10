//! The Delay node.

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Ports,
    Setup, Shape, SignalIn, SignalOut, Unit,
};

/// Delays its input by `time`. The output is the delayed signal alone; for an
/// echo, wire the output back into a mixer with the input (a feedback loop),
/// through a Gain that keeps the loop below 1.
///
/// **Interpolation.** `time` is in seconds and can move per sample (wire an
/// LFO into it for a chorus or flanger), and the line is read with linear
/// interpolation. A time of 0 passes the input through.
///
/// **Feedback loops.** Delay is the node that lets a wire close a loop. When
/// one does, the compiler runs Delay in two steps, one that writes the output
/// from what the line already holds and one, later in the block, that reads
/// the input. The output can then be at most as new as the block before, so
/// *inside a loop* the delay is at least one block, the engine's largest (about
/// 10 ms at 512 frames), and shorter times are held to that. Outside a
/// loop there is no minimum.
///
/// **Silence.** A lane whose input stays silent for as long as the line is
/// long is flagged silent, and costs almost nothing, so a polyphonic echo
/// doesn't spend time on voices that finished.
pub struct Delay;

pub const DELAY_ID: &str = "noodle.util.delay";

/// The longest delay, in seconds. The line is allocated for it up front.
pub const MAX_TIME: f32 = 2.0;

#[derive(Ports)]
struct DelayPorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "time",
        "Time",
        ParamInfo::new(0.0, MAX_TIME, 0.25).unit(Unit::Seconds).offset()
    )]
    time: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = DelayPorts::INPUT;
const TIME: usize = DelayPorts::TIME;

static INFO: NodeInfo = NodeInfo {
    id: DELAY_ID,
    version: 1,
    name: "Delay",
    category: "Effects",
};

impl NodeType for Delay {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(DelayPorts::layout())
    }

    fn loop_input(&self, _config: &Config) -> Option<&'static str> {
        Some("in")
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let shape = setup.output_shapes[0];
        // Room for the longest delay plus a block, since a split node
        // reads from before the block and writes after it.
        let samples = (MAX_TIME * setup.sample_rate).ceil() as usize + setup.max_frames + 4;
        let len = samples.next_power_of_two();
        Ok(Instance::realtime(DelayNode {
            shape,
            sample_rate: setup.sample_rate,
            max_frames: setup.max_frames,
            lines: (0..shape.lanes())
                .map(|_| vec![0.0; len].into_boxed_slice())
                .collect(),
            mask: len - 1,
            quiet: vec![len; shape.lanes()],
            written: 0,
        }))
    }
}

struct DelayNode {
    shape: Shape,
    sample_rate: f32,
    /// The most frames in a block, which is as short as a delay may be inside
    /// a loop, whatever the size of the block in hand.
    max_frames: usize,
    /// One ring buffer per lane, each `mask + 1` long.
    lines: Vec<Box<[f32]>>,
    mask: usize,
    /// Per lane, how many samples in a row the input has been silent. Once
    /// that reaches the line's length the line is all zeros.
    quiet: Vec<usize>,
    /// The number of samples written so far, which wraps. Sample `n` of the
    /// stream lives at `n & mask`.
    written: usize,
}

impl DelayNode {
    /// The delay, in samples, for each frame of the block, at least `min`.
    fn delay(
        &self,
        time: &SignalIn<'_>,
        voice: usize,
        channel: usize,
        min: f32,
    ) -> impl Fn(usize) -> f32 {
        let longest = (self.mask + 1 - 2) as f32;
        let scale = self.sample_rate;
        let constant = time.constant();
        let lane = time.lane(voice, channel);
        move |k| {
            let seconds = constant.unwrap_or_else(|| lane[k]);
            let samples = if seconds.is_finite() {
                seconds.clamp(0.0, MAX_TIME) * scale
            } else {
                0.0
            };
            samples.max(min).min(longest)
        }
    }

    /// Reads the line `delay` samples before stream position `at`.
    fn read(&self, lane: usize, at: usize, delay: f32) -> f32 {
        let whole = delay as usize;
        let frac = delay - whole as f32;
        let line = &self.lines[lane];
        let newer = line[at.wrapping_sub(whole) & self.mask];
        let older = line[at.wrapping_sub(whole + 1) & self.mask];
        newer + (older - newer) * frac
    }

    fn lane_of(&self, lane: usize) -> (usize, usize) {
        (lane / self.shape.channels, lane % self.shape.channels)
    }

    /// Writes the block's input to the lines.
    fn write(&mut self, input: &SignalIn<'_>, frames: usize) {
        let start = self.written;
        for lane in 0..self.shape.lanes() {
            let (voice, channel) = self.lane_of(lane);
            if input.is_silent(voice, channel) {
                // The line is all zeros once it has been written that many
                // zeros, so there is nothing more to write.
                if self.quiet[lane] <= self.mask {
                    for k in 0..frames {
                        self.lines[lane][start.wrapping_add(k) & self.mask] = 0.0;
                    }
                }
                self.quiet[lane] = self.quiet[lane].saturating_add(frames);
                continue;
            }
            let samples = input.lane(voice, channel);
            for (k, &x) in samples.iter().enumerate() {
                self.lines[lane][start.wrapping_add(k) & self.mask] = x;
            }
            self.quiet[lane] = 0;
        }
        self.written = start.wrapping_add(frames);
    }

    /// Writes the output lanes from the lines, for a block that starts at
    /// stream position `start`. `min` is the shortest delay allowed.
    fn read_block(&self, time: &SignalIn<'_>, output: &mut SignalOut<'_>, start: usize, min: f32) {
        for lane in 0..self.shape.lanes() {
            let (voice, channel) = self.lane_of(lane);
            if self.quiet[lane] > self.mask {
                output.silence(voice, channel);
                continue;
            }
            let delay = self.delay(time, voice, channel, min);
            let out = output.lane_mut(voice, channel);
            for (k, sample) in out.iter_mut().enumerate() {
                *sample = self.read(lane, start.wrapping_add(k), delay(k));
            }
        }
    }
}

impl Node for DelayNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        // Whole: each sample is written before it is read, so a delay of 0
        // passes the input through.
        let start = self.written;
        self.write(&io.inputs[IN], ctx.frames);
        self.read_block(&io.inputs[TIME], &mut io.outputs[0], start, 0.0);
    }

    fn process_output(&mut self, ctx: &Context, io: Io<'_, '_>) {
        // Everything the output reads was written by earlier blocks, so the
        // shortest delay is a whole block. It is the largest block rather than
        // this one's size, so the delay doesn't change as blocks do.
        let _ = ctx;
        let min = self.max_frames as f32;
        self.read_block(&io.inputs[TIME], &mut io.outputs[0], self.written, min);
    }

    fn process_input(&mut self, ctx: &Context, io: Io<'_, '_>) {
        self.write(&io.inputs[IN], ctx.frames);
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.fill(0.0);
        }
        self.quiet.fill(self.mask + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    const OUT: usize = 0;

    fn harness(max_frames: usize) -> Harness {
        Harness::new(
            &Delay,
            &Config::new(),
            &[(IN, Shape::MONO)],
            48_000.0,
            max_frames,
        )
        .unwrap()
    }

    fn impulse(h: &mut Harness, frames: usize, at: usize) {
        let mut input = h.input(IN, frames);
        input.fill(0.0);
        if at < frames {
            input.lane_mut(0, 0)[at] = 1.0;
        }
    }

    #[test]
    fn delays_by_the_time_across_blocks() {
        let mut h = harness(64);
        // 100 samples at 48 kHz.
        h.set(TIME, 100.0 / 48_000.0);
        let mut heard = Vec::new();
        for block in 0..4 {
            impulse(&mut h, 64, if block == 0 { 3 } else { 99 });
            h.run(64).unwrap();
            heard.extend_from_slice(h.output(OUT).lane(0, 0));
        }
        let peak = heard
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        assert_eq!(peak.0, 103);
        assert!((peak.1 - 1.0).abs() < 1e-4, "{}", peak.1);
        assert_eq!(heard.iter().filter(|x| x.abs() > 1e-4).count(), 1);
    }

    #[test]
    fn a_fractional_time_interpolates() {
        let mut h = harness(64);
        h.set(TIME, 10.5 / 48_000.0);
        impulse(&mut h, 64, 0);
        h.run(64).unwrap();
        let out = h.output(OUT).lane(0, 0);
        assert!((out[10] - 0.5).abs() < 1e-4, "{}", out[10]);
        assert!((out[11] - 0.5).abs() < 1e-4, "{}", out[11]);
    }

    #[test]
    fn a_time_of_zero_passes_the_input_through() {
        let mut h = harness(8);
        h.set(TIME, 0.0);
        h.input(IN, 8).fill(0.7);
        h.run(8).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[0.7; 8]);
    }

    #[test]
    fn a_silent_input_flags_the_output_once_the_line_is_empty() {
        let mut h = harness(64);
        h.set(TIME, 0.001);
        impulse(&mut h, 64, 0);
        h.run(64).unwrap();
        assert!(!h.output(OUT).is_silent(0, 0));
        // The line holds 131072 samples; feed it that many flagged zeros.
        let blocks = 131_072 / 64 + 2;
        for _ in 0..blocks {
            let mut input = h.input(IN, 64);
            input.silence(0, 0);
            h.run(64).unwrap();
        }
        assert!(h.output(OUT).is_silent(0, 0));
        assert_eq!(h.output(OUT).lane(0, 0), &[0.0; 64]);
        // A sound wakes it again.
        impulse(&mut h, 64, 0);
        h.run(64).unwrap();
        assert!(!h.output(OUT).is_silent(0, 0));
    }

    #[test]
    fn each_voice_has_its_own_line() {
        let shape = Shape::new(2, 1);
        let mut h = Harness::new(&Delay, &Config::new(), &[(IN, shape)], 48_000.0, 16).unwrap();
        h.set(TIME, 4.0 / 48_000.0);
        let mut input = h.input(IN, 16);
        input.fill(0.0);
        input.lane_mut(0, 0)[0] = 1.0;
        input.lane_mut(1, 0)[1] = 1.0;
        h.run(16).unwrap();
        let out = h.output(OUT);
        assert!((out.lane(0, 0)[4] - 1.0).abs() < 1e-4);
        assert!((out.lane(1, 0)[5] - 1.0).abs() < 1e-4);
        assert!(out.lane(0, 0)[5].abs() < 1e-4);
    }
}
