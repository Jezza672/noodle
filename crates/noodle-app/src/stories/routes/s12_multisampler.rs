//! S12: build a multisampled instrument from a folder of note-named
//! recordings.
//!
//! Done by hand, which is the point of routing it: nothing turns the folder
//! into an instrument (#331). The recordings are dropped on the timeline,
//! then for each one: a Sampler is added and the recording's clip is dragged
//! onto it (which sets its root from the recording's pitch), a Note Split
//! output gets the key range nearest to that note, and the Sampler is wired
//! in and out. Keytrack, on by default, pitch-shifts the other keys. The cost
//! grows with every recording, which the scorecard's per-item number shows.
//!
//! The route is sized: it uses the first `size` recordings of the folder.

use std::path::{Path, PathBuf};

use egui::{Key, Modifiers, Pos2, Vec2};
use noodle_core::{ClipId, Command, Endpoint, Node, NodeId, Project, spare};
use noodle_io::write_wav;
use noodle_nodes::{NOTE_SPLIT_ID, SAMPLER_ID, TRACK_INPUT_ID};

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
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

fn recording(dir: &Path, note: u8) -> PathBuf {
    dir.join("recordings")
        .join(format!("{}.wav", note_name(note)))
}

/// A folder of half-second tones named after their notes, a MIDI track to
/// play the instrument from, and an output.
fn folder(dir: &Path) -> App {
    std::fs::create_dir_all(dir.join("recordings")).expect("the folder is made");
    for note in NOTES {
        let hz = 440.0 * 2f32.powf((f32::from(note) - 69.0) / 12.0);
        let samples: Vec<f32> = (0..24_000)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                0.5 * (-t * 3.0).exp() * (std::f32::consts::TAU * hz * t).sin()
            })
            .collect();
        write_wav(&recording(dir, note), &samples, 1, 48_000).expect("the recording is written");
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
        name: "a Note Split and a Sampler per recording, by hand",
        fixture: folder,
        size: Some(4),
        steps,
        done,
        late: &[],
    }
}

fn node_of(p: &Project, type_id: &str) -> NodeId {
    p.graph()
        .nodes()
        .find(|(_, n)| n.type_id == type_id && n.parent.is_none())
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no {type_id} node"))
}

/// The MIDI track the fixture made (the drop adds an audio track after it).
fn midi_track(p: &Project) -> NodeId {
    node_of(p, TRACK_INPUT_ID)
}

/// The range of keys each recording plays: from halfway to the one below to
/// halfway to the one above, so every key plays the nearest recording.
fn ranges(notes: &[u8]) -> Vec<(u8, u8)> {
    (0..notes.len())
        .map(|i| {
            let low = if i == 0 {
                0
            } else {
                (notes[i - 1] + notes[i]) / 2 + 1
            };
            let high = if i + 1 == notes.len() {
                127
            } else {
                (notes[i] + notes[i + 1]) / 2
            };
            (low, high)
        })
        .collect()
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

/// The audio clips, in timeline order.
fn clips(p: &Project) -> Vec<ClipId> {
    let mut clips: Vec<_> = p
        .clips()
        .filter(|(_, c)| c.as_audio().is_some())
        .map(|(id, c)| (c.start, id))
        .collect();
    clips.sort();
    clips.into_iter().map(|(_, id)| id).collect()
}

/// Pans the canvas a node's width to the left, so the next node lands on
/// empty canvas, as a user scrolls to make room.
fn make_room(h: &mut Handle) {
    let canvas = h.probe().canvas();
    h.wheel(
        canvas.center(),
        egui::MouseWheelUnit::Point,
        Vec2::new(-260.0, 0.0),
    );
}

fn steps(h: &mut Handle, size: usize) {
    let notes = &NOTES[..size];
    let files: Vec<PathBuf> = {
        // The fixture's folder is next to the project; the clips name it.
        let dir = h.probe().project_dir();
        notes.iter().map(|&n| recording(&dir, n)).collect()
    };
    h.step("drop the recordings on the timeline");
    h.drop_files(Pos2::new(400.0, 230.0), &files);
    let (track, out) = {
        let probe = h.probe();
        let p = probe.project();
        (midi_track(p), node_of(p, noodle_engine::OUTPUT_ID))
    };
    let spot = Pos2::new(700.0, 340.0);
    h.step("add a Note Split with an output per recording");
    let split = add_node(h, Pos2::new(300.0, 340.0), "note split");
    let outputs = h.probe().find_value("2");
    h.click(outputs, Modifiers::NONE);
    h.press(outputs, Modifiers::COMMAND, Key::A);
    h.type_text(outputs, &size.to_string());
    h.press(outputs, Modifiers::NONE, Key::Enter);
    wire(h, (track, "midi"), (split, "in"));
    h.step("add a Mix for the Samplers");
    let mix = add_node(h, spot, "util.mix");
    wire(h, (mix, "out"), (out, "in"));
    let clip_ids = clips(h.probe().project());
    for (i, (&note, (low, high))) in notes.iter().zip(ranges(notes)).enumerate() {
        let n = i + 1;
        h.step(&format!("make the Sampler for {}", note_name(note)));
        make_room(h);
        let sampler = add_node(h, spot, "sampler");
        let rect = h.clip_rect(clip_ids[i]);
        let onto = h.port_row(sampler, false, "tune");
        h.drag_between(Modifiers::NONE, rect.center(), onto);
        h.step(&format!("give it the keys nearest {}", note_name(note)));
        set(h, split, &format!("low{n}"), &low.to_string());
        set(h, split, &format!("high{n}"), &high.to_string());
        h.step("wire it in and out");
        wire(h, (split, &format!("out{n}")), (sampler, "in"));
        wire(h, (sampler, "out"), (mix, &spare::mixer_input_key(n)));
    }
}

fn done(p: &mut Probe, size: usize) -> Result<(), String> {
    let project = p.project();
    let notes = &NOTES[..size];
    let graph = project.graph();
    let split = graph
        .nodes()
        .find(|(_, n)| n.type_id == NOTE_SPLIT_ID)
        .map(|(id, _)| id)
        .ok_or("there is no Note Split")?;
    let fed = graph
        .source(&Endpoint::new(split, "in"))
        .is_some_and(|s| s.node == midi_track(project));
    if !fed {
        return Err("the MIDI track does not play the Note Split".into());
    }
    let node = graph.node(split).ok_or("the Note Split is gone")?;
    let outputs = match node.config.get("outputs") {
        Some(noodle_core::Value::Int(n)) => *n as usize,
        _ => 2,
    };
    let param = |n: &Node, key: &str, default: f32| n.params.get(key).copied().unwrap_or(default);
    let out = node_of(project, noodle_engine::OUTPUT_ID);
    let mix = graph
        .source(&Endpoint::new(out, "in"))
        .map(|s| s.node)
        .filter(|&m| graph.node(m).is_some_and(|n| n.type_id == spare::MIXER))
        .ok_or("no Mix feeds the output")?;
    // Every key reaches exactly one Sampler, and it is the nearest
    // recording's, rooted at its note.
    for key in 0..=127u8 {
        let hits: Vec<usize> = (1..=outputs)
            .filter(|n| {
                param(node, &format!("low{n}"), 0.0) <= f32::from(key)
                    && f32::from(key) <= param(node, &format!("high{n}"), 127.0)
            })
            .collect();
        let [n] = hits[..] else {
            return Err(format!("key {key} goes to outputs {hits:?}"));
        };
        let nearest = notes
            .iter()
            .min_by_key(|&&note| (i32::from(note) - i32::from(key)).abs())
            .copied()
            .unwrap_or(0);
        let sampler = graph
            .connections()
            .find(|c| c.from == Endpoint::new(split, format!("out{n}")))
            .map(|c| c.to.node)
            .filter(|&s| graph.node(s).is_some_and(|n| n.type_id == SAMPLER_ID))
            .ok_or(format!("Note Split output {n} plays no Sampler"))?;
        let sampler_node = graph.node(sampler).ok_or("the Sampler is gone")?;
        let root = param(sampler_node, "root", f32::NAN);
        if (root - f32::from(nearest)).abs() > 0.5 {
            return Err(format!(
                "key {key} plays a Sampler rooted at {root}, wanted {nearest}"
            ));
        }
        if param(sampler_node, "keytrack", 1.0) < 0.5 {
            return Err(format!(
                "the Sampler for {nearest} does not follow the keys"
            ));
        }
        let mixed = graph
            .connections()
            .any(|c| c.from.node == sampler && c.to.node == mix);
        if !mixed {
            return Err(format!("the Sampler for {nearest} is not mixed"));
        }
    }
    Ok(())
}
