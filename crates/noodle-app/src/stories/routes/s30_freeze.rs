//! S30: keep a large project responsive by freezing heavy nodes.
//!
//! The project is the story's: 60 tracks, and a reverb beside them. Select
//! the reverb, press Freeze in its properties, then see where the CPU goes:
//! the CPU meters on the nodes, and the Playback cache panel, both from the
//! View menu. The node's badge says Frozen. The late change is unfreezing,
//! editing and freezing again.
//!
//! The fixture holds a fixed 60 tracks, so the route has no size and the
//! cost of each extra track is not measured.

use std::cell::Cell;
use std::path::Path;

use egui::Modifiers;
use noodle_core::Project;

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::patch::{Ref, nodes_of, seed};
use super::{add_tracks, finished, has_tracks, reveal, virtual_session};
use crate::app::App;

thread_local! {
    /// Whether the CPU meters and the cache panel were showing when the steps
    /// ended: views are not in a saved project, so the check cannot look
    /// after the reopen.
    static VIEWS: Cell<(bool, bool)> = const { Cell::new((false, false)) };
}

const VERB: Ref = Ref("effect.reverb", 0);

/// The story's project size.
const TRACKS: usize = 60;

/// 60 tracks, below a patch of a source, a reverb and the output.
fn fixture(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    seed(
        &mut session,
        &[
            ("osc.saw", 60.0, 300.0, &[("frequency", 196.0)]),
            ("effect.reverb", 400.0, 300.0, &[]),
            ("io.output", 800.0, 300.0, &[]),
        ],
        &[(0, "out", 1, "in"), (1, "out", 2, "in")],
    );
    add_tracks(&mut session, TRACKS, 0.0, 700.0);
    finished(session, dir)
}

fn frozen(p: &Project) -> bool {
    nodes_of(p, VERB.0).first().is_some_and(|&n| p.is_frozen(n))
}

/// Selects the reverb by clicking its title, then presses the button the
/// properties panel shows.
fn press(h: &mut Handle, button: &str) {
    // A frozen node renders in the background, which keeps the canvas repainting.
    let _busy = crate::testkit::playing_mode();
    let node = nodes_of(h.probe().project(), VERB.0)[0];
    let row = h.probe().port_rect(node, false, "in");
    let title = egui::Pos2::new(row.center().x, row.top() - 8.0);
    reveal(h, &[title]);
    h.click(title, Modifiers::NONE);
    h.probe().settle();
    let at = h.find(button);
    h.click(at, Modifiers::NONE);
    // The user waits for the render to finish before going on.
    let mut frames = 0;
    while h.probe().freezing() && frames < 400 {
        h.probe().settle();
        std::thread::sleep(std::time::Duration::from_millis(25));
        frames += 1;
    }
    h.waited(f64::from(frames) * 0.025);
}

/// Opens the View menu and picks `item`.
fn from_view(h: &mut Handle, item: &str) {
    let view = h.find("View");
    h.click(view, Modifiers::NONE);
    h.probe().settle();
    let at = h.find(item);
    h.click(at, Modifiers::NONE);
    h.probe().settle();
}

fn steps(h: &mut Handle, _size: usize) {
    VIEWS.with(|v| v.set((false, false)));
    h.step("freeze the reverb");
    press(h, "Freeze");
    h.step("show where the CPU goes");
    from_view(h, "CPU meters");
    h.step("see what the playback cache holds");
    from_view(h, "Playback cache");
    let probe = h.probe();
    let views = (probe.cpu_meters_on(), probe.cache_panel_open());
    VIEWS.with(|v| v.set(views));
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    has_tracks(p.project(), TRACKS)?;
    if !frozen(p.project()) {
        return Err("the reverb is not frozen".into());
    }
    let (cpu, cache) = VIEWS.with(Cell::get);
    if !cpu {
        return Err("the CPU meters are not showing".into());
    }
    if !cache {
        return Err("the Playback cache panel is not open".into());
    }
    Ok(())
}

fn edit_and_refreeze(h: &mut Handle, _size: usize) {
    h.step("unfreeze it");
    press(h, "Unfreeze");
    h.step("freeze it again");
    press(h, "Freeze");
}

pub fn route() -> Route {
    Route {
        story: "S30",
        tag: Tag::Default,
        name: "freeze a heavy node",
        fixture,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "unfreeze, edit and refreeze",
            steps: edit_and_refreeze,
            done,
            keep: &[],
        }],
    }
}
