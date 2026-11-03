//! S05: conform audio to a new tempo and key.
//!
//! Partial. Select the clip and press Warp in its panel: it follows the
//! timeline's tempo from the tempo found in the audio. There is no clip
//! pitch shift, project key or formant preservation (#348), so the key half
//! of the story is not routed. Changing the project tempo afterwards
//! is a tempo-lane edit that the warp follows; that late change is not
//! routed yet.

use egui::Modifiers;

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::audio_clip::{clicks_at, clip_of};

fn steps(h: &mut Handle, _size: usize) {
    h.step("select the clip");
    let id = clip_of(h.probe().project());
    let at = h.clip_rect(id).center();
    h.click(at, Modifiers::NONE);
    h.probe().settle();
    h.step("warp it to the project's tempo");
    let warp = h.find("Warp");
    h.click(warp, Modifiers::NONE);
    h.waited(0.5);
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let id = clip_of(p.project());
    let audio = p
        .project()
        .clip(id)
        .and_then(|c| c.as_audio())
        .ok_or("the clip")?;
    if audio.warp.is_none() {
        return Err("the clip is not warped".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S05",
        tag: Tag::Default,
        name: "warp a 100 bpm phrase to the project",
        fixture: |dir| clicks_at(dir, 100.0),
        size: None,
        steps,
        done,
        late: &[],
    }
}
