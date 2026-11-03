//! S15: a tempo-synced rhythmic gate on a sustained pad.
//!
//! Partial. A Clock node pulses at a note division of the project tempo and
//! a VCA lets the pad through on each pulse. The pattern is not editable
//! step by step: there is no step sequencer or gate pattern node (#354), so
//! the only patterns are the clock's divisions. Changing the division to a
//! triplet means choosing it in the node, which is not routed.

use std::path::Path;

use super::super::{Keep, LateChange, Route, Tag};
use super::patch::{Op, Ref, audible, check, nodes_of, run, seeded};
use crate::app::App;
use noodle_core::Project;

const PAD: Ref = Ref("osc.saw", 0);
const PAD2: Ref = Ref("osc.triangle", 0);
const GATE: Ref = Ref("util.vca", 0);
const GATE2: Ref = Ref("util.vca", 1);
const CLOCK: Ref = Ref("generator.clock", 0);
const OUT: Ref = Ref("io.output", 0);

fn fixture(dir: &Path) -> App {
    seeded(
        dir,
        &[
            ("osc.saw", 60.0, 120.0, &[("frequency", 220.0)]),
            ("osc.triangle", 60.0, 520.0, &[("frequency", 330.0)]),
            ("io.output", 1150.0, 300.0, &[]),
        ],
        &[],
    )
}

const BUILD: &[Op] = &[
    Op::Step("add the clock and the gate"),
    Op::Add(CLOCK, 300.0, 604.0),
    Op::Add(GATE, 560.0, 340.0),
    Op::Step("pulse the gate on the clock"),
    Op::Wire(PAD, "out", GATE, "in"),
    Op::Wire(CLOCK, "out", GATE, "level"),
    Op::Wire(GATE, "out", OUT, "in"),
    Op::Step("listen"),
    Op::Play,
];

const SECOND_TRACK: &[Op] = &[
    Op::Step("gate the second pad with the same clock"),
    Op::Add(GATE2, 560.0, 620.0),
    Op::Wire(PAD2, "out", GATE2, "in"),
    Op::Wire(CLOCK, "out", GATE2, "level"),
    Op::Wire(GATE2, "out", OUT, "in"),
];

pub fn route() -> Route {
    Route {
        story: "S15",
        tag: Tag::Default,
        name: "pulse a pad with a clock and a VCA",
        fixture,
        size: None,
        steps: |h, _| run(h, BUILD),
        done: |p, _| {
            check(p, BUILD)?;
            audible(p)
        },
        late: &[LateChange {
            name: "apply the same gate to a second pad",
            steps: |h, _| run(h, SECOND_TRACK),
            done: |p, _| check(p, SECOND_TRACK),
            keep: &[Keep {
                name: "the first gate",
                of: first_gate,
            }],
        }],
    }
}

fn first_gate(p: &Project) -> String {
    let gate = nodes_of(p, "util.vca").first().copied();
    let mut lines: Vec<String> = p
        .graph()
        .connections()
        .filter(|c| Some(c.to.node) == gate)
        .map(|c| format!("{:?}->{:?}", c.from, c.to))
        .collect();
    lines.sort();
    lines.join("\n")
}
