//! Flattening: turns a graph with group nodes into the plain graph the
//! compiler works on.
//!
//! A group costs nothing at run time. Its node and its boundary nodes are
//! dropped, and each wire that went through them is joined end to end, so
//! the nodes inside run as if they had been wired up at the top level.
//! Nodes keep their IDs, so a diagnostic still points at the right node.

use std::borrow::Cow;
use std::collections::BTreeSet;

use noodle_core::group::{GROUP, GROUP_INPUT, GROUP_OUTPUT, INPUT_PORT, OUTPUT_PORT};
use noodle_core::{Connection, Endpoint, Graph, NodeId};

/// Whether the graph has anything for [`flatten`] to do.
fn has_groups(graph: &Graph) -> bool {
    graph.nodes().any(|(_, node)| {
        matches!(node.type_id.as_str(), GROUP | GROUP_INPUT | GROUP_OUTPUT) || node.parent.is_some()
    })
}

/// Returns the graph with its groups flattened away. A graph with no groups
/// is returned as it is.
pub fn flatten(graph: &Graph) -> Cow<'_, Graph> {
    if !has_groups(graph) {
        return Cow::Borrowed(graph);
    }
    let is_structure = |id: NodeId| {
        graph
            .node(id)
            .is_some_and(|node| matches!(node.type_id.as_str(), GROUP | GROUP_INPUT | GROUP_OUTPUT))
    };

    let nodes = graph
        .nodes()
        .filter(|&(id, _)| !is_structure(id))
        .map(|(id, node)| {
            let mut node = node.clone();
            node.parent = None;
            (id, node)
        });
    let connections = graph
        .connections()
        .filter(|c| !is_structure(c.to.node))
        .filter_map(|c| {
            let from = resolve(graph, &c.to)?;
            Some(Connection { from, to: c.to })
        });
    Cow::Owned(
        Graph::from_parts(nodes, connections)
            .expect("a graph that loaded stays valid with its groups removed"),
    )
}

/// Follows an input's wire back through group boundaries to the real node
/// that produces its signal, or `None` if the chain ends at an unconnected
/// group input or output, or loops.
fn resolve(graph: &Graph, input: &Endpoint) -> Option<Endpoint> {
    let mut at = graph.source(input)?.clone();
    // Each endpoint is crossed at most once, so a loop of pass-through
    // groups ends instead of spinning.
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(at.clone()) {
            return None;
        }
        let node = graph.node(at.node)?;
        match node.type_id.as_str() {
            // Signal leaving a group: whatever feeds the matching output
            // node inside.
            GROUP => {
                let port = graph.group_ports(at.node).output(&at.port)?.node;
                at = graph.source(&Endpoint::new(port, OUTPUT_PORT))?.clone();
            }
            // Signal entering a group: whatever feeds the group's input of
            // that name from outside.
            GROUP_INPUT if at.port == INPUT_PORT => {
                let group = node.parent?;
                let name = graph
                    .group_ports(group)
                    .inputs
                    .into_iter()
                    .find(|p| p.node == at.node)?
                    .name;
                at = graph.source(&Endpoint::new(group, name))?.clone();
            }
            GROUP_INPUT | GROUP_OUTPUT => return None,
            _ => return Some(at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::group::{PORT_NAME, group_nodes};
    use noodle_core::{Command, Config, History, Node, Project, Value};

    fn add(project: &mut Project, history: &mut History, node: Node) -> NodeId {
        let id = project.new_node_id();
        history
            .apply(project, Command::AddNode { id, node })
            .unwrap();
        id
    }

    fn wire(p: &mut Project, h: &mut History, from: (NodeId, &str), to: (NodeId, &str)) {
        let c = Connection {
            from: Endpoint::new(from.0, from.1),
            to: Endpoint::new(to.0, to.1),
        };
        h.apply(p, Command::Connect(c)).unwrap();
    }

    fn named(kind: &str, name: &str, group: NodeId) -> Node {
        Node::new(kind)
            .with_config(Config::new().with(PORT_NAME, Value::Text(name.into())))
            .in_group(group)
    }

    fn chain() -> (Project, History, [NodeId; 3]) {
        let mut project = Project::new();
        let mut history = History::new();
        let osc = add(&mut project, &mut history, Node::new("osc"));
        let gain = add(&mut project, &mut history, Node::new("gain"));
        let out = add(&mut project, &mut history, Node::new("out"));
        wire(&mut project, &mut history, (osc, "out"), (gain, "in"));
        wire(&mut project, &mut history, (gain, "out"), (out, "in"));
        (project, history, [osc, gain, out])
    }

    #[test]
    fn a_graph_without_groups_is_untouched() {
        let (project, _, _) = chain();
        assert!(matches!(flatten(project.graph()), Cow::Borrowed(_)));
    }

    #[test]
    fn flattening_a_grouped_chain_gives_the_original_wiring() {
        let (mut project, mut history, [_, gain, _]) = chain();
        let flat_before = project.graph().clone();
        let (_, command) = group_nodes(&mut project, &[gain]).unwrap();
        history.apply(&mut project, command).unwrap();
        assert_ne!(project.graph(), &flat_before);
        assert_eq!(flatten(project.graph()).as_ref(), &flat_before);
    }

    #[test]
    fn nested_groups_flatten_through_every_level() {
        let (mut project, mut history, [_, gain, _]) = chain();
        let flat_before = project.graph().clone();
        let (inner, command) = group_nodes(&mut project, &[gain]).unwrap();
        history.apply(&mut project, command).unwrap();
        let (_, command) = group_nodes(&mut project, &[inner]).unwrap();
        history.apply(&mut project, command).unwrap();
        assert_eq!(flatten(project.graph()).as_ref(), &flat_before);
    }

    #[test]
    fn an_unconnected_group_input_leaves_the_inner_input_unwired() {
        let (mut project, mut history, [osc, gain, _]) = chain();
        let (_, command) = group_nodes(&mut project, &[gain]).unwrap();
        history.apply(&mut project, command).unwrap();
        let group = project
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == GROUP)
            .unwrap()
            .0;
        let input = Endpoint::new(group, "in1");
        history
            .apply(&mut project, Command::Disconnect { input })
            .unwrap();
        let flat = flatten(project.graph());
        assert!(flat.source(&Endpoint::new(gain, "in")).is_none());
        assert!(flat.node(osc).is_some());
    }

    #[test]
    fn a_pass_through_group_joins_its_two_sides() {
        let mut project = Project::new();
        let mut history = History::new();
        let osc = add(&mut project, &mut history, Node::new("osc"));
        let out = add(&mut project, &mut history, Node::new("out"));
        let group = add(&mut project, &mut history, Node::new(GROUP));
        let input = add(&mut project, &mut history, named(GROUP_INPUT, "a", group));
        let output = add(&mut project, &mut history, named(GROUP_OUTPUT, "b", group));
        wire(&mut project, &mut history, (osc, "out"), (group, "a"));
        wire(
            &mut project,
            &mut history,
            (input, INPUT_PORT),
            (output, OUTPUT_PORT),
        );
        wire(&mut project, &mut history, (group, "b"), (out, "in"));
        let flat = flatten(project.graph());
        assert_eq!(flat.nodes().count(), 2);
        assert_eq!(
            flat.source(&Endpoint::new(out, "in")),
            Some(&Endpoint::new(osc, "out"))
        );
    }

    #[test]
    fn a_loop_of_pass_through_groups_terminates() {
        let mut project = Project::new();
        let mut history = History::new();
        let out = add(&mut project, &mut history, Node::new("out"));
        let group = add(&mut project, &mut history, Node::new(GROUP));
        let input = add(&mut project, &mut history, named(GROUP_INPUT, "a", group));
        let output = add(&mut project, &mut history, named(GROUP_OUTPUT, "b", group));
        // group.b feeds group.a, and a passes straight to b.
        wire(&mut project, &mut history, (group, "b"), (group, "a"));
        wire(
            &mut project,
            &mut history,
            (input, INPUT_PORT),
            (output, OUTPUT_PORT),
        );
        wire(&mut project, &mut history, (group, "b"), (out, "in"));
        let flat = flatten(project.graph());
        assert!(flat.source(&Endpoint::new(out, "in")).is_none());
    }
}
