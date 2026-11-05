//! S17, the slicing half: slice a clip to a sampler by transient.
//!
//! Right-click the clip, Slice to Sampler: a new track holds a Sample node
//! (the clip's part of the file), a Slice node and a Sampler with keytrack off,
//! fed by the track's own MIDI, so each key plays one slice. The sensitivity is
//! the Slice node's config (a late change is one edit there; not routed). The
//! chain track, Reverse, To Sample, Slice, Sampler (live re-slicing when an
//! effect changes) needs the To Sample node, which is not built yet.

use egui::Modifiers;
use noodle_nodes::{SAMPLER_ID, SLICE_ID};

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
