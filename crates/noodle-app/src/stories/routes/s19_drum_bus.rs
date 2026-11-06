//! S19: a drum bus, a parallel compressed bus, and a bass ducked by the kick.
//!
//! Done in the node editor. A Mix node is the drum bus, and the kick and the
//! snare feed it. Its output feeds an ordinary Gain node, which is the drum
//! fader and feeds the master. A send is a fan-out wire: before the late
//! change the drum bus's output also feeds a second Mix, the parallel bus, so
//! the send is pre-fader; after it the send comes from the Gain's output, so
//! it is post-fader. The parallel Mix's input gain is the send level. A hard
//! Dynamics node compresses the parallel bus and joins the master. Switching
//! pre to post moves that one wire and leaves the parallel Mix in place. A
//! Dynamics node with its detector on the sidechain input ducks the bass.
//! Plugin delay compensation is not exercised. The done check reads the
//! wiring and that the mix is audible; it does not measure the bass ducking
//! itself (the sources are steady sines).

use std::path::Path;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{Command, Endpoint, Node, NodeId, Project, spare};
use noodle_nodes::{DYNAMICS_ID, GAIN_ID};

use super::super::handle::{Handle, Probe, peak};
use super::super::{Keep, LateChange, Route, Tag};
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
                keep: &[Keep {
                    name: "the parallel Mix",
                    of: parallel_mix,
                }],
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

fn feeds_node(p: &Project, from: NodeId, to: NodeId) -> bool {
    p.graph()
        .connections()
        .any(|c| c.from.node == from && c.to.node == to)
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
    let master = master(h.probe().project()).expect("the master");
    h.step("add the drum bus");
    let bus = add_node(h, Pos2::new(450.0, 300.0), "util.mix");
    h.step("send the kick and the snare to it");
    wire(h, (kick, "out"), (bus, "in1"));
    wire(h, (snare, "out"), (bus, "in2"));
    h.step("fade the bus with a Gain");
    let fader = add_node(h, Pos2::new(650.0, 300.0), "gain");
    wire(h, (bus, "out"), (fader, "in"));
    wire(h, (fader, "out"), (master, "in1"));
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
    let parallel = add_node(h, Pos2::new(780.0, 300.0), "util.mix");
    h.step("send from the drum bus before its fader");
    wire(h, (bus, "out"), (parallel, "in1"));
    h.step("compress it hard");
    let squash = add_node(h, Pos2::new(780.0, 430.0), "dynamics");
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
    let master = master(project)?;
    let bus = drum_bus(project)?;
    if !feeds(project, snare, &Endpoint::new(bus, "in2")) {
        return Err("the snare does not reach the drum bus".into());
    }
    // A fader after the bus is how the route mixes it, not what the story
    // asks for. With one, the send must not come from after it.
    let fader = gain_after(project, bus).ok();
    if let Some(fader) = fader {
        if feeds_node(project, bus, master) {
            return Err("the drum bus bypasses its Gain fader".into());
        }
        if !feeds_node(project, fader, master) {
            return Err("the drum bus Gain does not reach the master".into());
        }
    } else if !super::patch::reaches(project, bus, master) {
        return Err("the drum bus does not reach the master".into());
    }
    let dynamics = sidechain_dynamics(project)?;
    if !feeds(project, kick, &Endpoint::new(dynamics, "sidechain")) {
        return Err("the kick is not the sidechain".into());
    }
    let (squash, parallel) = parallel_bus(project)?;
    if parallel == bus || parallel == master {
        return Err("the compressor is fed by the drum bus, not a parallel bus".into());
    }
    if !feeds(project, bus, &Endpoint::new(parallel, "in1")) {
        return Err("the send does not come from the drum bus before its fader".into());
    }
    if fader.is_some_and(|f| feeds_node(project, f, parallel)) {
        return Err("the send comes from after the drum bus fader".into());
    }
    hard(project, squash)?;
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

fn master(p: &Project) -> Result<NodeId, String> {
    let out = of(p, noodle_engine::OUTPUT_ID)
        .into_iter()
        .next()
        .ok_or("there is no output")?;
    of(p, spare::MIXER)
        .into_iter()
        .find(|&m| feeds(p, m, &Endpoint::new(out, "in")))
        .ok_or_else(|| "no Mix feeds the output".into())
}

/// The drum bus: the Mix the kick feeds, other than the master.
fn drum_bus(p: &Project) -> Result<NodeId, String> {
    let kick = sources(p).0;
    let master = master(p)?;
    of(p, spare::MIXER)
        .into_iter()
        .find(|&m| m != master && feeds(p, kick, &Endpoint::new(m, "in1")))
        .ok_or_else(|| "the drum bus is gone".into())
}

/// The Gain after the drum bus, which is its fader.
fn gain_after(p: &Project, bus: NodeId) -> Result<NodeId, String> {
    of(p, GAIN_ID)
        .into_iter()
        .find(|&g| feeds(p, bus, &Endpoint::new(g, "in")))
        .ok_or_else(|| "the drum bus has no Gain after it".into())
}

fn detects_sidechain(p: &Project, node: NodeId) -> bool {
    param(p, node, "detect") >= 0.5
}

/// The Dynamics node that ducks the bass from its sidechain input.
fn sidechain_dynamics(p: &Project) -> Result<NodeId, String> {
    of(p, DYNAMICS_ID)
        .into_iter()
        .find(|&d| detects_sidechain(p, d))
        .ok_or_else(|| "there is no Dynamics node detecting from its sidechain".into())
}

/// The compressor of the parallel bus (the Dynamics node that detects from
/// its own input) and the Mix that feeds it.
fn parallel_bus(p: &Project) -> Result<(NodeId, NodeId), String> {
    let squash = of(p, DYNAMICS_ID)
        .into_iter()
        .find(|&d| !detects_sidechain(p, d))
        .ok_or("there is no compressor on a parallel bus")?;
    let source = p
        .graph()
        .source(&Endpoint::new(squash, "in"))
        .ok_or("nothing feeds the parallel compressor")?;
    if p.graph()
        .node(source.node)
        .is_none_or(|n| n.type_id != spare::MIXER)
    {
        return Err("the parallel compressor is not fed by a Mix".into());
    }
    Ok((squash, source.node))
}

/// The parallel Mix's identity and settings, for the late change to leave
/// alone. Only the send's source changes; its input stays `in1`.
fn parallel_mix(p: &Project) -> String {
    let Ok((_, mix)) = parallel_bus(p) else {
        return "the parallel Mix is missing".into();
    };
    let Some(node) = p.graph().node(mix) else {
        return format!("{mix} is missing");
    };
    format!("{mix} {} {:?} {:?}", node.type_id, node.config, node.params)
}

fn hard(p: &Project, squash: NodeId) -> Result<(), String> {
    let ratio = param(p, squash, "ratio");
    if ratio >= 10.0 {
        Ok(())
    } else {
        Err(format!(
            "the parallel bus is not compressed hard (ratio {ratio})"
        ))
    }
}

fn reaches_master(p: &Project, node: NodeId) -> Result<(), String> {
    let master = master(p)?;
    if super::patch::reaches(p, node, master) {
        Ok(())
    } else {
        Err("the parallel bus does not reach the master".into())
    }
}

fn duck_from_bus(h: &mut Handle, _size: usize) {
    let (dynamics, bus) = {
        let probe = h.probe();
        let project = probe.project();
        (
            sidechain_dynamics(project).expect("the ducking Dynamics"),
            drum_bus(project).expect("the drum bus"),
        )
    };
    h.step("take the sidechain from the bus");
    wire(h, (bus, "out"), (dynamics, "sidechain"));
}

fn ducks_from_bus(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let dynamics = sidechain_dynamics(project)?;
    let source = project
        .graph()
        .source(&Endpoint::new(dynamics, "sidechain"))
        .ok_or("the sidechain has no source")?;
    if source.node != drum_bus(project)? {
        return Err("the sidechain does not come from the drum bus".into());
    }
    Ok(())
}

fn post_fader(h: &mut Handle, _size: usize) {
    let (fader, parallel, send) = {
        let probe = h.probe();
        let project = probe.project();
        let bus = drum_bus(project).expect("the drum bus");
        let fader = gain_after(project, bus).expect("the drum bus Gain");
        let (_, parallel) = parallel_bus(project).expect("the parallel bus");
        let send = project
            .graph()
            .connections()
            .find(|c| c.from.node == bus && c.to.node == parallel)
            .map(|c| c.to.port.clone())
            .expect("the send wire");
        (fader, parallel, send)
    };
    h.step("move the send from before the fader to after it");
    wire(h, (fader, "out"), (parallel, &send));
}

fn is_post_fader(p: &mut Probe, _size: usize) -> Result<(), String> {
    let project = p.project();
    let master = master(project)?;
    let bus = drum_bus(project)?;
    let fader = gain_after(project, bus)?;
    let (squash, parallel) = parallel_bus(project)?;
    if parallel == bus || parallel == master {
        return Err("the compressor is not fed by a parallel bus".into());
    }
    if !feeds(project, fader, &Endpoint::new(parallel, "in1")) {
        return Err("the compressor is not fed from after the drum fader".into());
    }
    if feeds_node(project, bus, parallel) {
        return Err("the send still comes from before the drum fader".into());
    }
    if !feeds_node(project, fader, master) {
        return Err("the drum bus Gain no longer reaches the master".into());
    }
    let mixers = of(project, spare::MIXER).len();
    if mixers != 3 {
        return Err(format!("{mixers} Mix nodes; the parallel Mix is missing"));
    }
    hard(project, squash)?;
    reaches_master(project, squash)
}
