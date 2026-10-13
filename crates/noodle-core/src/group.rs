//! Group nodes: a node that holds a subgraph, so a patch can be folded into
//! one node and opened again.
//!
//! The project still has one graph. A node says which group it's inside with
//! [`Node::parent`], and wires only join nodes with the same parent. Signals
//! cross a group's boundary through two kinds of node inside it:
//!
//! - a [`GROUP_INPUT`] node, whose `out` port carries whatever is wired to
//!   the group's input of the same name;
//! - a [`GROUP_OUTPUT`] node, whose `in` port feeds the group's output of the
//!   same name.
//!
//! The group node's own ports are exactly its boundary nodes, named by their
//! `name` config. The engine flattens groups away before compiling, so group
//! and boundary nodes have no DSP of their own.

use std::collections::BTreeMap;

use crate::{
    Command, Connection, EditError, Endpoint, Graph, Node, NodeId, Position, Project, Value,
};

/// The node type of a group.
pub const GROUP: &str = "noodle.group";
/// Inside a group: brings one of the group's inputs into the subgraph.
pub const GROUP_INPUT: &str = "noodle.group.input";
/// Inside a group: sends a signal out through one of the group's outputs.
pub const GROUP_OUTPUT: &str = "noodle.group.output";
/// The config key holding a group's own name (and a track's), and, on a
/// boundary node of an older file, its one port's name.
pub const PORT_NAME: &str = "name";
/// The config key holding a boundary node's ports: one per line, `ID:name`.
pub const PORTS: &str = "ports";
/// The track's source node, inside its group. It has two outputs, [`AUDIO`]
/// and [`MIDI`]; the transport plays the track's clips out of them.
pub const TRACK_INPUT: &str = "noodle.track.input";
pub const AUDIO: &str = "audio";
pub const MIDI: &str = "midi";

/// The key of a port on its boundary node: the output of a [`GROUP_INPUT`]
/// node, or the input of a [`GROUP_OUTPUT`] node. `id` is the port's ID.
pub fn port_key(id: u32) -> String {
    format!("p{id}")
}

/// The port ID of a boundary node's port key, if it is one.
pub fn parse_port_key(key: &str) -> Option<u32> {
    key.strip_prefix('p')?.parse().ok()
}

/// The key of a port's [`GAIN`], [`MUTE`] or [`SOLO`] on its boundary node,
/// for `control` one of those.
pub fn control_key(control: &str, id: u32) -> String {
    format!("{control}.{id}")
}

/// The control and port ID a boundary node's gain, mute or solo key is for.
pub fn parse_control_key(key: &str) -> Option<(&'static str, u32)> {
    let (control, id) = key.split_once('.')?;
    let control = [GAIN, MUTE, SOLO].into_iter().find(|c| *c == control)?;
    Some((control, id.parse().ok()?))
}

/// The ID a boundary node's port has as a stage in the flattened graph. A
/// boundary node holds many ports, so the stage can't have its ID; this is
/// an ID that no project node can have (those are counted up from 1) and
/// that is the same on every compile.
pub fn stage_id(boundary: NodeId, port: u32) -> NodeId {
    NodeId((1 << 62) | (boundary.0 << 24) | u64::from(port))
}

/// The node type that stands in for a boundary port with something to do. It
/// exists only in the flattened graph the compiler sees: flatten swaps it in
/// for a port whose controls are off their defaults, under the port's
/// [`stage_id`], and routes automation aimed at the port's controls to it.
pub const GROUP_STAGE: &str = "noodle.group.stage";
/// The controls every boundary port carries, as parameters of its boundary
/// node (see [`control_key`]): a gain in decibels, and mute and solo switches
/// (on at 0.5 or more).
pub const GAIN: &str = "gain";
pub const MUTE: &str = "mute";
pub const SOLO: &str = "solo";
/// On a stage only: set by the compiler while another track's solo mutes
/// this one. Kept apart from [`MUTE`] so an automation lane driving the mute
/// can't override it.
pub const SOLO_MUTE: &str = "solo_mute";

/// A boundary port's gain, mute and solo, with the defaults for any that
/// aren't set. A track's controls are these, on its group's boundary ports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
    pub gain_db: f32,
    pub mute: bool,
    pub solo: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            mute: false,
            solo: false,
        }
    }
}

impl Node {
    /// Whether a gain or mute has been set on this boundary node's port, even
    /// to its default. A set control keeps the port's stage in the compiled
    /// graph, so moving it again changes a parameter and not the graph's
    /// shape.
    pub fn has_gain_or_mute(&self, port: u32) -> bool {
        self.params.contains_key(&control_key(GAIN, port))
            || self.params.contains_key(&control_key(MUTE, port))
    }

    /// The gain, mute and solo parameters of this boundary node's port.
    pub fn port_controls(&self, port: u32) -> Controls {
        let param = |control: &str| self.params.get(&control_key(control, port)).copied();
        Controls {
            gain_db: param(GAIN).filter(|g| g.is_finite()).unwrap_or(0.0),
            mute: param(MUTE).is_some_and(|m| m >= 0.5),
            solo: param(SOLO).is_some_and(|s| s >= 0.5),
        }
    }

    /// Whether any of this boundary node's ports has solo set, even to off.
    fn has_solo_param(&self) -> bool {
        self.params
            .keys()
            .any(|key| parse_control_key(key).is_some_and(|(control, _)| control == SOLO))
    }

    /// The ports this boundary node gives its group, as `(ID, name)` in
    /// order. Empty for any other kind of node.
    pub fn boundary_ports(&self) -> Vec<(u32, String)> {
        if !matches!(self.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT) {
            return Vec::new();
        }
        let Some(Value::Text(text)) = self.config.get(PORTS) else {
            return Vec::new();
        };
        let mut ports: Vec<(u32, String)> = Vec::new();
        for line in text.lines() {
            let Some((id, name)) = line.split_once(':') else {
                continue;
            };
            let Ok(id) = id.parse() else { continue };
            if !name.is_empty() && !ports.iter().any(|(other, _)| *other == id) {
                ports.push((id, name.to_owned()));
            }
        }
        ports
    }
}

/// The [`PORTS`] config value for `ports`.
pub fn ports_value(ports: &[(u32, String)]) -> Value {
    let lines: Vec<String> = ports
        .iter()
        .map(|(id, name)| format!("{id}:{}", name.replace('\n', " ")))
        .collect();
    Value::Text(lines.join("\n"))
}

/// A boundary node's config for `ports`.
pub fn ports_config(ports: &[(u32, String)]) -> crate::Config {
    crate::Config::new().with(PORTS, ports_value(ports))
}

/// One of a group's ports: its name, and the boundary node that holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPort {
    pub name: String,
    /// The boundary node the port is on.
    pub node: NodeId,
    /// The port's ID on that node.
    pub id: u32,
}

impl GroupPort {
    /// The port's key on its boundary node.
    pub fn key(&self) -> String {
        port_key(self.id)
    }

    /// The key of one of the port's controls ([`GAIN`], [`MUTE`] or
    /// [`SOLO`]) on its boundary node.
    pub fn control_key(&self, control: &str) -> String {
        control_key(control, self.id)
    }

    /// The port's place in the flattened graph when it has a stage.
    pub fn stage(&self) -> NodeId {
        stage_id(self.node, self.id)
    }
}

/// A group's inputs and outputs, in boundary-node ID and port order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupPorts {
    pub inputs: Vec<GroupPort>,
    pub outputs: Vec<GroupPort>,
}

impl GroupPorts {
    pub fn input(&self, name: &str) -> Option<&GroupPort> {
        self.inputs.iter().find(|p| p.name == name)
    }

    pub fn output(&self, name: &str) -> Option<&GroupPort> {
        self.outputs.iter().find(|p| p.name == name)
    }
}

impl Graph {
    /// The nodes directly inside a group, or at the top level for `None`.
    pub fn children(&self, parent: Option<NodeId>) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes().filter(move |(_, node)| node.parent == parent)
    }

    /// Whether `node` is somewhere inside `group`, at any depth.
    pub fn is_inside(&self, node: NodeId, group: NodeId) -> bool {
        // The depth limit stops a malformed cycle from looping forever.
        let mut at = self.node(node).and_then(|n| n.parent);
        for _ in 0..=self.nodes().count() {
            match at {
                Some(parent) if parent == group => return true,
                Some(parent) => at = self.node(parent).and_then(|n| n.parent),
                None => return false,
            }
        }
        false
    }

    /// Everything inside a group at any depth, with each group before its
    /// contents.
    pub fn descendants(&self, group: NodeId) -> Vec<NodeId> {
        let mut found = Vec::new();
        let mut pending = vec![group];
        while let Some(parent) = pending.pop() {
            let mut kids: Vec<NodeId> = self.children(Some(parent)).map(|(id, _)| id).collect();
            kids.sort();
            // Reversed, so the lowest ID is popped, and so listed, first.
            for &kid in kids.iter().rev() {
                pending.push(kid);
            }
            if parent != group {
                found.push(parent);
            }
        }
        found
    }

    /// The groups containing `node`, outermost first. The editor shows this
    /// as the breadcrumb.
    pub fn ancestors(&self, node: NodeId) -> Vec<NodeId> {
        let mut path = Vec::new();
        let mut at = self.node(node).and_then(|n| n.parent);
        while let Some(parent) = at {
            if path.contains(&parent) {
                break;
            }
            path.push(parent);
            at = self.node(parent).and_then(|n| n.parent);
        }
        path.reverse();
        path
    }

    /// The port a group's gain and mute are shown and set on: its first
    /// output (a track has one), or failing that its first input. `None` for
    /// a group with no ports.
    pub fn control_port(&self, group: NodeId) -> Option<GroupPort> {
        let ports = self.group_ports(group);
        ports
            .outputs
            .into_iter()
            .next()
            .or_else(|| ports.inputs.into_iter().next())
    }

    /// A group's gain, mute and solo, as the mixer shows them: the gain and
    /// mute of its [`control_port`](Self::control_port), and solo if any of
    /// the group's ports has it on.
    pub fn group_controls(&self, group: NodeId) -> Controls {
        let mut controls = self
            .control_port(group)
            .and_then(|port| Some(self.node(port.node)?.port_controls(port.id)))
            .unwrap_or_default();
        controls.solo = self.has_own_solo(group);
        controls
    }

    /// The groups that soloing silences. Among the groups sharing a parent
    /// (the tracks of one level), if any is soloed, or has a soloed group
    /// inside it, every one that isn't is muted, except those the soloed
    /// groups feed: whatever a soloed track routes into (a reverb return, a
    /// bus) stays audible, as in a mixer's implicit solo. "Feeds" follows
    /// wires through any nodes at that level. Plain nodes are left alone.
    /// Soloing is read here, from the project, rather than at run time, so it
    /// can't be automated.
    pub fn solo_muted(&self) -> std::collections::BTreeSet<NodeId> {
        use std::collections::BTreeSet;
        let groups: Vec<(NodeId, Option<NodeId>)> = self
            .nodes()
            .filter(|(_, node)| node.type_id == GROUP)
            .map(|(id, node)| (id, node.parent))
            .collect();
        let wires: Vec<(NodeId, NodeId)> = self
            .connections()
            .map(|c| (c.from.node, c.to.node))
            .collect();
        let mut muted = BTreeSet::new();
        let mut done = BTreeSet::new();
        for &(_, parent) in &groups {
            if !done.insert(parent) {
                continue;
            }
            let siblings: Vec<NodeId> = groups
                .iter()
                .filter(|(_, p)| *p == parent)
                .map(|&(id, _)| id)
                .collect();
            let mut audible: BTreeSet<NodeId> = siblings
                .iter()
                .copied()
                .filter(|&id| self.has_solo_inside(id))
                .collect();
            if audible.is_empty() {
                continue;
            }
            // Everything downstream of a soloed group.
            loop {
                let before = audible.len();
                for &(from, to) in &wires {
                    if audible.contains(&from) {
                        audible.insert(to);
                    }
                }
                if audible.len() == before {
                    break;
                }
            }
            muted.extend(siblings.into_iter().filter(|id| !audible.contains(id)));
        }
        muted
    }

    /// Whether solo has been used on the groups beside this one (or on it):
    /// any of them has a solo parameter set, even to off. While it has, the
    /// compiler keeps a mute stage on each of them, so soloing and unsoloing
    /// change a parameter rather than the shape of the graph.
    pub fn solo_in_use(&self, group: NodeId) -> bool {
        let Some(parent) = self.node(group).map(|n| n.parent) else {
            return false;
        };
        self.nodes()
            .filter(|(_, n)| n.type_id == GROUP && n.parent == parent)
            .any(|(id, _)| {
                self.children(Some(id)).any(|(_, b)| {
                    matches!(b.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT) && b.has_solo_param()
                })
            })
    }

    /// Whether any port of this group has solo on.
    pub fn has_own_solo(&self, group: NodeId) -> bool {
        self.children(Some(group)).any(|(_, node)| {
            node.boundary_ports()
                .iter()
                .any(|&(id, _)| node.port_controls(id).solo)
        })
    }

    /// Whether this group or any group inside it is soloed.
    pub fn has_solo_inside(&self, group: NodeId) -> bool {
        self.has_own_solo(group)
            || self.descendants(group).into_iter().any(|id| {
                self.node(id).is_some_and(|n| n.type_id == GROUP) && self.has_own_solo(id)
            })
    }

    /// A group's ports, from the boundary nodes inside it. If two ports of
    /// the same kind share a name, the first counts.
    pub fn group_ports(&self, group: NodeId) -> GroupPorts {
        let mut ports = GroupPorts::default();
        for (node_id, node) in self.children(Some(group)) {
            let list = match node.type_id.as_str() {
                GROUP_INPUT => &mut ports.inputs,
                GROUP_OUTPUT => &mut ports.outputs,
                _ => continue,
            };
            for (id, name) in node.boundary_ports() {
                if !list.iter().any(|p| p.name == name) {
                    list.push(GroupPort {
                        name,
                        node: node_id,
                        id,
                    });
                }
            }
        }
        ports
    }
}

/// Converts a graph saved before a group's ports shared one boundary node
/// each: every boundary node then was one port, named by its `name` config.
/// For each group and side, the lowest-ID such node takes all the ports, in ID
/// order, and the others go. Wires and lanes on a replaced node move to its
/// port on the survivor. `None` if no boundary node needs it.
pub(crate) fn migrate_boundaries(
    graph: &Graph,
    lanes: &mut BTreeMap<crate::LaneId, crate::AutomationLane>,
) -> Option<Graph> {
    let legacy = |node: &Node| {
        matches!(node.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT)
            && node.config.get(PORTS).is_none()
    };
    // (group, kind) -> the old boundary nodes, in ID order.
    let mut sets: BTreeMap<(Option<NodeId>, &str), Vec<NodeId>> = BTreeMap::new();
    for (id, node) in graph.nodes().filter(|(_, n)| legacy(n)) {
        let kind = if node.type_id == GROUP_INPUT {
            GROUP_INPUT
        } else {
            GROUP_OUTPUT
        };
        sets.entry((node.parent, kind)).or_default().push(id);
    }
    if sets.is_empty() {
        return None;
    }
    // Old node -> (survivor, port ID).
    let mut moved: BTreeMap<NodeId, (NodeId, u32)> = BTreeMap::new();
    let mut nodes: BTreeMap<NodeId, Node> = graph.nodes().map(|(id, n)| (id, n.clone())).collect();
    for ids in sets.values() {
        let survivor = ids[0];
        let mut ports: Vec<(u32, String)> = Vec::new();
        let (mut x, mut y) = (f32::INFINITY, f32::INFINITY);
        let mut params = BTreeMap::new();
        for (k, &id) in ids.iter().enumerate() {
            let port = k as u32 + 1;
            let node = &graph.node(id).expect("listed above");
            let mut name = match node.config.get(PORT_NAME) {
                Some(Value::Text(name)) if !name.is_empty() => name.replace('\n', " "),
                _ => format!("port{}", id.0),
            };
            if ports.iter().any(|(_, other)| *other == name) {
                name = format!("{name}_{port}");
            }
            ports.push((port, name));
            x = x.min(node.position.x);
            y = y.min(node.position.y);
            for control in [GAIN, MUTE, SOLO] {
                if let Some(value) = node.params.get(control) {
                    params.insert(control_key(control, port), *value);
                }
            }
            moved.insert(id, (survivor, port));
            if id != survivor {
                nodes.remove(&id);
            }
        }
        let node = nodes.get_mut(&survivor).expect("kept");
        node.config = ports_config(&ports);
        node.params = params;
        node.position = Position { x, y };
    }
    // An old node's wire ports: signal, or one of its controls.
    let remap = |endpoint: &Endpoint| -> Endpoint {
        let Some(&(survivor, port)) = moved.get(&endpoint.node) else {
            return endpoint.clone();
        };
        let key = match endpoint.port.as_str() {
            "in" | "out" => port_key(port),
            control @ (GAIN | MUTE | SOLO) => control_key(control, port),
            other => other.to_owned(),
        };
        Endpoint::new(survivor, key)
    };
    let connections: Vec<Connection> = graph
        .connections()
        .map(|c| Connection {
            from: remap(&c.from),
            to: remap(&c.to),
        })
        .collect();
    for lane in lanes.values_mut() {
        lane.target = remap(&lane.target);
    }
    Graph::from_parts(nodes, connections).ok()
}

/// Makes a track: a group holding a track input node, with its `audio`
/// output wired to the group's output by default, and a group input beside
/// them so the group has the same controls at both ends. One undo step.
///
/// Returns the group's ID and the command. `parent` is the group the track
/// goes in, `None` for the top level. The group input is named `in` and the
/// output `out`.
pub fn create_track(
    parent: Option<NodeId>,
    position: Position,
    mut new_id: impl FnMut() -> NodeId,
) -> (NodeId, Command) {
    let group = new_id();
    let (input, source, output) = (new_id(), new_id(), new_id());
    let inside = |node: Node, x: f32, y: f32| {
        let mut node = node.at(position.x + x, position.y + y);
        node.parent = Some(group);
        node
    };
    let named =
        |kind: &str, name: &str| Node::new(kind).with_config(ports_config(&[(1, name.to_owned())]));
    let mut group_node = Node::new(GROUP).at(position.x, position.y);
    group_node.parent = parent;
    let command = Command::Batch(vec![
        Command::AddNode {
            id: group,
            node: group_node,
        },
        Command::AddNode {
            id: input,
            node: inside(named(GROUP_INPUT, "in"), -300.0, 0.0),
        },
        Command::AddNode {
            id: source,
            node: inside(Node::new(TRACK_INPUT), 0.0, 0.0),
        },
        Command::AddNode {
            id: output,
            node: inside(
                named(GROUP_OUTPUT, "out")
                    .with_param(control_key(GAIN, 1), 0.0)
                    .with_param(control_key(MUTE, 1), 0.0),
                300.0,
                0.0,
            ),
        },
        Command::Connect(Connection {
            from: Endpoint::new(source, AUDIO),
            to: Endpoint::new(output, port_key(1)),
        }),
    ]);
    (group, command)
}

/// Folds `nodes` into a new group and returns its ID with the command that
/// does it, as one undo step.
///
/// The nodes must share a parent. Each wire that crossed the edge of the
/// selection now passes through a port of the new group's boundary nodes: one
/// input per distinct source outside, and one output per distinct source
/// inside, named `in1`, `in2`, … and `out1`, `out2`, … in wire order.
///
/// `new_id` hands out the IDs of the new nodes, e.g.
/// [`Project::new_node_id`]. It's a closure so the editor can use it with
/// only a shared borrow of the project.
pub fn group_nodes(
    project: &Project,
    nodes: &[NodeId],
    mut new_id: impl FnMut() -> NodeId,
) -> Result<(NodeId, Command), EditError> {
    let first = *nodes.first().ok_or(EditError::NothingToGroup)?;
    let graph = project.graph();
    let parent = graph
        .node(first)
        .ok_or(EditError::NoSuchNode(first))?
        .parent;
    let mut inside = std::collections::BTreeSet::new();
    for &id in nodes {
        let node = graph.node(id).ok_or(EditError::NoSuchNode(id))?;
        if node.parent != parent {
            return Err(EditError::NotSiblings(id));
        }
        // A boundary node is a port of the group it's in; moving it into a
        // new group would leave the old group with a port that has no node.
        if matches!(node.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT) {
            return Err(EditError::BoundaryNode(id));
        }
        inside.insert(id);
    }

    let mut centre = (0.0, 0.0);
    for id in &inside {
        let p = graph.node(*id).expect("checked above").position;
        centre = (
            centre.0 + p.x / inside.len() as f32,
            centre.1 + p.y / inside.len() as f32,
        );
    }

    // Wires that cross the selection's edge, in input order. Wires inside
    // the selection also come off for the move, since the nodes change group
    // one at a time, and go back on afterwards.
    let touching: Vec<Connection> = graph
        .connections()
        .filter(|c| inside.contains(&c.from.node) || inside.contains(&c.to.node))
        .collect();
    let crossing: Vec<Connection> = touching
        .iter()
        .filter(|c| inside.contains(&c.from.node) != inside.contains(&c.to.node))
        .cloned()
        .collect();

    let group = new_id();
    let mut cmds = Vec::new();
    for c in &touching {
        cmds.push(Command::Disconnect {
            input: c.to.clone(),
        });
    }
    let mut group_node = Node::new(GROUP).at(centre.0, centre.1);
    group_node.parent = parent;
    cmds.push(Command::AddNode {
        id: group,
        node: group_node,
    });
    for &id in &inside {
        cmds.push(Command::SetParent {
            node: id,
            parent: Some(group),
        });
    }

    // One port per distinct outside source feeding in, and one per distinct
    // inside source leading out, on a single boundary node of each kind.
    let mut inputs: BTreeMap<Endpoint, (u32, String)> = BTreeMap::new();
    let mut outputs: BTreeMap<Endpoint, (u32, String)> = BTreeMap::new();
    let (input_node, output_node) = (new_id(), new_id());
    let mut wires: Vec<Connection> = touching
        .iter()
        .filter(|c| !crossing.contains(c))
        .cloned()
        .collect();
    for c in &crossing {
        if inside.contains(&c.to.node) {
            if !inputs.contains_key(&c.from) {
                let id = inputs.len() as u32 + 1;
                let name = format!("in{id}");
                wires.push(Connection {
                    from: c.from.clone(),
                    to: Endpoint::new(group, name.clone()),
                });
                inputs.insert(c.from.clone(), (id, name));
            }
            wires.push(Connection {
                from: Endpoint::new(input_node, port_key(inputs[&c.from].0)),
                to: c.to.clone(),
            });
        } else {
            if !outputs.contains_key(&c.from) {
                let id = outputs.len() as u32 + 1;
                let name = format!("out{id}");
                wires.push(Connection {
                    from: c.from.clone(),
                    to: Endpoint::new(output_node, port_key(id)),
                });
                outputs.insert(c.from.clone(), (id, name));
            }
            wires.push(Connection {
                from: Endpoint::new(group, outputs[&c.from].1.clone()),
                to: c.to.clone(),
            });
        }
    }
    for (kind, id, ports, x) in [
        (GROUP_INPUT, input_node, &inputs, -300.0),
        (GROUP_OUTPUT, output_node, &outputs, 300.0),
    ] {
        if ports.is_empty() {
            continue;
        }
        let mut list: Vec<(u32, String)> = ports.values().cloned().collect();
        list.sort();
        let mut node = Node::new(kind).with_config(ports_config(&list));
        node.parent = Some(group);
        node.position = Position {
            x: centre.0 + x,
            y: centre.1,
        };
        cmds.push(Command::AddNode { id, node });
    }
    cmds.extend(wires.into_iter().map(Command::Connect));
    Ok((group, Command::Batch(cmds)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;

    fn add(project: &mut Project, history: &mut History, node: Node) -> NodeId {
        let id = project.new_node_id();
        history
            .apply(project, Command::AddNode { id, node })
            .unwrap();
        id
    }

    fn wire(
        project: &mut Project,
        history: &mut History,
        from: (NodeId, &str),
        to: (NodeId, &str),
    ) {
        let connection = Connection {
            from: Endpoint::new(from.0, from.1),
            to: Endpoint::new(to.0, to.1),
        };
        history
            .apply(project, Command::Connect(connection))
            .unwrap();
    }

    /// osc -> gain -> out, with the gain in the middle to be grouped.
    fn chain() -> (Project, History, [NodeId; 3]) {
        let mut project = Project::new();
        let mut history = History::new();
        let osc = add(&mut project, &mut history, Node::new("osc"));
        let gain = add(&mut project, &mut history, Node::new("gain").at(10.0, 20.0));
        let out = add(&mut project, &mut history, Node::new("out"));
        wire(&mut project, &mut history, (osc, "out"), (gain, "in"));
        wire(&mut project, &mut history, (gain, "out"), (out, "in"));
        (project, history, [osc, gain, out])
    }

    #[test]
    fn grouping_routes_crossing_wires_through_boundary_nodes() {
        let (mut project, mut history, [osc, gain, out]) = chain();
        let (group, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        let graph = project.graph();

        assert_eq!(graph.node(gain).unwrap().parent, Some(group));
        assert_eq!(graph.node(group).unwrap().parent, None);
        let ports = graph.group_ports(group);
        assert_eq!(ports.inputs.len(), 1);
        assert_eq!(ports.outputs.len(), 1);
        assert_eq!(ports.inputs[0].name, "in1");
        // Outside: osc -> group.in1, group.out1 -> out.
        assert_eq!(
            graph.source(&Endpoint::new(group, "in1")),
            Some(&Endpoint::new(osc, "out"))
        );
        assert_eq!(
            graph.source(&Endpoint::new(out, "in")),
            Some(&Endpoint::new(group, "out1"))
        );
        // Inside: boundary in -> gain -> boundary out.
        assert_eq!(
            graph.source(&Endpoint::new(gain, "in")),
            Some(&Endpoint::new(ports.inputs[0].node, ports.inputs[0].key()))
        );
        assert_eq!(
            graph.source(&Endpoint::new(
                ports.outputs[0].node,
                ports.outputs[0].key()
            )),
            Some(&Endpoint::new(gain, "out"))
        );
    }

    #[test]
    fn grouping_is_one_undo_step() {
        let (mut project, mut history, [_, gain, _]) = chain();
        let before = project.clone();
        let (_, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        assert_ne!(project, before);
        assert!(history.undo(&mut project).unwrap());
        assert_eq!(project, before);
        assert!(history.redo(&mut project).unwrap());
        assert_ne!(project, before);
    }

    #[test]
    fn a_groups_own_boundary_nodes_cannot_be_grouped() {
        let (mut project, mut history, [_, gain, _]) = chain();
        let (group, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        let boundary = project.graph().group_ports(group).inputs[0].node;
        let result = group_nodes(&project.clone(), &[gain, boundary], || {
            project.new_node_id()
        });
        assert_eq!(result.unwrap_err(), EditError::BoundaryNode(boundary));
    }

    #[test]
    fn one_boundary_input_per_source() {
        let mut project = Project::new();
        let mut history = History::new();
        let osc = add(&mut project, &mut history, Node::new("osc"));
        let a = add(&mut project, &mut history, Node::new("mix"));
        let b = add(&mut project, &mut history, Node::new("mix"));
        wire(&mut project, &mut history, (osc, "out"), (a, "in"));
        wire(&mut project, &mut history, (osc, "out"), (b, "in"));
        let (group, command) =
            group_nodes(&project.clone(), &[a, b], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        assert_eq!(project.graph().group_ports(group).inputs.len(), 1);
    }

    #[test]
    fn wires_between_groups_are_refused() {
        let (mut project, mut history, [osc, gain, _]) = chain();
        let (_, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        // gain is now inside a group; osc isn't.
        let error = history
            .apply(
                &mut project,
                Command::Connect(Connection {
                    from: Endpoint::new(osc, "out"),
                    to: Endpoint::new(gain, "in"),
                }),
            )
            .unwrap_err();
        assert!(matches!(error, EditError::DifferentGroups(_)), "{error}");
    }

    #[test]
    fn removing_a_group_removes_its_contents_and_undo_restores_them() {
        let (mut project, mut history, [_, gain, _]) = chain();
        let (group, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        let grouped = project.clone();
        history
            .apply(&mut project, Command::RemoveNode { id: group })
            .unwrap();
        assert!(project.graph().node(gain).is_none());
        assert_eq!(project.graph().nodes().count(), 2);
        assert_eq!(project.graph().connections().count(), 0);
        history.undo(&mut project).unwrap();
        assert_eq!(project, grouped);
    }

    #[test]
    fn nested_groups_remove_and_restore() {
        let (mut project, mut history, [_, gain, _]) = chain();
        let (inner, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        let (outer, command) =
            group_nodes(&project.clone(), &[inner], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        let graph = project.graph();
        assert_eq!(graph.ancestors(gain), vec![outer, inner]);
        assert!(graph.is_inside(gain, outer));
        assert!(!graph.is_inside(outer, gain));
        let nested = project.clone();
        history
            .apply(&mut project, Command::RemoveNode { id: outer })
            .unwrap();
        history.undo(&mut project).unwrap();
        assert_eq!(project, nested);
    }

    #[test]
    fn a_group_cannot_move_inside_itself_or_a_non_group() {
        let (mut project, mut history, [osc, gain, _]) = chain();
        let (group, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        let inside = |p, parent| Command::SetParent {
            node: p,
            parent: Some(parent),
        };
        assert!(matches!(
            history.apply(&mut project, inside(group, group)),
            Err(EditError::GroupInsideItself(_))
        ));
        assert!(matches!(
            history.apply(&mut project, inside(osc, gain)),
            Err(EditError::NotAGroup(_))
        ));
    }

    #[test]
    fn round_trips_through_ron_and_loads_children_before_their_group() {
        let (mut project, mut history, [_, gain, _]) = chain();
        // The group gets a higher ID than its contents, as when grouping.
        let (_, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        let text = project.to_ron();
        assert_eq!(Project::from_ron(&text).unwrap(), project, "{text}");
    }

    #[test]
    fn rejects_a_file_with_a_parent_that_is_not_a_group() {
        let text = r#"(format: 1, graph: (nodes: {
            1: (type: "osc"),
            2: (type: "gain", parent: Some(1)),
        }))"#;
        let error = Project::from_ron(text).unwrap_err().to_string();
        assert!(error.contains("isn't a group"), "{error}");
    }

    #[test]
    fn rejects_a_file_with_a_group_cycle() {
        let text = r#"(format: 1, graph: (nodes: {
            1: (type: "noodle.group", parent: Some(2)),
            2: (type: "noodle.group", parent: Some(1)),
        }))"#;
        assert!(Project::from_ron(text).is_err());
    }

    #[test]
    fn a_boundary_nodes_ports_round_trip_through_its_config() {
        let ports = vec![(1, "drums".to_owned()), (4, "a: b".to_owned())];
        let node = Node::new(GROUP_INPUT).with_config(ports_config(&ports));
        assert_eq!(node.boundary_ports(), ports);
        // A line break in a name can't split it into two ports.
        let node = Node::new(GROUP_INPUT).with_config(ports_config(&[(2, "a\nb".into())]));
        assert_eq!(node.boundary_ports(), vec![(2, "a b".to_owned())]);
        assert!(Node::new("osc").boundary_ports().is_empty());
        assert_eq!(parse_port_key(&port_key(12)), Some(12));
        assert_eq!(parse_control_key(&control_key(MUTE, 3)), Some((MUTE, 3)));
        assert_eq!(parse_control_key("gain"), None);
    }

    #[test]
    fn a_stage_id_is_unique_to_its_port_and_clear_of_project_nodes() {
        let (a, b) = (stage_id(NodeId(5), 1), stage_id(NodeId(5), 2));
        assert_ne!(a, b);
        assert_ne!(a, stage_id(NodeId(6), 1));
        assert!(a.0 > u64::from(u32::MAX));
    }

    /// A group saved with a boundary node per port (format 3): two inputs, two
    /// outputs, a gain on the second output, a wire into the first input's
    /// mute, and a lane on the second output's gain.
    const ONE_NODE_PER_PORT: &str = r#"(
        format: 3,
        graph: (
            nodes: {
                1: (type: "osc"),
                2: (type: "noodle.group"),
                3: (type: "noodle.group.input", config: {"name": "a"}, parent: Some(2), position: (x: -100.0, y: 40.0)),
                4: (type: "noodle.group.input", config: {"name": "b"}, parent: Some(2), position: (x: -120.0, y: 90.0)),
                5: (type: "noodle.group.output", config: {"name": "left"}, parent: Some(2)),
                6: (type: "noodle.group.output", config: {"name": "right"}, params: {"gain": -6.0, "solo": 1.0}, parent: Some(2)),
                7: (type: "gain", parent: Some(2)),
                8: (type: "const", parent: Some(2)),
                9: (type: "out"),
            },
            connections: [
                (from: (node: 1, port: "out"), to: (node: 2, port: "a")),
                (from: (node: 3, port: "out"), to: (node: 7, port: "in")),
                (from: (node: 4, port: "out"), to: (node: 5, port: "in")),
                (from: (node: 7, port: "out"), to: (node: 6, port: "in")),
                (from: (node: 8, port: "out"), to: (node: 3, port: "mute")),
                (from: (node: 2, port: "right"), to: (node: 9, port: "in")),
            ],
        ),
        lanes: {1: (target: (node: 6, port: "gain"), points: [(tick: 0, value: -3.0, curve: Linear)])},
    )"#;

    #[test]
    fn a_node_per_port_file_loads_as_one_node_per_side() {
        let project = Project::from_ron(ONE_NODE_PER_PORT).unwrap();
        let graph = project.graph();
        let ports = graph.group_ports(NodeId(2));
        let names = |list: &[GroupPort]| list.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&ports.inputs), ["a", "b"]);
        assert_eq!(names(&ports.outputs), ["left", "right"]);
        // The lowest ID of each side keeps the node; the rest are gone.
        assert!(ports.inputs.iter().all(|p| p.node == NodeId(3)));
        assert!(ports.outputs.iter().all(|p| p.node == NodeId(5)));
        assert_eq!(graph.nodes().count(), 7);
        // Ports are numbered in ID order, and wires follow them.
        assert_eq!(
            graph.source(&Endpoint::new(NodeId(7), "in")),
            Some(&Endpoint::new(NodeId(3), "p1"))
        );
        assert_eq!(
            graph.source(&Endpoint::new(NodeId(5), "p1")),
            Some(&Endpoint::new(NodeId(3), "p2"))
        );
        assert_eq!(
            graph.source(&Endpoint::new(NodeId(5), "p2")),
            Some(&Endpoint::new(NodeId(7), "out"))
        );
        // Outside wires use the names, as they always did.
        assert_eq!(
            graph.source(&Endpoint::new(NodeId(2), "a")),
            Some(&Endpoint::new(NodeId(1), "out"))
        );
        assert_eq!(
            graph.source(&Endpoint::new(NodeId(3), "mute.1")),
            Some(&Endpoint::new(NodeId(8), "out"))
        );
        // Controls move to their port, and so does a lane on them.
        let right = graph.node(NodeId(5)).unwrap();
        assert_eq!(right.params.get("gain.2"), Some(&-6.0));
        assert!(right.port_controls(2).solo && !right.port_controls(1).solo);
        assert!(!right.params.contains_key("gain"));
        let (_, lane) = project.lanes().next().unwrap();
        assert_eq!(lane.target, Endpoint::new(NodeId(5), "gain.2"));
        assert_eq!(graph.control_port(NodeId(2)).unwrap().name, "left");
        // And it saves in the new form, which loads as it is.
        let text = project.to_ron();
        assert_eq!(Project::from_ron(&text).unwrap(), project, "{text}");
    }

    /// A group at `parent` with an input and output node.
    fn group_with_ports(
        project: &mut Project,
        history: &mut History,
        parent: Option<NodeId>,
    ) -> (NodeId, NodeId) {
        let mut node = Node::new(GROUP);
        node.parent = parent;
        let group = add(project, history, node);
        let named = |kind: &str| {
            Node::new(kind)
                .with_config(ports_config(&[(1, "p".into())]))
                .in_group(group)
        };
        add(project, history, named(GROUP_INPUT));
        let output = add(project, history, named(GROUP_OUTPUT));
        (group, output)
    }

    fn set(project: &mut Project, history: &mut History, node: NodeId, key: &str, value: f32) {
        let command = Command::SetParam {
            node,
            key: key.into(),
            value: Some(value),
        };
        history.apply(project, command).unwrap();
    }

    #[test]
    fn a_groups_controls_are_its_boundary_nodes_parameters() {
        let mut project = Project::new();
        let mut history = History::new();
        let (group, output) = group_with_ports(&mut project, &mut history, None);
        assert_eq!(project.graph().group_controls(group), Controls::default());
        set(
            &mut project,
            &mut history,
            output,
            &control_key(GAIN, 1),
            -6.0,
        );
        set(
            &mut project,
            &mut history,
            output,
            &control_key(MUTE, 1),
            1.0,
        );
        set(
            &mut project,
            &mut history,
            output,
            &control_key(SOLO, 1),
            0.4,
        );
        let controls = project.graph().group_controls(group);
        assert_eq!(controls.gain_db, -6.0);
        assert!(controls.mute);
        assert!(!controls.solo, "under half is off");
        // A NaN gain from a damaged file is the default, not NaN.
        set(
            &mut project,
            &mut history,
            output,
            &control_key(GAIN, 1),
            f32::NAN,
        );
        assert_eq!(project.graph().group_controls(group).gain_db, 0.0);
    }

    #[test]
    fn solo_mutes_the_other_groups_at_the_same_level() {
        let mut project = Project::new();
        let mut history = History::new();
        let (a, a_out) = group_with_ports(&mut project, &mut history, None);
        let (b, _) = group_with_ports(&mut project, &mut history, None);
        assert!(project.graph().solo_muted().is_empty());
        set(
            &mut project,
            &mut history,
            a_out,
            &control_key(SOLO, 1),
            1.0,
        );
        assert_eq!(project.graph().solo_muted(), [b].into());
        assert!(project.graph().has_own_solo(a));
        assert!(!project.graph().has_own_solo(b));
    }

    #[test]
    fn solo_inside_a_bus_keeps_the_bus_and_mutes_its_siblings() {
        let mut project = Project::new();
        let mut history = History::new();
        let (bus1, _) = group_with_ports(&mut project, &mut history, None);
        let (bus2, _) = group_with_ports(&mut project, &mut history, None);
        let (track1, track1_out) = group_with_ports(&mut project, &mut history, Some(bus1));
        let (track2, _) = group_with_ports(&mut project, &mut history, Some(bus1));
        let (track3, _) = group_with_ports(&mut project, &mut history, Some(bus2));
        set(
            &mut project,
            &mut history,
            track1_out,
            &control_key(SOLO, 1),
            1.0,
        );
        let muted = project.graph().solo_muted();
        assert_eq!(muted, [bus2, track2].into());
        assert!(project.graph().has_solo_inside(bus1));
        assert!(!muted.contains(&bus1) && !muted.contains(&track1));
        // Nothing inside bus2 is soloed, so its track isn't muted separately
        // (the bus is, which silences it).
        assert!(!muted.contains(&track3));
    }

    #[test]
    fn what_a_soloed_group_feeds_stays_audible() {
        let mut project = Project::new();
        let mut history = History::new();
        let (vocal, vocal_out) = group_with_ports(&mut project, &mut history, None);
        let (drums, _) = group_with_ports(&mut project, &mut history, None);
        let (reverb, _) = group_with_ports(&mut project, &mut history, None);
        let (master, _) = group_with_ports(&mut project, &mut history, None);
        // vocal -> a mixer node -> reverb -> master; drums -> master.
        let mix = add(&mut project, &mut history, Node::new("mix"));
        wire(&mut project, &mut history, (vocal, "p"), (mix, "in1"));
        wire(&mut project, &mut history, (mix, "out"), (reverb, "p"));
        wire(&mut project, &mut history, (reverb, "p"), (master, "p"));
        wire(&mut project, &mut history, (drums, "p"), (master, "p2"));
        set(
            &mut project,
            &mut history,
            vocal_out,
            &control_key(SOLO, 1),
            1.0,
        );
        // Only the drums, which the vocal doesn't feed, are silenced.
        assert_eq!(project.graph().solo_muted(), [drums].into());
    }

    #[test]
    fn controls_are_set_on_the_output_port_when_there_is_one() {
        let mut project = Project::new();
        let mut history = History::new();
        let (group, output) = group_with_ports(&mut project, &mut history, None);
        let port = project.graph().control_port(group).unwrap();
        assert_eq!((port.node, port.id), (output, 1));
        let bare = add(&mut project, &mut history, Node::new(GROUP));
        assert_eq!(project.graph().control_port(bare), None);
        assert_eq!(project.graph().group_controls(bare), Controls::default());
    }
}

#[cfg(test)]
mod track_tests {
    use super::*;
    use crate::History;

    #[test]
    fn creating_a_track_is_one_undo_step_and_wires_audio_to_the_output() {
        let mut project = Project::new();
        let mut history = History::new();
        let before = project.clone();
        let (group, command) = create_track(None, Position::default(), || project.new_node_id());
        history.apply(&mut project, command).unwrap();

        let graph = project.graph();
        let ports = graph.group_ports(group);
        assert_eq!(ports.inputs.len(), 1);
        assert_eq!(ports.outputs.len(), 1);
        assert_eq!(ports.outputs[0].name, "out");
        let (source, _) = graph
            .children(Some(group))
            .find(|(_, n)| n.type_id == TRACK_INPUT)
            .expect("a track input node");
        assert_eq!(
            graph.source(&Endpoint::new(
                ports.outputs[0].node,
                ports.outputs[0].key()
            )),
            Some(&Endpoint::new(source, AUDIO))
        );

        assert!(history.undo(&mut project).unwrap());
        assert_eq!(project, before);
    }

    #[test]
    fn a_new_track_starts_with_its_output_controls_set_to_the_defaults() {
        let mut project = Project::new();
        let (group, command) = create_track(None, Position::default(), || project.new_node_id());
        History::new().apply(&mut project, command).unwrap();
        let graph = project.graph();
        let out = graph.group_ports(group).outputs[0].node;
        let node = graph.node(out).unwrap();
        assert!(node.has_gain_or_mute(1));
        assert_eq!(node.port_controls(1), Controls::default());
    }

    #[test]
    fn a_track_can_go_inside_another_group() {
        let mut project = Project::new();
        let mut history = History::new();
        let (outer, command) = create_track(None, Position::default(), || project.new_node_id());
        history.apply(&mut project, command).unwrap();
        let (inner, command) =
            create_track(Some(outer), Position::default(), || project.new_node_id());
        history.apply(&mut project, command).unwrap();
        assert_eq!(project.graph().ancestors(inner), vec![outer]);
    }
}
