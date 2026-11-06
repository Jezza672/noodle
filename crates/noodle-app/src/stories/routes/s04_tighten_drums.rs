//! S04: tighten a multitrack drum recording.
//!
//! Partial. Two mics (a close one and the overheads, a few milliseconds
//! later) are linked clips, so they share one warp. Right-click a clip and
//! "Quantise transients…" pins one marker per hit across the link (the hits
//! the mics hear apart merged), pulls them onto the 16th grid at a strength
//! (70% here) and joins the slices by crossfading them (Slices mode, which cuts
//! every mic at the shared markers so they stay in phase), as one undo step.
//! The markers are drawn on the clips. What is still missing is the quantise
//! regions for a fill, which are edited in the panel, not on the clip.
//!
//! The done check covers the pinning (the clips share one warp, and its
//! markers sit on the hits, one per hit across both mics, not just somewhere)
//! and the quantise: on, at the grid and strength asked for, in Slices mode.
//! A late change turns the strength down and fills the gaps instead.

use std::path::Path;

use egui::Modifiers;
use noodle_core::{Clip, Command, Node, Project, Tick};
use noodle_nodes::TRACK_INPUT_ID;

use super::super::handle::{Handle, Probe};
use super::super::{Keep, LateChange, Route, Tag};
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
    h.step("quantise the hits across both mics");
    let id = first_clip(h.probe().project());
    let at = h.clip_rect(id).center();
    h.right_click(at);
    let quantise = h.find_like("Quantise transients");
    h.click(quantise, Modifiers::NONE);
    h.probe().settle();
    // The strength starts at 100 %; dragging its number left turns it down.
    let strength = h.field_after("Strength");
    h.drag_between(
        Modifiers::NONE,
        strength,
        strength - egui::Vec2::new(60.0, 0.0),
    );
    let go = h.find("Quantise hits");
    h.click(go, Modifiers::NONE);
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
    // The kicks fall every 0.6 s from 0, the overheads 3 ms later. A marker
    // anywhere else is not on a hit.
    for m in &warp.markers {
        let off = (m.seconds / 0.6 - (m.seconds / 0.6).round()).abs() * 0.6;
        if off > 0.01 {
            return Err(format!("a marker at {:.3} s is not on a hit", m.seconds));
        }
    }
    let quantise = warp.quantise.as_ref().ok_or("quantise is off")?;
    if (quantise.grid, (quantise.amount * 100.0).round()) != (0.25, 70.0) {
        return Err(format!(
            "quantised to {} beats at {:.0} %, wanted the 16th grid at 70 %",
            quantise.grid,
            quantise.amount * 100.0
        ));
    }
    if warp.mode != noodle_core::WarpMode::Slices {
        return Err(format!("the warp mode is {:?}, wanted Slices", warp.mode));
    }
    Ok(())
}

/// The strength and join, as one setting of the same action.
fn loosen(h: &mut Handle, _size: usize) {
    h.step("quantise again, half as hard, filling the gaps");
    let id = first_clip(h.probe().project());
    let at = h.clip_rect(id).center();
    h.right_click(at);
    let quantise = h.find_like("Quantise transients");
    h.click(quantise, Modifiers::NONE);
    h.probe().settle();
    let strength = h.field_after("Strength");
    h.drag_between(
        Modifiers::NONE,
        strength,
        strength - egui::Vec2::new(70.0, 0.0),
    );
    let fill = h.find("Fill gaps by stretching");
    h.click(fill, Modifiers::NONE);
    let go = h.find("Quantise hits");
    h.click(go, Modifiers::NONE);
    h.probe().settle();
}

fn loosened(p: &mut Probe, _size: usize) -> Result<(), String> {
    let id = first_clip(p.project());
    let (_, warp) = p.project().warp_of(id).ok_or("the clips are not warped")?;
    let quantise = warp.quantise.as_ref().ok_or("quantise is off")?;
    if (quantise.amount * 100.0).round() != 35.0 {
        return Err(format!("the strength is {:.0} %", quantise.amount * 100.0));
    }
    if warp.mode != noodle_core::WarpMode::Beats {
        return Err(format!("the warp mode is {:?}, wanted Beats", warp.mode));
    }
    Ok(())
}

fn marker_times(p: &Project) -> String {
    let id = first_clip(p);
    p.warp_of(id).map_or(String::new(), |(_, w)| {
        format!(
            "{:?}",
            w.markers
                .iter()
                .map(|m| (m.seconds * 1000.0).round())
                .collect::<Vec<_>>()
        )
    })
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
        late: &[LateChange {
            name: "quantise again, half as hard, filling the gaps",
            steps: loosen,
            done: loosened,
            keep: &[Keep {
                name: "the pinned hits",
                of: marker_times,
            }],
        }],
    }
}
