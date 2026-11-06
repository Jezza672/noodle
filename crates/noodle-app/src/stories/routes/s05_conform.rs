//! S05: conform audio to a new tempo and key.
//!
//! Select the clip and press Warp in its panel: it follows the timeline's
//! tempo from the tempo found in the audio. Its Key row says what key it is
//! in (Detect key finds it from the audio; the route sets it by hand, since the
//! fixture is clicks), and it is carried to the timeline's key, which the
//! ruler's menu sets for the project. Its Keep formants box carries the
//! voice's formants back after the shift.
//!
//! The late change modulates the song: a marker's menu sets a new key from
//! there on, and the clip follows it.

use egui::{Modifiers, vec2};
use noodle_core::{Key, KeyTarget, Mode, Project, Transpose};

use super::super::handle::{Handle, Probe};
use super::super::{Keep, LateChange, Route, Tag};
use super::audio_clip::{clicks_at, clip_of};

const A_MINOR: Key = Key {
    tonic: 9,
    mode: Mode::Minor,
};
const F_MINOR: Key = Key {
    tonic: 5,
    mode: Mode::Minor,
};
const G_MINOR: Key = Key {
    tonic: 7,
    mode: Mode::Minor,
};

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
    h.step("say the clip is in A minor");
    let from = h.probe().find_value("Not set");
    h.click(from, Modifiers::NONE);
    let key = h.find("A minor");
    h.click(key, Modifiers::NONE);
    h.step("keep the formants natural");
    let keep = h.find("Keep formants");
    h.click(keep, Modifiers::NONE);
    h.step("set the project's key to F minor");
    let at = h.ruler_at(1.0);
    h.right_click(at);
    let submenu = h.find_like("Markers and selection");
    h.click(submenu, Modifiers::NONE);
    let key = h.find_like("Key at the start");
    h.click(key, Modifiers::NONE);
    let key = h.find("F minor");
    h.click(key, Modifiers::NONE);
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let id = clip_of(p.project());
    let audio = p
        .project()
        .clip(id)
        .and_then(|c| c.as_audio())
        .ok_or("the clip")?;
    let warp = audio
        .warp
        .and_then(|w| p.project().warp(w))
        .ok_or("the clip is not warped")?;
    // The clip is carried to the project's key. The key it starts from is
    // whatever the user set or detected, so it is not compared.
    if !matches!(
        warp.transpose,
        Some(Transpose {
            to: KeyTarget::Follow,
            keep_formants: true,
            ..
        })
    ) {
        return Err(format!(
            "the clip key is {:?}; warp {:?}",
            warp.transpose,
            warp.to_text()
        ));
    }
    if p.project().key() != F_MINOR {
        return Err(format!("the project is in {}", p.project().key()));
    }
    Ok(())
}

fn modulate(h: &mut Handle, _size: usize) {
    h.step("mark the second bar");
    let at = h.ruler_at(3.0);
    h.right_click(at);
    let submenu = h.find_like("Markers and selection");
    h.click(submenu, Modifiers::NONE);
    let item = h.find_like("Add marker");
    h.click(item, Modifiers::NONE);
    h.step("change the key there to G minor");
    let flag = h.ruler_at(2.05) + vec2(6.0, 0.0);
    h.right_click(flag);
    let same = h.probe().find_value("Same as before");
    h.click(same, Modifiers::NONE);
    let key = h.find("G minor");
    h.click(key, Modifiers::NONE);
}

fn modulated(p: &mut Probe, _size: usize) -> Result<(), String> {
    done(p, 0)?;
    let keys: Vec<_> = p.project().markers().iter().map(|m| m.key).collect();
    if keys != [Some(G_MINOR)] {
        return Err(format!("the markers' keys are {keys:?}"));
    }
    Ok(())
}

fn warp_text(p: &Project) -> String {
    p.warps()
        .map(|(_, warp)| warp.to_text())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn route() -> Route {
    Route {
        story: "S05",
        tag: Tag::Default,
        name: "warp a 100 bpm phrase to the project and carry it to F minor",
        fixture: |dir| clicks_at(dir, 100.0),
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "modulate to G minor halfway",
            steps: modulate,
            done: modulated,
            keep: &[Keep {
                name: "the clip's warp",
                of: warp_text,
            }],
        }],
    }
}
