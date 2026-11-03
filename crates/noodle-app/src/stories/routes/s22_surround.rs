//! S22: produce a surround mix.
//!
//! Partial. Surround Pan places a source among the speakers of a layout and
//! Convert Layout folds the result down to stereo to check it. There is no
//! 3D object panning with automation, no 7.1.4 layout and no ADM export
//! (#359). The route builds the bed: a source panned in 5.1, folded down.

use std::path::Path;

use super::super::{LateChange, Route, Tag};
use super::patch::{Op, Ref, audible, check, run, seeded};
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

const MOVE_OBJECT: &[Op] = &[
    Op::Step("move the source to the right rear"),
    Op::Set(PAN, "azimuth", 110.0),
];

pub fn route() -> Route {
    Route {
        story: "S22",
        tag: Tag::Default,
        name: "a 5.1 pan folded down to stereo",
        fixture,
        size: None,
        steps: |h, _| run(h, BUILD),
        done: |p, _| {
            check(p, BUILD)?;
            audible(p)
        },
        late: &[LateChange {
            name: "move one object's position",
            steps: |h, _| run(h, MOVE_OBJECT),
            done: |p, _| check(p, MOVE_OBJECT),
            keep: &[],
        }],
    }
}
