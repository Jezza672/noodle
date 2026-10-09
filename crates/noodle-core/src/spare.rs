//! Spare ports: nodes whose number of ports follows their wires, always with
//! one more than are used so there is somewhere to wire the next signal.
//!
//! Two kinds of node work this way:
//!
//! - A **mixer** ([`MIXER`]) has `in1`…`inN` inputs, where N is its `inputs`
//!   config. It shows up to the highest wired input plus a spare. The spare is
//!   only drawn: nothing is stored for it until a wire is dropped on it, and
//!   then [`prepare_mixer_input`] raises `inputs` in the same command. Ports
//!   the config has beyond the last wired one are just not shown.
//! - A **group** has the ports its boundary nodes give it, plus a spare input
//!   and a spare output. Wiring to one makes a boundary node inside
//!   ([`add_group_port`]); nothing is ever removed on its own, because the
//!   boundary node may be wired up inside.
//!
//! Every edit made on the user's behalf like this goes through [`wire`]: it
//! replaces whatever was feeding the input it takes over and leaves every
//! node alone. Outputs can fan out, so wires leaving an output stay.

use crate::group::{GROUP_INPUT, GROUP_OUTPUT, PORT_NAME};
use crate::{Command, Config, Connection, Endpoint, Graph, Node, NodeId, Position, Project, Value};

/// The mixer node type: a sum of its inputs.
pub const MIXER: &str = "noodle.util.mix";
/// The mixer's config key for how many inputs it has.
pub const MIXER_INPUTS: &str = "inputs";
/// How many inputs a mixer has when `inputs` isn't set.
pub const MIXER_DEFAULT_INPUTS: i64 = 2;
/// The most inputs a mixer can have.
pub const MIXER_MAX_INPUTS: i64 = 64;

/// The one way inferred edits connect things: a connection that replaces
/// whatever fed the input, and touches nothing else.
pub fn wire(from: Endpoint, to: Endpoint) -> Command {
    Command::Connect(Connection { from, to })
}

/// The key of a mixer's `index`th input, counting from 1.
pub fn mixer_input_key(index: usize) -> String {
    format!("in{index}")
}

/// Which input a mixer port key is (`in3` is 3), if it is one.
pub fn mixer_input_index(key: &str) -> Option<usize> {
    key.strip_prefix("in")?.parse().ok().filter(|&i| i >= 1)
}

/// How many inputs a mixer's config gives it.
pub fn mixer_inputs(node: &Node) -> i64 {
    match node.config.get(MIXER_INPUTS) {
        Some(Value::Int(n)) => *n,
        _ => MIXER_DEFAULT_INPUTS,
    }
}

/// The highest-numbered input of mixer `id` with a wire on it, or 0.
pub fn mixer_used(graph: &Graph, id: NodeId) -> usize {
    graph
        .connections()
        .filter(|c| c.to.node == id)
        .filter_map(|c| mixer_input_index(&c.to.port))
        .max()
        .unwrap_or(0)
}

/// The key of the input to wire the next signal to: just past the last wired.
pub fn mixer_spare_key(graph: &Graph, id: NodeId) -> String {
    mixer_input_key(mixer_used(graph, id) + 1)
}

/// What has to change before a wire can go to input `key` of mixer `id`: its
/// `inputs` raised to include it. Empty when it already does, or for a key
/// that isn't an input at all.
pub fn prepare_mixer_input(graph: &Graph, id: NodeId, key: &str) -> Vec<Command> {
    let (Some(node), Some(index)) = (graph.node(id), mixer_input_index(key)) else {
        return Vec::new();
    };
    let index = index as i64;
    if index <= mixer_inputs(node) || index > MIXER_MAX_INPUTS {
        return Vec::new();
    }
    vec![Command::SetConfig {
        node: id,
        key: MIXER_INPUTS.into(),
        value: Some(Value::Int(index)),
    }]
}

/// Which side of a group a port is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Input,
    Output,
}

/// The name for the group's next spare port on `side`: `in1`, `in2`, … or
/// `out1`, `out2`, …, the first not taken.
pub fn spare_group_name(graph: &Graph, group: NodeId, side: Side) -> String {
    let ports = graph.group_ports(group);
    let (taken, prefix) = match side {
        Side::Input => (&ports.inputs, "in"),
        Side::Output => (&ports.outputs, "out"),
    };
    (1..)
        .map(|i| format!("{prefix}{i}"))
        .find(|name| !taken.iter().any(|p| p.name == *name))
        .expect("an unbounded range has a free name")
}

/// Adds a boundary node to `group` for a new port named `name` on `side`,
/// below the others of its kind and outside the nodes inside.
pub fn add_group_port(
    graph: &Graph,
    group: NodeId,
    side: Side,
    name: &str,
    new_id: NodeId,
) -> Command {
    let (kind, edge) = match side {
        Side::Input => (GROUP_INPUT, f32::min as fn(f32, f32) -> f32),
        Side::Output => (GROUP_OUTPUT, f32::max as fn(f32, f32) -> f32),
    };
    let inside: Vec<&Node> = graph.children(Some(group)).map(|(_, n)| n).collect();
    let x = inside
        .iter()
        .map(|n| n.position.x)
        .reduce(edge)
        .map_or(0.0, |x| match side {
            Side::Input => x - 250.0,
            Side::Output => x + 250.0,
        });
    let y = inside
        .iter()
        .filter(|n| n.type_id == kind)
        .map(|n| n.position.y + 80.0)
        .fold(0.0, f32::max);
    let config = Config::new().with(PORT_NAME, Value::Text(name.into()));
    let mut node = Node::new(kind).with_config(config);
    node.position = Position { x, y };
    node.parent = Some(group);
    Command::AddNode { id: new_id, node }
}

/// Renames the port that boundary node `boundary` gives its group, keeping
/// the wires on it. `None` if the name is empty or another port on the same
/// side has it, or the node isn't a boundary node.
pub fn rename_group_port(project: &Project, boundary: NodeId, name: &str) -> Option<Command> {
    let graph = project.graph();
    let node = graph.node(boundary)?;
    let group = node.parent?;
    let side = match node.type_id.as_str() {
        GROUP_INPUT => Side::Input,
        GROUP_OUTPUT => Side::Output,
        _ => return None,
    };
    let old = crate::group::port_name(boundary, node);
    if name.is_empty() {
        return None;
    }
    if name == old {
        return Some(Command::Batch(Vec::new()));
    }
    let ports = graph.group_ports(group);
    let list = match side {
        Side::Input => &ports.inputs,
        Side::Output => &ports.outputs,
    };
    if list.iter().any(|p| p.name == name) {
        return None;
    }
    let rename = Command::SetConfig {
        node: boundary,
        key: PORT_NAME.into(),
        value: Some(Value::Text(name.into())),
    };
    let mut commands = vec![rename];
    let from_old = Endpoint::new(group, old.clone());
    let to_new = Endpoint::new(group, name);
    // A port is found by its name, so wires on it move to the new one.
    match side {
        Side::Input => {
            if let Some(source) = graph.source(&from_old) {
                commands.push(Command::Disconnect {
                    input: from_old.clone(),
                });
                commands.push(wire(source.clone(), to_new));
            }
        }
        Side::Output => {
            for c in graph.connections().filter(|c| c.from == from_old) {
                commands.push(wire(to_new.clone(), c.to));
            }
        }
    }
    Some(Command::Batch(commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;
    use crate::group::GROUP;

    fn add(p: &mut Project, h: &mut History, node: Node) -> NodeId {
        let id = p.new_node_id();
        h.apply(p, Command::AddNode { id, node }).unwrap();
        id
    }

    fn mixer(p: &mut Project, h: &mut History) -> NodeId {
        add(p, h, Node::new(MIXER))
    }

    #[test]
    fn a_mixers_spare_is_just_past_its_last_wired_input() {
        let (mut p, mut h) = (Project::new(), History::new());
        let (mix, src) = (mixer(&mut p, &mut h), add(&mut p, &mut h, Node::new("s")));
        assert_eq!(mixer_spare_key(p.graph(), mix), "in1");
        h.apply(
            &mut p,
            wire(Endpoint::new(src, "out"), Endpoint::new(mix, "in3")),
        )
        .unwrap();
        // Gaps stay: in3 is wired, so in4 is the spare.
        assert_eq!(mixer_used(p.graph(), mix), 3);
        assert_eq!(mixer_spare_key(p.graph(), mix), "in4");
    }

    #[test]
    fn wiring_to_a_spare_raises_the_inputs_only_when_needed() {
        let (mut p, mut h) = (Project::new(), History::new());
        let mix = mixer(&mut p, &mut h);
        assert!(prepare_mixer_input(p.graph(), mix, "in2").is_empty());
        let prep = prepare_mixer_input(p.graph(), mix, "in3");
        assert_eq!(
            prep,
            [Command::SetConfig {
                node: mix,
                key: "inputs".into(),
                value: Some(Value::Int(3)),
            }]
        );
        assert!(prepare_mixer_input(p.graph(), mix, "out").is_empty());
        assert!(prepare_mixer_input(p.graph(), mix, "in65").is_empty());
    }

    #[test]
    fn group_spares_take_the_first_free_name() {
        let (mut p, mut h) = (Project::new(), History::new());
        let group = add(&mut p, &mut h, Node::new(GROUP));
        assert_eq!(spare_group_name(p.graph(), group, Side::Input), "in1");
        let id = p.new_node_id();
        let add_port = add_group_port(p.graph(), group, Side::Input, "in1", id);
        h.apply(&mut p, add_port).unwrap();
        assert_eq!(spare_group_name(p.graph(), group, Side::Input), "in2");
        assert_eq!(spare_group_name(p.graph(), group, Side::Output), "out1");
        let ports = p.graph().group_ports(group);
        assert_eq!(ports.inputs.len(), 1);
        assert_eq!(p.graph().node(id).unwrap().parent, Some(group));
    }

    #[test]
    fn renaming_a_port_keeps_its_wires() {
        let (mut p, mut h) = (Project::new(), History::new());
        let group = add(&mut p, &mut h, Node::new(GROUP));
        let (i, o) = (p.new_node_id(), p.new_node_id());
        let inp = add_group_port(p.graph(), group, Side::Input, "in1", i);
        let out = add_group_port(p.graph(), group, Side::Output, "out1", o);
        h.apply(&mut p, inp).unwrap();
        h.apply(&mut p, out).unwrap();
        let (src, dst, dst2) = (
            add(&mut p, &mut h, Node::new("s")),
            add(&mut p, &mut h, Node::new("d")),
            add(&mut p, &mut h, Node::new("d")),
        );
        for command in [
            wire(Endpoint::new(src, "out"), Endpoint::new(group, "in1")),
            wire(Endpoint::new(group, "out1"), Endpoint::new(dst, "in")),
            wire(Endpoint::new(group, "out1"), Endpoint::new(dst2, "in")),
        ] {
            h.apply(&mut p, command).unwrap();
        }
        let before = p.clone();
        for (boundary, name) in [(i, "feed"), (o, "mix")] {
            let command = rename_group_port(&p, boundary, name).unwrap();
            h.apply(&mut p, command).unwrap();
        }
        let g = p.graph();
        assert_eq!(
            g.source(&Endpoint::new(group, "feed")),
            Some(&Endpoint::new(src, "out"))
        );
        assert_eq!(g.source(&Endpoint::new(group, "in1")), None);
        for d in [dst, dst2] {
            assert_eq!(
                g.source(&Endpoint::new(d, "in")),
                Some(&Endpoint::new(group, "mix"))
            );
        }
        // Two undo steps, one per rename.
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn a_name_that_is_empty_or_taken_is_refused() {
        let (mut p, mut h) = (Project::new(), History::new());
        let group = add(&mut p, &mut h, Node::new(GROUP));
        let (a, b) = (p.new_node_id(), p.new_node_id());
        for (id, name) in [(a, "x"), (b, "y")] {
            let command = add_group_port(p.graph(), group, Side::Input, name, id);
            h.apply(&mut p, command).unwrap();
        }
        assert!(rename_group_port(&p, a, "y").is_none());
        assert!(rename_group_port(&p, a, "").is_none());
        assert!(
            rename_group_port(&p, group, "z").is_none(),
            "not a boundary node"
        );
        assert!(rename_group_port(&p, a, "z").is_some());
    }

    #[test]
    fn an_inferred_wire_replaces_the_feeder_and_touches_nothing_else() {
        let (mut p, mut h) = (Project::new(), History::new());
        let (a, b, d, e) = (
            add(&mut p, &mut h, Node::new("s")),
            add(&mut p, &mut h, Node::new("s")),
            add(&mut p, &mut h, Node::new("d")),
            add(&mut p, &mut h, Node::new("d")),
        );
        for command in [
            wire(Endpoint::new(a, "out"), Endpoint::new(d, "in")),
            wire(Endpoint::new(a, "out"), Endpoint::new(e, "in")),
        ] {
            h.apply(&mut p, command).unwrap();
        }
        let nodes = p.graph().nodes().count();
        h.apply(
            &mut p,
            wire(Endpoint::new(b, "out"), Endpoint::new(d, "in")),
        )
        .unwrap();
        let g = p.graph();
        assert_eq!(g.nodes().count(), nodes, "no node was removed");
        assert_eq!(
            g.source(&Endpoint::new(d, "in")),
            Some(&Endpoint::new(b, "out"))
        );
        // The output fans out: its other wire is still there.
        assert_eq!(
            g.source(&Endpoint::new(e, "in")),
            Some(&Endpoint::new(a, "out"))
        );
    }
}
