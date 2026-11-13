//! The Slice node: a sample cut at its transients into a set a Sampler can
//! play, one slice a key.

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Ports,
    Setup,
};

use crate::sample::{SampleRef, SampleStore};
use crate::sample_set::{SetEntry, is_choice, is_set, set_to_text};

pub const SLICE_ID: &str = "noodle.instrument.slice";
pub const SLICE_SENSITIVITY_KEY: &str = "sensitivity";
pub const SLICE_FIRST_KEY: &str = "first_key";
pub const SLICE_EDITS_KEY: &str = "edits";
/// The sample input's key.
pub const SLICE_INPUT: &str = "sample";

/// The most slices one node makes: one for each key from 0 to 127.
pub const MAX_SLICES: usize = 128;

static CONFIG: [ConfigInfo; 3] = [
    ConfigInfo::int(SLICE_SENSITIVITY_KEY, "Sensitivity", 50),
    ConfigInfo::int(SLICE_FIRST_KEY, "First key", 36),
    ConfigInfo::text(SLICE_EDITS_KEY, "Cut edits"),
];

#[derive(Ports)]
#[ports(memoryless)]
struct SlicePorts {
    #[sample_input("sample", "Sample")]
    sample: (),
    #[sample_output("slices", "Slices")]
    slices: (),
}

static INFO: NodeInfo = NodeInfo {
    id: SLICE_ID,
    version: 1,
    name: "Slice",
    category: "Instruments",
};

/// Cuts the sample on its `sample` input at its transients and puts the slices
/// on its `slices` output as a sample set: slice `i` is on key `first_key + i`.
/// Wire it to a Sampler with `keytrack` off and each key plays its own slice at
/// its natural pitch (the "Slicer" template is this pair with the controls
/// exposed).
///
/// - `sensitivity` is 0 to 100, the same scale as the warp editor's transient
///   pins and Convert to MIDI; a higher one cuts at softer attacks. It is
///   config and not a parameter because it shapes a wire that is resolved when
///   the project is compiled.
/// - `first_key` is the key of the first slice.
/// - `edits` holds the cuts added (`+0.25`) and removed (`-0.5`) by hand, as
///   fractions of the whole sample, so they survive a change of sensitivity: a
///   removal takes out the detected cut nearest to it, within 20 ms.
///
/// The cuts are found when the project is compiled and remembered by sample and
/// sensitivity (see [`SampleStore::onsets`]). The node makes no sound and does
/// no work while the engine runs.
pub struct Slice {
    samples: SampleStore,
}

impl Slice {
    pub fn new(samples: &SampleStore) -> Self {
        Self {
            samples: samples.clone(),
        }
    }
}

/// How far a hand edit reaches to find the cut it means, in seconds.
const EDIT_REACH: f64 = 0.02;

/// A hit this soon after the start of the sample is the start of the first
/// slice, not the end of an empty one before it, in seconds.
const LEAD_IN: f64 = 0.1;

/// The sensitivity of a Slice node's config, 0 to 1.
pub fn sensitivity_of(config: &Config) -> f32 {
    CONFIG[0].get_int(config).clamp(0, 100) as f32 / 100.0
}

/// The key of the first slice.
pub fn first_key_of(config: &Config) -> u8 {
    CONFIG[1].get_int(config).clamp(0, 127) as u8
}

/// The cuts of the part of a sample `reference` names, for a Slice node's
/// `config`: the start of the part, every cut after it, and the end of the
/// part, as fractions of the whole sample. Always at least two long.
pub fn cuts(samples: &SampleStore, reference: &SampleRef, config: &Config) -> Vec<f32> {
    let (start, end) = (reference.start, reference.end.max(reference.start));
    let mut cuts: Vec<f32> = samples
        .onsets(reference, sensitivity_of(config))
        .iter()
        .copied()
        .collect();
    let (frames, rate) = samples.get(&reference.hash).map_or((1.0, 48_000.0), |s| {
        (s.frames().max(1) as f64, f64::from(s.sample_rate))
    });
    let reach = (EDIT_REACH * rate / frames) as f32;
    for token in CONFIG[2].get_text(config).split(',') {
        let token = token.trim();
        let Some(at) = token
            .get(1..)
            .and_then(|n| n.parse::<f32>().ok())
            .filter(|at| at.is_finite() && (start..end).contains(at))
        else {
            continue;
        };
        if token.starts_with('+') {
            if !cuts.iter().any(|c| (c - at).abs() <= reach) {
                cuts.push(at);
            }
        } else if token.starts_with('-') {
            let nearest = cuts
                .iter()
                .enumerate()
                .filter(|(_, c)| (*c - at).abs() <= reach)
                .min_by(|a, b| (a.1 - at).abs().total_cmp(&(b.1 - at).abs()))
                .map(|(i, _)| i);
            if let Some(i) = nearest {
                cuts.remove(i);
            }
        }
    }
    cuts.sort_by(f32::total_cmp);
    // Nothing closer than the reach to the end, and no lead-in slice: a hit
    // within the first moments is where the first slice already starts.
    let lead = (LEAD_IN * rate / frames) as f32;
    cuts.retain(|&c| c - start > lead.max(reach) && end - c > reach);
    let mut all = Vec::with_capacity(cuts.len() + 2);
    all.push(start);
    all.extend(cuts);
    all.push(end);
    all
}

/// The set a Slice node makes of `input` (sample text): one entry per slice.
/// `None` when `input` is no sample, or is a set or a pick between samples,
/// which can't be sliced.
pub fn slice_set(samples: &SampleStore, config: &Config, input: &str) -> Option<String> {
    if input.is_empty() || is_set(input) || is_choice(input) {
        return None;
    }
    let reference = SampleRef::parse(input);
    if reference.hash.is_empty() || samples.get(&reference.hash).is_none() {
        return None;
    }
    let first = first_key_of(config);
    let cuts = cuts(samples, &reference, config);
    let entries: Vec<SetEntry> = cuts
        .windows(2)
        .take(MAX_SLICES - usize::from(first))
        .enumerate()
        .map(|(i, w)| SetEntry {
            reference: SampleRef {
                hash: reference.hash.clone(),
                start: w[0],
                end: w[1],
            },
            root: Some(first + i as u8),
            layer: 1,
            rr: 1,
        })
        .collect();
    (!entries.is_empty()).then(|| set_to_text(&entries))
}

impl NodeType for Slice {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(SlicePorts::layout())
    }

    fn sample_through(&self, _config: &Config, _key: &str) -> Option<String> {
        Some(SLICE_INPUT.to_owned())
    }

    fn sample_derived(&self, config: &Config, _key: &str, input: Option<&str>) -> Option<String> {
        slice_set(&self.samples, config, input?)
    }

    fn stateless(&self) -> bool {
        true
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(SliceNode))
    }
}

struct SliceNode;

impl Node for SliceNode {
    fn process(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample_set::parse_set;
    use noodle_core::Value;
    use noodle_io::Audio;

    const RATE: u32 = 48_000;

    /// Four clicks, a quarter second apart, then a quiet tail.
    fn clicks() -> Audio {
        let mut samples = vec![0.0f32; RATE as usize];
        for n in 0..4 {
            let at = 2_000 + n * RATE as usize / 4;
            for i in 0..2_400 {
                let t = i as f32 / RATE as f32;
                samples[at + i] +=
                    0.8 * (std::f32::consts::TAU * 400.0 * t).sin() * (-t * 60.0).exp();
            }
        }
        Audio {
            samples,
            channels: 1,
            sample_rate: RATE,
        }
    }

    fn config(sensitivity: i64, first: i64, edits: &str) -> Config {
        Config::new()
            .with(SLICE_SENSITIVITY_KEY, Value::Int(sensitivity))
            .with(SLICE_FIRST_KEY, Value::Int(first))
            .with(SLICE_EDITS_KEY, Value::Text(edits.into()))
    }

    #[test]
    fn it_cuts_a_sample_at_its_hits_one_key_each() {
        let store = SampleStore::new();
        let hash = store.insert(&clicks());
        let text = slice_set(&store, &config(50, 36, ""), &hash).unwrap();
        let set = parse_set(&text);
        let roots: Vec<Option<u8>> = set.iter().map(|e| e.root).collect();
        assert_eq!(roots, [Some(36), Some(37), Some(38), Some(39)], "{text}");
        // The first slice runs from the start to the second hit, and the
        // last to the end.
        assert_eq!(set[0].reference.start, 0.0);
        assert_eq!(set[3].reference.end, 1.0);
        let second_hit = (2_000 + RATE as usize / 4) as f32 / RATE as f32;
        assert!((set[1].reference.start - second_hit).abs() < 0.003);
        assert_eq!(set[0].reference.end, set[1].reference.start);
    }

    #[test]
    fn it_starts_at_the_first_key_and_stops_at_the_last() {
        let store = SampleStore::new();
        let hash = store.insert(&clicks());
        let text = slice_set(&store, &config(50, 126, ""), &hash).unwrap();
        let roots: Vec<Option<u8>> = parse_set(&text).iter().map(|e| e.root).collect();
        assert_eq!(roots, [Some(126), Some(127)]);
    }

    #[test]
    fn hand_edits_add_and_remove_cuts_and_survive_a_new_sensitivity() {
        let store = SampleStore::new();
        let hash = store.insert(&clicks());
        let second_hit = (2_000 + RATE as usize / 4) as f32 / RATE as f32;
        let removed = format!("-{second_hit}");
        for sensitivity in [30, 50, 70] {
            let text = slice_set(
                &store,
                &config(sensitivity, 36, &format!("+0.9,{removed}")),
                &hash,
            )
            .unwrap();
            let set = parse_set(&text);
            let starts: Vec<f32> = set.iter().map(|e| e.reference.start).collect();
            assert!(starts.iter().any(|s| (s - 0.9).abs() < 1e-6), "{starts:?}");
            assert!(
                !starts.iter().any(|s| (s - second_hit).abs() < 0.003),
                "{starts:?}"
            );
        }
    }

    #[test]
    fn a_sample_with_no_hits_is_one_slice_and_a_missing_one_is_none() {
        let store = SampleStore::new();
        let hash = store.insert(&Audio {
            samples: vec![0.0; 4_800],
            channels: 1,
            sample_rate: RATE,
        });
        let set = parse_set(&slice_set(&store, &config(50, 36, ""), &hash).unwrap());
        assert_eq!(set.len(), 1);
        assert_eq!((set[0].reference.start, set[0].reference.end), (0.0, 1.0));
        assert!(slice_set(&store, &config(50, 36, ""), &"0".repeat(64)).is_none());
        assert!(slice_set(&store, &config(50, 36, ""), "").is_none());
    }

    #[test]
    fn a_window_is_cut_inside_its_bounds() {
        let store = SampleStore::new();
        let hash = store.insert(&clicks());
        let text = slice_set(&store, &config(50, 0, ""), &format!("{hash}:0.4:0.9")).unwrap();
        let set = parse_set(&text);
        assert_eq!(set.len(), 3, "{text}");
        assert_eq!(set[0].reference.start, 0.4);
        assert_eq!(set[2].reference.end, 0.9);
    }
}
