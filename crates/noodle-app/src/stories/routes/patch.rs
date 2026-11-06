//! A small script language for routes that build a patch in the node editor:
//! add nodes by searching for them, wire sockets, type parameter values, then
//! listen. The same list of [`Op`]s drives the steps and is checked by the
//! done check, so a route says what the patch is once.
//!
//! Nodes are named by type and by order of creation among nodes of that type
//! (`Ref("util.mix", 1)` is the second Mix node), so the fixture's nodes and
//! the ones the steps add can be mixed freely.

use std::path::Path;
use std::time::Duration;

use egui::{Key, Modifiers, Pos2};
use noodle_core::{Command, Endpoint, Node, NodeId, Project};

use super::super::handle::{Handle, Probe, peak};
use super::{add_node_playing, finished, reveal, virtual_session};
use crate::app::App;
use crate::session::Edit;

/// The nth node of a type, in creation order. The type is the id without the
/// leading `noodle.`.
#[derive(Clone, Copy)]
pub struct Ref(pub &'static str, pub usize);

pub enum Op {
    /// Starts a new top-level step of the route.
    Step(&'static str),
    /// Searches for the node's type and adds the first result at the position.
    Add(Ref, f32, f32),
    /// Drags from an output socket to an input socket or parameter.
    Wire(Ref, &'static str, Ref, &'static str),
    /// Clicks a parameter's field and types a number.
    Set(Ref, &'static str, f32),
    /// Presses Space.
    Play,
}

pub fn full_id(kind: &str) -> String {
    format!("noodle.{kind}")
}

pub fn nodes_of(p: &Project, kind: &str) -> Vec<NodeId> {
    let id = full_id(kind);
    p.graph()
        .nodes()
        .filter(|(_, n)| n.type_id == id)
        .map(|(id, _)| id)
        .collect()
}

pub fn find(p: &Project, r: Ref) -> Option<NodeId> {
    nodes_of(p, r.0).get(r.1).copied()
}

/// A node to seed: type (without `noodle.`), position and parameters.
pub type Seed<'a> = (&'a str, f32, f32, &'a [(&'a str, f32)]);

/// A saved session holding `nodes` (type id without `noodle.`, position and
/// parameters), wired by index as `(from, out, to, in)`.
pub fn seeded(dir: &Path, nodes: &[Seed], wires: &[(usize, &str, usize, &str)]) -> App {
    let mut session = virtual_session(dir);
    let mut ids = Vec::new();
    for (kind, x, y, params) in nodes {
        let id = session.new_node_id();
        let mut node = Node::new(full_id(kind)).at(*x, *y);
        for (key, value) in *params {
            node.params.insert((*key).into(), *value);
        }
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        ids.push(id);
    }
    for (from, out, to, inp) in wires {
        let wire = noodle_core::spare::wire(
            Endpoint::new(ids[*from], *out),
            Endpoint::new(ids[*to], *inp),
        );
        session.edit([Edit::Apply(wire)]);
    }
    finished(session, dir)
}

/// Drags between two sockets, panning the canvas first if either is off it.
pub fn wire(h: &mut Handle, from: (NodeId, &str), to: (NodeId, &str)) {
    let a = h.socket(from.0, true, from.1);
    let b = h.socket(to.0, false, to.1);
    let (a, b) = if reveal(h, &[a, b]) == egui::Vec2::ZERO {
        (a, b)
    } else {
        (h.socket(from.0, true, from.1), h.socket(to.0, false, to.1))
    };
    h.drag_playing(&[a, b]);
}

/// Clicks a parameter's field on `node` and types `value` into it.
pub fn set(h: &mut Handle, node: NodeId, key: &str, value: f32) {
    let at = h.port_row(node, false, key);
    h.click(at, Modifiers::NONE);
    h.press(at, Modifiers::COMMAND, Key::A);
    h.type_text(at, &format!("{value}"));
    h.press(at, Modifiers::NONE, Key::Enter);
}

/// Runs the ops as a user would.
pub fn run(h: &mut Handle, ops: &[Op]) {
    // Modulation nodes show live values, so the canvas never stops repainting.
    let _busy = crate::testkit::playing_mode();
    for op in ops {
        match op {
            Op::Step(name) => h.step(name),
            Op::Add(r, x, y) => {
                add_node_playing(h, Pos2::new(*x, *y), r.0);
            }
            Op::Wire(a, out, b, inp) => {
                let p = h.probe();
                let a = find(p.project(), *a).expect("the node to wire from");
                let b = find(p.project(), *b).expect("the node to wire to");
                wire(h, (a, out), (b, inp));
            }
            Op::Set(r, key, value) => {
                let n = find(h.probe().project(), *r).expect("the node to set");
                set(h, n, key, *value);
            }
            Op::Play => h.press_playing(Pos2::new(640.0, 760.0), Modifiers::NONE, Key::Space),
        }
    }
}

/// Whether the ops' patch is there: every node, every wire, every value.
pub fn check(p: &mut Probe, ops: &[Op]) -> Result<(), String> {
    let project = p.project();
    for op in ops {
        match op {
            Op::Add(r, ..) => {
                find(project, *r).ok_or_else(|| format!("no {} node #{}", r.0, r.1 + 1))?;
            }
            Op::Wire(a, out, b, inp) => {
                let from = find(project, *a).ok_or_else(|| format!("no {} node", a.0))?;
                let to = find(project, *b).ok_or_else(|| format!("no {} node", b.0))?;
                let source = project.graph().source(&Endpoint::new(to, *inp));
                if source != Some(&Endpoint::new(from, *out)) {
                    return Err(format!(
                        "{}.{out} is not wired to {}.{inp} (it is {source:?})",
                        a.0, b.0
                    ));
                }
            }
            Op::Set(r, key, value) => {
                let n = find(project, *r).ok_or_else(|| format!("no {} node", r.0))?;
                let got = project.graph().node(n).and_then(|n| n.params.get(*key));
                if got.is_none_or(|g| (g - value).abs() > value.abs() * 0.01 + 1e-3) {
                    return Err(format!("{}.{key} is {got:?}, wanted {value}", r.0));
                }
            }
            Op::Step(_) | Op::Play => {}
        }
    }
    Ok(())
}

/// Hears the output for a moment, for a route that left the transport
/// running, and fails if it is silent.
///
/// An offline or frozen node plays a background render, which is silent until
/// it is ready (on a slow runner that can outlast any fixed listening
/// window), so this first waits for the renders to finish, as the user would
/// before judging the sound.
pub fn audible(p: &mut Probe) -> Result<(), String> {
    p.settle();
    let mut polls = 0;
    while p.freezing() && polls < 1200 {
        p.settle();
        std::thread::sleep(Duration::from_millis(25));
        polls += 1;
    }
    let level = peak(&p.hear(Duration::from_millis(300)));
    if level < 0.01 {
        return Err(format!("nothing is heard (peak {level})"));
    }
    Ok(())
}
