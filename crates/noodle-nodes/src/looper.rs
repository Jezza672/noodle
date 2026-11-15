//! The Looper node.

use crate::meter::voice_sum;
use noodle_core::TimeSignature;
use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, LoopState, LoopWriter, Node, NodeError,
    NodeInfo, NodeType, ParamInfo, Ports, Setup, Shape, Telemetry,
};

/// Records the audio at its input when told to, then plays it back as a loop
/// that stays on the project's bars. While it plays you can overdub more
/// layers, undo the last one, or clear it and start again. The output is the
/// loop alone (like Delay's wet-only output): mix it with the dry signal in
/// the graph.
///
/// **Controls** are parameters, so a Button, a MIDI Control or a Launch
/// Quantise drives them through visible wires. `record` (rising edge) is the
/// pedal cycle: Empty to Recording, Recording to Playing, Playing to
/// Overdubbing, Overdubbing to Playing. `undo` and `clear` are rising edges.
/// `play` is a level: 0 mutes the output while the loop keeps running.
/// `feedback` (0 to 1) is how much of the older layers survives each new
/// overdub pass, `level` scales the output, and `length` is Auto (the stop
/// gesture is held to the next bar line) or a fixed 1, 2, 4 or 8 bars, which
/// end the recording by themselves.
///
/// **Whole passes.** Overdub starts and stops only at the loop's wrap, so
/// every layer covers a whole pass and undo is exact. An `undo` during an
/// overdub takes effect at the wrap too.
///
/// **Sync.** Recording starts at the gesture; Launch Quantise upstream is what
/// puts the gesture on a bar. The loop's length in samples is fixed from the
/// tempo at the end gesture, so a later tempo change does not stretch it. The
/// loop runs on the node's own sample counter, so it plays with the transport
/// stopped, and `reset` (a seek) keeps the recording.
///
/// **Seams.** The head of the loop is crossfaded over about 5 ms with the
/// audio that followed the end, so the wrap has no step, and overdub writes
/// ramp in and out over the same time.
///
/// **Undo.** Two buffers, `base` and `layer`; the output is their sum. A new
/// overdub pass folds the old `layer[p]` into `base[p]` (times `feedback`)
/// before overwriting it, so no bulk copy ever runs on the audio thread. One
/// level of undo.
///
/// **Shape.** Channels follow the input, voices are summed on the way in, and
/// the output is one voice. The node is nondeterministic (it holds a live
/// recording), so it and everything after it are never cached. The recording
/// is session state and is not saved. Changing `max_seconds` or the input's
/// channel count rebuilds the node and loses the loop.
pub struct Looper {
    telemetry: Telemetry,
}

impl Looper {
    /// A Looper that reports its state and playhead to `telemetry`, for the
    /// properties panel.
    pub fn new(telemetry: &Telemetry) -> Self {
        Self {
            telemetry: telemetry.clone(),
        }
    }
}

pub const LOOPER_ID: &str = "noodle.util.looper";

const MAX_SECONDS: ConfigInfo = ConfigInfo::int("max_seconds", "Max seconds", 30);
static CONFIG: [ConfigInfo; 1] = [MAX_SECONDS];

/// How many bars each `length` choice records; 0 is Auto.
const LENGTH_BARS: [usize; 5] = [0, 1, 2, 4, 8];

#[derive(Ports)]
#[ports(nondeterministic)]
struct LooperPorts {
    #[input("in", "In")]
    input: (),
    #[param("record", "Record", ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0))]
    record: (),
    #[param("play", "Play", ParamInfo::new(0.0, 1.0, 1.0).smoothing(0.0))]
    play: (),
    #[param("undo", "Undo", ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0))]
    undo: (),
    #[param("clear", "Clear", ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0))]
    clear: (),
    #[param("feedback", "Feedback", ParamInfo::new(0.0, 1.0, 1.0))]
    feedback: (),
    #[param("level", "Level", ParamInfo::new(0.0, 1.0, 1.0))]
    level: (),
    #[param(
        "length",
        "Length",
        ParamInfo::choice(["Auto", "1 bar", "2 bars", "4 bars", "8 bars"]).smoothing(0.0)
    )]
    length: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = LooperPorts::INPUT;
const RECORD: usize = LooperPorts::RECORD;
const PLAY: usize = LooperPorts::PLAY;
const UNDO: usize = LooperPorts::UNDO;
const CLEAR: usize = LooperPorts::CLEAR;
const FEEDBACK: usize = LooperPorts::FEEDBACK;
const LEVEL: usize = LooperPorts::LEVEL;
const LENGTH: usize = LooperPorts::LENGTH;

static INFO: NodeInfo = NodeInfo {
    id: LOOPER_ID,
    version: 1,
    name: "Looper",
    category: "Effects",
};

fn max_seconds(config: &Config) -> Result<usize, NodeError> {
    let n = MAX_SECONDS.get_int(config);
    if !(1..=600).contains(&n) {
        return Err(NodeError::config(format!(
            "Looper holds between 1 and 600 seconds, not {n}"
        )));
    }
    Ok(n as usize)
}

impl NodeType for Looper {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        max_seconds(config)?;
        Ok(LooperPorts::layout())
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let channels = inputs.first().map_or(1, |s| s.channels);
        Ok(vec![Shape::new(1, channels)])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let seconds = max_seconds(setup.config)?;
        let channels = setup.input_shapes.first().map_or(1, |s| s.channels);
        let cap = seconds * setup.sample_rate as usize;
        let buffer = || {
            (0..channels)
                .map(|_| vec![0.0; cap].into_boxed_slice())
                .collect::<Vec<_>>()
        };
        Ok(Instance::realtime(LooperNode {
            report: self.telemetry.open_loop(setup.node),
            sample_rate: setup.sample_rate,
            channels,
            cap,
            // About 5 ms, and at least one sample.
            seam: ((0.005 * setup.sample_rate) as usize).max(1),
            base: buffer(),
            layer: buffer(),
            state: State::Empty,
            len: 0,
            pos: 0,
            stop_at: None,
            bar_samples: 0.0,
            phase: 0.0,
            layer_valid: false,
            pending_start: false,
            pending_stop: false,
            pending_undo: false,
            ramp_in: false,
            postroll: false,
            prev: [false; 3],
        }))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Empty,
    Recording,
    Playing,
    Overdubbing,
}

struct LooperNode {
    /// Tells the UI what the loop is doing, once a block.
    report: LoopWriter,
    sample_rate: f32,
    channels: usize,
    /// Samples each buffer holds.
    cap: usize,
    /// The crossfade and ramp length in samples.
    seam: usize,
    base: Vec<Box<[f32]>>,
    layer: Vec<Box<[f32]>>,
    state: State,
    /// The loop's length in samples once recorded.
    len: usize,
    /// Position in the loop (Playing, Overdubbing) or samples recorded so far
    /// (Recording).
    pos: usize,
    /// While recording: the sample at which the loop ends, once known.
    stop_at: Option<usize>,
    /// A bar in samples, at the tempo when recording started.
    bar_samples: f64,
    /// How far into a bar the recording started, in samples.
    phase: f64,
    /// Whether `layer` holds the last overdub pass.
    layer_valid: bool,
    pending_start: bool,
    pending_stop: bool,
    pending_undo: bool,
    /// The pass being written is the first of an overdub: ramp in.
    ramp_in: bool,
    /// The first pass after recording, whose head is crossfaded with the live
    /// input that follows the end.
    postroll: bool,
    /// Whether record, undo and clear were high on the last sample.
    prev: [bool; 3],
}

fn bar_ticks(signature: TimeSignature) -> f64 {
    f64::from(signature.numerator) * 960.0 * 4.0 / f64::from(signature.denominator)
}

impl LooperNode {
    fn clear(&mut self) {
        self.state = State::Empty;
        self.len = 0;
        self.pos = 0;
        self.stop_at = None;
        self.layer_valid = false;
        self.pending_start = false;
        self.pending_stop = false;
        self.pending_undo = false;
        self.ramp_in = false;
        self.postroll = false;
    }

    fn start_recording(&mut self, ctx: &Context, frame: usize, bars: usize) {
        let t = &ctx.transport;
        let bar_ticks = bar_ticks(t.signature);
        let samples_per_tick = 60.0 / (t.bpm.max(1.0) * 960.0) * f64::from(self.sample_rate);
        self.bar_samples = bar_ticks * samples_per_tick;
        self.phase = if t.playing {
            let tick = t.tick + frame as f64 / samples_per_tick;
            (tick - t.signature_start).rem_euclid(bar_ticks) * samples_per_tick
        } else {
            0.0
        };
        self.state = State::Recording;
        self.pos = 0;
        self.stop_at = (bars > 0).then(|| self.fit(bars as f64 * self.bar_samples));
    }

    /// A loop length in samples, kept within what the buffers hold.
    fn fit(&self, samples: f64) -> usize {
        (samples.round() as usize).clamp(2 * self.seam, self.cap.max(2 * self.seam))
    }

    /// The recording ends at the next bar line after the gesture.
    fn end_recording(&mut self) {
        if self.stop_at.is_some() {
            return;
        }
        let bars = ((self.pos as f64 + self.phase) / self.bar_samples)
            .ceil()
            .max(1.0);
        self.stop_at = Some(self.fit(bars * self.bar_samples - self.phase));
    }

    fn wrap(&mut self) {
        self.pos = 0;
        self.postroll = false;
        match self.state {
            State::Overdubbing => {
                self.layer_valid = true;
                self.ramp_in = false;
                if self.pending_undo {
                    self.layer_valid = false;
                    self.pending_undo = false;
                    self.pending_stop = false;
                    self.state = State::Playing;
                } else if self.pending_stop {
                    self.pending_stop = false;
                    self.state = State::Playing;
                }
            }
            State::Playing if self.pending_start => {
                self.pending_start = false;
                self.state = State::Overdubbing;
                self.ramp_in = true;
            }
            _ => {}
        }
    }
}

impl LooperNode {
    /// Reports the state and the playhead for the display.
    fn publish(&self) {
        let (state, at) = match self.state {
            State::Empty => (LoopState::Empty, 0.0),
            State::Recording => {
                let end = self.stop_at.unwrap_or(self.cap).max(1);
                (LoopState::Recording, self.pos as f32 / end as f32)
            }
            State::Playing => (LoopState::Playing, self.pos as f32 / self.len.max(1) as f32),
            State::Overdubbing => (
                LoopState::Overdubbing,
                self.pos as f32 / self.len.max(1) as f32,
            ),
        };
        let queued = self.pending_start
            || self.pending_stop
            || self.pending_undo
            || (self.state == State::Recording && self.stop_at.is_some());
        self.report.write(state, self.layer_valid, queued, at);
    }
}

impl Node for LooperNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs, outputs, ..
        } = io;
        let seam = self.seam;
        let record = inputs[RECORD].lane(0, 0);
        let play = inputs[PLAY].lane(0, 0);
        let undo = inputs[UNDO].lane(0, 0);
        let clear = inputs[CLEAR].lane(0, 0);
        let feedback = inputs[FEEDBACK].lane(0, 0);
        let level = inputs[LEVEL].lane(0, 0);
        let length = inputs[LENGTH].lane(0, 0);
        let out = &mut outputs[0];

        for k in 0..ctx.frames {
            let now = [record[k] >= 0.5, undo[k] >= 0.5, clear[k] >= 0.5];
            let rise = [
                now[0] && !self.prev[0],
                now[1] && !self.prev[1],
                now[2] && !self.prev[2],
            ];
            self.prev = now;

            if rise[2] {
                self.clear();
            }
            if rise[1] {
                match self.state {
                    State::Playing => self.layer_valid = false,
                    State::Overdubbing => self.pending_undo = true,
                    _ => {}
                }
            }
            if rise[0] {
                match self.state {
                    State::Empty => {
                        let bars = LENGTH_BARS[(length[k].round().max(0.0) as usize).min(4)];
                        self.start_recording(ctx, k, bars);
                    }
                    State::Recording => self.end_recording(),
                    State::Playing => self.pending_start = !self.pending_start,
                    State::Overdubbing => self.pending_stop = !self.pending_stop,
                }
            }

            let gain = play[k].clamp(0.0, 1.0) * level[k].clamp(0.0, 1.0);
            let fb = feedback[k].clamp(0.0, 1.0);
            for c in 0..self.channels {
                let x = voice_sum(&inputs[IN], c, k);
                let x = if x.is_finite() { x } else { 0.0 };
                let sample = match self.state {
                    State::Empty => 0.0,
                    State::Recording => {
                        self.base[c][self.pos] = x;
                        0.0
                    }
                    State::Playing | State::Overdubbing => {
                        let p = self.pos;
                        if self.postroll && p < seam {
                            // The head fades from the audio that followed the
                            // end (the live input) into the recorded head.
                            let w = p as f32 / seam as f32;
                            let v = self.base[c][p] * w + x * (1.0 - w);
                            self.base[c][p] = v;
                        }
                        if self.state == State::Overdubbing {
                            let mut g: f32 = 1.0;
                            if self.ramp_in {
                                g = g.min(p as f32 / seam as f32);
                            }
                            if self.pending_stop || self.pending_undo {
                                g = g.min((self.len - p) as f32 / seam as f32);
                            }
                            if self.layer_valid {
                                self.base[c][p] += self.layer[c][p] * fb;
                            }
                            self.layer[c][p] = x * g;
                            self.base[c][p] + self.layer[c][p]
                        } else if self.layer_valid {
                            self.base[c][p] + self.layer[c][p]
                        } else {
                            self.base[c][p]
                        }
                    }
                };
                out.lane_mut(0, c)[k] = sample * gain;
            }

            match self.state {
                State::Recording => {
                    self.pos += 1;
                    let end = self.stop_at.unwrap_or(self.cap);
                    if self.pos >= end.min(self.cap) {
                        self.len = self.pos.max(2 * self.seam).min(self.cap);
                        self.pos = 0;
                        self.stop_at = None;
                        self.state = State::Playing;
                        self.postroll = true;
                        self.layer_valid = false;
                    }
                }
                State::Playing | State::Overdubbing => {
                    self.pos += 1;
                    if self.pos >= self.len {
                        self.wrap();
                    }
                }
                State::Empty => {}
            }
        }
        self.publish();
    }

    /// Keeps the recording: a seek moves the playhead, not the loop.
    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    /// 1 kHz, so a bar at 120 bpm in 4/4 is 2000 samples and the seam 5.
    const RATE: f32 = 1000.0;
    const BAR: usize = 2000;
    const OUT: usize = 0;

    fn harness() -> Harness {
        Harness::new(
            &Looper::new(&Telemetry::new()),
            &Config::new(),
            &[(IN, Shape::MONO)],
            RATE,
            100,
        )
        .unwrap()
    }

    /// Runs `samples` frames of `signal(n)`, from sample `start`, and returns
    /// the output.
    fn run(
        h: &mut Harness,
        start: usize,
        samples: usize,
        signal: impl Fn(usize) -> f32,
    ) -> Vec<f32> {
        let mut heard = Vec::new();
        for block in 0..samples / 100 {
            let mut input = h.input(IN, 100);
            for (k, s) in input.lane_mut(0, 0).iter_mut().enumerate() {
                *s = signal(start + block * 100 + k);
            }
            h.run(100).unwrap();
            heard.extend_from_slice(h.output(OUT).lane(0, 0));
        }
        heard
    }

    /// A pulse over one block, then released, as a Button press would be,
    /// while `x` plays at the input.
    fn press(h: &mut Harness, port: usize, x: f32) {
        h.set(port, 1.0);
        run(h, 0, 100, |_| x);
        h.set(port, 0.0);
    }

    fn sine(n: usize) -> f32 {
        0.5 * (std::f32::consts::TAU * 50.0 * n as f32 / RATE).sin()
    }

    /// Records exactly `bars` bars of `signal`, starting at sample 0.
    fn record_bars(h: &mut Harness, bars: usize, signal: impl Fn(usize) -> f32 + Copy) {
        h.set(RECORD, 1.0);
        run(h, 0, 100, signal);
        h.set(RECORD, 0.0);
        run(h, 100, bars * BAR - 100 - 100, signal);
        // The stop gesture, shortly before the bar line.
        h.set(RECORD, 1.0);
        run(h, bars * BAR - 100, 100, signal);
        h.set(RECORD, 0.0);
    }

    #[test]
    fn the_loop_is_a_whole_number_of_bars_and_repeats_a_sine() {
        let mut h = harness();
        record_bars(&mut h, 1, sine);
        // The first pass (head crossfaded with the live input that follows
        // the end), then two more.
        run(&mut h, BAR, BAR, sine);
        let second = run(&mut h, 2 * BAR, BAR, |_| 0.0);
        let third = run(&mut h, 3 * BAR, BAR, |_| 0.0);
        assert_eq!(second, third);
        // 50 Hz sine at 1 kHz repeats every 20 samples, so the loop is the
        // sine, whole bar long: the sample one bar on matches the first.
        for (n, &x) in second.iter().enumerate().take(BAR - 20).skip(20) {
            assert!((x - sine(n)).abs() < 1e-3, "{n}");
        }
        // Past the first pass the silent input does not leak in.
        assert!(second.iter().any(|x| x.abs() > 0.4));
    }

    #[test]
    fn a_gesture_mid_bar_holds_to_the_next_bar_line() {
        let mut h = harness();
        h.set(RECORD, 1.0);
        run(&mut h, 0, 100, sine);
        h.set(RECORD, 0.0);
        run(&mut h, 100, 700, sine);
        // Stop at 800 samples: the loop must still end at 2000.
        press(&mut h, RECORD, 0.0);
        let out = run(&mut h, 900, 1100, sine);
        assert!(out.iter().all(|x| *x == 0.0), "still recording");
        // Loop length 2000: output at pass sample 1500 is the sine there.
        let pass = run(&mut h, 2000, BAR, |_| 0.0);
        assert!((pass[1500] - sine(1500)).abs() < 1e-3);
        let again = run(&mut h, 4000, BAR, |_| 0.0);
        assert_eq!(pass, again);
    }

    #[test]
    fn a_fixed_length_ends_by_itself() {
        let mut h = harness();
        h.set(LENGTH, 2.0);
        h.set(RECORD, 1.0);
        run(&mut h, 0, 100, sine);
        h.set(RECORD, 0.0);
        run(&mut h, 100, 2 * BAR - 100, sine);
        let pass = run(&mut h, 2 * BAR, 2 * BAR, |_| 0.0);
        assert!((pass[3000] - sine(3000)).abs() < 1e-3);
    }

    #[test]
    fn overdub_sums_and_undo_removes_only_the_last_layer() {
        let mut h = harness();
        record_bars(&mut h, 1, |_| 0.25);
        run(&mut h, BAR, BAR, |_| 0.25);
        // Overdub starts at the next wrap, and the stop is asked for at once,
        // so it covers exactly one pass of 0.125.
        press(&mut h, RECORD, 0.0);
        run(&mut h, 0, BAR - 100, |_| 0.0);
        press(&mut h, RECORD, 0.125);
        run(&mut h, 0, BAR - 100, |_| 0.125);
        let heard = run(&mut h, 0, BAR, |_| 0.0);
        assert!((heard[900] - 0.375).abs() < 1e-3, "{}", heard[900]);
        // Undo drops the layer and leaves the base.
        press(&mut h, UNDO, 0.0);
        run(&mut h, 0, BAR - 100, |_| 0.0);
        let heard = run(&mut h, 0, BAR, |_| 0.0);
        assert!((heard[900] - 0.25).abs() < 1e-3, "{}", heard[900]);
    }

    #[test]
    fn a_second_pass_folds_the_first_into_the_base_scaled_by_feedback() {
        let mut h = harness();
        h.set(FEEDBACK, 0.5);
        record_bars(&mut h, 1, |_| 0.25);
        run(&mut h, 0, BAR, |_| 0.25);
        press(&mut h, RECORD, 0.0);
        run(&mut h, 0, BAR - 100, |_| 0.0);
        // Two passes: 0.125, then (stop asked for at its start) 0.1.
        run(&mut h, 0, BAR, |_| 0.125);
        press(&mut h, RECORD, 0.1);
        run(&mut h, 0, BAR - 100, |_| 0.1);
        let heard = run(&mut h, 0, BAR, |_| 0.0);
        // base 0.25 + 0.5 * 0.125, plus the last layer 0.1.
        assert!((heard[900] - 0.4125).abs() < 1e-3, "{}", heard[900]);
        press(&mut h, UNDO, 0.0);
        run(&mut h, 0, BAR - 100, |_| 0.0);
        let heard = run(&mut h, 0, BAR, |_| 0.0);
        assert!((heard[900] - 0.3125).abs() < 1e-3, "{}", heard[900]);
    }

    #[test]
    fn clear_empties() {
        let mut h = harness();
        record_bars(&mut h, 1, |_| 0.25);
        let heard = run(&mut h, BAR, BAR, |_| 0.0);
        assert!(heard[900].abs() > 0.2);
        press(&mut h, CLEAR, 0.0);
        let heard = run(&mut h, 2 * BAR, BAR, |_| 0.0);
        assert!(heard.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn reset_keeps_the_loop() {
        let mut h = harness();
        record_bars(&mut h, 1, |_| 0.25);
        run(&mut h, BAR, BAR, |_| 0.0);
        h.seek(12_345);
        let heard = run(&mut h, 2 * BAR, BAR, |_| 0.0);
        assert!((heard[900] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn the_seam_has_no_step() {
        let mut h = harness();
        // A tone that is not a whole number of cycles per bar.
        let tone = |n: usize| 0.5 * (std::f32::consts::TAU * 37.125 * n as f32 / RATE).sin();
        record_bars(&mut h, 1, tone);
        let mut heard = run(&mut h, BAR, BAR, tone);
        heard.extend(run(&mut h, 2 * BAR, 2 * BAR, |_| 0.0));
        let steepest = 0.5 * std::f32::consts::TAU * 37.125 / RATE;
        for wrap in [BAR, 2 * BAR] {
            for pair in heard[wrap - 10..wrap + 10].windows(2) {
                assert!(
                    (pair[1] - pair[0]).abs() < 2.0 * steepest,
                    "step {} -> {} at {wrap}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    #[test]
    fn mute_keeps_the_loop_running() {
        let mut h = harness();
        record_bars(&mut h, 1, |_| 0.25);
        h.set(PLAY, 0.0);
        let heard = run(&mut h, BAR, 1000, |_| 0.0);
        assert!(heard.iter().all(|x| *x == 0.0));
        h.set(PLAY, 1.0);
        let heard = run(&mut h, BAR + 1000, 1000, |_| 0.0);
        assert!((heard[500] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn the_output_is_one_voice_with_the_inputs_channels() {
        let shapes = Looper::new(&Telemetry::new())
            .output_shapes(&Config::new(), &LooperPorts::layout(), &[Shape::new(4, 2)])
            .unwrap();
        assert_eq!(shapes, vec![Shape::new(1, 2)]);
    }

    #[test]
    fn the_state_and_playhead_are_reported() {
        let telemetry = Telemetry::new();
        let mut h = Harness::new(
            &Looper::new(&telemetry),
            &Config::new(),
            &[(IN, Shape::MONO)],
            RATE,
            100,
        )
        .unwrap();
        let node = noodle_core::NodeId(0);
        run(&mut h, 0, 100, |_| 0.0);
        assert_eq!(telemetry.read_loop(node).unwrap().state, LoopState::Empty);
        h.set(RECORD, 1.0);
        run(&mut h, 0, 100, |_| 0.1);
        h.set(RECORD, 0.0);
        run(&mut h, 100, 500, |_| 0.1);
        let r = telemetry.read_loop(node).unwrap();
        assert_eq!(r.state, LoopState::Recording);
        assert!(r.at > 0.0 && r.at < 1.0, "{}", r.at);
        // Stop: the end is queued for the bar line.
        h.set(RECORD, 1.0);
        run(&mut h, 600, 100, |_| 0.1);
        h.set(RECORD, 0.0);
        assert!(telemetry.read_loop(node).unwrap().queued);
        run(&mut h, 700, BAR - 700 + 500, |_| 0.1);
        let r = telemetry.read_loop(node).unwrap();
        assert_eq!(r.state, LoopState::Playing);
        assert!(r.at > 0.1 && r.at < 0.5, "{}", r.at);
        // Overdub, then undo is offered once a pass has been written.
        press(&mut h, RECORD, 0.1);
        run(&mut h, 0, 2 * BAR, |_| 0.1);
        h.set(RECORD, 1.0);
        run(&mut h, 0, 100, |_| 0.1);
        h.set(RECORD, 0.0);
        run(&mut h, 0, 2 * BAR, |_| 0.1);
        assert!(telemetry.read_loop(node).unwrap().has_layer);
        press(&mut h, CLEAR, 0.0);
        run(&mut h, 0, 100, |_| 0.0);
        assert_eq!(telemetry.read_loop(node).unwrap().state, LoopState::Empty);
    }
}
