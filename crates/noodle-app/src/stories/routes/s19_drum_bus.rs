//! S19: a drum bus, a parallel compressed bus, and a bass ducked by the kick.
//!
//! Done in the node editor. A Mix node is the drum bus, and its input gains
//! are the drum faders. A send is a second wire out of a source: the kick and
//! the snare also feed a second Mix node, the parallel bus, straight from
//! their outputs, so the send is taken before the drum faders (pre-fader). A
//! hard Dynamics node compresses it and it joins the master. Switching the
//! send to post-fader means feeding the compressor from the drum bus's
//! output instead and deleting the parallel Mix: nothing in the mixer says
//! so (#332). A Dynamics node with its detector on the sidechain input ducks
//! the bass. Plugin delay compensation is not exercised. The done check reads
//! the wiring and that the mix is audible; it does not measure the bass
//! ducking itself (the sources are steady sines).

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{Command, Endpoint, Node, NodeId, Project, spare};
use noodle_nodes::DYNAMICS_ID;

use super::super::handle::{Handle, Probe, peak};
use super::super::{LateChange, Route, Tag};
use super::{add_node, finished, reveal, virtual_session};
use crate::app::App;
use crate::session::Edit;

const SINE: &str = "noodle.osc.sine";

/// A kick, a snare and a bass (sines at three pitches), a master Mix and an
/// output.
fn mix_session(dir: &Path) -> App {
    let mut session = virtual_session(dir);
    let add = |session: &mut crate::session::Session, node: Node| {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        id
    };
    add(&mut session, named(SINE, 60.0, 80.0));
    add(&mut session, named(SINE, 200.0, 220.0));
    add(&mut session, named(SINE, 55.0, 360.0));
    let master = add(&mut session, Node::new(spare::MIXER).at(900.0, 360.0));
    let out = add(
        &mut session,
        Node::new(noodle_engine::OUTPUT_ID).at(1150.0, 360.0),
    );
    session.edit([Edit::Apply(spare::wire(
        Endpoint::new(master, "out"),
        Endpoint::new(out, "in"),
    ))]);
    finished(session, dir)
}

fn named(type_id: &str, hz: f32, y: f32) -> Node {
    let mut node = Node::new(type_id).at(100.0, y);
    node.params.insert("freq".into(), hz);
    node
}

pub fn route() -> Route {
    Route {
        story: "S19",
        tag: Tag::Default,
        name: "drum bus and a kick-ducked bass",
        fixture: mix_session,
        size: None,
        steps,
        done,
        late: &[
            LateChange {
                name: "duck from the whole drum bus, not only the kick",
                steps: duck_from_bus,
                done: ducks_from_bus,
                keep: &[],
            },
            LateChange {
                name: "switch the send from pre-fader to post-fader",
                steps: post_fader,
                done: is_post_fader,
                keep: &[],
            },
        ],
    }
}

/// The nodes of a type, in ID order.
fn of(p: &Project, type_id: &str) -> Vec<NodeId> {
    p.graph()
        .nodes()
        .filter(|(_, n)| n.type_id == type_id)
        .map(|(id, _)| id)
        .collect()
}

fn feeds(p: &Project, from: NodeId, to: &Endpoint) -> bool {
    p.graph().source(to).is_some_and(|s| s.node == from)
}

fn wire(h: &mut Handle, from: (NodeId, &str), to: (NodeId, &str)) {
    let a = h.socket(from.0, true, from.1);
    let b = h.socket(to.0, false, to.1);
    let (a, b) = if reveal(h, &[a, b]) == egui::Vec2::ZERO {
        (a, b)
    } else {
        (h.socket(from.0, true, from.1), h.socket(to.0, false, to.1))
    };
    h.drag_playing(&[a, b]);
}

fn sources(p: &Project) -> (NodeId, NodeId, NodeId) {
    let s = of(p, SINE);
    (s[0], s[1], s[2])
}

fn steps(h: &mut Handle, _size: usize) {
    let (kick, snare, bass) = sources(h.probe().project());
    let master = of(h.probe().project(), spare::MIXER)[0];
    h.step("add the drum bus");
    let bus = add_node(h, Pos2::new(450.0, 300.0), "util.mix");
    h.step("send the kick and the snare to it");
    wire(h, (kick, "out"), (bus, "in1"));
    wire(h, (snare, "out"), (bus, "in2"));
    h.step("bring the bus into the master");
    wire(h, (bus, "out"), (master, "in1"));
    h.step("duck the bass from the kick");
    let dynamics = add_node(h, Pos2::new(450.0, 520.0), "dynamics");
    wire(h, (bass, "out"), (dynamics, "in"));
    wire(h, (kick, "out"), (dynamics, "sidechain"));
    let detect = h.find("Detect");
    h.click(detect, Modifiers::NONE);
    let side = h.find("Sidechain");
    h.click(side, Modifiers::NONE);
    wire(h, (dynamics, "out"), (master, "in2"));
    h.step("add a parallel bus");
    let parallel = add_node(h, Pos2::new(760.0, 300.0), "util.mix");
    h.step("send the kick and the snare to it, before their faders");
    wire(h, (kick, "out"), (parallel, "in1"));
    wire(h, (snare, "out"), (parallel, "in2"));
    h.step("compress it hard");
    let squash = add_node(h, Pos2::new(760.0, 430.0), "dynamics");
    wire(h, (parallel, "out"), (squash, "in"));
    set(h, squash, "ratio", "20");
    set(h, squash, "threshold", "-30");
    h.step("blend it into the master");
    wire(h, (squash, "out"), (master, "in3"));
    h.step("listen");
    let at = Pos2::new(640.0, 700.0);
    h.press_playing(at, Modifiers::NONE, Key::Space);
    let heard = h.probe().hear(std::time::Duration::from_millis(800));
    *LEVEL.lock().unwrap() = peak(&heard);
    h.press_playing(at, Modifiers::NONE, Key::Space);
}

/// Clicks a parameter's field on `node` and types `value` into it.
fn set(h: &mut Handle, node: NodeId, key: &str, value: &str) {
    let at = h.port_row(node, false, key);
    h.click(at, Modifiers::NONE);
    h.press(at, Modifiers::COMMAND, Key::A);
    h.type_text(at, value);
    h.press(at, Modifiers::NONE, Key::Enter);
}

/// The loudest sample heard while the mix played.
static LEVEL: std::sync::Mutex<f32> = std::sync::Mutex::new(0.0);

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let (kick, snare, _) = sources(project);
    let bus = of(project, spare::MIXER)
        .into_iter()
        .find(|&m| feeds(project, kick, &Endpoint::new(m, "in1")))
        .ok_or("nothing is a drum bus fed by the kick")?;
    if !feeds(project, snare, &Endpoint::new(bus, "in2")) {
        return Err("the snare does not reach the drum bus".into());
    }
    let dynamics = of(project, DYNAMICS_ID);
    let Some(&dynamics) = dynamics.first() else {
        return Err("there is no Dynamics node".into());
    };
    if !feeds(project, kick, &Endpoint::new(dynamics, "sidechain")) {
        return Err("the kick is not the sidechain".into());
    }
    let detect = project
        .graph()
        .node(dynamics)
        .and_then(|n| n.params.get("detect"));
    if detect.copied().unwrap_or(0.0) < 0.5 {
        return Err("the Dynamics node detects from its input, not the sidechain".into());
    }
    let (squash, parallel) = parallel_bus(project)?;
    if parallel == bus {
        return Err("the compressor is fed by the drum bus, not a parallel bus".into());
    }
    for (source, input) in [(kick, "in1"), (snare, "in2")] {
        if !feeds(project, source, &Endpoint::new(parallel, input)) {
            return Err(format!(
                "the parallel bus's {input} does not come straight from its drum"
            ));
        }
    }
    let ratio = param(project, squash, "ratio");
    if ratio < 10.0 {
        return Err(format!(
            "the parallel bus is not compressed hard (ratio {ratio})"
        ));
    }
    reaches_master(project, squash)?;
    let heard = *LEVEL.lock().unwrap();
    if heard < 0.05 {
        return Err(format!("the mix is silent (peak {heard})"));
    }
    Ok(())
}

fn param(p: &Project, node: NodeId, key: &str) -> f32 {
    p.graph()
        .node(node)
        .and_then(|n| n.params.get(key))
        .copied()
        .unwrap_or(f32::NAN)
}

/// The compressor of the parallel bus (the Dynamics node that detects from
/// its own input) and what feeds it.
fn parallel_bus(p: &Project) -> Result<(NodeId, NodeId), String> {
    let squash = of(p, DYNAMICS_ID)
        .into_iter()
        .find(|&d| {
            p.graph()
                .node(d)
                .and_then(|n| n.params.get("detect"))
                .is_none_or(|&v| v < 0.5)
        })
        .ok_or("there is no compressor on a parallel bus")?;
    let source = p
        .graph()
        .source(&Endpoint::new(squash, "in"))
        .ok_or("nothing feeds the parallel compressor")?;
    Ok((squash, source.node))
}

fn reaches_master(p: &Project, node: NodeId) -> Result<(), String> {
    let master = of(p, spare::MIXER)[0];
    let into_master = p
        .graph()
        .connections()
        .any(|c| c.from.node == node && c.to.node == master);
    if into_master {
        Ok(())
    } else {
        Err("the parallel bus does not reach the master".into())
    }
}

fn duck_from_bus(h: &mut Handle, _size: usize) {
    let dynamics = of(h.probe().project(), DYNAMICS_ID)[0];
    let bus = of(h.probe().project(), spare::MIXER)
        .into_iter()
        .find(|&m| m != of(h.probe().project(), spare::MIXER)[0])
        .expect("the drum bus");
    h.step("take the sidechain from the bus");
    wire(h, (bus, "out"), (dynamics, "sidechain"));
}

fn ducks_from_bus(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let dynamics = of(project, DYNAMICS_ID)[0];
    let source = project
        .graph()
        .source(&Endpoint::new(dynamics, "sidechain"))
        .ok_or("the sidechain has no source")?;
    let drum_bus = of(project, spare::MIXER)
        .into_iter()
        .find(|&m| feeds(project, sources(project).0, &Endpoint::new(m, "in1")))
        .ok_or("the drum bus is gone")?;
    if source.node != drum_bus {
        return Err("the sidechain does not come from the drum bus".into());
    }
    Ok(())
}

fn post_fader(h: &mut Handle, _size: usize) {
    let (squash, parallel, bus) = {
        let probe = h.probe();
        let project = probe.project();
        let (squash, parallel) = parallel_bus(project).expect("the parallel bus");
        (squash, parallel, drum_bus(project).expect("the drum bus"))
    };
    h.step("feed the compressor from after the drum faders");
    wire(h, (bus, "out"), (squash, "in"));
    h.step("delete the old parallel Mix");
    let at = h.socket(parallel, true, "out") + egui::Vec2::new(-40.0, 0.0);
    // The canvas keeps repainting while it eases the view, so these don't
    // wait for it to settle.
    h.drag_playing(&[at]);
    h.press_playing(at, Modifiers::NONE, Key::Delete);
}

fn drum_bus(p: &Project) -> Result<NodeId, String> {
    let kick = sources(p).0;
    of(p, spare::MIXER)
        .into_iter()
        .find(|&m| {
            feeds(p, kick, &Endpoint::new(m, "in1"))
                && p.graph()
                    .connections()
                    .any(|c| c.from.node == m && c.to.node == of(p, spare::MIXER)[0])
        })
        .ok_or_else(|| "the drum bus is gone".into())
}

fn is_post_fader(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let (squash, source) = parallel_bus(project)?;
    if source != drum_bus(project)? {
        return Err("the compressor is not fed from after the drum faders".into());
    }
    let mixers = of(project, spare::MIXER).len();
    if mixers != 2 {
        return Err(format!(
            "{mixers} Mix nodes; the old parallel Mix is still there"
        ));
    }
    reaches_master(project, squash)
}
