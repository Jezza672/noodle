//! S02: punch in on a live overdub.
//!
//! Partial. The punch range is set from the ruler's menu and moved later as
//! a late change. Punch, count-in, pre-roll and input monitoring exist (the
//! track input's `monitor` is Auto from the start, so the route needs no step
//! for it), but there is no headphone cue mix (#345, waiting on sends in the Mix
//! node, #332), and a route cannot feed a live input to record from (#334), so
//! the recording itself is not routed.

use std::path::Path;

use egui::Modifiers;
use noodle_core::{Project, Tick};

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::empty_project;
use crate::app::App;

/// Seconds per bar at the default 120 bpm in 4/4, and ticks per bar.
const BAR: f64 = 2.0;
const BAR_TICKS: i64 = 3840;

fn fixture(dir: &Path) -> App {
    empty_project(dir)
}

/// Right-clicks the ruler in the middle of bar `bar` (counted from 1) and
/// picks an entry of the Punch submenu.
fn punch_menu(h: &mut Handle, bar: u32, entry: &str) {
    let at = h.ruler_at((f64::from(bar) - 0.5) * BAR);
    h.right_click(at);
    let punch = h.find_like("Punch");
    h.click(punch, Modifiers::NONE);
    let item = h.find(entry);
    h.click(item, Modifiers::NONE);
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("set the punch-in at bar 2");
    punch_menu(h, 2, "Set punch in here");
    h.step("set the punch-out at bar 3");
    punch_menu(h, 3, "Set punch out here");
    h.step("switch the punch on");
    punch_menu(h, 1, "Punch on");
}

fn punch(p: &Project) -> (bool, Tick, Tick) {
    let punch = p.punch();
    (punch.on, punch.start, punch.end)
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let (on, start, end) = punch(p.project());
    if !on {
        return Err("the punch is off".into());
    }
    if (start, end) != (Tick(BAR_TICKS), Tick(2 * BAR_TICKS)) {
        return Err(format!(
            "the punch runs {start:?} to {end:?}, wanted bars 2 to 3"
        ));
    }
    Ok(())
}

fn later(h: &mut Handle, _size: usize) {
    h.step("move the punch-out one bar later");
    punch_menu(h, 4, "Set punch out here");
}

fn moved(p: &mut Probe, _size: usize) -> Result<(), String> {
    let (on, start, end) = punch(p.project());
    if !on || start != Tick(BAR_TICKS) || end != Tick(3 * BAR_TICKS) {
        return Err(format!("the punch is {on} {start:?} to {end:?}"));
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S02",
        tag: Tag::Default,
        name: "set a punch range from the ruler",
        fixture,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "move the punch-out one bar later",
            steps: later,
            done: moved,
            keep: &[],
        }],
    }
}
