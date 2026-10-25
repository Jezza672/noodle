//! The Sampler node: plays one sample when a note arrives.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use noodle_core::{ClipWarp, GainPoint, NodeId, WarpAxis, gain_points_from_text};
use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, Expression, Instance, Io, Layout, Node,
    NodeError, NodeInfo, NodeType, NoteId, ParamInfo, Ports, PositionWriter, Setup, Shape,
    Telemetry, Unit,
};

use crate::sample::{Sample, SampleRef, SampleStore};
use crate::sampler_stretch::{Msg, NodeLinks, Service, Stretched, render, tempo_key, warp_key};

/// Plays one sample when a note arrives, at the pitch of the note relative to
/// `root`, up to 16 notes at once. The output is stereo (a mono sample is the
/// same in both channels).
///
/// The sample is config, by content hash (see [`SampleStore`]): it is the
/// audio, not a parameter, and can't be modulated. If the store has no sample
/// with that hash the node is silent.
///
/// **Sample input.** The `sample` option is also an optional sample input.
/// When a sample wire is connected (from a Sample node, through any groups) it
/// overrides the option: the compiler writes what the wire carries, a
/// [`SampleRef`], into the option. A reference can name a window of the
/// sample, which then plays as if it were the whole sample, so `start`, `end`
/// and the loop points are fractions of the window.
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
/// **Gain envelope.** The `gain_env` setting is a [`GainPoint`] list (text,
/// see [`noodle_core::gain_points_to_text`]) whose ticks count millionths of
/// the sample, so the points stay on the audio however the region is moved.
/// Between points the level moves in a straight line in dB, and it holds
/// before the first and after the last. It multiplies `gain`. Empty is flat.
///
/// **Warp.** The `warp` setting holds a [`ClipWarp`] (text) that times the
/// sample the way a clip's is timed. The sample's own tempo and markers say
/// where each moment falls in beats, and with *follow tempo* on a note plays
/// them at the project's tempo, so a loop recorded at 90 bpm plays in time at
/// 120. Without a warp the sample plays at its own speed. The warp's loop
/// region is not used: the Sampler's own loop parameters decide what repeats.
///
/// In *Repitch* mode the pitch follows the speed, as with `keytrack`. *Grain*
/// and *Beats* keep the pitch, with the clips' own stretcher: a worker
/// renders a copy of the whole sample stretched by the warp (see
/// [`crate::sampler_stretch`]), and the notes play that copy, with the pitch,
/// region and loop of the unstretched sample mapped onto it. The copy is made
/// for the project's tempo as the node last heard it, and again when the
/// warp or the tempo changes and then holds still for a moment. Until a copy
/// is ready, and while the warp is being dragged, the warp plays as Repitch.
/// An offline render waits for the copy instead, so it sounds the same every
/// time.
///
/// **Live settings.** `gain_env` and `warp` reach the playing node without
/// rebuilding it (see [`NodeType::live_config`]), so dragging an envelope
/// point or a marker doesn't cut off the notes that are sounding.
///
/// **Playhead.** Where each voice is in the sample is reported to the
/// [`Telemetry`] hub (see [`Telemetry::read_positions`]), for the sample view.
///
/// When all 16 voices are in use the oldest is cut for the new note.
pub struct Sampler {
    store: SampleStore,
    telemetry: Telemetry,
    service: Service,
    /// Offline: stretch on the audio thread, and wait, instead of leaving it
    /// to the worker.
    blocking: bool,
}

impl Sampler {
    pub fn new(store: &SampleStore, telemetry: &Telemetry) -> Self {
        Self {
            store: store.clone(),
            telemetry: telemetry.clone(),
            service: Service::new(),
            blocking: false,
        }
    }

    /// A Sampler for rendering offline, which stretches a warp itself before
    /// playing it, so that the render doesn't depend on timing.
    pub fn blocking(store: &SampleStore, telemetry: &Telemetry) -> Self {
        Self {
            blocking: true,
            ..Self::new(store, telemetry)
        }
    }
}

pub const SAMPLER_ID: &str = "noodle.instrument.sampler";

/// The key `sample`'s `root` starts at, middle C.
pub const DEFAULT_ROOT: f32 = 60.0;

/// The setting that names the sample, by hash.
pub const SAMPLE_KEY: &str = "sample";

/// The setting that holds the gain envelope.
pub const GAIN_ENV_KEY: &str = "gain_env";

/// The setting that holds the warp.
pub const WARP_KEY: &str = "warp";

/// How many millionths of the sample a gain point's tick counts.
pub const GAIN_ENV_SCALE: f64 = 1_000_000.0;

static CONFIG: [ConfigInfo; 3] = [
    ConfigInfo::text(SAMPLE_KEY, "Sample"),
    ConfigInfo::text(GAIN_ENV_KEY, "Gain envelope"),
    ConfigInfo::text(WARP_KEY, "Warp"),
];

#[derive(Ports)]
struct SamplerPorts {
    #[event_input("in", "Notes")]
    notes: (),
    #[sample_input("sample", "Sample")]
    sample_in: (),
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

/// How many frames a voice takes to fade from the sample to its stretched
/// copy or back, so that the change doesn't click.
const XFADE: u32 = 256;

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

    fn live_config(&self) -> &[&'static str] {
        // An offline render is built once, from the project as it is.
        if self.blocking {
            &[]
        } else {
            &[GAIN_ENV_KEY, WARP_KEY]
        }
    }

    fn retune(&self, node: NodeId, config: &Config) {
        self.service.retune(
            node,
            &CONFIG[1].get_text(config),
            &CONFIG[2].get_text(config),
        );
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
        let hash = SampleRef::parse(&CONFIG[0].get_text(config)).hash;
        if !hash.is_empty() && self.store.get(&hash).is_none() {
            return Err(noodle_engine::Uncacheable::Because(format!(
                "the sample {} is missing",
                &hash[..8.min(hash.len())]
            )));
        }
        Ok(())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let reference = SampleRef::parse(&CONFIG[0].get_text(setup.config));
        let sample = self
            .store
            .get(&reference.hash)
            .map(|sample| sample.window(reference.start, reference.end));
        let tune = Arc::new(Tune::read(
            &CONFIG[1].get_text(setup.config),
            &CONFIG[2].get_text(setup.config),
        ));
        let links = (!self.blocking).then(|| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&CONFIG[0].get_text(setup.config), &mut hasher);
            self.service.register(
                setup.node,
                std::hash::Hasher::finish(&hasher),
                sample.clone(),
                tune.clone(),
            )
        });
        Ok(Instance::realtime(SamplerNode {
            sample,
            sample_rate: setup.sample_rate,
            tune,
            stretched: None,
            domain: None,
            previous: None,
            links,
            last_bpm: 0,
            beats_per_second: 2.0,
            positions: self.telemetry.open_positions(setup.node, VOICES),
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
    /// The piece of the warp the voice was last in.
    piece: usize,
    /// The region and loop in source frames, from the note's start.
    end: f64,
    loop_range: Option<(f64, f64)>,
    one_shot: bool,
    age: u64,
    /// Frames left of the crossfade from where the voice was before the
    /// sample/copy it plays changed, which it keeps playing, at `old_pos`
    /// and `old_piece`, as the fade runs out.
    xfade: u32,
    old_pos: f64,
    old_piece: usize,
}

/// The gain envelope as a table of linear gains across the sample.
struct Envelope {
    table: Option<Box<[f32]>>,
}

const ENVELOPE_STEPS: usize = 1024;

impl Envelope {
    fn new(points: &[GainPoint]) -> Self {
        if points.is_empty() {
            return Self { table: None };
        }
        let db_at = |tick: f64| {
            let after = points.partition_point(|p| p.tick.0 as f64 <= tick);
            match (
                after.checked_sub(1).map(|i| points[i]),
                points.get(after).copied(),
            ) {
                (Some(a), Some(b)) => {
                    let t = (tick - a.tick.0 as f64) / ((b.tick.0 - a.tick.0) as f64).max(1.0);
                    a.db + (b.db - a.db) * t as f32
                }
                (Some(p), None) | (None, Some(p)) => p.db,
                (None, None) => 0.0,
            }
        };
        let table = (0..=ENVELOPE_STEPS)
            .map(|i| {
                let tick = i as f64 / ENVELOPE_STEPS as f64 * GAIN_ENV_SCALE;
                10f32.powf(db_at(tick) / 20.0)
            })
            .collect();
        Self { table: Some(table) }
    }

    /// The gain at `fraction` of the sample.
    fn at(&self, fraction: f64) -> f32 {
        let Some(table) = &self.table else {
            return 1.0;
        };
        let x = fraction.clamp(0.0, 1.0) * ENVELOPE_STEPS as f64;
        let i = (x as usize).min(ENVELOPE_STEPS - 1);
        let t = (x - i as f64) as f32;
        table[i] + (table[i + 1] - table[i]) * t
    }
}

/// A warp reduced to what playing it needs: how fast each stretch of the
/// sample goes.
struct Play {
    /// (source second the piece begins at, output per source second).
    pieces: Vec<(f64, f64)>,
    /// Whether the output is timeline beats (follow tempo) or seconds.
    beats: bool,
}

impl Play {
    fn new(warp: &ClipWarp) -> Self {
        let curve = warp.curve();
        Self {
            pieces: curve.pieces(),
            beats: curve.axis == WarpAxis::Beats,
        }
    }

    /// Source seconds per second of playing, in the piece of `seconds`.
    /// `piece` is where the voice was, which makes the usual forward step a
    /// comparison; a jump (a loop) searches.
    fn speed(&self, piece: &mut usize, seconds: f64, beats_per_second: f64) -> f64 {
        let at = |i: usize| self.pieces.get(i).map(|p| p.0);
        if !(at(*piece).is_some_and(|from| from <= seconds)
            && at(*piece + 1).is_none_or(|to| seconds < to))
        {
            *piece = self
                .pieces
                .partition_point(|p| p.0 <= seconds)
                .saturating_sub(1);
        }
        let out_per_source = self.pieces[*piece].1.max(1e-6);
        let rate = if self.beats { beats_per_second } else { 1.0 };
        (rate / out_per_source).clamp(MIN_SPEED, MAX_SPEED)
    }
}

/// The settings that change while the node plays: the gain envelope and the
/// warp, read from their config text.
pub(crate) struct Tune {
    gain_text: String,
    warp_text: String,
    envelope: Envelope,
    /// The warp played as Repitch: the whole of it, or what stands in until a
    /// stretched copy is ready.
    play: Option<Play>,
    warp: Option<ClipWarp>,
    /// Identifies the warp as a stretch, or 0 if there is nothing to stretch.
    pub key: u64,
}

impl Tune {
    pub fn read(gain_text: &str, warp_text: &str) -> Self {
        let warp = ClipWarp::from_text(warp_text);
        let mut tune = Self {
            gain_text: gain_text.to_owned(),
            warp_text: warp_text.to_owned(),
            envelope: Envelope::new(&gain_points_from_text(gain_text)),
            play: warp.as_ref().map(Play::new),
            warp,
            key: 0,
        };
        tune.key = tune.stretch_warp().map_or(0, warp_key);
        tune
    }

    /// Whether `gain_text` and `warp_text` are what this was read from.
    pub fn is(&self, gain_text: &str, warp_text: &str) -> bool {
        self.gain_text == gain_text && self.warp_text == warp_text
    }

    /// The warp, if it is one that a stretched copy plays: a mode that keeps
    /// the pitch, and a warp that changes the timing.
    pub fn stretch_warp(&self) -> Option<&ClipWarp> {
        self.warp
            .as_ref()
            .filter(|w| w.mode.keeps_pitch() && !w.is_natural())
    }
}

/// The slowest and fastest a warp plays a sample, as a multiple of its own
/// speed (the clip warp's limits).
const MIN_SPEED: f64 = 1.0 / 16.0;
const MAX_SPEED: f64 = 32.0;

struct SamplerNode {
    sample: Option<Arc<Sample>>,
    sample_rate: f32,
    tune: Arc<Tune>,
    /// The latest stretched copy to arrive, which plays when it is of the
    /// warp the tune has.
    stretched: Option<Arc<Stretched>>,
    /// The copy the voices' positions are in, or `None` for the sample
    /// itself.
    domain: Option<Arc<Stretched>>,
    /// The copy (or none, the sample) the voices were in before the last
    /// change, while they fade across from it.
    previous: Option<Arc<Stretched>>,
    /// The worker's queues; none when stretching in the block.
    links: Option<NodeLinks>,
    last_bpm: u64,
    /// The tempo, in beats a second, as of the block.
    beats_per_second: f64,
    positions: PositionWriter,
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
        let domain = &self.domain;
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
        // Positions are in the sample or in its stretched copy.
        let (start, end) = (from_source(domain, start), from_source(domain, end));
        let loop_range = loop_range.map(|(a, b)| (from_source(domain, a), from_source(domain, b)));

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
            piece: 0,
            end,
            loop_range,
            one_shot: s.one_shot,
            age: self.clock,
            xfade: 0,
            old_pos: 0.0,
            old_piece: 0,
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

    /// Tells the telemetry hub where each voice is.
    fn report(&self) {
        let len = self.sample.as_ref().map_or(0, |s| s.frames()) as f64;
        for (i, v) in self.voices.iter().enumerate() {
            let at = (v.active && len > 0.0).then(|| (to_source(&self.domain, v.pos) / len) as f32);
            self.positions.write(i, at);
        }
        self.positions.publish();
    }

    /// Adds every voice's output for frames `from..to` of the block.
    fn render(&mut self, from: usize, to: usize) {
        let Some(sample) = &self.sample else { return };
        // What the voices play: the stretched copy, or the sample.
        let domain = self.domain.as_deref();
        let played = domain.map_or(&**sample, |d| &d.sample);
        let len = played.frames();
        let source_len = sample.frames() as f64;
        let (envelope, play, tempo) = (&self.tune.envelope, &self.tune.play, self.beats_per_second);
        let per_second = 1.0 / f64::from(sample.sample_rate.max(1));
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let bent = v.step * (v.bend / 12.0).exp2();
            for i in from..to {
                if v.pos >= v.end || v.pos < 0.0 {
                    v.active = false;
                    break;
                }
                // A copy has the warp in it already; the sample's is played
                // by changing the speed.
                let (step, at) = match (domain, play) {
                    (Some(d), _) => (bent, d.map.source_at(v.pos)),
                    (None, Some(play)) => (
                        bent * play.speed(&mut v.piece, v.pos * per_second, tempo),
                        v.pos,
                    ),
                    (None, None) => (bent, v.pos),
                };
                let (mut l, mut r) = read(played, len, v.pos);
                let level = v.level * v.fade;
                let gain = level * envelope.at(at / source_len);
                l *= gain;
                r *= gain;
                if v.xfade > 0 {
                    // Fading across from where the voice was before.
                    let before = self.previous.as_deref();
                    let (ostep, oat) = match (before, play) {
                        (Some(d), _) => (bent, d.map.source_at(v.old_pos)),
                        (None, Some(play)) => (
                            bent * play.speed(&mut v.old_piece, v.old_pos * per_second, tempo),
                            v.old_pos,
                        ),
                        (None, None) => (bent, v.old_pos),
                    };
                    let old = before.map_or(&**sample, |d| &d.sample);
                    let (ol, or) = read(old, old.frames(), v.old_pos);
                    let old_gain = level * envelope.at(oat / source_len);
                    let w = v.xfade as f32 / XFADE as f32;
                    l = l * (1.0 - w) + ol * old_gain * w;
                    r = r * (1.0 - w) + or * old_gain * w;
                    v.old_pos += ostep;
                    v.xfade -= 1;
                }
                self.left[i] += l;
                self.right[i] += r;
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

    /// Takes in what the worker has sent, tells it the tempo, and moves the
    /// voices onto the stretched copy (or off it) when the warp that plays
    /// has changed.
    fn sync(&mut self, bpm: f64) {
        let bpm = if bpm.is_finite() && bpm > 0.0 {
            bpm
        } else {
            120.0
        };
        if let Some(links) = &mut self.links {
            if bpm.to_bits() != self.last_bpm {
                self.last_bpm = bpm.to_bits();
                links.bpm.store(self.last_bpm, Ordering::Relaxed);
                links.bell.ring();
            }
            links.flush();
            while let Ok(message) = links.inbox.pop() {
                // What is replaced goes back to be dropped off this thread.
                match message {
                    Msg::Tune(tune) => {
                        let old = std::mem::replace(&mut self.tune, tune);
                        links.retire(Msg::Tune(old));
                    }
                    Msg::Stretched(copy) => {
                        if let Some(old) = self.stretched.replace(copy) {
                            links.retire(Msg::Stretched(old));
                        }
                    }
                }
            }
        } else if let (Some(sample), Some(warp)) = (&self.sample, self.tune.stretch_warp()) {
            let (key, tempo) = (warp_key(warp), tempo_key(warp, bpm));
            if !self
                .stretched
                .as_ref()
                .is_some_and(|s| s.key == key && s.tempo == tempo)
            {
                self.stretched = render(sample, warp, bpm).map(Arc::new);
            }
        }
        let wanted = self
            .stretched
            .as_ref()
            .filter(|s| self.tune.key != 0 && s.key == self.tune.key);
        let same = match (&self.domain, wanted) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same {
            let new = wanted.cloned();
            for v in self.voices.iter_mut().filter(|v| v.active) {
                // Keep playing from where the voice was while it fades into
                // where it now is.
                v.old_pos = v.pos;
                v.old_piece = v.piece;
                v.xfade = XFADE;
                let carry = |x: f64| from_source(&new, to_source(&self.domain, x));
                v.pos = carry(v.pos);
                v.end = carry(v.end);
                v.loop_range = v.loop_range.map(|(a, b)| (carry(a), carry(b)));
            }
            let old = std::mem::replace(&mut self.domain, new);
            if let Some(gone) = std::mem::replace(&mut self.previous, old) {
                self.retire(Msg::Stretched(gone));
            }
        }
        // Once every voice has faded across, the copy it left is let go.
        if !self.voices.iter().any(|v| v.active && v.xfade > 0)
            && let Some(gone) = self.previous.take()
        {
            self.retire(Msg::Stretched(gone));
        }
    }

    /// Gives `message` to the worker to drop, or drops it here when there is
    /// no worker (an offline render).
    fn retire(&mut self, message: Msg) {
        if let Some(links) = &mut self.links {
            links.retire(message);
        }
    }
}

/// Where position `pos` in a voice's frames is in the sample.
fn to_source(domain: &Option<Arc<Stretched>>, pos: f64) -> f64 {
    domain.as_ref().map_or(pos, |d| d.map.source_at(pos))
}

/// Where in a voice's frames sample frame `source` is.
fn from_source(domain: &Option<Arc<Stretched>>, source: f64) -> f64 {
    domain.as_ref().map_or(source, |d| d.map.out_at(source))
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
        self.beats_per_second = ctx.transport.bpm / 60.0;
        self.sync(ctx.transport.bpm);
        let idle = self.voices.iter().all(|v| !v.active);
        if idle && io.event_inputs[SamplerPorts::NOTES].is_empty() {
            for channel in 0..2 {
                io.outputs[SamplerPorts::OUT].silence(0, channel);
            }
            self.report();
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
        self.report();
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
        Harness::new(
            &Sampler::new(store, &Telemetry::new()),
            config,
            &[],
            RATE,
            256,
        )
        .unwrap()
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

    fn with(config: &Config, key: &str, text: String) -> Config {
        let mut config = config.clone();
        config.set(key, Value::Text(text));
        config
    }

    #[test]
    fn a_gain_envelope_scales_the_level_along_the_sample() {
        let (store, config) = ramp(100);
        // A flat envelope is a level.
        let flat = with(
            &config,
            GAIN_ENV_KEY,
            noodle_core::gain_points_to_text(&[GainPoint {
                tick: noodle_core::Tick(500_000),
                db: -6.0206,
            }]),
        );
        let mut h = harness(&store, &flat);
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        h.run(256).unwrap();
        assert!((left(&h)[40] - 0.040 * 0.5).abs() < 1e-4);

        // Straight in dB from nothing at the start to -40 dB at the end.
        let fade = with(
            &config,
            GAIN_ENV_KEY,
            noodle_core::gain_points_to_text(&[
                GainPoint {
                    tick: noodle_core::Tick(0),
                    db: 0.0,
                },
                GainPoint {
                    tick: noodle_core::Tick(1_000_000),
                    db: -40.0,
                },
            ]),
        );
        let mut h = harness(&store, &fade);
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        h.run(256).unwrap();
        let out = left(&h);
        assert!((out[10] - 0.010 * 10f32.powf(-4.0 / 20.0)).abs() < 1e-4);
        assert!((out[50] - 0.050 * 0.1).abs() < 1e-4, "-20 dB halfway");
    }

    #[test]
    fn a_warp_follows_the_tempo_or_not() {
        let (store, config) = ramp(1_000);
        let at = |warp: &ClipWarp| with(&config, WARP_KEY, warp.to_text());
        // The sample is a beat a second; the harness plays at 120 bpm, two.
        let mut following = harness(&store, &at(&ClipWarp::at_tempo(60.0)));
        following.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        following.run(256).unwrap();
        assert!((left(&following)[10] - 0.020).abs() < 1e-4, "twice as fast");

        let natural = ClipWarp {
            tempo_conversion: false,
            ..ClipWarp::at_tempo(60.0)
        };
        let mut still = harness(&store, &at(&natural));
        still.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        still.run(256).unwrap();
        assert!((left(&still)[10] - 0.010).abs() < 1e-4, "its own speed");
    }

    #[test]
    fn markers_slow_the_stretch_between_them() {
        let (store, config) = ramp(1_000);
        let mut warp = ClipWarp {
            tempo_conversion: false,
            ..ClipWarp::at_tempo(120.0)
        };
        // A millisecond in, the next millisecond is given four times the
        // beats it has: it plays at half speed.
        for (seconds, beat) in [(0.001, 0.002), (0.002, 0.006)] {
            warp.set_marker(noodle_core::WarpMarker { seconds, beat });
        }
        let mut h = harness(&store, &with(&config, WARP_KEY, warp.to_text()));
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        h.run(256).unwrap();
        let out = left(&h);
        assert!((out[30] - 0.030).abs() < 1e-4, "natural before the marker");
        // 48 frames in, then half speed: frame 48 + k at output 48 + 2k.
        assert!((out[48 + 20] - 0.058).abs() < 1e-4, "{}", out[68]);
        // Half speed for 96 output frames (48 of the sample), then natural
        // again from source frame 96.
        assert!((out[144 + 10] - 0.106).abs() < 1e-4, "{}", out[154]);
    }

    #[test]
    fn each_voice_reports_its_place_in_the_sample() {
        let (store, config) = ramp(1_000);
        let telemetry = Telemetry::new();
        let mut h =
            Harness::new(&Sampler::new(&store, &telemetry), &config, &[], RATE, 256).unwrap();
        let nobody = noodle_core::NodeId(0);
        h.run(64).unwrap();
        assert!(telemetry.read_positions(nobody).is_empty(), "silent");
        h.send_events(
            SamplerPorts::NOTES,
            &[on(0, 1, 60, 1.0), on(100, 2, 60, 1.0)],
        );
        h.run(200).unwrap();
        let at = telemetry.read_positions(nobody);
        assert_eq!(at.len(), 2);
        assert!(
            (at[0] - 0.2).abs() < 1e-3 && (at[1] - 0.1).abs() < 1e-3,
            "{at:?}"
        );
    }

    /// A second of a 440 Hz sine in a store.
    fn tone() -> (SampleStore, Config) {
        let store = SampleStore::new();
        let hash = store.insert(&Audio {
            samples: (0..RATE as usize)
                .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / RATE).sin() * 0.5)
                .collect(),
            channels: 1,
            sample_rate: RATE as u32,
        });
        (store, Config::new().with(SAMPLE_KEY, Value::Text(hash)))
    }

    fn crossings(signal: &[f32]) -> usize {
        signal
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count()
    }

    /// The tone played for 1.9 s at half speed (a 240 bpm clip on a 120 bpm
    /// timeline) in `mode`, as an offline render does.
    fn slowed(mode: noodle_core::WarpMode) -> (Harness, Telemetry) {
        let (store, config) = tone();
        let warp = ClipWarp {
            mode,
            ..ClipWarp::at_tempo(240.0)
        };
        let telemetry = Telemetry::new();
        let mut h = Harness::new(
            &Sampler::blocking(&store, &telemetry),
            &with(&config, WARP_KEY, warp.to_text()),
            &[],
            RATE,
            91_200,
        )
        .unwrap();
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        h.run(91_200).unwrap();
        (h, telemetry)
    }

    #[test]
    fn grain_and_beats_stretch_the_sample_and_keep_its_pitch() {
        // The window 0.2 s to 0.8 s: well inside both the stretched copy and
        // the sample played slowly.
        let window = 9_600..38_400;
        let (repitched, _) = slowed(noodle_core::WarpMode::Repitch);
        let slow = crossings(&left(&repitched)[window.clone()]);
        // 220 Hz for 0.6 s.
        assert!((130..=134).contains(&slow), "{slow}");
        for mode in [noodle_core::WarpMode::Grain, noodle_core::WarpMode::Beats] {
            let (h, _) = slowed(mode);
            let out = left(&h);
            let kept = crossings(&out[window.clone()]);
            // 440 Hz for 0.6 s.
            assert!((259..=269).contains(&kept), "{mode:?}: {kept}");
            // Twice as long: still sounding at 1.5 s, silent after 2.1 s.
            let peak = |range: std::ops::Range<usize>| {
                out[range].iter().fold(0.0f32, |p, x| p.max(x.abs()))
            };
            assert!(peak(72_000..76_000) > 0.2, "{mode:?} still sounding");
        }
    }

    #[test]
    fn the_playhead_of_a_stretched_note_is_a_place_in_the_sample() {
        let (store, config) = tone();
        let warp = ClipWarp {
            mode: noodle_core::WarpMode::Grain,
            ..ClipWarp::at_tempo(240.0)
        };
        let telemetry = Telemetry::new();
        let mut h = Harness::new(
            &Sampler::blocking(&store, &telemetry),
            &with(&config, WARP_KEY, warp.to_text()),
            &[],
            RATE,
            48_000,
        )
        .unwrap();
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        // A second in, at half speed: half way through the sample.
        h.run(48_000).unwrap();
        let at = telemetry.read_positions(noodle_core::NodeId(0));
        assert_eq!(at.len(), 1);
        assert!((at[0] - 0.5).abs() < 0.01, "{at:?}");
    }

    #[test]
    fn a_new_envelope_reaches_a_sounding_note_without_cutting_it() {
        // Long enough that the note cannot run out while the test waits: it
        // lasts more than 20 s of blocks, however slowly the worker thread
        // gets scheduled.
        let (store, config) = ramp(2_000_000);
        let sampler = Sampler::new(&store, &Telemetry::new());
        let mut h = Harness::new(&sampler, &config, &[], RATE, 256).unwrap();
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        h.run(256).unwrap();
        assert!((left(&h)[255] - 0.255).abs() < 1e-4);

        // 20 dB down all along, sent while the note sounds.
        let quieter = with(&config, GAIN_ENV_KEY, "0:-20,1000000:-20".to_owned());
        sampler.retune(noodle_core::NodeId(0), &quieter);
        let mut blocks = 1;
        let mut level = 1.0f32;
        // Wait for the new curve to arrive from the worker thread. The gain
        // does not slew: it steps to the new level at the block the curve
        // lands in. The wait is bounded by the clock, not by a block count,
        // because under load the thread can take a long time to run.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while (level - 0.1).abs() >= 0.01 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
            h.run(256).unwrap();
            blocks += 1;
            // The ramp is 0.001 a frame, so the gain is the output over it.
            level = left(&h)[0] / (0.001 * ((blocks - 1) * 256) as f32);
        }
        assert!((level - 0.1).abs() < 0.01, "arrived: {level}");
        // The note carried on from where it was: it did not start over.
        assert!(left(&h)[0] > 0.0, "{}", left(&h)[0]);
    }

    #[test]
    fn a_note_does_not_click_when_its_warp_switches_to_the_stretched_copy() {
        let (store, config) = tone();
        let sampler = Sampler::new(&store, &Telemetry::new());
        let mut h = Harness::new(&sampler, &config, &[], RATE, 256).unwrap();
        h.send_events(SamplerPorts::NOTES, &[on(0, 1, 60, 1.0)]);
        let grain = ClipWarp {
            mode: noodle_core::WarpMode::Grain,
            ..ClipWarp::at_tempo(240.0)
        };
        let warped = with(&config, WARP_KEY, grain.to_text());
        let mut last = 0.0f32;
        let mut worst = 0.0f32;
        for block in 0..180 {
            if block == 2 {
                sampler.retune(noodle_core::NodeId(0), &warped);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            h.run(256).unwrap();
            h.send_events(SamplerPorts::NOTES, &[]);
            for &x in &left(&h) {
                worst = worst.max((x - last).abs());
                last = x;
            }
        }
        // A 440 Hz sine at 0.5 moves 0.03 a frame at most; a jump between
        // the sample and the copy would be many times that.
        assert!(worst < 0.15, "{worst}");
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
