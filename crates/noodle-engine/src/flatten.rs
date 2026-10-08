//! Flattening: turns a graph with group nodes into the plain graph the
//! compiler works on.
//!
//! A group costs nothing at run time. Its node and its boundary nodes are
//! dropped, and each wire that went through them is joined end to end, so
//! the nodes inside run as if they had been wired up at the top level.
//! Nodes keep their IDs, so a diagnostic still points at the right node.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use noodle_core::group::{
    GAIN, GROUP, GROUP_INPUT, GROUP_OUTPUT, GROUP_STAGE, INPUT_PORT, MUTE, OUTPUT_PORT,
};
use noodle_core::{Connection, Endpoint, Graph, Node, NodeId};

/// Whether the graph has anything for [`flatten`] to do.
fn has_groups(graph: &Graph) -> bool {
    graph.nodes().any(|(_, node)| {
        matches!(node.type_id.as_str(), GROUP | GROUP_INPUT | GROUP_OUTPUT) || node.parent.is_some()
    })
}

/// The port a stage takes its signal in at, and sends it out of.
const STAGE_IN: &str = "in";
const STAGE_OUT: &str = "out";

/// Returns the graph with its groups flattened away. A graph with no groups
/// is returned as it is.
///
/// A boundary node whose controls are off their defaults (a gain, or mute,
/// including a mute that soloing another track brings) stays as a stage: a
/// real node in the flat graph under the boundary node's ID. The rest are
/// dropped, so a group left at its defaults costs nothing and renders like
/// the flat patch.
pub fn flatten(graph: &Graph) -> Cow<'_, Graph> {
    if !has_groups(graph) {
        return Cow::Borrowed(graph);
    }
    let is_structure = |id: NodeId| {
        graph
            .node(id)
            .is_some_and(|node| matches!(node.type_id.as_str(), GROUP | GROUP_INPUT | GROUP_OUTPUT))
    };
    let stages = stages(graph);

    let nodes = graph
        .nodes()
        .filter(|&(id, _)| !is_structure(id) || stages.contains_key(&id))
        .map(|(id, node)| {
            let mut node = stages.get(&id).cloned().unwrap_or_else(|| node.clone());
            node.parent = None;
            (id, node)
        });
    let mut connections: Vec<Connection> = graph
        .connections()
        .filter(|c| !is_structure(c.to.node) || stages.contains_key(&c.to.node))
        .filter_map(|c| {
            let from = resolve(graph, &stages, &c.to)?;
            Some(Connection { from, to: c.to })
        })
        .collect();
    // A kept group input has nothing wired to it inside the group; its signal
    // comes from whatever feeds the group's input of that name.
    for &id in stages.keys() {
        let Some(node) = graph.node(id).filter(|n| n.type_id == GROUP_INPUT) else {
            continue;
        };
        let Some(group) = node.parent else { continue };
        let Some(name) = graph
            .group_ports(group)
            .inputs
            .into_iter()
            .find(|p| p.node == id)
            .map(|p| p.name)
        else {
            continue;
        };
        if let Some(from) = resolve(graph, &stages, &Endpoint::new(group, name)) {
            connections.push(Connection {
                from,
                to: Endpoint::new(id, STAGE_IN),
            });
        }
    }
    Cow::Owned(
        Graph::from_parts(nodes, connections)
            .expect("a graph that loaded stays valid with its groups removed"),
    )
}

/// The boundary nodes that stay in the flat graph, as the stage nodes that
/// replace them.
///
/// A stage stays when its gain or mute is off its default, and also once
/// either has been set at all (even back to its default): adding or removing
/// a node in the audible path makes the engine fade the whole output out and
/// in, so a control that is moved again has to find its stage in place, and
/// then moving it is only a parameter change. Soloing works the same way:
/// the muted tracks get their stages at their outputs, and while solo is in
/// use on a level, every group on it keeps one.
fn stages(graph: &Graph) -> BTreeMap<NodeId, Node> {
    let muted_by_solo = graph.solo_muted();
    graph
        .nodes()
        .filter(|(_, node)| matches!(node.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT))
        .filter_map(|(id, node)| {
            let controls = node.controls();
            let group = node.parent?;
            // Solo mutes at a group's outputs, which is enough to silence
            // it. A group with none is muted at its inputs.
            let solo_here =
                node.type_id == GROUP_OUTPUT || graph.group_ports(group).outputs.is_empty();
            let by_solo = solo_here && muted_by_solo.contains(&group);
            let keep_for_solo = solo_here && graph.solo_in_use(group);
            let mute = controls.mute || by_solo;
            if !(node.has_gain_or_mute() || mute || keep_for_solo) {
                return None;
            }
            let mut stage = Node::new(GROUP_STAGE);
            stage.params.insert(GAIN.into(), controls.gain_db);
            stage.params.insert(MUTE.into(), f32::from(u8::from(mute)));
            stage.position = node.position;
            Some((id, stage))
        })
        .collect()
}

/// Follows an input's wire back through group boundaries to the real node
/// that produces its signal, or `None` if the chain ends at an unconnected
/// group input or output, or loops. A stage ends the chain too: it is the
/// node that produces the signal.
fn resolve(graph: &Graph, stages: &BTreeMap<NodeId, Node>, input: &Endpoint) -> Option<Endpoint> {
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
            // node inside, or that node's stage if it has one.
            GROUP => {
                let port = graph.group_ports(at.node).output(&at.port)?.node;
                if stages.contains_key(&port) {
                    return Some(Endpoint::new(port, STAGE_OUT));
                }
                at = graph.source(&Endpoint::new(port, OUTPUT_PORT))?.clone();
            }
            // Signal entering a group: whatever feeds the group's input of
            // that name from outside, or the input node's stage.
            GROUP_INPUT if at.port == INPUT_PORT => {
                if stages.contains_key(&at.node) {
                    return Some(Endpoint::new(at.node, STAGE_OUT));
                }
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
    use noodle_core::group::{GAIN, MUTE, PORT_NAME, SOLO, group_nodes};
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
        let (_, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        assert_ne!(project.graph(), &flat_before);
        assert_eq!(flatten(project.graph()).as_ref(), &flat_before);
    }

    #[test]
    fn nested_groups_flatten_through_every_level() {
        let (mut project, mut history, [_, gain, _]) = chain();
        let flat_before = project.graph().clone();
        let (inner, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        let (_, command) =
            group_nodes(&project.clone(), &[inner], || project.new_node_id()).unwrap();
        history.apply(&mut project, command).unwrap();
        assert_eq!(flatten(project.graph()).as_ref(), &flat_before);
    }

    #[test]
    fn an_unconnected_group_input_leaves_the_inner_input_unwired() {
        let (mut project, mut history, [osc, gain, _]) = chain();
        let (_, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
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

    /// A chain `osc -> gain -> out` with the gain folded into a group, and the
    /// group's input and output nodes.
    fn grouped() -> (Project, History, NodeId, NodeId, NodeId) {
        let (mut project, mut history, [_, gain, _]) = chain();
        let (group, command) = group_nodes(&mut project, &[gain]).unwrap();
        history.apply(&mut project, command).unwrap();
        let ports = project.graph().group_ports(group);
        let (input, output) = (ports.inputs[0].node, ports.outputs[0].node);
        (project, history, group, input, output)
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
    fn controls_nobody_touched_cost_nothing() {
        let (project, _, _, _, _) = grouped();
        let flat = flatten(project.graph());
        assert_eq!(flat.nodes().count(), 3);
        assert!(flat.nodes().all(|(_, n)| n.type_id != GROUP_STAGE));
    }

    #[test]
    fn a_control_set_back_to_its_default_keeps_its_stage() {
        // So that moving it again is a parameter change, not a change to the
        // shape of the graph, which would fade the whole output.
        let (mut project, mut history, _, input, output) = grouped();
        set(&mut project, &mut history, output, GAIN, -6.0);
        set(&mut project, &mut history, output, GAIN, 0.0);
        set(&mut project, &mut history, input, MUTE, 0.0);
        let flat = flatten(project.graph());
        for id in [input, output] {
            let stage = flat.node(id).expect("kept");
            assert_eq!((stage.params[GAIN], stage.params[MUTE]), (0.0, 0.0));
        }
        assert_eq!(flat.nodes().count(), 5);
        // The stages are transparent: wired through, nothing else changed.
        assert_eq!(
            flat.source(&Endpoint::new(NodeId(3), "in")),
            Some(&Endpoint::new(output, "out"))
        );
    }

    #[test]
    fn a_gain_on_a_group_output_stays_as_a_stage() {
        let (mut project, mut history, _, _, output) = grouped();
        set(&mut project, &mut history, output, GAIN, -6.0);
        let flat = flatten(project.graph());
        let stage = flat
            .node(output)
            .expect("kept under the boundary node's ID");
        assert_eq!(stage.type_id, GROUP_STAGE);
        assert_eq!(stage.params[GAIN], -6.0);
        assert_eq!(stage.params[MUTE], 0.0);
        assert_eq!(stage.parent, None);
        // osc -> gain -> stage -> out
        let gain = NodeId(2);
        let out = NodeId(3);
        assert_eq!(
            flat.source(&Endpoint::new(output, "in")),
            Some(&Endpoint::new(gain, "out"))
        );
        assert_eq!(
            flat.source(&Endpoint::new(out, "in")),
            Some(&Endpoint::new(output, "out"))
        );
        assert_eq!(flat.nodes().count(), 4);
    }

    #[test]
    fn a_mute_on_a_group_input_stays_as_a_stage() {
        let (mut project, mut history, _, input, _) = grouped();
        set(&mut project, &mut history, input, MUTE, 1.0);
        let flat = flatten(project.graph());
        assert_eq!(flat.node(input).unwrap().type_id, GROUP_STAGE);
        assert_eq!(flat.node(input).unwrap().params[MUTE], 1.0);
        // osc -> stage -> gain
        assert_eq!(
            flat.source(&Endpoint::new(input, "in")),
            Some(&Endpoint::new(NodeId(1), "out"))
        );
        assert_eq!(
            flat.source(&Endpoint::new(NodeId(2), "in")),
            Some(&Endpoint::new(input, "out"))
        );
    }

    #[test]
    fn a_stage_with_nothing_wired_into_it_is_left_unwired() {
        let (mut project, mut history, group, _, output) = grouped();
        set(&mut project, &mut history, output, MUTE, 1.0);
        let input = Endpoint::new(group, "in1");
        history
            .apply(&mut project, Command::Disconnect { input })
            .unwrap();
        let flat = flatten(project.graph());
        assert!(flat.node(output).is_some());
        assert!(flat.source(&Endpoint::new(NodeId(2), "in")).is_none());
    }

    /// Two tracks, each `osc -> group -> out`.
    fn two_tracks() -> (Project, History, [NodeId; 2], [NodeId; 2]) {
        let mut project = Project::new();
        let mut history = History::new();
        let mut groups = Vec::new();
        let mut outputs = Vec::new();
        for _ in 0..2 {
            let osc = add(&mut project, &mut history, Node::new("osc"));
            let out = add(&mut project, &mut history, Node::new("out"));
            let group = add(&mut project, &mut history, Node::new(GROUP));
            let input = add(&mut project, &mut history, named(GROUP_INPUT, "in", group));
            let output = add(
                &mut project,
                &mut history,
                named(GROUP_OUTPUT, "out", group),
            );
            wire(&mut project, &mut history, (osc, "out"), (group, "in"));
            wire(
                &mut project,
                &mut history,
                (input, INPUT_PORT),
                (output, OUTPUT_PORT),
            );
            wire(&mut project, &mut history, (group, "out"), (out, "in"));
            groups.push(group);
            outputs.push(output);
        }
        (
            project,
            history,
            [groups[0], groups[1]],
            [outputs[0], outputs[1]],
        )
    }

    #[test]
    fn soloing_a_track_mutes_the_others_at_their_outputs() {
        let (mut project, mut history, _, [a, b]) = two_tracks();
        assert_eq!(flatten(project.graph()).nodes().count(), 4);
        set(&mut project, &mut history, a, SOLO, 1.0);
        let flat = flatten(project.graph());
        let muted = |id| flat.node(id).map(|n| n.params[MUTE]);
        assert_eq!(muted(b), Some(1.0), "the other track is muted");
        assert_eq!(muted(a), Some(0.0), "the soloed track has a stage, open");
        // Muting at the output is enough: the inputs have no stage.
        let inputs = [NodeId(a.0 - 1), NodeId(b.0 - 1)];
        assert!(inputs.iter().all(|&id| flat.node(id).is_none()));
        assert_eq!(flat.node(b).unwrap().type_id, GROUP_STAGE);
    }

    #[test]
    fn unsoloing_keeps_the_stages_so_toggling_solo_is_a_parameter_change() {
        let (mut project, mut history, _, [a, b]) = two_tracks();
        set(&mut project, &mut history, a, SOLO, 1.0);
        let shape = |project: &Project| {
            let flat = flatten(project.graph());
            let ids: Vec<_> = flat.nodes().map(|(id, _)| id).collect();
            (ids, flat.connections().collect::<Vec<_>>())
        };
        let soloed = shape(&project);
        set(&mut project, &mut history, a, SOLO, 0.0);
        let flat = flatten(project.graph());
        assert_eq!(shape(&project), soloed);
        assert_eq!(flat.node(b).unwrap().params[MUTE], 0.0);
    }

    #[test]
    fn soloing_every_track_mutes_none() {
        let (mut project, mut history, _, [a, b]) = two_tracks();
        set(&mut project, &mut history, a, SOLO, 1.0);
        set(&mut project, &mut history, b, SOLO, 1.0);
        let flat = flatten(project.graph());
        assert_eq!(flat.node(a).unwrap().params[MUTE], 0.0);
        assert_eq!(flat.node(b).unwrap().params[MUTE], 0.0);
    }

    #[test]
    fn solo_and_mute_together_mute() {
        let (mut project, mut history, _, [a, _]) = two_tracks();
        set(&mut project, &mut history, a, SOLO, 1.0);
        set(&mut project, &mut history, a, MUTE, 1.0);
        let flat = flatten(project.graph());
        assert_eq!(flat.node(a).unwrap().params[MUTE], 1.0);
    }
}
