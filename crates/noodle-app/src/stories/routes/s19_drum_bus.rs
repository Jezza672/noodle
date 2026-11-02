//! S19: a drum bus, and a bass ducked by the kick.
//!
//! Partial. The route is done in the node editor: a Mix node is the bus, and a
//! Dynamics node with its detector on the sidechain input ducks the bass.
//! There are no pre-fader sends in the mixer, so the parallel compressed bus
//! and the late change of switching a send from pre- to post-fader are not
//! routed (#332). Plugin delay compensation is not exercised. The done check
//! reads the wiring and that the mix is audible; it does not measure the
//! bass ducking itself (the sources are steady sines).

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{Command, Endpoint, Node, NodeId, Project, spare};
use noodle_nodes::DYNAMICS_ID;

use super::super::handle::{Handle, Probe, peak};
use super::super::{LateChange, Route, Tag};
use super::{add_node, finished, virtual_session};
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
        late: &[LateChange {
            name: "duck from the whole drum bus, not only the kick",
            steps: duck_from_bus,
            done: ducks_from_bus,
            keep: &[],
        }],
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
    h.step("listen");
    let at = Pos2::new(640.0, 700.0);
    h.press_playing(at, Modifiers::NONE, Key::Space);
    let heard = h.probe().hear(std::time::Duration::from_millis(800));
    *LEVEL.lock().unwrap() = peak(&heard);
    h.press_playing(at, Modifiers::NONE, Key::Space);
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
    let heard = *LEVEL.lock().unwrap();
    if heard < 0.05 {
        return Err(format!("the mix is silent (peak {heard})"));
    }
    Ok(())
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
