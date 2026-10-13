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
//!   and a spare output. Wiring to one adds a port to the group's boundary
//!   node inside ([`add_group_port`]); nothing is ever removed on its own,
//!   because the port may be wired up inside.
//!
//! Every edit made on the user's behalf like this goes through [`wire`]: it
//! replaces whatever was feeding the input it takes over and leaves every
//! node alone. Outputs can fan out, so wires leaving an output stay.

use crate::group::{GROUP_INPUT, GROUP_OUTPUT, GroupPort, control_key, ports_config, ports_value};
use crate::{Command, Connection, Endpoint, Graph, Node, NodeId, Position, Project, Value};

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

/// The key of the gain, in dB, of a mixer's `index`th input, counting from 1.
pub fn mixer_gain_key(index: usize) -> String {
    format!("gain{index}")
}

/// The key of the mute, 0 or 1, of a mixer's `index`th input, counting from 1.
pub fn mixer_mute_key(index: usize) -> String {
    format!("mute{index}")
}

/// Which input a mixer port key is (`in3` is 3), if it is one.
pub fn mixer_input_index(key: &str) -> Option<usize> {
    key.strip_prefix("in")?.parse().ok().filter(|&i| i >= 1)
}

/// Which input a mixer's gain or mute key belongs to (`gain3` and `mute3`
/// are 3), if it is one.
pub fn mixer_param_index(key: &str) -> Option<usize> {
    let digits = key
        .strip_prefix("gain")
        .or_else(|| key.strip_prefix("mute"))?;
    digits.parse().ok().filter(|&i| i >= 1)
}

/// How many inputs a mixer's config gives it.
pub fn mixer_inputs(node: &Node) -> i64 {
    match node.config.get(MIXER_INPUTS) {
        Some(Value::Int(n)) => *n,
        _ => MIXER_DEFAULT_INPUTS,
    }
}

/// The highest-numbered input of mixer `id` with a wire on it or on its gain
/// or mute, or 0.
pub fn mixer_used(graph: &Graph, id: NodeId) -> usize {
    graph
        .connections()
        .filter(|c| c.to.node == id)
        .filter_map(|c| mixer_input_index(&c.to.port).or_else(|| mixer_param_index(&c.to.port)))
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

/// Adds a port named `name` on `side` of `group`, and returns the command with
/// the new port's key on its boundary node. The port goes on the group's
/// boundary node of that side; if it has none yet, `new_id` is the ID of the
/// one made, placed outside the nodes inside.
pub fn add_group_port(
    graph: &Graph,
    group: NodeId,
    side: Side,
    name: &str,
    new_id: NodeId,
) -> (Command, String) {
    let (kind, edge) = match side {
        Side::Input => (GROUP_INPUT, f32::min as fn(f32, f32) -> f32),
        Side::Output => (GROUP_OUTPUT, f32::max as fn(f32, f32) -> f32),
    };
    if let Some((node_id, node)) = graph.children(Some(group)).find(|(_, n)| n.type_id == kind) {
        let mut ports = node.boundary_ports();
        let id = ports.iter().map(|&(id, _)| id).max().map_or(1, |id| id + 1);
        ports.push((id, name.to_owned()));
        let command = Command::SetConfig {
            node: node_id,
            key: crate::group::PORTS.into(),
            value: Some(ports_value(&ports)),
        };
        return (command, crate::group::port_key(id));
    }
    let x = graph
        .children(Some(group))
        .map(|(_, n)| n.position.x)
        .reduce(edge)
        .map_or(0.0, |x| match side {
            Side::Input => x - 250.0,
            Side::Output => x + 250.0,
        });
    let mut node = Node::new(kind).with_config(ports_config(&[(1, name.to_owned())]));
    node.position = Position { x, y: 0.0 };
    node.parent = Some(group);
    (
        Command::AddNode { id: new_id, node },
        crate::group::port_key(1),
    )
}

/// Renames port `port` of boundary node `boundary`, keeping the wires on it:
/// the group's wires move to the new name, and nothing inside changes, since
/// the port is found there by its ID. `None` if the name is empty or has a
/// line break, or another port on the same side has it, or `boundary` isn't a
/// boundary node with that port.
pub fn rename_group_port(
    project: &Project,
    boundary: NodeId,
    port: u32,
    name: &str,
) -> Option<Command> {
    let graph = project.graph();
    let node = graph.node(boundary)?;
    let group = node.parent?;
    let side = match node.type_id.as_str() {
        GROUP_INPUT => Side::Input,
        GROUP_OUTPUT => Side::Output,
        _ => return None,
    };
    let mut ports = node.boundary_ports();
    let old = ports.iter().find(|&&(id, _)| id == port)?.1.clone();
    if name.is_empty() || name.contains(['\n', '\r']) {
        return None;
    }
    if name == old {
        return Some(Command::Batch(Vec::new()));
    }
    let group_ports = graph.group_ports(group);
    let list = match side {
        Side::Input => &group_ports.inputs,
        Side::Output => &group_ports.outputs,
    };
    if list.iter().any(|p| p.name == name) {
        return None;
    }
    for entry in &mut ports {
        if entry.0 == port {
            entry.1 = name.to_owned();
        }
    }
    let rename = Command::SetConfig {
        node: boundary,
        key: crate::group::PORTS.into(),
        value: Some(ports_value(&ports)),
    };
    let mut commands = vec![rename];
    let from_old = Endpoint::new(group, old.clone());
    let to_new = Endpoint::new(group, name);
    // The group's port is found by its name, so wires on it move to the new
    // one.
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
    // The port keeps its place in the column under its new name.
    if let Some(group_node) = graph.node(group)
        && group_node.port_order.contains(&old)
    {
        let order = group_node
            .port_order
            .iter()
            .map(|k| {
                if *k == old {
                    name.to_string()
                } else {
                    k.clone()
                }
            })
            .collect();
        commands.push(Command::SetPortOrder { node: group, order });
    }
    Some(Command::Batch(commands))
}

/// Removes `port` from its boundary node, with everything that points at it:
/// the wires on the port and on its gain and mute, outside the group too,
/// its controls, and the automation lanes on them.
pub fn remove_group_port(project: &Project, port: &GroupPort) -> Option<Command> {
    let graph = project.graph();
    let node = graph.node(port.node)?;
    let group = node.parent?;
    let side = match node.type_id.as_str() {
        GROUP_INPUT => Side::Input,
        GROUP_OUTPUT => Side::Output,
        _ => return None,
    };
    let mut ports = node.boundary_ports();
    let before = ports.len();
    ports.retain(|&(id, _)| id != port.id);
    if ports.len() == before {
        return None;
    }
    let key = port.key();
    let controls: Vec<String> = [crate::group::GAIN, crate::group::MUTE, crate::group::SOLO]
        .iter()
        .map(|c| control_key(c, port.id))
        .collect();
    let outside = Endpoint::new(group, port.name.clone());
    let mut commands = Vec::new();
    for c in graph.connections() {
        let inside = (c.to.node == port.node
            && (c.to.port == key || controls.contains(&c.to.port)))
            || (c.from.node == port.node && c.from.port == key);
        let outer = match side {
            Side::Input => c.to == outside,
            Side::Output => c.from == outside,
        };
        if inside || outer {
            commands.push(Command::Disconnect { input: c.to });
        }
    }
    for control in &controls {
        if node.params.contains_key(control) {
            commands.push(Command::SetParam {
                node: port.node,
                key: control.clone(),
                value: None,
            });
        }
    }
    for (id, lane) in project.lanes() {
        if lane.target.node == port.node && controls.contains(&lane.target.port) {
            commands.push(Command::RemoveLane { id });
        }
    }
    commands.push(Command::SetConfig {
        node: port.node,
        key: crate::group::PORTS.into(),
        value: Some(ports_value(&ports)),
    });
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

    /// Adds a port to `group` through [`add_group_port`], returning its
    /// boundary node and ID.
    fn port(
        p: &mut Project,
        h: &mut History,
        group: NodeId,
        side: Side,
        name: &str,
    ) -> (NodeId, u32) {
        let id = p.new_node_id();
        let (command, key) = add_group_port(p.graph(), group, side, name, id);
        h.apply(p, command).unwrap();
        let ports = p.graph().group_ports(group);
        let list = if side == Side::Input {
            ports.inputs
        } else {
            ports.outputs
        };
        let port = list.into_iter().find(|port| port.name == name).unwrap();
        assert_eq!(port.key(), key);
        (port.node, port.id)
    }

    #[test]
    fn group_spares_take_the_first_free_name() {
        let (mut p, mut h) = (Project::new(), History::new());
        let group = add(&mut p, &mut h, Node::new(GROUP));
        assert_eq!(spare_group_name(p.graph(), group, Side::Input), "in1");
        let (node, _) = port(&mut p, &mut h, group, Side::Input, "in1");
        assert_eq!(spare_group_name(p.graph(), group, Side::Input), "in2");
        assert_eq!(spare_group_name(p.graph(), group, Side::Output), "out1");
        assert_eq!(p.graph().node(node).unwrap().parent, Some(group));
    }

    #[test]
    fn a_groups_ports_share_one_node_per_side() {
        let (mut p, mut h) = (Project::new(), History::new());
        let group = add(&mut p, &mut h, Node::new(GROUP));
        let (a, a_id) = port(&mut p, &mut h, group, Side::Input, "a");
        let (b, b_id) = port(&mut p, &mut h, group, Side::Input, "b");
        let (out, _) = port(&mut p, &mut h, group, Side::Output, "out");
        assert_eq!((a, a_id, b, b_id), (a, 1, a, 2));
        assert_ne!(a, out);
        assert_eq!(p.graph().children(Some(group)).count(), 2);
        assert_eq!(p.graph().group_ports(group).inputs.len(), 2);
        // One undo takes the second port off the node, and no more.
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p.graph().group_ports(group).inputs.len(), 1);
        assert_eq!(p.graph().node(a).unwrap().boundary_ports().len(), 1);
    }

    #[test]
    fn renaming_a_port_keeps_its_wires() {
        let (mut p, mut h) = (Project::new(), History::new());
        let group = add(&mut p, &mut h, Node::new(GROUP));
        let (i, i_id) = port(&mut p, &mut h, group, Side::Input, "in1");
        let (o, o_id) = port(&mut p, &mut h, group, Side::Output, "out1");
        let (src, dst, dst2) = (
            add(&mut p, &mut h, Node::new("s")),
            add(&mut p, &mut h, Node::new("d")),
            add(&mut p, &mut h, Node::new("d")),
        );
        // Inside, the input feeds the output.
        for command in [
            wire(Endpoint::new(src, "out"), Endpoint::new(group, "in1")),
            wire(Endpoint::new(group, "out1"), Endpoint::new(dst, "in")),
            wire(Endpoint::new(group, "out1"), Endpoint::new(dst2, "in")),
        ] {
            h.apply(&mut p, command).unwrap();
        }
        let before = p.clone();
        for (boundary, id, name) in [(i, i_id, "feed"), (o, o_id, "mix")] {
            let command = rename_group_port(&p, boundary, id, name).unwrap();
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
        // The port's ID, and so its key inside, is unchanged.
        assert_eq!(g.group_ports(group).inputs[0].id, i_id);
        // Two undo steps, one per rename.
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn a_name_that_is_empty_or_taken_is_refused() {
        let (mut p, mut h) = (Project::new(), History::new());
        let group = add(&mut p, &mut h, Node::new(GROUP));
        let (a, a_id) = port(&mut p, &mut h, group, Side::Input, "x");
        port(&mut p, &mut h, group, Side::Input, "y");
        assert!(rename_group_port(&p, a, a_id, "y").is_none());
        assert!(rename_group_port(&p, a, a_id, "").is_none());
        assert!(rename_group_port(&p, a, a_id, "a\nb").is_none());
        assert!(rename_group_port(&p, a, 99, "z").is_none(), "no such port");
        assert!(
            rename_group_port(&p, group, 1, "z").is_none(),
            "not a boundary node"
        );
        assert!(rename_group_port(&p, a, a_id, "z").is_some());
    }

    #[test]
    fn removing_a_port_takes_its_wires_controls_and_lanes_with_it() {
        use crate::group::{GAIN, control_key};
        use crate::{AutomationLane, LaneId};
        let (mut p, mut h) = (Project::new(), History::new());
        let group = add(&mut p, &mut h, Node::new(GROUP));
        let (node, _) = port(&mut p, &mut h, group, Side::Output, "a");
        port(&mut p, &mut h, group, Side::Output, "b");
        let (src, dst) = (
            add(&mut p, &mut h, Node::new("s").in_group(group)),
            add(&mut p, &mut h, Node::new("d")),
        );
        let ports = p.graph().group_ports(group);
        let (a, b) = (ports.outputs[0].clone(), ports.outputs[1].clone());
        h.apply(
            &mut p,
            wire(Endpoint::new(src, "out"), Endpoint::new(node, a.key())),
        )
        .unwrap();
        h.apply(
            &mut p,
            wire(Endpoint::new(src, "out"), Endpoint::new(node, b.key())),
        )
        .unwrap();
        h.apply(
            &mut p,
            wire(Endpoint::new(group, "a"), Endpoint::new(dst, "in")),
        )
        .unwrap();
        h.apply(
            &mut p,
            Command::SetParam {
                node,
                key: control_key(GAIN, a.id),
                value: Some(-3.0),
            },
        )
        .unwrap();
        h.apply(
            &mut p,
            Command::AddLane {
                id: LaneId(1),
                lane: AutomationLane::new(Endpoint::new(node, control_key(GAIN, a.id)), Vec::new()),
            },
        )
        .unwrap();
        let before = p.clone();
        let command = remove_group_port(&p, &a).unwrap();
        h.apply(&mut p, command).unwrap();
        let g = p.graph();
        assert_eq!(g.group_ports(group).outputs.len(), 1);
        assert_eq!(g.group_ports(group).outputs[0].name, "b");
        assert_eq!(g.source(&Endpoint::new(node, a.key())), None);
        assert!(
            g.source(&Endpoint::new(node, b.key())).is_some(),
            "b stays wired"
        );
        assert_eq!(g.source(&Endpoint::new(dst, "in")), None);
        assert!(g.node(node).unwrap().params.is_empty());
        assert_eq!(p.lanes().count(), 0);
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
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
