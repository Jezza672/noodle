//! S17: resample and mangle audio.
//!
//! Partial. Reverse is a node, and a processed track can be rendered to a
//! clip from the track menu (not routed here). Slicing a clip at its
//! transients onto a sampler's pads does not exist (#356), so the route
//! covers the processing chain: a source through Distortion and Reverse.

use std::path::Path;

use super::super::{LateChange, Route, Tag};
use super::patch::{Op, Ref, audible, check, run, seeded};
use crate::app::App;

const SRC: Ref = Ref("osc.saw", 0);
const DRIVE: Ref = Ref("effect.distortion", 0);
const REVERSE: Ref = Ref("offline.reverse", 0);
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
    Op::Step("process the source"),
    Op::Add(DRIVE, 300.0, 460.0),
    Op::Wire(SRC, "out", DRIVE, "in"),
    Op::Step("reverse it"),
    Op::Add(REVERSE, 600.0, 460.0),
    Op::Wire(DRIVE, "out", REVERSE, "in"),
    Op::Wire(REVERSE, "out", OUT, "in"),
    Op::Step("listen"),
    Op::Play,
];

const CHANGE_EFFECT: &[Op] = &[
    Op::Step("turn the drive down on the source"),
    Op::Set(DRIVE, "drive", 3.0),
];

pub fn route() -> Route {
    Route {
        story: "S17",
        tag: Tag::Default,
        name: "distort then reverse a source",
        fixture,
        size: None,
        steps: |h, _| run(h, BUILD),
        done: |p, _| {
            check(p, BUILD)?;
            audible(p)
        },
        late: &[LateChange {
            name: "change the effect on the source after reversing",
            steps: |h, _| run(h, CHANGE_EFFECT),
            done: |p, _| check(p, CHANGE_EFFECT),
            keep: &[],
        }],
    }
}
