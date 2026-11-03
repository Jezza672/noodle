//! S30: keep a large project responsive by freezing heavy nodes.
//!
//! Full for the freeze itself: select a node, press Freeze in its
//! properties, change it, freeze again. The 60-track size is not exercised
//! and the CPU meter (where is the CPU going?) is not routed.

use std::path::Path;

use egui::Modifiers;
use noodle_core::Project;

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::patch::{Ref, nodes_of, seeded};
use super::reveal;
use crate::app::App;

const VERB: Ref = Ref("effect.reverb", 0);

fn fixture(dir: &Path) -> App {
    seeded(
        dir,
        &[
            ("osc.saw", 60.0, 300.0, &[("frequency", 196.0)]),
            ("effect.reverb", 400.0, 300.0, &[]),
            ("io.output", 800.0, 300.0, &[]),
        ],
        &[(0, "out", 1, "in"), (1, "out", 2, "in")],
    )
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
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("freeze the reverb");
    press(h, "Freeze");
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    frozen(p.project())
        .then_some(())
        .ok_or_else(|| "the reverb is not frozen".into())
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
