//! The Wavetable oscillator: one cycle of a waveform at the pitch it is given,
//! picked from a stack of cycles (the table) by a `position` parameter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType,
    ParamInfo, PerLane, Ports, Setup, Skip, Unit,
};
use realfft::RealFftPlanner;

use crate::sample::{Sample, SampleRef, SampleStore};

pub const WAVETABLE_ID: &str = "noodle.osc.wavetable";

/// The setting (and sample input) that names the table, by hash.
pub const TABLE_KEY: &str = "table";

/// The setting that holds how many samples one cycle of the file is.
pub const CYCLE_KEY: &str = "cycle";

/// The cycle lengths the node accepts. Each is a power of two, so the read
/// wraps with a mask.
pub const CYCLE_LENGTHS: [usize; 5] = [256, 512, 1024, 2048, 4096];

/// How many samples one cycle is unless the config says otherwise.
pub const DEFAULT_CYCLE: usize = 2048;

static CONFIG: [ConfigInfo; 2] = [
    ConfigInfo::text(TABLE_KEY, "Table"),
    ConfigInfo::int(CYCLE_KEY, "Cycle length", DEFAULT_CYCLE as i64),
];

#[derive(Ports)]
struct WavetablePorts {
    #[sample_input("table", "Table")]
    table: (),
    #[param(
        "frequency",
        "Frequency",
        ParamInfo::new(0.01, 20_000.0, 440.0)
            .log()
            .unit(Unit::Hertz)
    )]
    frequency: (),
    #[param("position", "Position", ParamInfo::new(0.0, 1.0, 0.0).offset().featured())]
    position: (),
    #[param(
        "phase",
        "Phase",
        ParamInfo::new(-1.0, 1.0, 0.0).offset()
    )]
    phase: (),
    #[output("out", "Out")]
    out: (),
}

static INFO: NodeInfo = NodeInfo {
    id: WAVETABLE_ID,
    version: 1,
    name: "Wavetable",
    category: "Oscillators",
};

/// A wavetable oscillator. It plays one cycle of a waveform at the pitch of
/// `frequency`, which runs at audio rate (so FM works as on Sine). The cycle
/// is picked from a stack of them by `position`, 0 for the first and 1 for
/// the last; between two cycles it crossfades, so a sweep is smooth. `phase`
/// is added to the running phase.
///
/// **The table** is a sample in the store (see [`SampleStore`]): the left and
/// right channels are averaged, and the file is cut every `cycle` samples
/// (256 to 4096, default 2048; a ragged end is dropped, and a file shorter
/// than one cycle is one cycle, stretched to fit). The `table` option is also
/// a sample input: a wire from a Sample node overrides it, as on the Sampler.
/// A table that isn't in the store plays silence.
///
/// **Aliasing.** The work is done when the node is built, off the audio
/// thread: each cycle is transformed once and written back at one band-limited
/// level per octave, each with the harmonics above that octave's limit
/// removed. The audio thread picks the level from the frequency, crossfades two
/// levels across the octave boundary and reads with linear interpolation.
/// Nothing allocates or transforms there. Two nodes with the same table and
/// `cycle` share one built table, as Samplers share a sample.
pub struct Wavetable {
    store: SampleStore,
    built: Mutex<HashMap<(String, usize), Weak<Table>>>,
}

impl Wavetable {
    pub fn new(store: &SampleStore) -> Self {
        Self {
            store: store.clone(),
            built: Mutex::new(HashMap::new()),
        }
    }

    /// The table for `text` (a sample reference) cut every `cycle` samples,
    /// shared with any other node that has built the same one.
    fn table(&self, text: &str, cycle: usize) -> Option<Arc<Table>> {
        let reference = SampleRef::parse(text);
        let sample = self
            .store
            .get(&reference.hash)?
            .window(reference.start, reference.end);
        let key = (reference.to_text(), cycle);
        let mut built = self.built.lock().ok()?;
        if let Some(table) = built.get(&key).and_then(Weak::upgrade) {
            return Some(table);
        }
        built.retain(|_, weak| weak.strong_count() > 0);
        let table = Arc::new(Table::build(&sample, cycle));
        built.insert(key, Arc::downgrade(&table));
        Some(table)
    }
}

/// The cycle length a config asks for: the nearest allowed one.
pub fn cycle_of(config: &Config) -> usize {
    let asked = CONFIG[1].get_int(config).max(1) as usize;
    *CYCLE_LENGTHS
        .iter()
        .min_by_key(|&&len| len.abs_diff(asked))
        .expect("the list isn't empty")
}

/// A stack of cycles, each stored at every band limit.
pub struct Table {
    /// Samples in one cycle.
    n: usize,
    cycles: usize,
    /// `levels[l]` holds every cycle with the harmonics above `n / 2 >> l`
    /// removed, one cycle after another.
    levels: Vec<Box<[f32]>>,
}

impl Table {
    /// Cuts `sample` into cycles of `n` samples and builds every level.
    pub fn build(sample: &Sample, n: usize) -> Self {
        debug_assert!(n.is_power_of_two());
        let mono: Vec<f32> = sample
            .left
            .iter()
            .zip(sample.right.iter())
            .map(|(l, r)| (l + r) * 0.5)
            .collect();
        let source = if mono.len() >= n {
            mono
        } else if mono.is_empty() {
            vec![0.0; n]
        } else {
            // Shorter than one cycle: it is one cycle, stretched to fit.
            (0..n)
                .map(|i| {
                    let at = i as f32 * mono.len() as f32 / n as f32;
                    let a = at as usize;
                    let b = (a + 1) % mono.len();
                    mono[a] + (mono[b] - mono[a]) * (at - a as f32)
                })
                .collect()
        };
        let cycles = (source.len() / n).max(1);
        let top = n.trailing_zeros() as usize;
        let mut levels: Vec<Vec<f32>> = (0..top).map(|_| Vec::with_capacity(cycles * n)).collect();
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(n);
        let inverse = planner.plan_fft_inverse(n);
        let mut time = forward.make_input_vec();
        let mut spectrum = forward.make_output_vec();
        let mut kept = inverse.make_input_vec();
        let mut back = inverse.make_output_vec();
        for cycle in source.chunks_exact(n).take(cycles) {
            time.copy_from_slice(cycle);
            forward
                .process(&mut time, &mut spectrum)
                .expect("the buffers are the planned size");
            for (level, out) in levels.iter_mut().enumerate() {
                if level == 0 {
                    out.extend_from_slice(cycle);
                    continue;
                }
                let limit = (n / 2) >> level;
                for (k, (bin, dest)) in spectrum.iter().zip(kept.iter_mut()).enumerate() {
                    *dest = if k <= limit {
                        *bin
                    } else {
                        realfft::num_complex::Complex::new(0.0, 0.0)
                    };
                }
                // The inverse wants these two to be real.
                kept[0].im = 0.0;
                if let Some(last) = kept.last_mut() {
                    last.im = 0.0;
                }
                inverse
                    .process(&mut kept, &mut back)
                    .expect("the buffers are the planned size");
                out.extend(back.iter().map(|x| x / n as f32));
            }
        }
        Self {
            n,
            cycles,
            levels: levels.into_iter().map(Vec::into_boxed_slice).collect(),
        }
    }

    pub fn cycles(&self) -> usize {
        self.cycles
    }

    pub fn cycle_len(&self) -> usize {
        self.n
    }

    /// One sample of one cycle at one level, `at` samples in (0 to `n`).
    #[inline]
    fn read(&self, level: usize, cycle: usize, at: f32) -> f32 {
        let data = &self.levels[level][cycle * self.n..(cycle + 1) * self.n];
        let mask = self.n - 1;
        let i = at as usize;
        let frac = at - i as f32;
        let a = data[i & mask];
        let b = data[(i + 1) & mask];
        a + (b - a) * frac
    }

    /// The cycle at `position` (0 to 1) read at `phase` (0 to 1), for a
    /// frequency whose step through the table is `step` samples per output
    /// sample.
    #[inline]
    pub fn sample(&self, position: f32, phase: f32, step: f32) -> f32 {
        let top = self.levels.len() - 1;
        let octaves = step.max(1.0).log2();
        let upper = (octaves.ceil() as usize).min(top);
        // A band-limited level is right at the top of its octave; the one
        // below it (brighter) is blended in over the bottom half.
        let lower_weight = if upper == 0 {
            0.0
        } else {
            (1.0 - (octaves - (upper - 1) as f32) * 2.0).clamp(0.0, 1.0)
        };
        let travel = position.clamp(0.0, 1.0) * (self.cycles - 1) as f32;
        let first = (travel as usize).min(self.cycles - 1);
        let next = (first + 1).min(self.cycles - 1);
        let across = travel - first as f32;
        let at = phase * self.n as f32;
        let level = |l: usize| {
            let a = self.read(l, first, at);
            if across > 0.0 && next != first {
                a + (self.read(l, next, at) - a) * across
            } else {
                a
            }
        };
        let high = level(upper);
        if lower_weight > 0.0 {
            high + (level(upper - 1) - high) * lower_weight
        } else {
            high
        }
    }
}

impl NodeType for Wavetable {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(WavetablePorts::layout())
    }

    fn cache_inputs(
        &self,
        _node: noodle_core::NodeId,
        config: &Config,
        _env: &noodle_engine::CacheEnv<'_>,
        _key: &mut noodle_core::KeyBuilder,
    ) -> Result<(), noodle_engine::Uncacheable> {
        // The hash in the config keys the sound. A table that can't be found
        // renders as silence, and must not be remembered as the answer.
        let text = CONFIG[0].get_text(config);
        let reference = SampleRef::parse(&text);
        if !reference.hash.is_empty() && self.store.get(&reference.hash).is_none() {
            return Err(noodle_engine::Uncacheable::Because(format!(
                "the table {} is missing",
                &reference.hash[..8.min(reference.hash.len())]
            )));
        }
        Ok(())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let text = CONFIG[0].get_text(setup.config);
        let table = self.table(&text, cycle_of(setup.config));
        Ok(Instance::realtime(PerLane::new(
            WavetableKernel { table },
            setup,
        )))
    }
}

struct WavetableKernel {
    table: Option<Arc<Table>>,
}

impl LaneKernel for WavetableKernel {
    /// Phase in cycles, from 0 to 1.
    type State = f32;

    /// No pitch, no sound: a voice the Voices node isn't using has a silent
    /// frequency.
    fn skip(&self) -> Skip {
        Skip::AnySilent(&[WavetablePorts::FREQUENCY])
    }

    fn process_lane(&mut self, phase: &mut f32, ctx: &Context, mut lane: Lane<'_, '_>) {
        let out = lane.outputs.get_mut(WavetablePorts::OUT);
        let Some(table) = &self.table else {
            out.fill(0.0);
            return;
        };
        let frequency = lane.inputs.get(WavetablePorts::FREQUENCY);
        let position = lane.inputs.get(WavetablePorts::POSITION);
        let shift = lane.inputs.get(WavetablePorts::PHASE);
        let seconds_per_sample = 1.0 / ctx.sample_rate;
        let per_hertz = table.n as f32 * seconds_per_sample;
        for (((out, &f), &p), &s) in out.iter_mut().zip(frequency).zip(position).zip(shift) {
            let at = (*phase + s).rem_euclid(1.0);
            *out = table.sample(p, at, f.abs() * per_hertz);
            *phase = (*phase + f * seconds_per_sample).rem_euclid(1.0);
        }
        if !phase.is_finite() {
            *phase = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_of(data: Vec<f32>) -> Sample {
        Sample {
            left: data.clone().into(),
            right: data.into(),
            sample_rate: 48_000,
        }
    }

    fn sine_cycle(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 / n as f32 * std::f32::consts::TAU).sin())
            .collect()
    }

    #[test]
    fn a_ragged_end_is_dropped_and_a_short_file_is_one_cycle() {
        let mut data = sine_cycle(256);
        data.extend(sine_cycle(256));
        data.extend(vec![0.5; 100]);
        let table = Table::build(&sample_of(data), 256);
        assert_eq!(table.cycles(), 2);
        let table = Table::build(&sample_of(sine_cycle(100)), 256);
        assert_eq!(table.cycles(), 1);
        assert_eq!(table.cycle_len(), 256);
        // Stretched: still a sine.
        let got = table.sample(0.0, 0.25, 0.0);
        assert!((got - 1.0).abs() < 0.01, "{got}");
        let empty = Table::build(&sample_of(Vec::new()), 256);
        assert_eq!(empty.sample(0.0, 0.3, 1.0), 0.0);
    }

    #[test]
    fn nodes_with_the_same_table_and_cycle_share_one_build() {
        let store = SampleStore::new();
        let hash = store.insert_sample(sample_of(sine_cycle(512)));
        let node_type = Wavetable::new(&store);
        let a = node_type.table(&hash, 256).unwrap();
        let b = node_type.table(&hash, 256).unwrap();
        let other = node_type.table(&hash, 512).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&a, &other));
        assert!(node_type.table("nothere", 256).is_none());
    }

    #[test]
    fn position_halfway_is_the_mean_of_two_cycles() {
        let a = sine_cycle(512);
        let b: Vec<f32> = a.iter().map(|x| x.signum() * 0.5).collect();
        let mut data = a.clone();
        data.extend(&b);
        let table = Table::build(&sample_of(data), 512);
        for phase in [0.1, 0.3, 0.6, 0.9] {
            let got = table.sample(0.5, phase, 0.0);
            let at = (phase * 512.0) as usize;
            let want = (a[at] + b[at]) * 0.5;
            assert!((got - want).abs() < 0.02, "{phase}: {got} vs {want}");
        }
    }

    #[test]
    fn levels_remove_the_harmonics_above_their_limit() {
        // A saw has every harmonic. At level 8 of 2048 only 4 remain, so the
        // fifth harmonic is gone.
        let n = 2048;
        let saw: Vec<f32> = (0..n).map(|i| 2.0 * i as f32 / n as f32 - 1.0).collect();
        let table = Table::build(&sample_of(saw), n);
        assert_eq!(table.levels.len(), 11);
        let level = &table.levels[8];
        let harmonic = |k: usize| {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, x) in level.iter().enumerate() {
                let a = std::f32::consts::TAU * (k * i) as f32 / n as f32;
                re += x * a.cos();
                im += x * a.sin();
            }
            (re * re + im * im).sqrt() / n as f32
        };
        assert!(harmonic(4) > 0.05);
        assert!(harmonic(5) < 1e-4, "{}", harmonic(5));
    }
}
