//! The Grains node: a sample played as a cloud of short, windowed pieces.

use std::f32::consts::{FRAC_PI_4, PI, SQRT_2};
use std::sync::Arc;

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, Ports, Setup, Shape, Unit,
};

use crate::sample::{Sample, SampleRef, SampleStore};

pub const GRAINS_ID: &str = "noodle.instrument.grains";

/// The setting (and sample input) that names the sample, by hash.
pub const SAMPLE_KEY: &str = "sample";

/// The setting that holds how many grains one cloud plays at once.
pub const GRAINS_KEY: &str = "grains";

/// The most grains one cloud can play at once: the size of its fixed array.
pub const MAX_GRAINS: usize = 64;
pub const MIN_GRAINS: usize = 4;
pub const DEFAULT_GRAINS: usize = 24;

static CONFIG: [ConfigInfo; 2] = [
    ConfigInfo::text(SAMPLE_KEY, "Sample"),
    ConfigInfo::int(GRAINS_KEY, "Grains", DEFAULT_GRAINS as i64),
];

#[derive(Ports)]
#[ports(cost = 40)]
struct GrainsPorts {
    #[sample_input("sample", "Sample")]
    sample: (),
    #[param("gate", "Gate", ParamInfo::new(0.0, 1.0, 1.0).smoothing(0.0))]
    gate: (),
    #[param(
        "position",
        "Position",
        ParamInfo::new(0.0, 1.0, 0.0).offset().featured()
    )]
    position: (),
    #[param("spray", "Spray", ParamInfo::new(0.0, 1.0, 0.0).featured())]
    spray: (),
    #[param(
        "size",
        "Size",
        ParamInfo::new(0.005, 1.0, 0.1)
            .log()
            .unit(Unit::Seconds)
            .featured()
    )]
    size: (),
    #[param(
        "density",
        "Density",
        ParamInfo::new(0.5, 200.0, 20.0).log().featured()
    )]
    density: (),
    #[param("jitter", "Jitter", ParamInfo::new(0.0, 1.0, 0.0))]
    jitter: (),
    #[param(
        "pitch",
        "Pitch",
        ParamInfo::new(-48.0, 48.0, 0.0)
            .unit(Unit::Semitones)
            .offset()
    )]
    pitch: (),
    #[param(
        "frequency",
        "Frequency",
        ParamInfo::new(0.0, 20_000.0, 0.0).unit(Unit::Hertz)
    )]
    frequency: (),
    #[param("root", "Root", ParamInfo::new(0.0, 127.0, 60.0).smoothing(0.0))]
    root: (),
    #[param(
        "pitch_spray",
        "Pitch spray",
        ParamInfo::new(0.0, 24.0, 0.0).unit(Unit::Semitones)
    )]
    pitch_spray: (),
    #[param("reverse", "Reverse", ParamInfo::new(0.0, 1.0, 0.0))]
    reverse: (),
    #[param("spread", "Spread", ParamInfo::new(0.0, 1.0, 0.0))]
    spread: (),
    #[param("shape", "Shape", ParamInfo::new(0.0, 1.0, 0.0))]
    shape: (),
    #[param("seed", "Seed", ParamInfo::new(0.0, 999.0, 0.0).smoothing(0.0))]
    seed: (),
    #[output("out", "Out")]
    out: (),
    #[output("playhead", "Playhead")]
    playhead: (),
}

static INFO: NodeInfo = NodeInfo {
    id: GRAINS_ID,
    version: 1,
    name: "Grains",
    category: "Instruments",
};

/// A granular sample player. It plays the sample as a cloud of short,
/// windowed pieces (grains). Where in the sample the grains come from, how
/// long they are, how many start a second, their pitch and how much each of
/// those is scattered are all parameters, so each can be driven by a wire.
/// Holding `position` still freezes a sound, sweeping it slowly stretches it,
/// and `spray` scatters it into a texture.
///
/// The node is driven by signals, not notes, so it sits behind a Voices node
/// like an oscillator and runs one cloud per voice. The output is stereo.
///
/// - `gate`: grains start only while it is high (above one half); grains
///   already playing finish. Unwired it is 1, so the node drones.
/// - `position`: where grains start, as a fraction of the sample (or of the
///   window, when the sample wire carries one). `spray` spreads each grain's
///   start randomly around it, `spray` of 1 over the whole sample.
/// - `size` is a grain's length; `density` is how many start a second;
///   `jitter` 0 starts them on a regular clock and 1 at random times with
///   the same mean.
/// - `pitch` is each grain's playback rate in semitones. `frequency`, when
///   wired above 0, adds the distance from `root` (a key) to that frequency,
///   so the cloud follows a Voices node's pitch. `pitch_spray` detunes each
///   grain by a random amount up to that many semitones either way.
/// - `reverse` is the chance a grain plays backwards; `spread` scatters grains
///   across the stereo field.
/// - `shape` is the window, from a Hann bell (0) to a near-rectangle with
///   short ramps (1).
/// - `playhead` is the position in force, for a display and for wiring on.
///
/// **Config.** `grains` (4 to 64, default 24) is the most grains one cloud
/// plays at once. When all are busy a new grain is skipped, never stolen, so a
/// dense cloud thins out and doesn't click.
///
/// **Random and repeatable.** Each cloud has its own generator, seeded from
/// the node's seed, the `seed` parameter and the voice number, started again
/// by `reset`, and advanced only when a grain is due, so an offline render is
/// the same every time.
///
/// **Level.** Overlapping grains add up, so the output is scaled by
/// `1 / sqrt(max(1, density x size))`: turning the density up thickens the
/// sound without a large jump in level.
///
/// **Real-time rules.** A grain reads the sample, held whole in memory, with
/// linear interpolation. Nothing allocates: the grains are an array in the
/// cloud.
pub struct Grains {
    store: SampleStore,
}

impl Grains {
    pub fn new(store: &SampleStore) -> Self {
        Self {
            store: store.clone(),
        }
    }
}

/// The most grains the config asks for.
pub fn grain_limit(config: &Config) -> usize {
    (CONFIG[1].get_int(config).max(0) as usize).clamp(MIN_GRAINS, MAX_GRAINS)
}

impl NodeType for Grains {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(GrainsPorts::layout())
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        // One cloud per voice; the sound is stereo whatever the inputs'
        // channels are, and the playhead has one channel.
        let voices = Shape::broadcast_all(inputs.iter().map(|s| Shape::new(s.voices, 1)))?.voices;
        Ok(vec![Shape::new(voices, 2), Shape::new(voices, 1)])
    }

    fn cache_inputs(
        &self,
        _node: noodle_core::NodeId,
        config: &Config,
        _env: &noodle_engine::CacheEnv<'_>,
        _key: &mut noodle_core::KeyBuilder,
    ) -> Result<(), noodle_engine::Uncacheable> {
        let reference = SampleRef::parse(&CONFIG[0].get_text(config));
        if !reference.hash.is_empty() && self.store.get(&reference.hash).is_none() {
            return Err(noodle_engine::Uncacheable::Because(format!(
                "the sample {} is missing",
                &reference.hash[..8.min(reference.hash.len())]
            )));
        }
        Ok(())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let reference = SampleRef::parse(&CONFIG[0].get_text(setup.config));
        let sample = self
            .store
            .get(&reference.hash)
            .map(|sample| sample.window(reference.start, reference.end))
            .filter(|sample| sample.frames() > 0);
        let voices = setup.output_shapes.first().map_or(1, |s| s.voices);
        Ok(Instance::realtime(GrainsNode {
            sample,
            limit: grain_limit(setup.config),
            sample_rate: setup.sample_rate,
            base: setup.seed,
            clouds: (0..voices).map(|_| Cloud::new()).collect(),
        }))
    }
}

#[derive(Clone, Copy, Default)]
struct Grain {
    active: bool,
    /// Position in the sample, in frames.
    pos: f64,
    /// Frames of the sample per output frame, negative when reversed.
    step: f64,
    age: u32,
    len: u32,
    /// The window's ramp, as a fraction of the grain's length.
    ramp: f32,
    left: f32,
    right: f32,
}

impl Grain {
    /// The window at `age`: a raised-cosine ramp up and down, flat between.
    /// A ramp of one half is a Hann window.
    #[inline]
    fn window(&self) -> f32 {
        let t = self.age as f32 / self.len as f32;
        let edge = t.min(1.0 - t);
        if edge >= self.ramp {
            1.0
        } else {
            0.5 - 0.5 * (PI * edge / self.ramp).cos()
        }
    }
}

struct Cloud {
    grains: [Grain; MAX_GRAINS],
    random: u64,
    /// The `seed` parameter `random` was last started from.
    seeded_with: u32,
    /// Frames until the next grain is due.
    until_next: f64,
}

impl Cloud {
    fn new() -> Self {
        Self {
            grains: [Grain::default(); MAX_GRAINS],
            random: 1,
            seeded_with: u32::MAX,
            until_next: 0.0,
        }
    }

    fn reset(&mut self) {
        self.grains = [Grain::default(); MAX_GRAINS];
        self.seeded_with = u32::MAX;
        self.until_next = 0.0;
    }

    fn playing(&self) -> usize {
        self.grains.iter().filter(|g| g.active).count()
    }

    /// A number from 0 up to (not including) 1.
    fn next(&mut self) -> f32 {
        // xorshift64*
        self.random ^= self.random >> 12;
        self.random ^= self.random << 25;
        self.random ^= self.random >> 27;
        let bits = self.random.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        bits as f32 / (1u64 << 24) as f32
    }
}

/// The parameters' values for one frame.
struct Frame {
    position: f32,
    spray: f32,
    size: f32,
    density: f32,
    jitter: f32,
    pitch: f32,
    frequency: f32,
    root: f32,
    pitch_spray: f32,
    reverse: f32,
    spread: f32,
    shape: f32,
}

struct GrainsNode {
    sample: Option<Arc<Sample>>,
    limit: usize,
    sample_rate: f32,
    base: u64,
    clouds: Vec<Cloud>,
}

impl GrainsNode {
    /// Starts the grain that is due, if there is room for it. Always draws the
    /// same five numbers, so the run of random numbers doesn't depend on
    /// whether there was room.
    fn start(&mut self, voice: usize, sample: &Sample, f: &Frame) -> f64 {
        let rate = f64::from(self.sample_rate);
        let cloud = &mut self.clouds[voice];
        let [r_start, r_pitch, r_reverse, r_pan, r_time] = [(); 5].map(|()| cloud.next());
        let frames = sample.frames();
        if cloud.playing() < self.limit
            && let Some(slot) = cloud.grains.iter_mut().find(|g| !g.active)
        {
            let at = (f.position + (r_start * 2.0 - 1.0) * f.spray * 0.5).clamp(0.0, 1.0);
            let semitones = f.pitch + (r_pitch * 2.0 - 1.0) * f.pitch_spray;
            let follow = if f.frequency > 0.0 {
                let root_hz = 440.0 * ((f.root - 69.0) / 12.0).exp2();
                f.frequency / root_hz
            } else {
                1.0
            };
            let speed = f64::from((semitones / 12.0).exp2() * follow)
                * f64::from(sample.sample_rate)
                / rate;
            let backwards = r_reverse < f.reverse;
            let pan = (r_pan * 2.0 - 1.0) * f.spread;
            let angle = (pan + 1.0) * FRAC_PI_4;
            *slot = Grain {
                active: true,
                pos: f64::from(at) * (frames - 1) as f64,
                step: if backwards { -speed } else { speed },
                age: 0,
                len: ((f.size * self.sample_rate) as u32).max(2),
                ramp: 0.5 - 0.49 * f.shape.clamp(0.0, 1.0),
                left: angle.cos() * SQRT_2,
                right: angle.sin() * SQRT_2,
            };
        }
        // Exponentially spread times with the same mean as the regular clock.
        let period = rate / f64::from(f.density.max(0.01));
        let jitter = f64::from(f.jitter.clamp(0.0, 1.0));
        let random = f64::from(-(1.0 - r_time).ln());
        (period * (1.0 - jitter + jitter * random)).max(1.0)
    }
}

#[inline]
fn read(data: &[f32], pos: f64) -> f32 {
    let i = pos as usize;
    let frac = (pos - i as f64) as f32;
    let a = data[i];
    let b = data.get(i + 1).copied().unwrap_or(a);
    a + (b - a) * frac
}

impl Node for GrainsNode {
    fn reset(&mut self) {
        for cloud in &mut self.clouds {
            cloud.reset();
        }
    }

    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let Io {
            inputs, outputs, ..
        } = io;
        let (audio, heads) = outputs.split_at_mut(1);
        let (out, head) = (&mut audio[0], &mut heads[0]);
        let frames = ctx.frames;
        let Some(sample) = self.sample.clone() else {
            for voice in 0..self.clouds.len() {
                out.silence(voice, 0);
                out.silence(voice, 1);
                head.silence(voice, 0);
            }
            return;
        };
        let lane = |port: usize, voice: usize| inputs[port].lane(voice, 0);
        for voice in 0..self.clouds.len() {
            let seed = lane(GrainsPorts::SEED, voice)
                .first()
                .map_or(0, |s| s.round().clamp(0.0, 999.0) as u32);
            let cloud = &mut self.clouds[voice];
            if cloud.seeded_with != seed {
                cloud.seeded_with = seed;
                cloud.random = (self.base
                    ^ (u64::from(seed) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    ^ (voice as u64 + 1).wrapping_mul(0xD6E8_FEB8_6659_FD93))
                    | 1;
            }
            let gate = lane(GrainsPorts::GATE, voice);
            let quiet = inputs[GrainsPorts::GATE].is_silent(voice, 0);
            let position = lane(GrainsPorts::POSITION, voice);
            if quiet && cloud.playing() == 0 {
                out.silence(voice, 0);
                out.silence(voice, 1);
                head.lane_mut(voice, 0).copy_from_slice(position);
                continue;
            }
            let ports = [
                GrainsPorts::SPRAY,
                GrainsPorts::SIZE,
                GrainsPorts::DENSITY,
                GrainsPorts::JITTER,
                GrainsPorts::PITCH,
                GrainsPorts::FREQUENCY,
                GrainsPorts::ROOT,
                GrainsPorts::PITCH_SPRAY,
                GrainsPorts::REVERSE,
                GrainsPorts::SPREAD,
                GrainsPorts::SHAPE,
            ]
            .map(|port| lane(port, voice));
            let [
                spray,
                size,
                density,
                jitter,
                pitch,
                frequency,
                root,
                pitch_spray,
                reverse,
                spread,
                shape,
            ] = ports;
            for i in 0..frames {
                let f = Frame {
                    position: position[i],
                    spray: spray[i],
                    size: size[i],
                    density: density[i],
                    jitter: jitter[i],
                    pitch: pitch[i],
                    frequency: frequency[i],
                    root: root[i],
                    pitch_spray: pitch_spray[i],
                    reverse: reverse[i],
                    spread: spread[i],
                    shape: shape[i],
                };
                if gate[i] > 0.5 {
                    if self.clouds[voice].until_next <= 0.0 {
                        let next = self.start(voice, &sample, &f);
                        self.clouds[voice].until_next += next;
                    }
                    self.clouds[voice].until_next -= 1.0;
                }
                let (mut l, mut r) = (0.0f32, 0.0f32);
                let last = (sample.frames() - 1) as f64;
                for grain in self.clouds[voice].grains.iter_mut().filter(|g| g.active) {
                    let w = grain.window();
                    l += read(&sample.left, grain.pos) * w * grain.left;
                    r += read(&sample.right, grain.pos) * w * grain.right;
                    grain.pos += grain.step;
                    grain.age += 1;
                    if grain.age >= grain.len || grain.pos < 0.0 || grain.pos > last {
                        grain.active = false;
                    }
                }
                let level = 1.0 / (f.density * f.size).max(1.0).sqrt();
                out.lane_mut(voice, 0)[i] = l * level;
                out.lane_mut(voice, 1)[i] = r * level;
            }
            head.lane_mut(voice, 0).copy_from_slice(position);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::testing::Harness;
    use noodle_io::Audio;

    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 256;

    /// A sample in a store, and a config that names it.
    fn sample_of(data: Vec<f32>, rate: u32) -> (SampleStore, Config) {
        let store = SampleStore::new();
        let hash = store.insert(&Audio {
            samples: data,
            channels: 1,
            sample_rate: rate,
        });
        let mut config = Config::new();
        config.set(SAMPLE_KEY, Value::Text(hash));
        (store, config)
    }

    /// Deterministic noise between -1 and 1.
    fn noise(frames: usize) -> Vec<f32> {
        let mut state = 12_345u32;
        (0..frames)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn harness(store: &SampleStore, config: &Config, seed: u64) -> Harness {
        Harness::with_seed(&Grains::new(store), config, &[], RATE, BLOCK, seed).unwrap()
    }

    /// Runs `blocks` blocks and returns the left and right channels.
    fn play(h: &mut Harness, blocks: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for _ in 0..blocks {
            h.run(BLOCK).unwrap();
            left.extend_from_slice(h.output(GrainsPorts::OUT).lane(0, 0));
            right.extend_from_slice(h.output(GrainsPorts::OUT).lane(0, 1));
        }
        (left, right)
    }

    fn hann(i: usize, len: usize) -> f32 {
        0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / len as f32).cos()
    }

    /// A cloud with one grain in it: a very low density.
    fn single(store: &SampleStore, config: &Config) -> Harness {
        let mut h = harness(store, config, 0);
        h.set(GrainsPorts::DENSITY, 0.5);
        h.set(GrainsPorts::SIZE, 0.1);
        h.set(GrainsPorts::POSITION, 0.25);
        h
    }

    #[test]
    fn one_grain_with_no_scatter_is_the_windowed_slice_of_the_sample() {
        // 24,001 frames, so a position of 0.25 is exactly frame 6,000.
        let data = noise(24_001);
        let (store, config) = sample_of(data.clone(), 48_000);
        let mut h = single(&store, &config);
        let (left, right) = play(&mut h, 24);
        let start = (0.25 * (data.len() - 1) as f64) as usize;
        let len = 4800;
        for i in 0..len {
            let want = data[start + i] * hann(i, len);
            assert!((left[i] - want).abs() < 1e-3, "{i}: {} vs {want}", left[i]);
            assert!((right[i] - want).abs() < 1e-3);
        }
        // Nothing before another grain is due, in 2 seconds.
        assert!(left[len + 10..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn density_10_without_jitter_starts_a_grain_every_100_ms() {
        let (store, config) = sample_of(vec![1.0; 48_000], 48_000);
        let mut h = harness(&store, &config, 0);
        h.set(GrainsPorts::DENSITY, 10.0);
        h.set(GrainsPorts::SIZE, 0.01);
        let (left, _) = play(&mut h, 80);
        let onsets: Vec<usize> = (1..left.len())
            .filter(|&i| left[i] > 0.0 && left[i - 1] == 0.0)
            .collect();
        assert!(onsets.len() >= 4, "{onsets:?}");
        for pair in onsets.windows(2) {
            assert_eq!(pair[1] - pair[0], 4800, "{onsets:?}");
        }
    }

    fn scattered(seed: f32, engine_seed: u64) -> (Vec<f32>, Vec<f32>) {
        let (store, config) = sample_of(noise(48_000), 48_000);
        let mut h = harness(&store, &config, engine_seed);
        for (port, value) in [
            (GrainsPorts::DENSITY, 40.0),
            (GrainsPorts::SIZE, 0.05),
            (GrainsPorts::JITTER, 1.0),
            (GrainsPorts::SPRAY, 1.0),
            (GrainsPorts::PITCH_SPRAY, 7.0),
            (GrainsPorts::REVERSE, 0.5),
            (GrainsPorts::SPREAD, 1.0),
            (GrainsPorts::SEED, seed),
        ] {
            h.set(port, value);
        }
        play(&mut h, 40)
    }

    #[test]
    fn a_seed_repeats_and_another_differs() {
        assert_eq!(scattered(3.0, 7), scattered(3.0, 7));
        assert_ne!(scattered(3.0, 7).0, scattered(4.0, 7).0);
        assert_ne!(scattered(3.0, 7).0, scattered(3.0, 8).0);
    }

    #[test]
    fn two_voices_with_one_seed_make_different_clouds() {
        let (store, config) = sample_of(noise(48_000), 48_000);
        let shape = Shape::new(2, 1);
        let mut h = Harness::with_seed(
            &Grains::new(&store),
            &config,
            &[(GrainsPorts::GATE, shape)],
            RATE,
            BLOCK,
            5,
        )
        .unwrap();
        for (port, value) in [
            (GrainsPorts::DENSITY, 40.0),
            (GrainsPorts::SPRAY, 1.0),
            (GrainsPorts::JITTER, 1.0),
        ] {
            h.set(port, value);
        }
        h.input(GrainsPorts::GATE, BLOCK).fill(1.0);
        h.run(BLOCK).unwrap();
        let out = h.output(GrainsPorts::OUT);
        assert_eq!(out.shape(), Shape::new(2, 2));
        assert_ne!(out.lane(0, 0), out.lane(1, 0));
    }

    #[test]
    fn a_low_gate_lets_grains_finish_and_starts_none() {
        let (store, config) = sample_of(vec![1.0; 48_000], 48_000);
        let mut h = Harness::new(
            &Grains::new(&store),
            &config,
            &[(GrainsPorts::GATE, Shape::MONO)],
            RATE,
            BLOCK,
        )
        .unwrap();
        h.set(GrainsPorts::DENSITY, 20.0);
        h.set(GrainsPorts::SIZE, 0.1);
        let mut heard = Vec::new();
        for block in 0..60 {
            h.input(GrainsPorts::GATE, BLOCK)
                .fill(if block < 10 { 1.0 } else { 0.0 });
            h.run(BLOCK).unwrap();
            heard.extend_from_slice(h.output(GrainsPorts::OUT).lane(0, 0));
        }
        let closed = 10 * BLOCK;
        // The grains playing when the gate fell go on to their ends (0.1 s).
        assert!(heard[closed..closed + 2000].iter().any(|&x| x > 0.1));
        assert!(heard[closed + 4800 + 1..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn level_stays_within_a_few_db_from_density_5_to_100() {
        let rms = |density: f32| {
            let (store, config) = sample_of(noise(48_000), 48_000);
            let mut h = harness(&store, &config, 1);
            h.set(GrainsPorts::DENSITY, density);
            h.set(GrainsPorts::SIZE, 0.1);
            h.set(GrainsPorts::JITTER, 1.0);
            h.set(GrainsPorts::SPRAY, 1.0);
            let (left, _) = play(&mut h, 1900);
            let tail = &left[48_000..];
            (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
        };
        let (low, high) = (rms(5.0), rms(100.0));
        let db = 20.0 * (high / low).log10();
        assert!(db.abs() < 4.0, "{db} dB between density 5 and 100");
    }

    #[test]
    fn a_full_cloud_skips_grains_without_clicking() {
        let (store, mut config) = sample_of(vec![1.0; 96_000], 48_000);
        config.set(GRAINS_KEY, Value::Int(4));
        let mut h = harness(&store, &config, 0);
        h.set(GrainsPorts::DENSITY, 200.0);
        h.set(GrainsPorts::SIZE, 1.0);
        h.set(GrainsPorts::POSITION, 0.0);
        let (left, _) = play(&mut h, 400);
        // At most four grains of a window height of 1, at a level of 1/sqrt(200).
        assert!(left.iter().all(|&x| x <= 4.0 / 200f32.sqrt() + 1e-4));
        let step = left
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(step < 1e-3, "{step}");
        assert!(left.iter().any(|&x| x > 0.1));
    }

    #[test]
    fn reset_is_the_same_as_new() {
        let (store, config) = sample_of(noise(48_000), 48_000);
        let make = || {
            let mut h = harness(&store, &config, 9);
            h.set(GrainsPorts::JITTER, 1.0);
            h.set(GrainsPorts::SPRAY, 1.0);
            h.set(GrainsPorts::DENSITY, 30.0);
            h
        };
        let mut used = make();
        play(&mut used, 50);
        used.seek(0);
        let again = play(&mut used, 20);
        assert_eq!(again, play(&mut make(), 20));
    }

    #[test]
    fn the_playhead_reports_the_position() {
        let (store, config) = sample_of(noise(4800), 48_000);
        let mut h = harness(&store, &config, 0);
        h.set(GrainsPorts::POSITION, 0.4);
        h.run(BLOCK).unwrap();
        let head = h.output(GrainsPorts::PLAYHEAD);
        assert!(head.lane(0, 0).iter().all(|&x| (x - 0.4).abs() < 1e-6));
    }

    #[test]
    fn a_missing_sample_is_silent() {
        let store = SampleStore::new();
        let mut config = Config::new();
        config.set(SAMPLE_KEY, Value::Text("beef".into()));
        let mut h = harness(&store, &config, 0);
        let (left, right) = play(&mut h, 4);
        assert!(left.iter().chain(&right).all(|&x| x == 0.0));
    }
}
