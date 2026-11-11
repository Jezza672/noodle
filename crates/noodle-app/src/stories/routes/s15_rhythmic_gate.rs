//! S15: a tempo-synced rhythmic gate on a sustained pad.
//!
//! A Pattern Player loops a one-bar pattern of sixteenth steps, drawn
//! in its step grid, and its `gate` output opens a VCA on the pad. The
//! pattern is a MIDI clip the player owns, so it is edited step by step and
//! follows the project tempo. The triplet change is the grid's "To triplets"
//! button, which moves the notes and the player's step together. Changing the
//! project tempo is not routed.

use std::path::Path;

use egui::{Modifiers, Pos2};

use super::super::handle::Handle;
use super::super::{Keep, LateChange, Route, Tag};
use super::patch::{Op, Ref, audible, check, find, nodes_of, run, seeded};
use crate::app::App;
use noodle_core::Project;

const PAD: Ref = Ref("osc.saw", 0);
const PAD2: Ref = Ref("osc.triangle", 0);
const GATE: Ref = Ref("util.vca", 0);
const GATE2: Ref = Ref("util.vca", 1);
const PLAYER: Ref = Ref("events.pattern", 0);
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
    Op::Step("add the Pattern Player and the gate"),
    Op::Add(PLAYER, 300.0, 604.0),
    Op::Add(GATE, 560.0, 340.0),
    Op::Step("open the gate where the pattern has notes"),
    Op::Wire(PAD, "out", GATE, "in"),
    Op::Wire(PLAYER, "gate", GATE, "level"),
    Op::Wire(GATE, "out", OUT, "in"),
];

/// The steps (of sixteen) that are on, counted from 1.
const STEPS: [usize; 10] = [1, 2, 4, 5, 7, 9, 10, 12, 13, 15];

/// Selects the player, gives it a slot and a row for key 36, and clicks the
/// steps in.
fn draw(h: &mut Handle) {
    let player = find(h.probe().project(), PLAYER).expect("the player");
    h.step("select the player");
    let row = h.probe().port_rect(player, false, "mode");
    h.click(Pos2::new(row.center().x, row.top() - 8.0), Modifiers::NONE);
    h.step("add a pattern");
    let new = h.find("+ New slot");
    h.click(new, Modifiers::NONE);
    h.step("add a row");
    let row = h.find("+ Row");
    h.click(row, Modifiers::NONE);
    h.step("click the steps in");
    for step in STEPS {
        let cell = h.find(&format!("Step {step} on key 36"));
        h.click(cell, Modifiers::NONE);
    }
    h.step("listen");
    h.press_playing(Pos2::new(640.0, 760.0), Modifiers::NONE, egui::Key::Space);
}

fn build(h: &mut Handle, _size: usize) {
    run(h, BUILD);
    draw(h);
}

/// The player's pattern: the starts of its notes, in ticks.
fn starts(p: &Project) -> Vec<i64> {
    let Some(player) = nodes_of(p, "events.pattern").first().copied() else {
        return Vec::new();
    };
    let mut starts: Vec<i64> = p
        .patterns_of(player)
        .filter_map(|(_, c)| c.as_midi())
        .flat_map(|m| m.notes.iter().map(|n| n.start.0))
        .collect();
    starts.sort();
    starts
}

fn drawn(p: &Project) -> Result<(), String> {
    let found = starts(p);
    let wanted: Vec<i64> = STEPS.iter().map(|s| (*s as i64 - 1) * 240).collect();
    if found != wanted {
        return Err(format!("the pattern is {found:?}, not {wanted:?}"));
    }
    Ok(())
}

fn triplets(p: &Project) -> Result<(), String> {
    let player = nodes_of(p, "events.pattern")
        .first()
        .copied()
        .ok_or("no Pattern Player")?;
    let step = p
        .graph()
        .node(player)
        .and_then(|n| n.params.get("step"))
        .copied()
        .unwrap_or(4.0);
    if (step - 9.0).abs() > 0.01 {
        return Err(format!("the step is {step}, not the sixteenth triplet (9)"));
    }
    let found = starts(p);
    let wanted: Vec<i64> = STEPS.iter().map(|s| (*s as i64 - 1) * 160).collect();
    if found != wanted {
        return Err(format!("the pattern is {found:?}, not {wanted:?}"));
    }
    Ok(())
}

const SECOND_TRACK: &[Op] = &[
    Op::Step("gate the second pad with the same clock"),
    Op::Add(GATE2, 560.0, 620.0),
    Op::Wire(PAD2, "out", GATE2, "in"),
    Op::Wire(PLAYER, "gate", GATE2, "level"),
    Op::Wire(GATE2, "out", OUT, "in"),
];

pub fn route() -> Route {
    Route {
        story: "S15",
        tag: Tag::Default,
        name: "gate a pad with a pattern drawn in the step grid",
        fixture,
        size: None,
        steps: build,
        done: |p, _| {
            check(p, BUILD)?;
            drawn(p.project())?;
            audible(p)
        },
        late: &[
            LateChange {
                name: "change the pattern to triplets",
                steps: |h, _| {
                    h.step("convert the pattern");
                    let button = h.find("To triplets");
                    h.click(button, Modifiers::NONE);
                },
                done: |p, _| triplets(p.project()),
                keep: &[],
            },
            LateChange {
                name: "apply the same gate to a second pad",
                steps: |h, _| run(h, SECOND_TRACK),
                done: |p, _| check(p, SECOND_TRACK),
                keep: &[Keep {
                    name: "the first gate",
                    of: first_gate,
                }],
            },
        ],
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
