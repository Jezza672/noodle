//! S04: tighten a multitrack drum recording.
//!
//! Partial. Two mics (a close one and the overheads, a few milliseconds
//! later) are linked clips, so they share one warp. Select a clip, Warp, Pin
//! transients (one marker per hit across the link, the hits the mics hear
//! apart merged) from the clip's menu. Quantise (the 16th grid at 70% is the
//! panel's grid and amount, with regions for the fill) and the warp mode
//! Slices, which cuts every mic at the shared markers so they stay in phase
//! and crossfades overlaps, are set in the clip panel, which a route cannot
//! reach for a linked pair yet (the panel shows for one selected clip).

use std::path::Path;

use egui::Modifiers;
use noodle_core::{Clip, Command, Node, Project, Tick};
use noodle_nodes::TRACK_INPUT_ID;

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

const RATE: u32 = 48_000;

/// Four seconds of kicks at 100 bpm on two tracks, the second a few
/// milliseconds late, linked.
fn fixture(dir: &Path) -> App {
    let gap = (60.0 / 100.0 * RATE as f32) as usize;
    let drums = |late: usize| -> Vec<f32> {
        (0..4 * RATE as usize)
            .map(|i| {
                let k = (i + gap - late) % gap;
                if k < 2_400 {
                    0.8 * (-(k as f32) / 600.0).exp() * (k as f32 * 0.05).sin()
                } else {
                    0.0
                }
            })
            .collect()
    };
    let mut session = virtual_session(dir);
    let mut ids = Vec::new();
    for (file, late) in [("close.wav", 0usize), ("overheads.wav", 150)] {
        let samples = drums(late);
        noodle_io::write_wav(&dir.join(file), &samples, 1, RATE).expect("the audio is written");
        let track = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id: track,
            node: Node::new(TRACK_INPUT_ID),
        })]);
        let id = session.project().next_clip_id();
        let clip = Clip::audio(track, Tick(0), file, samples.len() as u64);
        session.edit([Edit::Apply(Command::AddClip { id, clip })]);
        ids.push(id);
    }
    let link = session
        .project()
        .link_command(&ids)
        .expect("the clips link");
    session.edit([Edit::Apply(link)]);
    finished(session, dir)
}

fn first_clip(p: &Project) -> noodle_core::ClipId {
    p.clips().map(|(id, _)| id).next().expect("a clip")
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("pin the hits across both mics");
    let id = first_clip(h.probe().project());
    let at = h.clip_rect(id).center();
    h.right_click(at);
    let pin = h.find("Pin transients");
    h.click(pin, Modifiers::NONE);
    h.probe().settle();
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let id = first_clip(p.project());
    let (warp_id, warp) = p.project().warp_of(id).ok_or("the clips are not warped")?;
    let others = p
        .project()
        .linked_with(id)
        .into_iter()
        .filter(|&c| p.project().warp_of(c).map(|(w, _)| w) != Some(warp_id))
        .count();
    if others != 0 {
        return Err("the linked clips do not share the warp".into());
    }
    // 4 s at 100 bpm is six or seven hits; each mic hears each once, and the
    // pair count once.
    if !(5..=8).contains(&warp.markers.len()) {
        return Err(format!("{} markers pinned", warp.markers.len()));
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S04",
        tag: Tag::Default,
        name: "pin the hits of two linked mics",
        fixture,
        size: None,
        steps,
        done,
        late: &[],
    }
}
