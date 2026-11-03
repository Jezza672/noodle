//! S06: separate a song into stems and remix it.
//!
//! Partial. "Split to stems" is in the clip's menu. It needs the stem model,
//! which is downloaded after the user agrees, so the route stops at that
//! question and declines: the separation itself, and the remix steps after
//! it, are not routed. The clip panel has a stem Quality choice (Fast, Better,
//! Best) and a Re-separate button on clips that have stems; both need the model
//! too, so they are covered by session tests instead (#349).

use egui::Modifiers;

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::audio_clip::{clicks_at, clip_of};

fn steps(h: &mut Handle, _size: usize) {
    h.step("open the clip's menu");
    let id = clip_of(h.probe().project());
    let at = h.clip_rect(id).center();
    h.right_click(at);
    h.step("split it to stems");
    let split = h.find_first("Split to stems");
    h.click(split, Modifiers::NONE);
    h.probe().settle();
    h.step("decline the model download");
    let cancel = h.find("Cancel");
    h.click(cancel, Modifiers::NONE);
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    // The question was answered, and the model was not fetched.
    if p.has("Download the stem model?") {
        return Err("the download question is still open".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S06",
        tag: Tag::Default,
        name: "start a stem split and decline the model download",
        fixture: |dir| clicks_at(dir, 100.0),
        size: None,
        steps,
        done,
        late: &[],
    }
}
