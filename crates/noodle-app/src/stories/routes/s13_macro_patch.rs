//! S13: a layered, modulated synth patch with one macro knob.
//!
//! Partial. Two oscillators are layered in a Mix and run through a filter,
//! distortion and reverb. The macro is a MIDI Control knob feeding three Remap
//! nodes, one per target, each with its own range. There is no macro control
//! that maps one knob to several targets with a range and curve each,
//! so the user builds one by hand. Saving the patch as a preset and seeing
//! the update in another project are not routed.
//!
//! A second route gets the same gesture the short way: "Morph between
//! presets..." on a Lead Synth group adds a Morph node wired into everything
//! the first two presets differ in, and one more inside each nested group that
//! holds differing parameters (the voices), with its `position` as the group's
//! `morph` control, so one knob moves the whole sound.

use std::path::Path;

use egui::Modifiers;
use noodle_core::{Command, Endpoint, NodeId, Position, spare};
use noodle_nodes::MORPH_ID;

use super::virtual_session;
use crate::session::Edit;

use super::super::handle::Probe;
use super::super::{Keep, LateChange, Route, Tag};
use super::patch::{Op, Ref, audible, check, kind, nodes_of, output, run, seeded, upstream};
use crate::app::App;
use noodle_core::Project;

const SINE: Ref = Ref("osc.sine", 0);
const SAW: Ref = Ref("osc.saw", 0);
const MIX: Ref = Ref("util.mix", 0);
const FILTER: Ref = Ref("filter.svf", 0);
const DRIVE: Ref = Ref("effect.distortion", 0);
const VERB: Ref = Ref("effect.reverb", 0);
const OUT: Ref = Ref("io.output", 0);
const KNOB: Ref = Ref("event.control", 0);
const REMAP: [Ref; 3] = [
    Ref("util.remap", 0),
    Ref("util.remap", 1),
    Ref("util.remap", 2),
];

fn fixture(dir: &Path) -> App {
    seeded(
        dir,
        &[
            ("osc.sine", 60.0, 120.0, &[("frequency", 110.0)]),
            ("osc.saw", 60.0, 300.0, &[("frequency", 110.5)]),
            ("io.output", 1150.0, 300.0, &[]),
            ("event.control", 60.0, 560.0, &[]),
        ],
        &[],
    )
}

const BUILD: &[Op] = &[
    Op::Step("layer the oscillators"),
    Op::Add(MIX, 260.0, 380.0),
    Op::Wire(SINE, "out", MIX, "in1"),
    Op::Wire(SAW, "out", MIX, "in2"),
    Op::Step("add the filter, the drive and the reverb"),
    Op::Add(FILTER, 460.0, 380.0),
    Op::Add(DRIVE, 660.0, 380.0),
    Op::Add(VERB, 860.0, 380.0),
    Op::Wire(MIX, "out", FILTER, "in"),
    Op::Wire(FILTER, "low", DRIVE, "in"),
    Op::Wire(DRIVE, "out", VERB, "in"),
    Op::Wire(VERB, "out", OUT, "in"),
    Op::Step("map the macro to the three targets"),
    Op::Add(REMAP[0], 300.0, 604.0),
    Op::Add(REMAP[1], 560.0, 604.0),
    Op::Add(REMAP[2], 820.0, 604.0),
    Op::Set(REMAP[0], "out_min", 200.0),
    Op::Set(REMAP[0], "out_max", 4000.0),
    Op::Set(REMAP[1], "out_max", 20.0),
    Op::Set(REMAP[2], "out_max", 0.8),
    Op::Wire(KNOB, "out", REMAP[0], "in"),
    Op::Wire(KNOB, "out", REMAP[1], "in"),
    Op::Wire(KNOB, "out", REMAP[2], "in"),
    Op::Wire(REMAP[0], "out", FILTER, "cutoff"),
    Op::Wire(REMAP[1], "out", DRIVE, "drive"),
    Op::Wire(REMAP[2], "out", VERB, "mix"),
    Op::Step("listen"),
    Op::Play,
];

pub fn route() -> Route {
    Route {
        story: "S13",
        tag: Tag::Default,
        name: "two layers, one macro knob to three targets",
        fixture,
        size: None,
        steps: |h, _| run(h, BUILD),
        done: |p, _| {
            macro_patch(p)?;
            audible(p)
        },
        late: &[
            LateChange {
                name: "add a third layer",
                steps: |h, _| run(h, THIRD_LAYER),
                done: |p, _| check(p, THIRD_LAYER),
                keep: &[Keep {
                    name: "the macro mapping",
                    of: mapping,
                }],
            },
            LateChange {
                name: "narrow the reverb target's range",
                steps: |h, _| run(h, NARROW),
                done: |p, _| check(p, NARROW),
                keep: &[Keep {
                    name: "the macro mapping",
                    of: mapping,
                }],
            },
        ],
    }
}

const THIRD_LAYER: &[Op] = &[
    Op::Step("add a square layer"),
    Op::Add(Ref("osc.square", 0), 60.0, 440.0),
    Op::Wire(Ref("osc.square", 0), "out", MIX, "in3"),
];

const NARROW: &[Op] = &[
    Op::Step("pull the reverb target's top down"),
    Op::Set(REMAP[2], "out_max", 0.4),
];

/// Which macro output goes where, for the late changes to leave alone.
/// The goal: at least two sound sources layered into the output, and one
/// knob that drives three or more parameters. How the layers are combined
/// and how each target's range is made is the user's business.
fn macro_patch(p: &mut Probe) -> Result<(), String> {
    let project = p.project();
    let out = output(project)?;
    let sources = upstream(project, out)
        .into_iter()
        .filter(|&n| {
            kind(project, n).starts_with("osc.") || kind(project, n).starts_with("sampler")
        })
        .count();
    if sources < 2 {
        return Err(format!(
            "{sources} sound sources reach the output, wanted 2"
        ));
    }
    // The parameters the knob drives itself, through any Remaps that give
    // each target its range.
    let mut reached = 0;
    for k in nodes_of(project, KNOB.0) {
        let mut targets = std::collections::HashSet::new();
        let mut seen = std::collections::HashSet::new();
        let mut todo = vec![k];
        while let Some(n) = todo.pop() {
            for c in project.graph().connections().filter(|c| c.from.node == n) {
                let to = kind(project, c.to.node);
                if to == "util.remap" {
                    if seen.insert(c.to.node) {
                        todo.push(c.to.node);
                    }
                } else if to != "io.output" {
                    targets.insert((c.to.node, c.to.port.clone()));
                }
            }
        }
        reached = reached.max(targets.len());
    }
    if reached < 3 {
        return Err(format!("the knob drives {reached} parameters, wanted 3"));
    }
    Ok(())
}

fn mapping(p: &Project) -> String {
    let remaps = nodes_of(p, "util.remap");
    let mut lines: Vec<String> = p
        .graph()
        .connections()
        .filter(|c| remaps.contains(&c.from.node))
        .map(|c| format!("{:?}->{:?}", c.from, c.to))
        .collect();
    lines.sort();
    lines.join("\n")
}

/// A Lead Synth group wired to the output, with presets to morph between.
fn morph_fixture(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    let template = session
        .library()
        .templates()
        .find(|(_, t)| t.name == "Lead Synth")
        .map(|(id, t)| (id.to_string(), t.clone()))
        .expect("the library has the Lead Synth");
    let mut next = session.new_node_id().0;
    let mut ids = || {
        let id = NodeId(next);
        next += 1;
        id
    };
    let (lead, add) =
        template
            .1
            .instantiate(&template.0, None, Position { x: 300.0, y: 300.0 }, &mut ids);
    session.edit([Edit::Apply(add)]);
    let out = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: out,
        node: noodle_core::Node::new(noodle_engine::OUTPUT_ID).at(900.0, 300.0),
    })]);
    session.edit([Edit::Apply(spare::wire(
        Endpoint::new(lead, "out"),
        Endpoint::new(out, "in"),
    ))]);
    super::finished(session, dir)
}

fn lead_of(p: &Project) -> Option<NodeId> {
    p.graph()
        .nodes()
        .find(|(_, n)| n.parent.is_none() && n.type_id == noodle_core::group::GROUP)
        .map(|(id, _)| id)
}

pub fn morph_route() -> Route {
    Route {
        story: "S13",
        tag: Tag::Default,
        name: "morph a synth between two of its presets with one control",
        fixture: morph_fixture,
        size: None,
        steps: |h, _| {
            h.step("select the synth");
            let lead = lead_of(h.probe().project()).expect("the Lead Synth group");
            let first_input = {
                let ports = h.probe().project().graph().group_ports(lead);
                ports.inputs.first().map(|p| p.name.clone())
            }
            .expect("the synth has an input");
            let title = h.port_row(lead, false, &first_input);
            h.click(title, Modifiers::NONE);
            h.probe().settle();
            h.step("open Morph between presets");
            let header = h.find("Morph between presets...");
            h.click(header, Modifiers::NONE);
            h.probe().settle();
            h.step("morph the first two presets");
            let button = h.find("Morph");
            h.click(button, Modifiers::NONE);
            h.probe().settle();
        },
        done: |p, _| {
            let project = p.project();
            let lead = lead_of(project).ok_or("the group is gone")?;
            let graph = project.graph();
            // The Lead Synth's voice is a nested group, and everything that
            // differs between the presets is inside it: the Morph is there, fed
            // from the synth's own `morph` control.
            let inside = |mut at: Option<NodeId>| {
                while let Some(g) = at {
                    if g == lead {
                        return true;
                    }
                    at = graph.node(g).and_then(|n| n.parent);
                }
                false
            };
            let morphs: Vec<NodeId> = graph
                .nodes()
                .filter(|(_, n)| n.type_id == MORPH_ID && inside(n.parent))
                .map(|(id, _)| id)
                .collect();
            let wired: usize = morphs
                .iter()
                .map(|&m| graph.connections().filter(|c| c.from.node == m).count())
                .sum();
            if morphs.is_empty() || wired < 2 {
                return Err(format!(
                    "{} Morph nodes drive {wired} parameters, wanted 2 or more",
                    morphs.len()
                ));
            }
            if graph.group_ports(lead).input("morph").is_none() {
                return Err("the group has no `morph` control".into());
            }
            Ok(())
        },
        late: &[],
    }
}
