//! S12: build a multisampled instrument from a folder of note-named
//! recordings.
//!
//! The folder is dropped on the node editor, which makes a Sample Set that
//! reads each file's note from its name. One Sampler plays the set, so any
//! key plays the nearest recording, repitched, and the cost does not grow
//! with the number of recordings. The late changes are a Rescan of the same
//! folder, which shows what changed before it applies it, and a parameter on
//! the Sampler.
//!
//! The route is sized: it uses the first `size` recordings of the folder.

use std::path::{Path, PathBuf};

use egui::{Key, Modifiers, Pos2};
use noodle_core::{Command, Endpoint, Node, NodeId, Project};
use noodle_io::write_wav;
use noodle_nodes::{SAMPLE_SET_ID, SAMPLER_ID, TRACK_INPUT_ID};

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::{add_node, finished, in_view, reveal, virtual_session};
use crate::app::App;
use crate::session::Edit;

/// The recordings in the folder, by MIDI note: a fourth apart.
const NOTES: [u8; 8] = [48, 53, 58, 63, 68, 73, 78, 83];

fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[note as usize % 12], note as i32 / 12 - 1)
}

fn hz(note: u8) -> f32 {
    440.0 * 2f32.powf((f32::from(note) - 69.0) / 12.0)
}

/// A half-second tone that dies away, written to `path`.
fn tone(path: &Path, hz: f32) {
    let samples: Vec<f32> = (0..24_000)
        .map(|i| {
            let t = i as f32 / 48_000.0;
            0.5 * (-t * 3.0).exp() * (std::f32::consts::TAU * hz * t).sin()
        })
        .collect();
    write_wav(path, &samples, 1, 48_000).expect("the recording is written");
}

fn recording(dir: &Path, note: u8) -> PathBuf {
    dir.join("recordings")
        .join(format!("{}.wav", note_name(note)))
}

/// A folder of half-second tones named after their notes, a MIDI track to
/// play the instrument from, and an output.
fn folder(dir: &Path) -> App {
    std::fs::create_dir_all(dir.join("recordings")).expect("the folder is made");
    for note in NOTES {
        tone(&recording(dir, note), hz(note));
    }
    let mut session = virtual_session(dir);
    for (type_id, x) in [(TRACK_INPUT_ID, 0.0), (noodle_engine::OUTPUT_ID, 900.0)] {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new(type_id).at(x, 300.0),
        })]);
    }
    finished(session, dir)
}

pub fn route() -> Route {
    Route {
        story: "S12",
        tag: Tag::Default,
        name: "drop the folder on a Sample Set and play it with a Sampler",
        fixture: folder,
        size: Some(4),
        steps,
        done,
        late: &[
            LateChange {
                name: "replace one recording",
                steps: replace_one,
                done: one_replaced,
                keep: &[],
            },
            LateChange {
                name: "add three notes to the folder",
                steps: add_three,
                done: three_added,
                keep: &[],
            },
            LateChange {
                name: "change the naming scheme to MIDI numbers",
                steps: rename_to_numbers,
                done: renamed,
                keep: &[],
            },
            LateChange {
                name: "change the bend range",
                steps: wider_bend,
                done: bend_is_wide,
                keep: &[],
            },
        ],
    }
}

fn node_of(p: &Project, type_id: &str) -> NodeId {
    p.graph()
        .nodes()
        .find(|(_, n)| n.type_id == type_id && n.parent.is_none())
        .map_or_else(|| panic!("no {type_id} node"), |(id, _)| id)
}

fn wire(h: &mut Handle, from: (NodeId, &str), to: (NodeId, &str)) {
    let ends = |h: &mut Handle| [h.socket(from.0, true, from.1), h.socket(to.0, false, to.1)];
    let [a, b] = in_view(h, ends);
    h.drag(Modifiers::NONE, &[a, b]);
}

/// Clicks a parameter's field on `node` and types `value` over it.
fn set(h: &mut Handle, node: NodeId, key: &str, value: &str) {
    let at = h.port_row(node, false, key);
    reveal(h, &[at]);
    let at = h.port_row(node, false, key);
    h.click(at, Modifiers::NONE);
    h.press(at, Modifiers::COMMAND, Key::A);
    h.type_text(at, value);
    h.press(at, Modifiers::NONE, Key::Enter);
}

fn folder_of(h: &mut Handle) -> PathBuf {
    h.probe().project_dir().join("recordings")
}

fn steps(h: &mut Handle, size: usize) {
    let project = h.probe().project_dir();
    let dir = folder_of(h);
    // The folder holds `size` recordings: the others are set aside, not part
    // of the route.
    let spare = project.join("unused");
    std::fs::create_dir_all(&spare).expect("a place to set them aside");
    for &note in &NOTES[size..] {
        let from = recording(&project, note);
        std::fs::rename(&from, spare.join(from.file_name().expect("a file")))
            .expect("the recording is set aside");
    }
    h.step("drop the folder on the canvas");
    let spot = h.probe().canvas().center() + egui::vec2(0.0, 160.0);
    h.drop_files(spot, &[dir]);
    let set = node_of(h.probe().project(), SAMPLE_SET_ID);
    let (track, out) = {
        let probe = h.probe();
        let p = probe.project();
        (
            node_of(p, TRACK_INPUT_ID),
            node_of(p, noodle_engine::OUTPUT_ID),
        )
    };
    h.step("add a Sampler");
    let sampler = add_node(h, Pos2::new(700.0, 340.0), "sampler");
    h.step("play the set with it");
    wire(h, (set, "sample"), (sampler, "sample"));
    wire(h, (track, "midi"), (sampler, "in"));
    wire(h, (sampler, "out"), (out, "in"));
}

/// The Sample Set's recordings, as `(file name, root, hash)`.
fn rows(p: &Project) -> Vec<(String, Option<u8>, String)> {
    let node = node_of(p, SAMPLE_SET_ID);
    noodle_nodes::rows_of(&p.graph().node(node).expect("the set").config)
        .into_iter()
        .map(|r| (r.name, r.entry.root, r.entry.reference.hash))
        .collect()
}

/// The hash the store gives a file's audio.
fn hash_of(path: &Path) -> String {
    noodle_nodes::SampleStore::new()
        .import(path)
        .expect("the recording decodes")
}

/// Every key reaches the recording whose root is nearest, from a Sampler fed
/// by the MIDI track and the set, that plays to the output.
fn plays_nearest(p: &Project, roots: &[u8]) -> Result<(), String> {
    let graph = p.graph();
    let set = node_of(p, SAMPLE_SET_ID);
    let sampler = node_of(p, SAMPLER_ID);
    if graph
        .source(&Endpoint::new(sampler, "sample"))
        .is_none_or(|s| s.node != set)
    {
        return Err("the Sampler does not play the Sample Set".into());
    }
    if graph
        .source(&Endpoint::new(sampler, "in"))
        .is_none_or(|s| s.node != node_of(p, TRACK_INPUT_ID))
    {
        return Err("the MIDI track does not play the Sampler".into());
    }
    if graph
        .source(&Endpoint::new(node_of(p, noodle_engine::OUTPUT_ID), "in"))
        .is_none_or(|s| s.node != sampler)
    {
        return Err("the Sampler does not reach the output".into());
    }
    let mut sorted = roots.to_vec();
    sorted.sort_unstable();
    let have: Vec<u8> = {
        let mut have: Vec<u8> = rows(p).iter().filter_map(|r| r.1).collect();
        have.sort_unstable();
        have
    };
    if have != sorted {
        return Err(format!("the set has the keys {have:?}, wanted {sorted:?}"));
    }
    let entries: Vec<noodle_nodes::SetEntry> =
        noodle_nodes::rows_of(&graph.node(set).expect("the set").config)
            .into_iter()
            .map(|r| r.entry)
            .collect();
    let tables = noodle_nodes::SetTables::build(&entries);
    for key in 0..=127u8 {
        let zone = tables
            .layer(key, 1.0)
            .ok_or(format!("key {key} plays nothing"))?;
        let root = entries[zone.entries[0]].root.expect("placed");
        let nearest = sorted
            .iter()
            .min_by_key(|&&r| ((i32::from(r) - i32::from(key)).abs(), r))
            .copied()
            .expect("some roots");
        if root != nearest {
            return Err(format!(
                "key {key} plays the recording at {root}, wanted {nearest}"
            ));
        }
    }
    Ok(())
}

fn done(p: &mut Probe, size: usize) -> Result<(), String> {
    plays_nearest(p.project(), &NOTES[..size])
}

/// Selects the Sample Set and rescans its folder, applying what the dialog
/// shows.
fn rescan(h: &mut Handle) {
    h.step("select the Sample Set");
    let set = node_of(h.probe().project(), SAMPLE_SET_ID);
    // A point on the node's title bar, above its one port.
    let title = |h: &mut Handle| {
        let row = h.probe().port_rect(set, true, "sample");
        Pos2::new(row.left() + 4.0, row.center().y)
    };
    let first = title(h);
    reveal(h, &[first]);
    let at = title(h);
    h.click(at, Modifiers::NONE);
    h.step("rescan the folder");
    let rescan = h.find("Rescan folder");
    h.click(rescan, Modifiers::NONE);
    // Enter takes the diff.
    let at = h.probe().canvas().center();
    h.press(at, Modifiers::NONE, Key::Enter);
}

fn replace_one(h: &mut Handle, size: usize) {
    let note = NOTES[size.min(NOTES.len()) / 2];
    let path = recording(&h.probe().project_dir(), note);
    h.step("replace a recording in the folder");
    tone(&path, hz(note) * 1.02);
    rescan(h);
}

fn one_replaced(p: &mut Probe, size: usize) -> Result<(), String> {
    let note = NOTES[size.min(NOTES.len()) / 2];
    let dir = p.project_dir();
    let want = hash_of(&recording(&dir, note));
    let name = format!("{}.wav", note_name(note));
    let row = rows(p.project())
        .into_iter()
        .find(|r| r.0 == name)
        .ok_or("the recording is gone from the set")?;
    if row.2 != want {
        return Err("the set still has the old audio".into());
    }
    plays_nearest(p.project(), &NOTES[..size])
}

/// Three notes the folder did not have.
const EXTRA: [u8; 3] = [50, 55, 60];

fn add_three(h: &mut Handle, _size: usize) {
    let dir = h.probe().project_dir();
    h.step("add three recordings to the folder");
    for note in EXTRA {
        tone(&recording(&dir, note), hz(note));
    }
    rescan(h);
}

fn three_added(p: &mut Probe, size: usize) -> Result<(), String> {
    let mut roots = NOTES[..size].to_vec();
    roots.extend(EXTRA);
    plays_nearest(p.project(), &roots)
}

fn rename_to_numbers(h: &mut Handle, _size: usize) {
    let dir = folder_of(h);
    h.step("rename the recordings to MIDI numbers");
    for entry in std::fs::read_dir(&dir).expect("the folder") {
        let path = entry.expect("an entry").path();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let info = noodle_nodes::parse_name(stem, 4);
        if let Some(note) = info.note {
            std::fs::rename(&path, dir.join(format!("{note}.wav"))).expect("renamed");
        }
    }
    rescan(h);
}

fn renamed(p: &mut Probe, size: usize) -> Result<(), String> {
    let mut roots = NOTES[..size].to_vec();
    roots.extend(EXTRA);
    let names = rows(p.project());
    if names
        .iter()
        .any(|r| r.0.contains('#') || !r.0.chars().next().is_some_and(|c| c.is_ascii_digit()))
    {
        return Err(format!(
            "the set still has the old names: {:?}",
            names.iter().map(|r| &r.0).collect::<Vec<_>>()
        ));
    }
    plays_nearest(p.project(), &roots)
}

fn wider_bend(h: &mut Handle, _size: usize) {
    let sampler = node_of(h.probe().project(), SAMPLER_ID);
    h.step("change the Sampler's bend range");
    set(h, sampler, "bend_range", "12");
}

fn bend_is_wide(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let sampler = project
        .graph()
        .node(node_of(project, SAMPLER_ID))
        .ok_or("the Sampler is gone")?;
    let bend = sampler.params.get("bend_range").copied().unwrap_or(2.0);
    if (bend - 12.0).abs() > 0.01 {
        return Err(format!("the bend range is {bend}, wanted 12"));
    }
    Ok(())
}
