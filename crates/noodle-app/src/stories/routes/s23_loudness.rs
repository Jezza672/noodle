//! S23: master to loudness targets.
//!
//! Partial. A master chain is a Dynamics node as the limiter and a Normalize
//! node, and the export dialog normalises the mixdown to a loudness target,
//! holds its true peak under a ceiling, dithers to 16 bits, writes the ISRC
//! into the file and a CUE sheet beside it. Choosing the file is a native
//! dialog that a route cannot drive, so the route sets the options and stops
//! before "Export…". There is no album sequence with gaps and crossfades:
//! that waits on the setlist design. DDP is not planned.

use std::path::Path;

use egui::{Modifiers, vec2};

use super::super::handle::{Handle, Probe};
use super::super::{Route, Tag};
use super::patch::{Op, Ref, kind, output, param, run, seeded, upstream};
use crate::app::App;

const SRC: Ref = Ref("osc.saw", 0);
const LIMIT: Ref = Ref("effect.dynamics", 0);
const NORM: Ref = Ref("offline.normalize", 0);
const OUT: Ref = Ref("io.output", 0);

fn fixture(dir: &Path) -> App {
    seeded(
        dir,
        &[
            ("osc.saw", 60.0, 300.0, &[("frequency", 110.0)]),
            ("io.output", 1000.0, 300.0, &[]),
        ],
        &[],
    )
}

const CHAIN: &[Op] = &[
    Op::Step("limit the master"),
    Op::Add(LIMIT, 300.0, 300.0),
    Op::Wire(SRC, "out", LIMIT, "in"),
    Op::Set(LIMIT, "ratio", 20.0),
    Op::Step("normalise it"),
    Op::Add(NORM, 600.0, 300.0),
    Op::Wire(LIMIT, "out", NORM, "in"),
    Op::Wire(NORM, "out", OUT, "in"),
];

fn steps(h: &mut Handle, _size: usize) {
    // The nodes show live values, so the canvas never stops repainting.
    let _busy = crate::testkit::playing_mode();
    run(h, CHAIN);
    h.step("open the export dialog");
    let file = h.find("File");
    h.click(file, Modifiers::NONE);
    h.probe().settle();
    let export = h.find("Export Audio…");
    h.click(export, Modifiers::NONE);
    h.probe().settle();
    h.step("normalise the mixdown to a loudness target");
    // The label is the checkbox's: the step cannot go on without it.
    let normalise = h.find("Normalise mixdown to");
    h.click(normalise, Modifiers::NONE);
    h.probe().settle();
    h.step("hold the true peak under the ceiling");
    let limit = h.find("Limit the mixdown to");
    h.click(limit, Modifiers::NONE);
    h.probe().settle();
    h.step("type the ISRC");
    // The field sits right of its label.
    let isrc = h.find("ISRC") + vec2(110.0, 0.0);
    h.type_text(isrc, "US-RC1-76-07839");
    h.probe().settle();
    h.step("ask for a CUE sheet");
    let cue = h.find("Write one next to the mixdown");
    h.click(cue, Modifiers::NONE);
    h.probe().settle();
}

/// The goal: the master is limited and normalised, by whatever nodes do
/// that. A Dynamics node with a high ratio limits; Normalize normalises.
fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let out = output(project)?;
    let chain = upstream(project, out);
    let limited = chain.iter().any(|&n| {
        kind(project, n) == "effect.dynamics"
            && param(project, n, "ratio").is_some_and(|r| r >= 10.0)
    });
    if !limited {
        return Err("nothing limits the master".into());
    }
    if !chain
        .iter()
        .any(|&n| kind(project, n) == "offline.normalize")
    {
        return Err("nothing normalises the master".into());
    }
    Ok(())
}

pub fn route() -> Route {
    Route {
        story: "S23",
        tag: Tag::Default,
        name: "a limited, normalised master and its export options",
        fixture,
        size: None,
        steps,
        done,
        late: &[],
    }
}
