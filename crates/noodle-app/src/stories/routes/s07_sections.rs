//! S07: restructure an arrangement by section.
//!
//! A section is the span between two named markers, across all tracks. The
//! route marks the verse and the chorus from the ruler's menu, selects the
//! verse (its marker's menu), copies it and pastes it after the chorus: the
//! clips, the automation and the tempo come along, and the copy is not linked
//! to the verse. The late change, lengthening the chorus by four bars, is
//! two actions: insert time (Edit menu), then copy and paste.
//!
//! Reordering and deleting whole sections are the same copy, paste, cut and
//! Delete Time on a selection, so the route covers one of each kind.

use std::path::Path;

use egui::{Key, Modifiers, Pos2, vec2};
use noodle_core::group::{GROUP_OUTPUT, TRACK_INPUT, create_track};
use noodle_core::{
    AutomationLane, AutomationPoint, Clip, Command, Curve, Endpoint, Position, Project,
    TempoChange, TempoMap, Tick,
};

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

const BAR: i64 = 3840;

/// A song of a two bar verse (120 bpm) and a two bar chorus (100 bpm): a MIDI
/// clip a bar long on one track, and a gain lane with a point at each bar
/// whose value says which bar it is. Zoomed out so nine bars fit.
fn fixture(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    let (_, command) = create_track(None, Position { x: 0.0, y: 0.0 }, || session.new_node_id());
    session.edit([Edit::Apply(command)]);
    let find = |session: &crate::session::Session, type_id: &str| {
        session
            .project()
            .graph()
            .nodes()
            .find(|(_, node)| node.type_id == type_id)
            .map(|(id, _)| id)
            .expect("the track has its nodes")
    };
    let input = find(&session, TRACK_INPUT);
    let output = find(&session, GROUP_OUTPUT);
    for bar in 0..4 {
        let id = session.project().next_clip_id();
        session.edit([Edit::Apply(Command::AddClip {
            id,
            clip: Clip::midi(input, Tick(bar * BAR), Tick(BAR)),
        })]);
    }
    let points = (0..4)
        .map(|bar| AutomationPoint {
            tick: Tick(bar * BAR),
            value: (bar + 1) as f32 / 10.0,
            curve: Curve::Linear,
        })
        .collect();
    let id = session.project().next_lane_id();
    session.edit([Edit::Apply(Command::AddLane {
        id,
        lane: AutomationLane::new(Endpoint::new(output, "gain.1"), points),
    })]);
    let map = TempoMap::new(
        vec![
            TempoChange {
                tick: Tick::ZERO,
                bpm: 120.0,
            },
            TempoChange {
                tick: Tick(2 * BAR),
                bpm: 100.0,
            },
        ],
        session.project().tempo_map().signatures().to_vec(),
    )
    .expect("a valid tempo map");
    session.edit([Edit::Apply(Command::SetTempoMap(map))]);
    let mut app = finished(session, dir);
    app.timeline_mut().set_zoom(40.0);
    app
}

/// Seconds into the song at the start of bar `bar` (counted from 0), with
/// the tempo the project has now.
fn at_bar(h: &mut Handle, bar: f64) -> f64 {
    let map = h.probe().project().tempo_map().clone();
    // Between whole bars, interpolate on the bar's own length.
    let whole = bar.floor();
    let start = map.seconds_at(Tick(whole as i64 * BAR));
    let end = map.seconds_at(Tick((whole as i64 + 1) * BAR));
    start + (end - start) * (bar - whole)
}

/// Right-clicks the ruler in the middle of bar `bar` (counted from 0) and
/// picks an entry of its menu.
fn ruler_menu(h: &mut Handle, bar: f64, entry: &str) {
    let seconds = at_bar(h, bar + 0.5);
    let at = h.ruler_at(seconds);
    h.right_click(at);
    let submenu = h.find_like("Markers and selection");
    h.click(submenu, Modifiers::NONE);
    let item = h.find_like(entry);
    h.click(item, Modifiers::NONE);
}

/// Right-clicks the flag of the marker at the start of bar `bar`.
fn marker_menu(h: &mut Handle, bar: f64) -> Pos2 {
    let seconds = at_bar(h, bar);
    let at = h.ruler_at(seconds + 0.05) + vec2(6.0, 0.0);
    h.right_click(at);
    at
}

/// Marks bar `bar` (from 0) and names the marker.
fn add_marker(h: &mut Handle, bar: f64, was: &str, name: &str) {
    ruler_menu(h, bar, "Add marker");
    let seconds = at_bar(h, bar);
    let flag = h.ruler_at(seconds + 0.05) + vec2(6.0, 0.0);
    h.right_click(flag);
    let properties = h.find_like("Properties");
    h.click(properties, Modifiers::NONE);
    let field = h.probe().find_value(was);
    h.click(field, Modifiers::NONE);
    h.press(field, Modifiers::COMMAND, Key::A);
    h.type_text(field, name);
    h.press(field, Modifiers::NONE, Key::Enter);
}

/// Moves the playhead to the start of bar `bar` by clicking the ruler.
fn seek(h: &mut Handle, bar: f64) {
    // A little to the right of the bar line, which snaps back to it: the
    // loop and punch handles sit on the line and would take the click.
    let seconds = at_bar(h, bar) + 0.15;
    let at = h.ruler_at(seconds);
    h.click(at, Modifiers::NONE);
}

/// A point over the arrangement, for key chords.
fn over_tracks(h: &mut Handle) -> Pos2 {
    let seconds = at_bar(h, 1.0);
    h.ruler_at(seconds) + vec2(0.0, 120.0)
}

fn copy(h: &mut Handle) {
    let at = over_tracks(h);
    h.press(at, Modifiers::COMMAND, Key::C);
}

fn paste_at(h: &mut Handle, bar: f64) {
    seek(h, bar);
    let at = over_tracks(h);
    h.press(at, Modifiers::COMMAND, Key::V);
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("mark the verse");
    add_marker(h, 0.0, "Marker 1", "Verse");
    h.step("mark the chorus");
    add_marker(h, 2.0, "Marker 1", "Chorus");
    h.step("select the verse, up to the chorus");
    marker_menu(h, 0.0);
    let item = h.find_like("Select to");
    h.click(item, Modifiers::NONE);
    h.step("copy it");
    copy(h);
    h.step("paste it after the chorus");
    paste_at(h, 4.0);
}

fn starts(p: &Project) -> Vec<i64> {
    let mut starts: Vec<i64> = p.clips().map(|(_, clip)| clip.start.0 / BAR).collect();
    starts.sort();
    starts
}

fn gain(p: &Project) -> Vec<(i64, f32)> {
    p.lanes()
        .next()
        .map(|(_, lane)| {
            lane.points
                .iter()
                .map(|point| (point.tick.0 / BAR, point.value))
                .collect()
        })
        .unwrap_or_default()
}

fn tempos(p: &Project) -> Vec<(i64, f64)> {
    p.tempo_map()
        .tempos()
        .iter()
        .map(|t| (t.tick.0 / BAR, t.bpm))
        .collect()
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let names: Vec<_> = project
        .markers()
        .iter()
        .map(|m| (m.name.as_str(), m.at.0 / BAR))
        .collect();
    if names != [("Verse", 0), ("Chorus", 2)] {
        return Err(format!("the markers are {names:?}"));
    }
    if starts(project) != [0, 1, 2, 3, 4, 5] {
        return Err(format!("the clips start at bars {:?}", starts(project)));
    }
    // The copy has the verse's automation, and the verse's tempo.
    let lane = gain(project);
    if !lane.contains(&(4, 0.1)) || !lane.contains(&(5, 0.2)) {
        return Err(format!("the gain lane is {lane:?}"));
    }
    let tempo = tempos(project);
    if tempo != [(0, 120.0), (2, 100.0), (4, 120.0), (6, 100.0)] {
        return Err(format!("the tempo changes are {tempo:?}"));
    }
    Ok(())
}

fn lengthen(h: &mut Handle, _size: usize) {
    h.step("select the four bars after the chorus");
    ruler_menu(h, 4.0, "Start selection here");
    ruler_menu(h, 8.0, "End selection here");
    h.step("insert that much time");
    let edit = h.find("Edit");
    h.click(edit, Modifiers::NONE);
    let item = h.find("Insert Time");
    h.click(item, Modifiers::NONE);
    h.step("select the chorus");
    ruler_menu(h, 2.0, "Start selection here");
    ruler_menu(h, 4.0, "End selection here");
    h.step("copy it and paste it into the gap, twice");
    copy(h);
    paste_at(h, 4.0);
    paste_at(h, 6.0);
}

fn lengthened(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    if starts(project) != [0, 1, 2, 3, 4, 5, 6, 7, 8, 9] {
        return Err(format!("the clips start at bars {:?}", starts(project)));
    }
    let tempo = tempos(project);
    if tempo != [(0, 120.0), (2, 100.0), (8, 120.0), (10, 100.0)] {
        return Err(format!("the tempo changes are {tempo:?}"));
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S07",
        tag: Tag::Default,
        name: "copy the verse after the chorus",
        fixture,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "lengthen the chorus by four bars",
            steps: lengthen,
            done: lengthened,
            keep: &[],
        }],
    }
}
