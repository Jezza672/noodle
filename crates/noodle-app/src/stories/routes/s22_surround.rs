//! S22: produce a surround or immersive mix.
//!
//! Partial. Surround Pan places a source among the speakers of a layout, up to
//! 7.1.4, and its `elevation` raises the source into the height speakers;
//! Convert Layout folds the result down to stereo to check it. Objects are
//! exported as stems by ticking their group in the export dialog (the
//! pre-pan mono signal) beside the bed and the fold-down. ADM export is not
//! planned (#359). The export dialog is a native file dialog away, so the
//! route stops before it. The route builds the bed in 5.1 and folds it down;
//! the late changes move to 7.1.4 and move one object through the heights.

use std::path::Path;

use egui::{Modifiers, vec2};
use noodle_core::Value;

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::patch::{Op, Ref, audible, check, find, kind, output, run, seeded, upstream};
use crate::app::App;

const SRC: Ref = Ref("osc.saw", 0);
const PAN: Ref = Ref("util.surround_pan", 0);
const FOLD: Ref = Ref("util.convert_layout", 0);
const OUT: Ref = Ref("io.output", 0);

fn fixture(dir: &Path) -> App {
    seeded(
        dir,
        &[
            ("osc.saw", 60.0, 300.0, &[("frequency", 196.0)]),
            ("io.output", 1000.0, 300.0, &[]),
        ],
        &[],
    )
}

const BUILD: &[Op] = &[
    Op::Step("pan the source in the surround layout"),
    Op::Add(PAN, 300.0, 460.0),
    Op::Wire(SRC, "out", PAN, "in"),
    Op::Step("fold it down to stereo for a check"),
    Op::Add(FOLD, 600.0, 460.0),
    Op::Wire(PAN, "out", FOLD, "in"),
    Op::Wire(FOLD, "out", OUT, "in"),
    Op::Step("listen"),
    Op::Play,
];

/// Chooses the pan's layout in the properties panel.
fn choose_layout(h: &mut Handle, label: &str) {
    // The nodes show live values while the project plays.
    let _busy = crate::testkit::playing_mode();
    h.step("select the panner");
    let pan = find(h.probe().project(), PAN).expect("the surround pan");
    let row = h.port_row(pan, true, "out");
    // The row's left end is the node's body, clear of the socket.
    h.click(row - vec2(40.0, 0.0), Modifiers::NONE);
    h.probe().settle();
    h.step("set the layout of the bus");
    let layout = h.find("Layout");
    h.click(layout, Modifiers::NONE);
    h.probe().settle();
    let choice = h.find(label);
    h.click(choice, Modifiers::NONE);
    h.probe().settle();
}

const RAISE: &[Op] = &[
    Op::Step("raise the source into the heights"),
    Op::Set(PAN, "elevation", 45.0),
    Op::Set(PAN, "azimuth", 135.0),
];

fn layout_is(p: &mut Probe, name: &str) -> Result<(), String> {
    let project = p.project();
    let pan = find(project, PAN).ok_or("no surround pan")?;
    let layout = project
        .graph()
        .node(pan)
        .and_then(|n| n.config.get("layout").cloned());
    if layout == Some(Value::Text(name.to_owned())) {
        Ok(())
    } else {
        Err(format!("the layout is {layout:?}, not {name}"))
    }
}

/// The goal: a source is panned in a surround layout and folded down, the
/// fold-down reaching the output. Which node folds down is not checked.
fn folded(p: &mut Probe) -> Result<(), String> {
    let project = p.project();
    let out = output(project)?;
    let chain = upstream(project, out);
    if !chain
        .iter()
        .any(|&n| kind(project, n) == "util.surround_pan")
    {
        return Err("nothing pans the sound in a surround layout".into());
    }
    if !chain
        .iter()
        .any(|&n| kind(project, n) == "util.convert_layout")
    {
        return Err("nothing folds the surround bed down for the output".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S22",
        tag: Tag::Default,
        name: "a 5.1 pan folded down to stereo",
        fixture,
        size: None,
        steps: |h, _| run(h, BUILD),
        done: |p, _| {
            folded(p)?;
            audible(p)
        },
        late: &[
            LateChange {
                name: "change the layout from 5.1 to 7.1.4",
                steps: |h, _| choose_layout(h, "7.1.4 (12 channels)"),
                done: |p, _| layout_is(p, "7.1.4"),
                keep: &[],
            },
            LateChange {
                name: "move one object's path in 3D",
                steps: |h, _| {
                    choose_layout(h, "7.1.4 (12 channels)");
                    run(h, RAISE);
                },
                done: |p, _| {
                    layout_is(p, "7.1.4")?;
                    check(p, RAISE)
                },
                keep: &[],
            },
        ],
    }
}
