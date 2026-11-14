//! The routes. One file per story, named `sNN_what.rs`; the framework's own
//! check is `framework_smoke`.

mod audio_clip;
mod framework_smoke;
mod patch;
mod roll;
mod s01_comp;
mod s02_punch;
mod s03_external_synth;
mod s04_tighten_drums;
mod s05_conform;
mod s06_stems;
mod s07_sections;
mod s08_midi_detail;
mod s09_drum_pattern;
mod s10_keyswitch;
mod s11_audio_to_midi;
mod s12_multisampler;
mod s13_macro_patch;
mod s14_shared_modulation;
mod s15_rhythmic_gate;
mod s16_spectral_cross;
mod s17_resample;
mod s17_slice;
mod s18_plugins;
mod s18_replace;
mod s19_drum_bus;
mod s20_automation;
mod s21_mix_versions;
mod s22_surround;
mod s23_loudness;
mod s24_stems_export;
mod s27_clip_launching;
mod s28_setlist;
mod s29_organise;
mod s30_freeze;
mod s31_sounds;
pub mod tripwire;

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{NodeId, Project};

use super::Route;
use super::handle::Handle;
use crate::app::App;
use crate::session::{Edit, Nodes, Session};
use crate::testkit::virtual_audio;

/// Every route, in story order.
pub fn all() -> Vec<Route> {
    vec![
        framework_smoke::route(),
        s01_comp::route(),
        s02_punch::route(),
        s03_external_synth::route(),
        s04_tighten_drums::route(),
        s05_conform::route(),
        s06_stems::route(),
        s07_sections::route(),
        s08_midi_detail::route(),
        s08_midi_detail::humanise_route(),
        s08_midi_detail::wire_route(),
        s09_drum_pattern::route(),
        s09_drum_pattern::pattern_route(),
        s09_drum_pattern::rack_from_clip_route(),
        s10_keyswitch::route(),
        s11_audio_to_midi::route(),
        s12_multisampler::route(),
        s13_macro_patch::route(),
        s14_shared_modulation::route(),
        s15_rhythmic_gate::route(),
        s16_spectral_cross::route(),
        s17_resample::route(),
        s17_slice::route(),
        s17_slice::convert_route(),
        s17_slice::region_route(),
        s18_plugins::route(),
        s18_replace::route(),
        s19_drum_bus::route(),
        s20_automation::route(),
        s21_mix_versions::route(),
        s22_surround::route(),
        s23_loudness::route(),
        s24_stems_export::route(),
        s27_clip_launching::route(),
        s28_setlist::route(),
        s29_organise::route(),
        s30_freeze::route(),
        s31_sounds::route(),
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

/// Adds `n` empty tracks to `session`, in a grid from `(x, y)` so they do not
/// sit on top of each other or of a patch beside them.
pub fn add_tracks(session: &mut Session, n: usize, x: f32, y: f32) {
    for i in 0..n {
        let ids: Vec<_> = (0..16).map(|_| session.new_node_id()).collect();
        let mut ids = ids.into_iter();
        let at = noodle_core::Position {
            x: x + (i % 10) as f32 * 260.0,
            y: y + (i / 10) as f32 * 200.0,
        };
        let (_, create) = noodle_core::group::create_track(None, at, || {
            ids.next().expect("a track needs few ids")
        });
        session.edit([Edit::Apply(create)]);
    }
}

/// Fails unless the project holds at least `n` tracks. A story that names a
/// size (150 tracks, 60 tracks) checks its fixture is that size, so a run on
/// a smaller project says nothing and cannot pass.
pub fn has_tracks(p: &Project, n: usize) -> Result<(), String> {
    let have = crate::timeline::tracks(p).len();
    if have >= n {
        Ok(())
    } else {
        Err(format!(
            "the project has {have} tracks, the story has {n}: the fixture is not the story's"
        ))
    }
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
