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
/// The config key holding a boundary node's port name.
pub const PORT_NAME: &str = "name";
/// The port of a [`GROUP_INPUT`] node.
pub const INPUT_PORT: &str = "out";
/// The port of a [`GROUP_OUTPUT`] node.
pub const OUTPUT_PORT: &str = "in";

/// The node type that stands in for a boundary node with something to do. It
/// exists only in the flattened graph the compiler sees: flatten swaps it in
/// for a boundary node whose controls are off their defaults, under the same
/// ID, so automation aimed at the boundary node reaches it.
pub const GROUP_STAGE: &str = "noodle.group.stage";
/// The controls every boundary node carries, as parameters: a gain in
/// decibels, and mute and solo switches (on at 0.5 or more).
pub const GAIN: &str = "gain";
pub const MUTE: &str = "mute";
pub const SOLO: &str = "solo";
/// On a stage only: set by the compiler while another track's solo mutes
/// this one. Kept apart from [`MUTE`] so an automation lane driving the mute
/// can't override it.
pub const SOLO_MUTE: &str = "solo_mute";

/// A boundary node's gain, mute and solo, with the defaults for any that
/// aren't set. A track's controls are these, on its group's boundary nodes.
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
    /// Whether a gain or mute has been set on this node, even to its
    /// default. A set control keeps the node's stage in the compiled graph,
    /// so moving it again changes a parameter and not the graph's shape.
    pub fn has_gain_or_mute(&self) -> bool {
        self.params.contains_key(GAIN) || self.params.contains_key(MUTE)
    }

    /// This node's gain, mute and solo parameters. Meaningful for boundary
    /// nodes.
    pub fn controls(&self) -> Controls {
        let param = |key: &str| self.params.get(key).copied();
        Controls {
            gain_db: param(GAIN).filter(|g| g.is_finite()).unwrap_or(0.0),
            mute: param(MUTE).is_some_and(|m| m >= 0.5),
            solo: param(SOLO).is_some_and(|s| s >= 0.5),
        }
    }
}

/// One of a group's ports, and the boundary node inside that backs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPort {
    pub name: String,
    pub node: NodeId,
}

/// A group's inputs and outputs, in boundary-node ID order.
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

/// The name a boundary node exposes its port under. A node without a `name`
/// config gets one from its ID, so it still has a port.
pub fn port_name(id: NodeId, node: &Node) -> String {
    match node.config.get(PORT_NAME) {
        Some(Value::Text(name)) if !name.is_empty() => name.clone(),
        _ => format!("port{}", id.0),
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

    /// A group's gain, mute and solo, as the mixer shows them: the gain and
    /// mute of its first output node (a track has one), and solo if any of
    /// the group's boundary nodes has it on.
    pub fn group_controls(&self, group: NodeId) -> Controls {
        let ports = self.group_ports(group);
        let first = ports.outputs.first().or(ports.inputs.first());
        let mut controls = first
            .and_then(|port| self.node(port.node))
            .map(Node::controls)
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
                    matches!(b.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT)
                        && b.params.contains_key(SOLO)
                })
            })
    }

    /// Whether any boundary node of this group has solo on.
    pub fn has_own_solo(&self, group: NodeId) -> bool {
        self.children(Some(group)).any(|(_, node)| {
            matches!(node.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT) && node.controls().solo
        })
    }

    /// Whether this group or any group inside it is soloed.
    pub fn has_solo_inside(&self, group: NodeId) -> bool {
        self.has_own_solo(group)
            || self.descendants(group).into_iter().any(|id| {
                self.node(id).is_some_and(|n| n.type_id == GROUP) && self.has_own_solo(id)
            })
    }

    /// A group's ports, from the boundary nodes inside it. If two boundary
    /// nodes of the same kind share a name, the one with the lower ID counts.
    pub fn group_ports(&self, group: NodeId) -> GroupPorts {
        let mut ports = GroupPorts::default();
        for (id, node) in self.children(Some(group)) {
            let list = match node.type_id.as_str() {
                GROUP_INPUT => &mut ports.inputs,
                GROUP_OUTPUT => &mut ports.outputs,
                _ => continue,
            };
            let name = port_name(id, node);
            if !list.iter().any(|p| p.name == name) {
                list.push(GroupPort { name, node: id });
            }
        }
        ports
    }
}

/// Folds `nodes` into a new group and returns its ID with the command that
/// does it, as one undo step.
///
/// The nodes must share a parent. Each wire that crossed the edge of the
/// selection now passes through a boundary node: one input per distinct
/// source outside, and one output per distinct source inside, named `in1`,
/// `in2`, … and `out1`, `out2`, … in wire order.
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

    // One boundary node per distinct outside source feeding in, and one per
    // distinct inside source leading out.
    let mut inputs: BTreeMap<Endpoint, (NodeId, String)> = BTreeMap::new();
    let mut outputs: BTreeMap<Endpoint, (NodeId, String)> = BTreeMap::new();
    let mut wires: Vec<Connection> = touching
        .iter()
        .filter(|c| !crossing.contains(c))
        .cloned()
        .collect();
    let boundary = |kind: &str, name: &str, at: Position| {
        let config = crate::Config::new().with(PORT_NAME, Value::Text(name.to_string()));
        let mut node = Node::new(kind).with_config(config);
        node.parent = Some(group);
        node.position = at;
        node
    };
    for c in &crossing {
        if inside.contains(&c.to.node) {
            if !inputs.contains_key(&c.from) {
                let id = new_id();
                let name = format!("in{}", inputs.len() + 1);
                let at = Position {
                    x: centre.0 - 300.0,
                    y: centre.1 + 60.0 * inputs.len() as f32,
                };
                cmds.push(Command::AddNode {
                    id,
                    node: boundary(GROUP_INPUT, &name, at),
                });
                wires.push(Connection {
                    from: c.from.clone(),
                    to: Endpoint::new(group, name.clone()),
                });
                inputs.insert(c.from.clone(), (id, name));
            }
            wires.push(Connection {
                from: Endpoint::new(inputs[&c.from].0, INPUT_PORT),
                to: c.to.clone(),
            });
        } else {
            if !outputs.contains_key(&c.from) {
                let id = new_id();
                let name = format!("out{}", outputs.len() + 1);
                let at = Position {
                    x: centre.0 + 300.0,
                    y: centre.1 + 60.0 * outputs.len() as f32,
                };
                cmds.push(Command::AddNode {
                    id,
                    node: boundary(GROUP_OUTPUT, &name, at),
                });
                wires.push(Connection {
                    from: c.from.clone(),
                    to: Endpoint::new(id, OUTPUT_PORT),
                });
                outputs.insert(c.from.clone(), (id, name));
            }
            wires.push(Connection {
                from: Endpoint::new(group, outputs[&c.from].1.clone()),
                to: c.to.clone(),
            });
        }
    }
    cmds.extend(wires.into_iter().map(Command::Connect));
    Ok((group, Command::Batch(cmds)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, History};

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
            Some(&Endpoint::new(ports.inputs[0].node, INPUT_PORT))
        );
        assert_eq!(
            graph.source(&Endpoint::new(ports.outputs[0].node, OUTPUT_PORT)),
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
    fn ports_without_a_name_get_one_from_their_id() {
        let node = Node::new(GROUP_INPUT);
        assert_eq!(port_name(NodeId(7), &node), "port7");
        let named = node.with_config(Config::new().with(PORT_NAME, Value::Text("drums".into())));
        assert_eq!(port_name(NodeId(7), &named), "drums");
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
                .with_config(Config::new().with(PORT_NAME, Value::Text("p".into())))
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
        set(&mut project, &mut history, output, GAIN, -6.0);
        set(&mut project, &mut history, output, MUTE, 1.0);
        set(&mut project, &mut history, output, SOLO, 0.4);
        let controls = project.graph().group_controls(group);
        assert_eq!(controls.gain_db, -6.0);
        assert!(controls.mute);
        assert!(!controls.solo, "under half is off");
        // A NaN gain from a damaged file is the default, not NaN.
        set(&mut project, &mut history, output, GAIN, f32::NAN);
        assert_eq!(project.graph().group_controls(group).gain_db, 0.0);
    }

    #[test]
    fn solo_mutes_the_other_groups_at_the_same_level() {
        let mut project = Project::new();
        let mut history = History::new();
        let (a, a_out) = group_with_ports(&mut project, &mut history, None);
        let (b, _) = group_with_ports(&mut project, &mut history, None);
        assert!(project.graph().solo_muted().is_empty());
        set(&mut project, &mut history, a_out, SOLO, 1.0);
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
        set(&mut project, &mut history, track1_out, SOLO, 1.0);
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
        set(&mut project, &mut history, vocal_out, SOLO, 1.0);
        // Only the drums, which the vocal doesn't feed, are silenced.
        assert_eq!(project.graph().solo_muted(), [drums].into());
    }
}
