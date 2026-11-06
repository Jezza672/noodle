//! S06: separate a song into stems and remix it.
//!
//! Partial. "Split to stems" is in the clip's menu. It needs the stem model,
//! which is downloaded after the user agrees, so the route stops at that
//! question and declines: the separation itself, and the remix steps after
//! it, are not routed. The clip panel has a stem Quality choice (Fast, Better,
//! Best) and a Re-separate button on clips that have stems; both need the model
//! too, so they are covered by session tests instead (#349).

use std::cell::Cell;

use egui::Modifiers;

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::audio_clip::{clicks_at, clip_of};

thread_local! {
    /// Whether the steps saw the download question and then closed it. The
    /// question is not in a saved project, so the check cannot look for it
    /// after the reopen.
    static ASKED_AND_DECLINED: Cell<bool> = const { Cell::new(false) };
}

fn steps(h: &mut Handle, _size: usize) {
    ASKED_AND_DECLINED.with(|a| a.set(false));
    h.step("open the clip's menu");
    let id = clip_of(h.probe().project());
    let at = h.clip_rect(id).center();
    h.right_click(at);
    h.step("split it to stems");
    let split = h.find_first("Split to stems");
    h.click(split, Modifiers::NONE);
    h.probe().settle();
    h.step("decline the model download");
    let asked = h.probe().has("Download the stem model?");
    let cancel = h.find("Cancel");
    h.click(cancel, Modifiers::NONE);
    h.probe().settle();
    let closed = !h.probe().has("Download the stem model?");
    ASKED_AND_DECLINED.with(|a| a.set(asked && closed));
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    // The split was started, the model question was asked and declined, and
    // nothing was made: no stems appear without the model.
    if !ASKED_AND_DECLINED.with(Cell::get) {
        return Err("the split never reached the model question, or it was left open".into());
    }
    if p.has("Download the stem model?") {
        return Err("the download question is still open".into());
    }
    if p.project().clips().count() != 1 {
        return Err("declining the download changed the clips".into());
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
