//! The routes. One file per story, named `sNN_what.rs`; the framework's own
//! check is `framework_smoke`.

mod framework_smoke;
mod s01_comp;
mod s09_drum_pattern;
mod s12_multisampler;
mod s19_drum_bus;
mod s20_automation;

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{NodeId, Project};

use super::Route;
use super::handle::Handle;
use crate::app::App;
use crate::session::{Nodes, Session};
use crate::testkit::virtual_audio;

/// Every route, in story order.
pub fn all() -> Vec<Route> {
    vec![
        framework_smoke::route(),
        s01_comp::route(),
        s09_drum_pattern::route(),
        s12_multisampler::route(),
        s19_drum_bus::route(),
        s20_automation::route(),
    ]
}

/// An empty project that saves to a file in `dir`, playing on the virtual
/// output.
pub fn empty_project(dir: &Path) -> App {
    let path = dir.join("project.ron");
    std::fs::write(&path, Project::new().to_ron()).expect("the project file is written");
    let mut session = Session::open(Nodes::all(), &path).expect("the project opens");
    session.set_audio_config(virtual_audio());
    App::new(session)
}

/// A new, saved session for fixtures that build a project by hand.
pub fn virtual_session(dir: &Path) -> Session {
    let mut session = Session::new(Nodes::all());
    assert!(
        session.save_as(&dir.join("project.ron")),
        "the project saves"
    );
    session
}

/// The fixture's session saved and opened again on the virtual output, so
/// that building it leaves nothing on the undo stack for a route to undo.
pub fn finished(mut session: Session, dir: &Path) -> App {
    session.save();
    let mut session =
        Session::open(Nodes::all(), &dir.join("project.ron")).expect("the project opens");
    session.set_audio_config(virtual_audio());
    App::new(session)
}

/// [`finished`] on the virtual output and the virtual input, which plays
/// `wav`.
pub fn finished_recording(mut session: Session, dir: &Path, wav: &Path) -> App {
    session.save();
    let mut session =
        Session::open(Nodes::all(), &dir.join("project.ron")).expect("the project opens");
    session.set_audio_config(crate::testkit::virtual_input(wav));
    App::new(session)
}

/// Shift+A at `at`, types `query`, Enter on the first result.
pub fn add_node(h: &mut Handle, at: Pos2, query: &str) -> NodeId {
    let before = h.probe().nodes();
    h.press(at, Modifiers::SHIFT, Key::A);
    h.type_text(at, query);
    h.press(at, Modifiers::NONE, Key::Enter);
    added(h, &before, query)
}

/// [`add_node`] for when the app keeps repainting (audio playing, or live
/// values on the canvas), so it cannot wait for the UI to settle.
pub fn add_node_playing(h: &mut Handle, at: Pos2, query: &str) -> NodeId {
    let before = h.probe().nodes();
    h.press_playing(at, Modifiers::SHIFT, Key::A);
    h.type_text_playing(at, query);
    h.press_playing(at, Modifiers::NONE, Key::Enter);
    added(h, &before, query)
}

fn added(h: &mut Handle, before: &[NodeId], query: &str) -> NodeId {
    let after = h.probe().nodes();
    *after
        .iter()
        .find(|id| !before.contains(id))
        .unwrap_or_else(|| panic!("searching for {query:?} added no node"))
}

/// Pans the canvas, as a user scrolls, until every point is on it (they
/// must fit at once). Returns how far the view moved, so the caller can look
/// again for what it is aiming at.
pub fn reveal(h: &mut Handle, points: &[Pos2]) -> egui::Vec2 {
    let canvas = h.probe().canvas().shrink(30.0);
    let shift = |low: f32, high: f32, min: f32, max: f32| {
        if high > max {
            max - high
        } else if low < min {
            min - low
        } else {
            0.0
        }
    };
    let fold = |f: fn(&Pos2) -> f32| {
        let lo = points.iter().map(f).fold(f32::INFINITY, f32::min);
        let hi = points.iter().map(f).fold(f32::NEG_INFINITY, f32::max);
        (lo, hi)
    };
    let (left, right) = fold(|p| p.x);
    let (top, bottom) = fold(|p| p.y);
    let pan = egui::Vec2::new(
        shift(left, right, canvas.left(), canvas.right()),
        shift(top, bottom, canvas.top(), canvas.bottom()),
    );
    if pan != egui::Vec2::ZERO {
        h.wheel(canvas.center(), egui::MouseWheelUnit::Point, pan);
    }
    pan
}

/// Brings the points `at` gives into view on the canvas together: zooms out
/// (the mouse wheel) while they are too far apart to fit, then pans. Returns
/// where they are now.
pub fn in_view<const N: usize>(h: &mut Handle, at: impl Fn(&mut Handle) -> [Pos2; N]) -> [Pos2; N] {
    for _ in 0..12 {
        let points = at(h);
        let canvas = h.probe().canvas().shrink(30.0);
        let span = egui::Rect::from_points(&points);
        if span.width() <= canvas.width() && span.height() <= canvas.height() {
            reveal(h, &points);
            return at(h);
        }
        h.wheel(
            canvas.center(),
            egui::MouseWheelUnit::Line,
            egui::Vec2::new(0.0, -1.0),
        );
        h.probe().settle();
    }
    panic!("the points never fit on the canvas: {:?}", at(h));
}
