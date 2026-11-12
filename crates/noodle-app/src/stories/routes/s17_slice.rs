//! S17, the slicing half: slice a clip to a sampler by transient.
//!
//! Two ways to slice. Right-click the clip, Slice to Sampler: a new track holds
//! a Sample node (the clip's part of the file), a Slice node and a Sampler with
//! keytrack off, fed by the track's own MIDI, so each key plays one slice; the
//! sensitivity is the Slice node's config (a late change is one edit there; not
//! routed). Or open the clip in the editor, Warp it to show its hits, pick the
//! hits to cut at, right-click, Convert to sampler: a track holds a Sampler
//! group (a Sample Set into a Sampler, its controls as group inputs) with a key
//! for each stretch between two picked hits. The second route takes that way;
//! a late change there is picking other hits and converting again.
//!
//! Also routed: a region of the clip made a Sample node (Shift+drag, right-click,
//! Convert to sample node). The chain track, Reverse, To Sample, Slice, Sampler
//! (live re-slicing when an effect changes) needs the To Sample node, which is
//! not built yet.

use egui::Modifiers;
use noodle_nodes::{SAMPLE_INPUT_ID, SAMPLE_SET_ID, SAMPLER_ID, SLICE_ID};

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::audio_clip::{clicks_at, clip_of};

fn steps(h: &mut Handle, _size: usize) {
    h.step("slice the clip to a sampler");
    let id = clip_of(h.probe().project());
    let at = h.clip_rect(id).center();
    h.right_click(at);
    let slice = h.find_first("Slice to Sampler");
    h.click(slice, Modifiers::NONE);
    h.probe().settle();
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let graph = p.project().graph();
    let has = |ty: &str| graph.nodes().any(|(_, n)| n.type_id == ty);
    if !has(SLICE_ID) || !has(SAMPLER_ID) {
        return Err("there is no Slice and Sampler".into());
    }
    let wired = graph
        .connections()
        .any(|c| c.from.port == "slices" && c.to.port == "sample");
    if !wired {
        return Err("the slices are not wired to the Sampler".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S17",
        tag: Tag::Default,
        name: "slice a loop to a sampler",
        fixture: |dir| clicks_at(dir, 100.0),
        size: None,
        steps,
        done,
        late: &[],
    }
}

/// Opens the clip in the clip editor and warps it, so its hits show as handles.
fn open_with_hits(h: &mut Handle) -> noodle_core::ClipId {
    h.step("open the clip in the editor and show its hits");
    let id = clip_of(h.probe().project());
    // Selecting the clip opens its panel, which moves the clip.
    let at = h.clip_rect(id).center();
    h.click(at, Modifiers::NONE);
    h.probe().settle();
    let at = h.clip_rect(id).center();
    h.double_click(at);
    h.probe().settle();
    let warp = h.find_first("Warp");
    h.click(warp, Modifiers::NONE);
    // The analysis arrives from a background worker.
    h.waited(0.5);
    id
}

fn convert_steps(h: &mut Handle, _size: usize) {
    let id = open_with_hits(h);
    h.step("pick the hits to cut at");
    // The clicks fall every 0.6 s.
    let first = h.editor_handle(id, 0.6);
    h.click(first, Modifiers::NONE);
    for seconds in [1.2, 2.4] {
        let at = h.editor_handle(id, seconds);
        h.click(at, Modifiers::SHIFT);
    }
    h.step("convert them to a sampler");
    let at = h.editor_handle(id, 2.4);
    h.right_click(at);
    let convert = h.find_like("Convert to sampler");
    h.click(convert, Modifiers::NONE);
    h.probe().settle();
}

fn convert_done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let graph = p.project().graph();
    let set = graph
        .nodes()
        .find(|(_, n)| n.type_id == SAMPLE_SET_ID)
        .map(|(id, _)| id)
        .ok_or("there is no Sample Set")?;
    let sampler = graph
        .nodes()
        .find(|(_, n)| n.type_id == SAMPLER_ID)
        .map(|(id, _)| id)
        .ok_or("there is no Sampler")?;
    if graph.node(set).and_then(|n| n.parent) != graph.node(sampler).and_then(|n| n.parent) {
        return Err("the Sample Set and the Sampler are not in one group".into());
    }
    if !graph
        .connections()
        .any(|c| c.from.node == set && c.to.node == sampler && c.to.port == "sample")
    {
        return Err("the Sample Set does not feed the Sampler".into());
    }
    let Some(noodle_core::Value::Text(text)) = graph
        .node(set)
        .and_then(|n| n.config.get(noodle_nodes::SAMPLE_KEY))
    else {
        return Err("the Sample Set is empty".into());
    };
    let slices = noodle_nodes::parse_set(text).len();
    if slices != 2 {
        return Err(format!(
            "{slices} slices, wanted 2 (one a gap between 3 hits)"
        ));
    }
    Ok(())
}

pub fn convert_route() -> Route {
    Route {
        story: "S17",
        tag: Tag::Default,
        name: "pick hits in the clip editor and convert them to a sampler",
        fixture: |dir| clicks_at(dir, 100.0),
        size: None,
        steps: convert_steps,
        done: convert_done,
        late: &[],
    }
}

fn region_steps(h: &mut Handle, _size: usize) {
    let id = open_with_hits(h);
    h.step("select the stretch to keep");
    let (from, to) = (h.editor_wave(id, 1.2), h.editor_wave(id, 2.4));
    h.drag_between(Modifiers::SHIFT, from, to);
    h.step("convert it to a sample node");
    let inside = h.editor_wave(id, 1.8);
    h.right_click(inside);
    let convert = h.find("Convert to sample node");
    h.click(convert, Modifiers::NONE);
    h.probe().settle();
}

fn region_done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let graph = p.project().graph();
    let nodes: Vec<_> = graph
        .nodes()
        .filter(|(_, n)| n.type_id == SAMPLE_INPUT_ID)
        .collect();
    let [(_, node)] = nodes[..] else {
        return Err(format!("{} Sample nodes, wanted 1", nodes.len()));
    };
    let Some(noodle_core::Value::Text(text)) = node.config.get(noodle_nodes::SAMPLE_KEY) else {
        return Err("the Sample node has no sample".into());
    };
    // Six seconds of file: 1.2 s to 2.4 s.
    let window = noodle_nodes::SampleRef::parse(text);
    if (window.start - 0.2).abs() > 0.02 || (window.end - 0.4).abs() > 0.02 {
        return Err(format!(
            "the node plays {:.2} to {:.2} of the file, wanted 0.2 to 0.4",
            window.start, window.end
        ));
    }
    Ok(())
}

pub fn region_route() -> Route {
    Route {
        story: "S17",
        tag: Tag::Default,
        name: "select a stretch of a clip and make a sample node of it",
        fixture: |dir| clicks_at(dir, 100.0),
        size: None,
        steps: region_steps,
        done: region_done,
        late: &[],
    }
}
