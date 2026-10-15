//! The Sampler node: plays one sample when a note arrives.

use std::sync::Arc;

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Expression, Instance, Io, Layout, Node,
    NodeError, NodeInfo, NodeType, NoteId, ParamInfo, Ports, Setup, Shape, Unit,
};

use crate::sample::{Sample, SampleStore};

/// Plays one sample when a note arrives, at the pitch of the note relative to
/// `root`, up to 16 notes at once. The output is stereo (a mono sample is the
/// same in both channels).
///
/// The sample is config, by content hash (see [`SampleStore`]): it is the
/// audio, not a parameter, and can't be modulated. If the store has no sample
/// with that hash the node is silent.
///
/// **Region.** `start` and `end` are fractions of the sample. A note plays
/// from `start` to `end`.
///
/// **Mode.** *One-shot* plays the region to its end whatever the note-off
/// does, which is how drums behave. *Gate* plays while the note is held and
/// fades out over `release` after the note-off, and with `loop` on it repeats
/// between `loop_start` and `loop_end` while held. In one-shot mode the loop
/// does nothing, since nothing would ever end the note.
///
/// **Pitch.** With `keytrack` on, a note plays `root` at the sample's own
/// pitch and other keys scale it by semitones; `tune` adds cents. With it
/// off every note plays at the sample's pitch (a drum sample), and `tune`
/// still applies. A pitch expression bends a playing note in semitones.
///
/// **Quality.** The sample is read with four-point cubic (Catmull-Rom)
/// interpolation, always. It is in memory in full.
///
/// **Level.** A note's velocity scales it linearly, then `gain`.
///
/// When all 16 voices are in use the oldest is cut for the new note.
pub struct Sampler {
    store: SampleStore,
}

impl Sampler {
    pub fn new(store: &SampleStore) -> Self {
        Self {
            store: store.clone(),
        }
    }
}

pub const SAMPLER_ID: &str = "noodle.instrument.sampler";

/// The key `sample`'s `root` starts at, middle C.
pub const DEFAULT_ROOT: f32 = 60.0;

/// The setting that names the sample, by hash.
pub const SAMPLE_KEY: &str = "sample";

static CONFIG: [ConfigInfo; 1] = [ConfigInfo::text(SAMPLE_KEY, "Sample")];

#[derive(Ports)]
struct SamplerPorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[param("root", "Root", ParamInfo::new(0.0, 127.0, DEFAULT_ROOT).smoothing(0.0))]
    root: (),
    #[param(
        "tune",
        "Tune",
        ParamInfo::new(-1_200.0, 1_200.0, 0.0).unit(Unit::Cents).offset().featured()
    )]
    tune: (),
    #[param("keytrack", "Keytrack", ParamInfo::choice(["Off", "On"]).default_to(1.0))]
    keytrack: (),
    #[param("start", "Start", ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0).featured())]
    start: (),
    #[param("end", "End", ParamInfo::new(0.0, 1.0, 1.0).smoothing(0.0).featured())]
    end: (),
    #[param("mode", "Mode", ParamInfo::choice(["One-shot", "Gate"]))]
    mode: (),
    #[param("loop", "Loop", ParamInfo::choice(["Off", "On"]))]
    looping: (),
    #[param("loop_start", "Loop start", ParamInfo::new(0.0, 1.0, 0.0).smoothing(0.0))]
    loop_start: (),
    #[param("loop_end", "Loop end", ParamInfo::new(0.0, 1.0, 1.0).smoothing(0.0))]
    loop_end: (),
    #[param(
        "release",
        "Release",
        ParamInfo::new(0.001, 5.0, 0.02).log().unit(Unit::Seconds).smoothing(0.0)
    )]
    release: (),
    #[param(
        "gain",
        "Gain",
        ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels).offset().featured()
    )]
    gain: (),
    #[output("out", "Out")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: SAMPLER_ID,
    version: 1,
    name: "Sampler",
    category: "Instruments",
};

const VOICES: usize = 16;

impl NodeType for Sampler {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(SamplerPorts::layout())
    }

    fn output_shapes(
        &self,
        _config: &Config,
        layout: &Layout,
        _inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        Ok(vec![Shape::STEREO; layout.outputs.len()])
    }

    fn cache_inputs(
        &self,
        _node: noodle_core::NodeId,
        config: &Config,
        _env: &noodle_engine::CacheEnv<'_>,
        _key: &mut noodle_core::KeyBuilder,
    ) -> Result<(), noodle_engine::Uncacheable> {
        // The hash in the config keys the sound. A sample that can't be found
        // renders as silence, and must not be remembered as the answer.
        let hash = CONFIG[0].get_text(config);
        if !hash.is_empty() && self.store.get(&hash).is_none() {
            return Err(noodle_engine::Uncacheable::Because(format!(
                "the sample {} is missing",
                &hash[..8.min(hash.len())]
            )));
        }
        Ok(())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let hash = CONFIG[0].get_text(setup.config);
        let sample = self.store.get(&hash);
        Ok(Instance::realtime(SamplerNode {
            sample,
            sample_rate: setup.sample_rate,
            voices: [Voice::default(); VOICES],
            clock: 0,
            left: vec![0.0; setup.max_frames].into_boxed_slice(),
            right: vec![0.0; setup.max_frames].into_boxed_slice(),
        }))
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    note: u32,
    /// Position in the sample, in its own frames.
    pos: f64,
    /// Source frames per output frame, before any bend.
    step: f64,
    bend: f64,
    level: f32,
    held: bool,
    /// The note-off has come, in gate mode: fading out.
    releasing: bool,
    /// Fade gain, 1 until the release.
    fade: f32,
    fade_step: f32,
    /// The region and loop in source frames, from the note's start.
    end: f64,
    loop_range: Option<(f64, f64)>,
    one_shot: bool,
    age: u64,
}

struct SamplerNode {
    sample: Option<Arc<Sample>>,
    sample_rate: f32,
    voices: [Voice; VOICES],
    clock: u64,
    left: Box<[f32]>,
    right: Box<[f32]>,
}

/// The settings of the block, read once at its start.
struct Settings {
    root: f32,
    tune: f32,
    keytrack: bool,
    start: f32,
    end: f32,
    one_shot: bool,
    looping: bool,
    loop_start: f32,
    loop_end: f32,
    release: f32,
    gain: f32,
}

impl Settings {
    fn read(io: &Io<'_, '_>) -> Self {
        let get = |port: usize| {
            let x = io.inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
            if x.is_finite() { x } else { 0.0 }
        };
        Self {
            root: get(SamplerPorts::ROOT),
            tune: get(SamplerPorts::TUNE),
            keytrack: get(SamplerPorts::KEYTRACK) >= 0.5,
            start: get(SamplerPorts::START).clamp(0.0, 1.0),
            end: get(SamplerPorts::END).clamp(0.0, 1.0),
            one_shot: get(SamplerPorts::MODE) < 0.5,
            looping: get(SamplerPorts::LOOPING) >= 0.5,
            loop_start: get(SamplerPorts::LOOP_START).clamp(0.0, 1.0),
            loop_end: get(SamplerPorts::LOOP_END).clamp(0.0, 1.0),
            release: get(SamplerPorts::RELEASE).max(0.001),
            gain: 10f32.powf(get(SamplerPorts::GAIN) / 20.0),
        }
    }
}

impl SamplerNode {
    fn note_on(&mut self, s: &Settings, id: NoteId, key: u8, velocity: f32) {
        let Some(sample) = &self.sample else { return };
        let frames = sample.frames() as f64;
        let start = f64::from(s.start) * frames;
        let end = (f64::from(s.end) * frames).max(start);
        if end - start < 1.0 {
            return;
        }
        let semitones = if s.keytrack {
            f64::from(key) - f64::from(s.root)
        } else {
            0.0
        } + f64::from(s.tune) / 100.0;
        let step =
            f64::from(sample.sample_rate) / f64::from(self.sample_rate) * (semitones / 12.0).exp2();
        let loop_range = (s.looping && !s.one_shot).then(|| {
            let a = f64::from(s.loop_start) * frames;
            let b = f64::from(s.loop_end) * frames;
            // A loop must lie in the region and be a few samples long.
            let (a, b) = (a.max(start), b.min(end));
            (a, b)
        });
        let loop_range = loop_range.filter(|&(a, b)| b - a >= 2.0);

        // A free voice, else the oldest.
        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map_or(0, |(i, _)| i)
            });
        self.clock += 1;
        self.voices[slot] = Voice {
            active: true,
            note: id.0,
            pos: start,
            step,
            bend: 0.0,
            level: velocity.clamp(0.0, 1.0) * s.gain,
            held: true,
            releasing: false,
            fade: 1.0,
            fade_step: 0.0,
            end,
            loop_range,
            one_shot: s.one_shot,
            age: self.clock,
        };
    }

    fn note_off(&mut self, s: &Settings, id: NoteId) {
        for v in self.voices.iter_mut().filter(|v| v.active && v.held) {
            if v.note == id.0 {
                v.held = false;
                if !v.one_shot {
                    v.releasing = true;
                    v.fade_step = 1.0 / (s.release * self.sample_rate).max(1.0);
                }
            }
        }
    }

    fn apply(&mut self, s: &Settings, event: &Event) {
        match event.kind {
            EventKind::NoteOn {
                note,
                key,
                velocity,
                ..
            } => self.note_on(s, note, key, velocity),
            EventKind::NoteOff { note, .. } => self.note_off(s, note),
            EventKind::Expression {
                note,
                expression: Expression::Pitch,
                value,
            } => {
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.active && v.note == note.0)
                {
                    v.bend = f64::from(value);
                }
            }
            _ => {}
        }
    }

    /// Adds every voice's output for frames `from..to` of the block.
    fn render(&mut self, from: usize, to: usize) {
        let Some(sample) = &self.sample else { return };
        let len = sample.frames();
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let step = v.step * (v.bend / 12.0).exp2();
            for i in from..to {
                if v.pos >= v.end || v.pos < 0.0 {
                    v.active = false;
                    break;
                }
                let (l, r) = read(sample, len, v.pos);
                let gain = v.level * v.fade;
                self.left[i] += l * gain;
                self.right[i] += r * gain;
                v.pos += step;
                if let Some((a, b)) = v.loop_range
                    && v.held
                    && v.pos >= b
                {
                    v.pos = a + (v.pos - b) % (b - a);
                }
                if v.releasing {
                    v.fade -= v.fade_step;
                    if v.fade <= 0.0 {
                        v.active = false;
                        break;
                    }
                }
            }
        }
    }
}

/// The sample at fractional frame `pos`, by Catmull-Rom interpolation.
fn read(sample: &Sample, len: usize, pos: f64) -> (f32, f32) {
    let i = pos as usize;
    let t = (pos - i as f64) as f32;
    let at = |data: &[f32], k: isize| -> f32 {
        let k = i as isize + k;
        if k < 0 || k as usize >= len {
            0.0
        } else {
            data[k as usize]
        }
    };
    let cubic = |data: &[f32]| {
        let (a, b, c, d) = (at(data, -1), at(data, 0), at(data, 1), at(data, 2));
        0.5 * ((2.0 * b)
            + (-a + c) * t
            + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t
            + (-a + 3.0 * b - 3.0 * c + d) * t * t * t)
    };
    (cubic(&sample.left), cubic(&sample.right))
}

impl Node for SamplerNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let settings = Settings::read(&io);
        let frames = ctx.frames;
        let idle = self.voices.iter().all(|v| !v.active);
        if idle && io.event_inputs[SamplerPorts::NOTES].is_empty() {
            for channel in 0..2 {
                io.outputs[SamplerPorts::OUT].silence(0, channel);
            }
            return;
        }
        self.left[..frames].fill(0.0);
        self.right[..frames].fill(0.0);
        let mut at = 0;
        for event in io.event_inputs[SamplerPorts::NOTES] {
            let time = (event.time as usize).min(frames);
            self.render(at, time);
            at = time;
            self.apply(&settings, event);
        }
        self.render(at, frames);
        let out = &mut io.outputs[SamplerPorts::OUT];
        out.lane_mut(0, 0).copy_from_slice(&self.left[..frames]);
        out.lane_mut(0, 1).copy_from_slice(&self.right[..frames]);
    }

    fn reset(&mut self) {
        self.voices = [Voice::default(); VOICES];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;
    use noodle_io::Audio;

    const OUT: usize = SamplerPorts::OUT;
    const RATE: f32 = 48_000.0;

    fn on(time: u32, id: u32, key: u8, velocity: f32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity,
            },
        }
    }

    fn off(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        }
    }

    /// A ramp from 0 up by 0.001 a frame, `frames` long, in a store.
    fn ramp(frames: usize) -> (SampleStore, Config) {
        let store = SampleStore::new();
        let samples = (0..frames).map(|i| i as f32 * 0.001).collect();
        let hash = store.insert(&Audio {
            samples,
            channels: 1,
            sample_rate: RATE as u32,
        });
        let mut config = Config::new();
        config.set(SAMPLE_KEY, Value::Text(hash));
        (store, config)
    }

    fn harness(store: &SampleStore, config: &Config) -> Harness {
        Harness::new(&Sampler::new(store), config, &[], RATE, 256).unwrap()
    }

    fn left(h: &Harness) -> Vec<f32> {
        h.output(OUT).lane(0, 0).to_vec()
    }

    #[test]
    fn a_note_plays_the_sample_from_the_start() {
        let (store, config) = ramp(100);
        let mut h = harness(&store, &config);
        h.send_events(SamplerPorts::NOTES, &[on(10, 1, 60, 1.0)]);
        h.run(256).unwrap();
        let out = left(&h);
        assert!(out[..10].iter().all(|&x| x == 0.0));
        for k in 1..99 {
            assert!((out[10 + k] - k as f32 * 0.001).abs() < 1e-5, "{k}");
        }
        assert!(out[120..].iter().all(|&x| x == 0.0), "ends at the end");
        assert_eq!(h.output(OUT).lane(0, 1), h.output(OUT).lane(0, 0));
    }

    #[test]
    fn an_octave_up_plays_twice_as_fast() {
        let (store, config) = ramp(100);
        let mut h = harness(&store, &config);
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 72, 1.0)]);
        h.run(256).unwrap();
        let out = left(&h);
        assert!((out[10] - 0.020).abs() < 1e-4, "{}", out[10]);
        assert!(out[60..].iter().all(|&x| x == 0.0), "half as long");
    }

    #[test]
    fn keytrack_off_ignores_the_key() {
        let (store, config) = ramp(100);
        let mut h = harness(&store, &config);
        h.set(SamplerPorts::KEYTRACK, 0.0);
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 90, 1.0)]);
        h.run(256).unwrap();
        assert!((left(&h)[10] - 0.010).abs() < 1e-5);
    }

    #[test]
    fn velocity_and_gain_scale_the_level() {
        let (store, config) = ramp(100);
        let mut h = harness(&store, &config);
        h.set(SamplerPorts::GAIN, -6.0206);
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 0.5)]);
        h.run(256).unwrap();
        assert!((left(&h)[40] - 0.040 * 0.25).abs() < 1e-5);
    }

    #[test]
    fn one_shot_ignores_the_note_off_and_gate_fades_out_after_it() {
        let (store, config) = ramp(200);
        let mut one_shot = harness(&store, &config);
        one_shot.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0), off(20, 1)]);
        one_shot.run(256).unwrap();
        assert!((left(&one_shot)[100] - 0.100).abs() < 1e-5);

        let mut gate = harness(&store, &config);
        gate.set(SamplerPorts::MODE, 1.0);
        gate.set(SamplerPorts::RELEASE, 0.001);
        gate.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0), off(20, 1)]);
        gate.run(256).unwrap();
        let out = left(&gate);
        assert!((out[19] - 0.019).abs() < 1e-5);
        assert!(out[20] > 0.0 && out[20] < 0.021);
        assert!(out[80..].iter().all(|&x| x == 0.0), "the release ended");
    }

    #[test]
    fn a_held_note_loops_between_the_loop_points() {
        let (store, config) = ramp(100);
        let mut h = harness(&store, &config);
        h.set(SamplerPorts::MODE, 1.0);
        h.set(SamplerPorts::LOOPING, 1.0);
        h.set(SamplerPorts::LOOP_START, 0.2);
        h.set(SamplerPorts::LOOP_END, 0.4);
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        h.run(256).unwrap();
        let out = left(&h);
        // 40 is the loop end; the position wraps to 20, so it never passes 0.04.
        assert!(out[..256].iter().all(|&x| x < 0.0401), "stays in the loop");
        assert!(out[255] > 0.019, "still sounding at the end");
    }

    #[test]
    fn start_and_end_pick_the_region() {
        let (store, config) = ramp(100);
        let mut h = harness(&store, &config);
        h.set(SamplerPorts::START, 0.5);
        h.set(SamplerPorts::END, 0.6);
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        h.run(256).unwrap();
        let out = left(&h);
        assert!((out[0] - 0.050).abs() < 1e-5);
        assert!(out[12..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn a_missing_sample_is_silence() {
        let mut config = Config::new();
        config.set(SAMPLE_KEY, Value::Text("0".repeat(64)));
        let mut h = harness(&SampleStore::new(), &config);
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        h.run(256).unwrap();
        assert!(left(&h).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn a_seventeenth_note_cuts_the_oldest() {
        let (store, config) = ramp(10_000);
        let mut h = harness(&store, &config);
        let notes: Vec<_> = (0..17).map(|i| on(i, i, 60, 0.01)).collect();
        h.send_events(SamplerPorts::NOTES, &notes);
        h.run(256).unwrap();
        // No panic, and all sixteen newest are mixed in.
        assert!(left(&h)[255] > 0.0);
    }
}
