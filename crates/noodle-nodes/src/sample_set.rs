//! The Sample Set node and the pieces a Sampler needs to play one.
//!
//! A set is a list of recordings, each with the key it was played at, a
//! velocity layer and a round-robin slot. It travels on the sample wire as
//! text, one entry after another, so it keys the cache and needs no new wire
//! kind.

use std::collections::BTreeMap;

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, Ports,
    Setup,
};

use crate::sample::SampleRef;

/// One recording in a set.
#[derive(Clone, Debug, PartialEq)]
pub struct SetEntry {
    pub reference: SampleRef,
    /// The key the recording was played at. `None` is a recording whose note
    /// could not be read; it is kept in the table, with a warning, and the
    /// Sampler skips it.
    pub root: Option<u8>,
    /// The velocity layer, counted from 1 (softest) among the layers of the
    /// same root.
    pub layer: u8,
    /// The round-robin slot, counted from 1.
    pub rr: u8,
}

impl SetEntry {
    /// `hash[:start:end]@root[/layer][#rr]`. The root is empty when unknown.
    pub fn to_text(&self) -> String {
        let mut text = self.reference.to_text();
        text.push('@');
        if let Some(root) = self.root {
            text.push_str(&root.to_string());
        }
        if self.layer != 1 {
            text.push_str(&format!("/{}", self.layer));
        }
        if self.rr != 1 {
            text.push_str(&format!("#{}", self.rr));
        }
        text
    }

    /// Reads one entry; `None` if it has no `@`.
    pub fn parse(text: &str) -> Option<Self> {
        let (reference, rest) = text.split_once('@')?;
        let (rest, rr) = match rest.split_once('#') {
            Some((a, b)) => (a, b.parse::<u8>().ok().filter(|&n| n >= 1).unwrap_or(1)),
            None => (rest, 1),
        };
        let (root, layer) = match rest.split_once('/') {
            Some((a, b)) => (a, b.parse::<u8>().ok().filter(|&n| n >= 1).unwrap_or(1)),
            None => (rest, 1),
        };
        Some(Self {
            reference: SampleRef::parse(reference),
            root: root.parse::<u8>().ok().filter(|&n| n <= 127),
            layer,
            rr,
        })
    }
}

/// Separates the candidates of a sample that is picked while audio runs, in
/// the text the compiler writes into the receiver's config (see
/// [`NodeType::sample_chooser`](noodle_engine::NodeType::sample_chooser)). Each
/// candidate is one sample or a set, or empty for none.
pub const CHOICE_SEPARATOR: char = '|';

/// What the picker's signal adds to the number of the candidate when the
/// notes already sounding on the others are to be cut at the switch (Cut
/// held). There are at most 64 candidates, so the sum says both.
pub const CHOICE_CUT: f32 = 64.0;

/// Whether sample text is a list of candidates to pick from.
pub fn is_choice(text: &str) -> bool {
    text.contains(CHOICE_SEPARATOR)
}

/// Whether sample text is a set rather than one sample.
pub fn is_set(text: &str) -> bool {
    text.contains('@')
}

pub fn parse_set(text: &str) -> Vec<SetEntry> {
    text.split(';').filter_map(SetEntry::parse).collect()
}

pub fn set_to_text(entries: &[SetEntry]) -> String {
    entries
        .iter()
        .map(SetEntry::to_text)
        .collect::<Vec<_>>()
        .join(";")
}

/// The hashes in sample text, whether it is one sample or a set.
pub fn sample_hashes(text: &str) -> Vec<String> {
    if is_choice(text) {
        text.split(CHOICE_SEPARATOR)
            .flat_map(sample_hashes)
            .collect()
    } else if is_set(text) {
        parse_set(text)
            .into_iter()
            .map(|e| e.reference.hash)
            .filter(|h| !h.is_empty())
            .collect()
    } else {
        let hash = SampleRef::parse(text).hash;
        if hash.is_empty() { vec![] } else { vec![hash] }
    }
}

/// What a file name says about its recording.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NameInfo {
    pub note: Option<u8>,
    /// A rank for the velocity layer: only the order between files matters.
    pub layer: Option<f32>,
    pub rr: Option<u8>,
}

const DYNAMICS: [&str; 8] = ["ppp", "pp", "p", "mp", "mf", "f", "ff", "fff"];

fn note_token(token: &str, middle_c: i64) -> Option<u8> {
    let mut chars = token.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let base = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: &str = chars.as_str();
    let (accidental, octave) = match rest.chars().next() {
        Some('#') => (1, &rest[1..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, rest),
    };
    let octave: i64 = octave.parse().ok()?;
    // Middle C is C4 by default (MIDI 60), or C3.
    let offset = if middle_c == 3 { 2 } else { 1 };
    let note = (octave + offset) * 12 + base + accidental;
    u8::try_from(note).ok().filter(|&n| n <= 127)
}

fn number_after(token: &str, prefix: &str) -> Option<u32> {
    token
        .strip_prefix(prefix)
        .filter(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .and_then(|rest| rest.parse().ok())
}

/// Reads the note, velocity layer and round-robin take from a file name
/// (without its extension), in any order and with any separators: `C#4`,
/// `Db4` or `61`; `ppp` to `fff`, `v1` to `v16`, `vel100`; `rr2` or `t3`.
/// `middle_c` is 4 or 3, the octave that holds MIDI note 60.
pub fn parse_name(stem: &str, middle_c: i64) -> NameInfo {
    let mut info = NameInfo::default();
    let mut bare = None;
    for token in stem.split(['_', ' ', '.', '(', ')', ',', '-']) {
        if token.is_empty() {
            continue;
        }
        let lower = token.to_ascii_lowercase();
        if let Some(n) = number_after(&lower, "rr").or_else(|| number_after(&lower, "take")) {
            info.rr = Some(n.clamp(1, 255) as u8);
        } else if let Some(n) = number_after(&lower, "t") {
            info.rr = Some(n.clamp(1, 255) as u8);
        } else if let Some(n) = number_after(&lower, "vel") {
            info.layer = Some(n as f32);
        } else if let Some(n) = number_after(&lower, "v") {
            info.layer = Some(n as f32);
        } else if let Some(i) = DYNAMICS.iter().position(|d| *d == lower) {
            info.layer = Some(i as f32 + 1.0);
        } else if info.note.is_none()
            && let Some(note) = note_token(token, middle_c)
        {
            info.note = Some(note);
        } else if bare.is_none()
            && !token.is_empty()
            && token.chars().all(|c| c.is_ascii_digit())
            && let Ok(n) = token.parse::<u32>()
            && n <= 127
        {
            bare = Some(n as u8);
        }
    }
    if info.note.is_none() {
        info.note = bare;
    }
    info
}

/// A file ready to become a row: its name (with the extension) and the hash
/// of its audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub entry: SetEntry,
}

/// Turns names and hashes into rows, ranking each key's layers from softest
/// up. A note that can't be read leaves the root empty.
pub fn rows_from_files(files: &[(String, String)], middle_c: i64) -> Vec<Row> {
    let infos: Vec<NameInfo> = files
        .iter()
        .map(|(name, _)| parse_name(file_stem(name), middle_c))
        .collect();
    // Per root, the ranks that occur, in order.
    let mut ranks: BTreeMap<Option<u8>, Vec<f32>> = BTreeMap::new();
    for info in &infos {
        if let Some(rank) = info.layer {
            let list = ranks.entry(info.note).or_default();
            if !list.contains(&rank) {
                list.push(rank);
            }
        }
    }
    for list in ranks.values_mut() {
        list.sort_by(f32::total_cmp);
    }
    files
        .iter()
        .zip(&infos)
        .map(|((name, hash), info)| {
            let layer = info.layer.map_or(1, |rank| {
                ranks[&info.note]
                    .iter()
                    .position(|r| *r == rank)
                    .map_or(1, |i| (i + 1).min(255) as u8)
            });
            Row {
                name: name.clone(),
                entry: SetEntry {
                    reference: SampleRef::whole(hash.clone()),
                    root: info.note,
                    layer,
                    rr: info.rr.unwrap_or(1),
                },
            }
        })
        .collect()
}

fn file_stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// What a rescan of a folder would change in a set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rescan {
    /// The rows after the rescan.
    pub rows: Vec<Row>,
    pub added: Vec<String>,
    /// Same name, new audio.
    pub replaced: Vec<String>,
    pub removed: Vec<String>,
    /// Same audio under a new name: `(old, new)`.
    pub renamed: Vec<(String, String)>,
    /// Files whose note could not be read.
    pub unplaced: Vec<String>,
}

impl Rescan {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.replaced.is_empty()
            && self.removed.is_empty()
            && self.renamed.is_empty()
    }
}

/// Compares the `old` rows with the files now in the folder. A row whose file
/// is unchanged keeps its columns, hand edits included; a replaced file is
/// read afresh; a renamed one keeps its columns under the new name.
pub fn rescan(old: &[Row], found: &[(String, String)], middle_c: i64) -> Rescan {
    let fresh = rows_from_files(found, middle_c);
    let mut out = Rescan::default();
    let mut taken = vec![false; old.len()];
    for row in fresh {
        let same_name = old.iter().position(|o| o.name == row.name);
        if let Some(i) = same_name {
            taken[i] = true;
            if old[i].entry.reference.hash == row.entry.reference.hash {
                out.rows.push(old[i].clone());
            } else {
                out.replaced.push(row.name.clone());
                out.rows.push(row);
            }
            continue;
        }
        let same_audio = old.iter().enumerate().position(|(i, o)| {
            !taken[i]
                && o.entry.reference.hash == row.entry.reference.hash
                && !found.iter().any(|(n, _)| *n == o.name)
        });
        if let Some(i) = same_audio {
            taken[i] = true;
            out.renamed.push((old[i].name.clone(), row.name.clone()));
            out.rows.push(Row {
                name: row.name,
                entry: old[i].entry.clone(),
            });
        } else {
            out.added.push(row.name.clone());
            out.rows.push(row);
        }
    }
    for (i, row) in old.iter().enumerate() {
        if !taken[i] {
            out.removed.push(row.name.clone());
        }
    }
    out.unplaced = out
        .rows
        .iter()
        .filter(|r| r.entry.root.is_none())
        .map(|r| r.name.clone())
        .collect();
    out
}

/// One velocity layer of one key: the entries that take turns.
#[derive(Clone, Debug)]
pub struct LayerZone {
    /// Indices into the entries the tables were built from, in round-robin
    /// order.
    pub entries: Vec<usize>,
    /// Which counter this layer uses.
    pub slot: usize,
}

#[derive(Clone, Debug)]
struct RootZone {
    layers: Vec<LayerZone>,
}

/// The tables a Sampler looks a note up in: key to root, root to layers,
/// layer to entries. Built once, read on the audio thread.
#[derive(Clone, Debug)]
pub struct SetTables {
    key_root: [u8; 128],
    roots: Vec<RootZone>,
    layer_count: usize,
}

const NO_ROOT: u8 = u8::MAX;

impl SetTables {
    pub fn build(entries: &[SetEntry]) -> Self {
        // root -> layer -> (rr, index)
        let mut by_root: BTreeMap<u8, BTreeMap<u8, Vec<(u8, usize)>>> = BTreeMap::new();
        for (i, e) in entries.iter().enumerate() {
            if let Some(root) = e.root {
                by_root
                    .entry(root)
                    .or_default()
                    .entry(e.layer)
                    .or_default()
                    .push((e.rr, i));
            }
        }
        let mut roots = Vec::new();
        let mut root_keys = Vec::new();
        let mut slot = 0;
        for (root, layers) in by_root {
            let layers = layers
                .into_values()
                .map(|mut list| {
                    list.sort_by_key(|&(rr, i)| (rr, i));
                    slot += 1;
                    LayerZone {
                        entries: list.into_iter().map(|(_, i)| i).collect(),
                        slot: slot - 1,
                    }
                })
                .collect();
            root_keys.push(root);
            roots.push(RootZone { layers });
        }
        let mut key_root = [NO_ROOT; 128];
        for (key, cell) in key_root.iter_mut().enumerate() {
            // Ascending roots, strictly closer wins: ties go to the lower.
            let mut best: Option<(usize, usize)> = None;
            for (i, &r) in root_keys.iter().enumerate() {
                let d = key.abs_diff(r as usize);
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
            if let Some((i, _)) = best {
                *cell = i as u8;
            }
        }
        Self {
            key_root,
            roots,
            layer_count: slot,
        }
    }

    /// How many layers (and so counters) there are in all.
    pub fn layer_count(&self) -> usize {
        self.layer_count
    }

    /// The layer a note plays: the nearest key with any entries, then the
    /// velocity layer (`velocity` is 0 to 1, after any curve): a key's layers
    /// split 1 to 127 evenly.
    pub fn layer(&self, key: u8, velocity: f32) -> Option<&LayerZone> {
        let root = *self.key_root.get(key as usize)?;
        let zone = self
            .roots
            .get(usize::from(root))
            .filter(|_| root != NO_ROOT)?;
        let n = zone.layers.len();
        let midi = (velocity.clamp(0.0, 1.0) * 127.0).round().clamp(1.0, 127.0) as usize;
        zone.layers.get(((midi - 1) * n / 127).min(n - 1))
    }
}

/// Holds a list of recordings, each with its key, velocity layer and
/// round-robin slot, and puts the whole set on its `sample` output. Wire it to
/// a Sampler's sample input: the Sampler plays the recording nearest to each
/// key, from the layer the velocity picks, taking turns among the takes.
///
/// The `sample` option is the set as text (see [`SetEntry`]); `names` holds
/// the file names, one a line in the same order, so a rescan can tell what
/// changed; `folder` is where they came from; `middle_c` is the octave of
/// MIDI note 60, 4 or 3, used when reading names. The node makes no sound and
/// does no work while the engine runs; the compiler reads it.
pub struct SampleSet;

pub const SAMPLE_SET_ID: &str = "noodle.instrument.sample_set";
pub const SET_NAMES_KEY: &str = "names";
pub const SET_FOLDER_KEY: &str = "folder";
pub const SET_MIDDLE_C_KEY: &str = "middle_c";

static CONFIG: [ConfigInfo; 4] = [
    ConfigInfo::text(crate::SAMPLE_KEY, "Samples"),
    ConfigInfo::text(SET_NAMES_KEY, "File names"),
    ConfigInfo::text(SET_FOLDER_KEY, "Folder"),
    ConfigInfo::int(SET_MIDDLE_C_KEY, "Middle C is C", 4),
];

#[derive(Ports)]
#[ports(memoryless)]
struct SampleSetPorts {
    #[sample_output("sample", "Sample set")]
    sample: (),
}

static INFO: NodeInfo = NodeInfo {
    id: SAMPLE_SET_ID,
    version: 1,
    name: "Sample Set",
    category: "Instruments",
};

/// The rows a Sample Set node holds.
pub fn rows_of(config: &Config) -> Vec<Row> {
    let entries = parse_set(&CONFIG[0].get_text(config));
    let names = CONFIG[1].get_text(config);
    let mut names = names.split('\n');
    entries
        .into_iter()
        .map(|entry| Row {
            name: names.next().unwrap_or_default().to_owned(),
            entry,
        })
        .collect()
}

/// The config values that hold `rows`: `(sample, names)`.
pub fn rows_to_config(rows: &[Row]) -> (String, String) {
    let entries: Vec<SetEntry> = rows.iter().map(|r| r.entry.clone()).collect();
    (
        set_to_text(&entries),
        rows.iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub fn middle_c_of(config: &Config) -> i64 {
    if CONFIG[3].get_int(config) == 3 { 3 } else { 4 }
}

pub fn folder_of(config: &Config) -> String {
    CONFIG[2].get_text(config)
}

impl NodeType for SampleSet {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(SampleSetPorts::layout())
    }

    fn sample_output(&self, config: &Config, _key: &str) -> Option<String> {
        let text = CONFIG[0].get_text(config);
        (!text.is_empty()).then_some(text)
    }

    fn stateless(&self) -> bool {
        true
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(SampleSetNode))
    }
}

struct SampleSetNode;

impl Node for SampleSetNode {
    fn process(&mut self, _ctx: &Context, _io: Io<'_, '_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: u8, layer: u8, rr: u8) -> SetEntry {
        SetEntry {
            reference: SampleRef::whole(format!("h{root}-{layer}-{rr}")),
            root: Some(root),
            layer,
            rr,
        }
    }

    #[test]
    fn an_entry_round_trips_through_its_text() {
        for e in [entry(61, 2, 3), entry(60, 1, 1), entry(0, 1, 2)] {
            assert_eq!(SetEntry::parse(&e.to_text()).unwrap(), e);
        }
        let windowed = SetEntry {
            reference: SampleRef {
                hash: "abc".into(),
                start: 0.25,
                end: 0.5,
            },
            root: None,
            layer: 1,
            rr: 1,
        };
        assert_eq!(SetEntry::parse(&windowed.to_text()).unwrap(), windowed);
        assert_eq!(entry(61, 2, 3).to_text(), "h61-2-3@61/2#3");
    }

    #[test]
    fn names_are_read_in_any_scheme() {
        let note = |s: &str, mc| parse_name(s, mc).note;
        assert_eq!(note("C#4", 4), Some(61));
        assert_eq!(note("Db4", 4), Some(61));
        assert_eq!(note("c#4", 4), Some(61));
        assert_eq!(note("61", 4), Some(61));
        assert_eq!(note("Piano_C#4_mf", 4), Some(61));
        assert_eq!(note("C3", 3), Some(60));
        assert_eq!(note("C4", 4), Some(60));
        assert_eq!(note("kick", 4), None);
        assert_eq!(note("Piano_A0", 4), Some(21));
    }

    #[test]
    fn names_say_layer_and_take() {
        let info = parse_name("Piano_C4_mf_rr2", 4);
        assert_eq!((info.note, info.rr), (Some(60), Some(2)));
        assert_eq!(info.layer, Some(5.0));
        assert_eq!(parse_name("C4_v3_t2", 4).layer, Some(3.0));
        assert_eq!(parse_name("C4_vel100", 4).layer, Some(100.0));
        assert_eq!(parse_name("C4_t3", 4).rr, Some(3));
        // The bare number is the note, not a take.
        let info = parse_name("62_ff", 4);
        assert_eq!((info.note, info.layer), (Some(62), Some(7.0)));
    }

    #[test]
    fn layers_are_ranked_per_note_softest_first() {
        let files: Vec<(String, String)> = [
            ("C4_ff.wav", "a"),
            ("C4_pp.wav", "b"),
            ("D4_mf.wav", "c"),
            ("C4_pp_rr2.wav", "d"),
        ]
        .iter()
        .map(|(n, h)| ((*n).to_owned(), (*h).to_owned()))
        .collect();
        let rows = rows_from_files(&files, 4);
        let by = |name: &str| rows.iter().find(|r| r.name == name).unwrap().entry.clone();
        assert_eq!(by("C4_pp.wav").layer, 1);
        assert_eq!(by("C4_ff.wav").layer, 2);
        assert_eq!(by("D4_mf.wav").layer, 1, "D4 has one layer");
        assert_eq!(by("C4_pp_rr2.wav").rr, 2);
    }

    #[test]
    fn keys_pick_the_nearest_root_with_ties_to_the_lower() {
        let t = SetTables::build(&[entry(60, 1, 1), entry(64, 1, 1), entry(72, 1, 1)]);
        let root_of = |key| {
            let z = t.layer(key, 1.0).unwrap();
            [60, 64, 72][z.entries[0]]
        };
        assert_eq!(root_of(0), 60);
        assert_eq!(root_of(61), 60);
        assert_eq!(root_of(62), 60, "a tie goes down");
        assert_eq!(root_of(63), 64);
        assert_eq!(root_of(67), 64);
        assert_eq!(root_of(68), 64);
        assert_eq!(root_of(69), 72);
        assert_eq!(root_of(127), 72);
    }

    #[test]
    fn velocity_splits_a_keys_layers_evenly_and_a_short_key_uses_what_it_has() {
        let t = SetTables::build(&[entry(60, 1, 1), entry(60, 2, 1), entry(72, 1, 1)]);
        let layer = |key, v: f32| t.layer(key, v).unwrap().entries[0];
        assert_eq!(layer(60, 0.1), 0);
        assert_eq!(layer(60, 63.0 / 127.0), 0);
        assert_eq!(layer(60, 64.0 / 127.0), 0, "64 is still the softer");
        assert_eq!(layer(60, 65.0 / 127.0), 1, "the layers meet at 64");
        assert_eq!(layer(60, 1.0), 1);
        // The key with one layer plays it whatever the velocity.
        assert_eq!(layer(72, 0.05), 2);
        assert_eq!(layer(72, 1.0), 2);
    }

    #[test]
    fn takes_share_a_layer_in_order() {
        let t = SetTables::build(&[entry(60, 1, 3), entry(60, 1, 1), entry(60, 1, 2)]);
        assert_eq!(t.layer(60, 0.5).unwrap().entries, vec![1, 2, 0]);
        assert_eq!(t.layer_count(), 1);
    }

    #[test]
    fn an_empty_set_or_an_unplaced_entry_plays_nothing() {
        assert!(SetTables::build(&[]).layer(60, 1.0).is_none());
        let mut e = entry(60, 1, 1);
        e.root = None;
        assert!(SetTables::build(&[e]).layer(60, 1.0).is_none());
    }

    fn files(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(n, h)| ((*n).to_owned(), (*h).to_owned()))
            .collect()
    }

    #[test]
    fn a_rescan_finds_added_replaced_removed_and_renamed() {
        let old = rows_from_files(
            &files(&[
                ("C4.wav", "h1"),
                ("D4.wav", "h2"),
                ("E4.wav", "h3"),
                ("F4.wav", "h4"),
            ]),
            4,
        );
        let now = files(&[
            ("C4.wav", "h1"),
            ("D4.wav", "h2b"),
            ("Gb4.wav", "h4"),
            ("A4.wav", "h5"),
        ]);
        let plan = rescan(&old, &now, 4);
        assert_eq!(plan.added, vec!["A4.wav"]);
        assert_eq!(plan.replaced, vec!["D4.wav"]);
        assert_eq!(plan.removed, vec!["E4.wav"]);
        assert_eq!(
            plan.renamed,
            vec![("F4.wav".to_owned(), "Gb4.wav".to_owned())]
        );
        assert_eq!(plan.rows.len(), 4);
    }

    #[test]
    fn a_rescan_keeps_hand_edits_unless_the_file_was_replaced() {
        let mut old = rows_from_files(&files(&[("C4.wav", "h1"), ("D4.wav", "h2")]), 4);
        old[0].entry.root = Some(50);
        old[1].entry.root = Some(51);
        let plan = rescan(&old, &files(&[("C4.wav", "h1"), ("D4.wav", "h2b")]), 4);
        let root = |n: &str| plan.rows.iter().find(|r| r.name == n).unwrap().entry.root;
        assert_eq!(root("C4.wav"), Some(50), "unchanged keeps its edit");
        assert_eq!(root("D4.wav"), Some(62), "replaced is read afresh");
        assert!(
            rescan(
                &plan.rows,
                &files(&[("C4.wav", "h1"), ("D4.wav", "h2b")]),
                4
            )
            .is_empty()
        );
    }

    #[test]
    fn rows_survive_the_trip_through_config() {
        let rows = rows_from_files(&files(&[("C4_pp.wav", "h1"), ("C4_ff.wav", "h2")]), 4);
        let (sample, names) = rows_to_config(&rows);
        let config = Config::new()
            .with(crate::SAMPLE_KEY, noodle_core::Value::Text(sample))
            .with(SET_NAMES_KEY, noodle_core::Value::Text(names));
        assert_eq!(rows_of(&config), rows);
    }

    #[test]
    fn the_node_puts_its_set_on_the_wire() {
        let config = Config::new().with(
            crate::SAMPLE_KEY,
            noodle_core::Value::Text("h@60".to_owned()),
        );
        assert_eq!(
            SampleSet.sample_output(&config, "sample").as_deref(),
            Some("h@60")
        );
        assert_eq!(SampleSet.sample_output(&Config::new(), "sample"), None);
    }
}
