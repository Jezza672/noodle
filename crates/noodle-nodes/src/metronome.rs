use std::f32::consts::TAU;

use noodle_engine::{
    Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo, Setup,
    Unit,
};

/// A click on every beat while the transport plays, with a higher one on the
/// first beat of the bar. It follows the tempo map and the time signature, so
/// it stays in time through tempo changes (from the next block, as for any
/// node that reads the transport) and through seeks and loops.
///
/// Its `on` parameter is what the transport bar's metronome button drives,
/// through a [`Button`](crate::Button) node.
pub struct Metronome;

pub const METRONOME_ID: &str = "noodle.util.metronome";

/// The `on` parameter's key.
pub const METRONOME_ON: &str = "on";

const ON: usize = 0;
const LEVEL: usize = 1;
const OUT: usize = 0;

/// Ticks to a quarter note; see `noodle_core::Tick`.
const TICKS_PER_QUARTER: f64 = 960.0;
const BAR_HZ: f32 = 1_500.0;
const BEAT_HZ: f32 = 1_000.0;
/// The click's loudness before the level, relative to the bar click.
const BEAT_GAIN: f32 = 0.7;
/// How long a click takes to fall to 1/e of its start.
const DECAY_SECONDS: f32 = 0.010;
/// A click ends when it has fallen this far.
const FLOOR: f32 = 1e-4;
/// Slack in the float comparison of a beat's tick with the block's.
const EPSILON: f64 = 1e-6;

static INFO: NodeInfo = NodeInfo {
    id: METRONOME_ID,
    version: 1,
    name: "Metronome",
    category: "Generators",
};

impl NodeType for Metronome {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime()
            .param(
                METRONOME_ON,
                "On",
                ParamInfo {
                    default: 1.0,
                    ..ParamInfo::choice(["Off", "On"])
                },
            )
            .param(
                "level",
                "Level",
                ParamInfo::new(-60.0, 0.0, -12.0).unit(Unit::Decibels),
            )
            .output("out", "Out"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(Click::default()))
    }
}

#[derive(Default)]
struct Click {
    /// The tick of the beat that last started a click, so a beat on
    /// the border between two blocks sounds once.
    last_tick: Option<f64>,
    /// Where the next block should start if time is going on unbroken. A
    /// different position means a seek or a loop wrap.
    next_position: Option<u64>,
    /// A click that's sounding: its oscillator phase, frequency and envelope.
    phase: f32,
    hz: f32,
    peak: f32,
    envelope: f32,
}

impl Node for Click {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let out = io.outputs[OUT].lane_mut(0, 0);
        out.fill(0.0);
        let transport = &ctx.transport;
        let playing = transport.playing;
        if !playing || self.next_position != Some(transport.position) {
            self.last_tick = None;
        }
        self.next_position = playing.then(|| transport.position + ctx.frames as u64);

        let on = io.inputs[ON]
            .lane(0, 0)
            .first()
            .is_some_and(|&on| on >= 0.5);
        let db = io.inputs[LEVEL]
            .lane(0, 0)
            .first()
            .copied()
            .unwrap_or(-12.0);
        let level = 10f32.powf(db / 20.0);

        // The first beat to start in this block, as a frame. Beats are
        // counted from tick 0 (see the roadmap for signature changes). A
        // stopped transport starts none, but a click that's sounding rings out.
        let beat_ticks =
            TICKS_PER_QUARTER * 4.0 / f64::from(transport.signature.denominator.max(1));
        let frames_per_tick =
            60.0 * f64::from(ctx.sample_rate) / (transport.bpm.max(1.0) * TICKS_PER_QUARTER);
        let end_tick = transport.tick + ctx.frames as f64 / frames_per_tick;
        let mut beat = ((transport.tick - EPSILON) / beat_ticks).ceil() as i64;
        if self
            .last_tick
            .is_some_and(|last| beat as f64 * beat_ticks <= last + EPSILON)
        {
            beat += 1;
        }
        let beat_tick = beat as f64 * beat_ticks;
        let starts = playing && beat_tick < end_tick - EPSILON;
        let start = (starts && on).then(|| {
            let frame = ((beat_tick - transport.tick).max(0.0) * frames_per_tick) as usize;
            (frame.min(ctx.frames.saturating_sub(1)), beat)
        });
        if starts {
            self.last_tick = Some(beat_tick);
        }

        let decay = (-1.0 / (DECAY_SECONDS * ctx.sample_rate)).exp();
        let numerator = i64::from(transport.signature.numerator.max(1));
        for (i, sample) in out.iter_mut().enumerate() {
            if let Some((frame, beat)) = start
                && i == frame
            {
                let first = beat.rem_euclid(numerator) == 0;
                self.hz = if first { BAR_HZ } else { BEAT_HZ };
                self.peak = if first { 1.0 } else { BEAT_GAIN };
                self.phase = 0.0;
                self.envelope = 1.0;
            }
            if self.envelope > FLOOR {
                *sample = self.phase.sin() * self.envelope * self.peak * level;
                self.phase = (self.phase + TAU * self.hz / ctx.sample_rate).rem_euclid(TAU);
                self.envelope *= decay;
            }
        }
    }

    fn reset(&mut self) {
        self.last_tick = None;
        self.next_position = None;
        self.envelope = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::TimeSignature;
    use noodle_engine::{Shape, SignalIn, SignalOut, Transport};

    const RATE: f32 = 48_000.0;
    /// 120 bpm is half a second a beat.
    const BEAT_FRAMES: usize = 24_000;

    struct Rig {
        click: Click,
        on: bool,
        position: u64,
        signature: TimeSignature,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                click: Click::default(),
                on: true,
                position: 0,
                signature: TimeSignature::COMMON,
            }
        }

        /// Plays `frames` from the current position, as at 120 bpm.
        fn block(&mut self, frames: usize, playing: bool) -> Vec<f32> {
            let tick = self.position as f64 / (RATE as f64 * 60.0 / (120.0 * 960.0));
            let ctx = Context {
                sample_rate: RATE,
                frames,
                transport: Transport {
                    playing,
                    position: self.position,
                    tick,
                    bpm: 120.0,
                    signature: self.signature,
                    ..Transport::default()
                },
            };
            let on = vec![f32::from(self.on); frames];
            let level = vec![-12.0; frames];
            let inputs = [
                SignalIn::new(&on, Shape::MONO, frames),
                SignalIn::new(&level, Shape::MONO, frames),
            ];
            let mut data = vec![0.0; frames];
            let mut outputs = [SignalOut::new(&mut data, Shape::MONO, frames)];
            let io = Io {
                inputs: &inputs,
                outputs: &mut outputs,
                event_inputs: &[],
                event_outputs: &mut [],
            };
            self.click.process(&ctx, io);
            self.position += frames as u64;
            data
        }

        fn run(&mut self, total: usize, block: usize) -> Vec<f32> {
            let mut all = Vec::new();
            while all.len() < total {
                all.extend(self.block(block, true));
            }
            all.truncate(total);
            all
        }
    }

    /// The frames where a click starts: where silence gives way to sound.
    fn onsets(samples: &[f32]) -> Vec<usize> {
        let mut found = Vec::new();
        let mut quiet_since = 0;
        for (i, x) in samples.iter().enumerate() {
            if x.abs() > 1e-3 {
                if i - quiet_since > 2_000 || found.is_empty() {
                    found.push(i);
                }
                quiet_since = i;
            }
        }
        found
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, x| m.max(x.abs()))
    }

    #[test]
    fn clicks_on_every_beat_whatever_the_block_size() {
        for block in [64, 480, 512, 1_000, 7_777] {
            let samples = Rig::new().run(BEAT_FRAMES * 4, block);
            let clicks = onsets(&samples);
            assert_eq!(clicks.len(), 4, "block {block}: {clicks:?}");
            for (n, &at) in clicks.iter().enumerate() {
                // The sine starts at zero, so allow a few frames to rise.
                assert!(
                    at.abs_diff(n * BEAT_FRAMES) <= 8,
                    "block {block}: {clicks:?}"
                );
            }
        }
    }

    #[test]
    fn the_first_beat_of_the_bar_is_louder() {
        let samples = Rig::new().run(BEAT_FRAMES * 8, 512);
        let bar = |n: usize| peak(&samples[n * BEAT_FRAMES..(n + 1) * BEAT_FRAMES]);
        assert!(bar(0) > bar(1) * 1.2, "{} {}", bar(0), bar(1));
        assert!((bar(1) - bar(2)).abs() < 1e-3);
        assert!(bar(4) > bar(3) * 1.2);
    }

    #[test]
    fn follows_the_time_signature() {
        let mut rig = Rig::new();
        rig.signature = TimeSignature {
            numerator: 3,
            denominator: 8,
        };
        // Eighth-note beats, 12 000 frames apart at 120 bpm; the bar is three.
        let samples = rig.run(12_000 * 6, 512);
        let clicks = onsets(&samples);
        assert_eq!(clicks.len(), 6);
        let loud = |n: usize| peak(&samples[n * 12_000..(n + 1) * 12_000]);
        assert!(loud(0) > loud(1) * 1.2 && loud(3) > loud(4) * 1.2);
    }

    #[test]
    fn is_silent_when_off_or_stopped() {
        let mut rig = Rig::new();
        rig.on = false;
        assert_eq!(peak(&rig.run(BEAT_FRAMES * 2, 512)), 0.0);
        rig.on = true;
        assert_eq!(peak(&rig.block(512, false)), 0.0);
    }

    #[test]
    fn a_signature_change_to_bigger_beats_does_not_silence_the_metronome() {
        let mut rig = Rig::new();
        rig.signature = TimeSignature {
            numerator: 6,
            denominator: 8,
        };
        // Two bars of 6/8 are 12 eighths of 12 000 frames.
        rig.run(12_000 * 12, 512);
        rig.signature = TimeSignature::COMMON;
        let samples = rig.run(BEAT_FRAMES * 3, 512);
        assert!(onsets(&samples).len() >= 2, "{:?}", onsets(&samples));
    }

    #[test]
    fn stopping_lets_a_click_ring_out_and_starts_no_more() {
        let mut rig = Rig::new();
        rig.run(100, 100);
        let tail = rig.block(512, false);
        assert!(peak(&tail) > 0.0);
        assert_eq!(peak(&rig.block(BEAT_FRAMES, false)[5_000..]), 0.0);
    }

    #[test]
    fn a_seek_back_to_a_beat_clicks_again() {
        let mut rig = Rig::new();
        rig.run(BEAT_FRAMES / 2, 512);
        rig.position = 0;
        let samples = rig.block(512, true);
        assert!(peak(&samples) > 0.1);
    }
}
