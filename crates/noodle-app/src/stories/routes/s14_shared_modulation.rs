//! S14: one LFO, one envelope follower and one random source modulating
//! several parameters on different devices, each with its own depth.
//!
//! A modulation source's output can be wired to any number of parameter
//! sockets, and a Remap before each socket gives it its own depth. The step
//! sequencer to swap in for the LFO is a Pattern Player: Replace with
//! keeps the LFO's wires, because the player's `out` has the LFO's key, and
//! "+ New slot" gives it a pattern to draw.

use std::path::Path;

use egui::{Modifiers, Pos2};

use super::super::handle::{Handle, Probe};
use super::super::{Keep, LateChange, Route, Tag};
use super::patch::{Op, Ref, audible, check, find, kind, nodes_of, run, seeded};
use crate::app::App;
use noodle_core::Project;

const SAW: Ref = Ref("osc.saw", 0);
const NOISE: Ref = Ref("noise.white", 0);
const FILTER: Ref = Ref("filter.svf", 0);
const DRIVE: Ref = Ref("effect.distortion", 0);
const VERB: Ref = Ref("effect.reverb", 0);
const LFO: Ref = Ref("mod.lfo", 0);
const FOLLOW: Ref = Ref("mod.follower", 0);
const HOLD: Ref = Ref("mod.samplehold", 0);
const CLOCK: Ref = Ref("generator.clock", 0);
const DEPTH: Ref = Ref("util.remap", 0);

fn fixture(dir: &Path) -> App {
    seeded(
        dir,
        &[
            ("osc.saw", 60.0, 120.0, &[("frequency", 110.0)]),
            ("noise.white", 60.0, 560.0, &[]),
            ("io.output", 1150.0, 150.0, &[]),
            ("filter.svf", 300.0, 120.0, &[]),
            ("effect.distortion", 560.0, 120.0, &[]),
            ("effect.reverb", 820.0, 120.0, &[]),
        ],
        &[
            (0, "out", 3, "in"),
            (3, "low", 4, "in"),
            (4, "out", 5, "in"),
            (5, "out", 2, "in"),
        ],
    )
}

const BUILD: &[Op] = &[
    Op::Step("add the three sources"),
    Op::Add(LFO, 300.0, 540.0),
    Op::Add(FOLLOW, 560.0, 540.0),
    Op::Add(HOLD, 820.0, 540.0),
    Op::Add(CLOCK, 560.0, 700.0),
    Op::Step("feed the follower and the random source"),
    Op::Wire(SAW, "out", FOLLOW, "in"),
    Op::Wire(NOISE, "out", HOLD, "in"),
    Op::Wire(CLOCK, "out", HOLD, "trigger"),
    Op::Step("send each source to its targets"),
    Op::Wire(LFO, "out", FILTER, "cutoff"),
    Op::Wire(FOLLOW, "out", DRIVE, "drive"),
    Op::Wire(HOLD, "out", VERB, "mix"),
    Op::Wire(LFO, "out", VERB, "damping"),
    Op::Step("listen"),
    Op::Play,
];

pub fn route() -> Route {
    Route {
        story: "S14",
        tag: Tag::Default,
        name: "an LFO, a follower and a random source across three devices",
        fixture,
        size: None,
        steps: |h, _| run(h, BUILD),
        done: |p, _| {
            shared(p)?;
            audible(p)
        },
        late: &[
            LateChange {
                name: "change the LFO's depth",
                steps: |h, _| run(h, DEPTH_DOWN),
                done: |p, _| check(p, DEPTH_DOWN),
                keep: &[Keep {
                    name: "the targets",
                    of: targets,
                }],
            },
            LateChange {
                name: "add a new target",
                steps: |h, _| run(h, NEW_TARGET),
                done: |p, _| check(p, NEW_TARGET),
                keep: &[Keep {
                    name: "the targets",
                    of: targets,
                }],
            },
            LateChange {
                name: "give one target its own depth",
                steps: |h, _| run(h, OWN_DEPTH),
                done: |p, _| check(p, OWN_DEPTH),
                keep: &[],
            },
            LateChange {
                name: "replace the LFO with a step sequencer",
                steps: |h, _| replace_lfo(h),
                done: |p, _| replaced(p),
                keep: &[Keep {
                    name: "the targets",
                    of: sequenced_targets,
                }],
            },
        ],
    }
}

/// The goal, whatever made it: at least three modulation sources (an LFO
/// set to Random does as well as a Sample & Hold), each driving something,
/// at least one of them driving several parameters on different devices.
/// A Remap on the way is the source's own depth, so it is looked through.
fn shared(p: &mut Probe) -> Result<(), String> {
    let project = p.project();
    let is_source = |k: &str| k.starts_with("mod.") || k == "events.pattern";
    let mut sources = 0;
    let mut shared_one = false;
    for (id, _) in project.graph().nodes() {
        if !is_source(&kind(project, id)) {
            continue;
        }
        let mut targets = std::collections::HashSet::new();
        let mut todo = vec![id];
        let mut seen = std::collections::HashSet::new();
        while let Some(n) = todo.pop() {
            for c in project.graph().connections().filter(|c| c.from.node == n) {
                let k = kind(project, c.to.node);
                if k == "util.remap" {
                    if seen.insert(c.to.node) {
                        todo.push(c.to.node);
                    }
                } else if !is_source(&k) && k != "io.output" {
                    targets.insert(c.to.node);
                }
            }
        }
        if !targets.is_empty() {
            sources += 1;
        }
        shared_one |= targets.len() >= 2;
    }
    if sources < 3 {
        return Err(format!(
            "{sources} modulation sources drive something, wanted 3"
        ));
    }
    if !shared_one {
        return Err("no source drives parameters on more than one device".into());
    }
    Ok(())
}

/// Right-clicks the LFO, chooses "Replace with...", picks the Pattern Player,
/// then gives it a slot to draw in.
fn replace_lfo(h: &mut Handle) {
    let node = find(h.probe().project(), LFO).expect("the LFO");
    h.step("open the LFO's menu");
    let row = h.probe().port_rect(node, false, "rate");
    h.right_click(Pos2::new(row.center().x, row.top() - 8.0));
    h.step("choose Replace with");
    let item = h.find("Replace with...");
    h.click(item, Modifiers::NONE);
    h.step("pick the Pattern Player");
    h.type_text(Pos2::new(640.0, 300.0), "pattern player");
    h.press(Pos2::new(640.0, 300.0), Modifiers::NONE, egui::Key::Enter);
    h.step("give it a pattern");
    let row = h.probe().port_rect(node, false, "mode");
    h.click(Pos2::new(row.center().x, row.top() - 8.0), Modifiers::NONE);
    let new = h.find("+ New slot");
    h.click(new, Modifiers::NONE);
}

/// Where the LFO or its replacement sends its signal.
fn sequenced_targets(p: &Project) -> String {
    let sources: Vec<_> = ["mod.lfo", "events.pattern"]
        .iter()
        .flat_map(|k| nodes_of(p, k))
        .collect();
    let mut lines: Vec<String> = p
        .graph()
        .connections()
        .filter(|c| sources.contains(&c.from.node) && c.from.port == "out")
        .map(|c| format!("{:?}", c.to))
        .collect();
    lines.sort();
    lines.join("\n")
}

fn replaced(p: &mut Probe) -> Result<(), String> {
    let project = p.project();
    if !nodes_of(project, "mod.lfo").is_empty() {
        return Err("the LFO is still there".into());
    }
    let player = *nodes_of(project, "events.pattern")
        .first()
        .ok_or("no Pattern Player")?;
    if sequenced_targets(project).lines().count() < 2 {
        return Err("the player lost the LFO's targets".into());
    }
    if project.patterns_of(player).next().is_none() {
        return Err("the player has no pattern to draw in".into());
    }
    Ok(())
}

const DEPTH_DOWN: &[Op] = &[Op::Step("lower the LFO depth"), Op::Set(LFO, "depth", 0.4)];

const NEW_TARGET: &[Op] = &[
    Op::Step("send the follower to the filter resonance as well"),
    Op::Wire(FOLLOW, "out", FILTER, "resonance"),
];

/// A depth that belongs to one target is a Remap between the source and it.
const OWN_DEPTH: &[Op] = &[
    Op::Step("scale the follower for the drive only"),
    Op::Add(DEPTH, 560.0, 420.0),
    Op::Set(DEPTH, "out_max", 0.5),
    Op::Wire(FOLLOW, "out", DEPTH, "in"),
    Op::Wire(DEPTH, "out", DRIVE, "drive"),
];

/// Which source feeds which socket, for the late changes to leave alone.
fn targets(p: &Project) -> String {
    let sources: Vec<_> = ["mod.follower", "mod.samplehold"]
        .iter()
        .flat_map(|k| nodes_of(p, k))
        .collect();
    let mut lines: Vec<String> = p
        .graph()
        .connections()
        .filter(|c| sources.contains(&c.from.node) && c.to.port != "resonance")
        .map(|c| format!("{:?}->{:?}", c.from, c.to))
        .collect();
    lines.sort();
    lines.join("\n")
}
