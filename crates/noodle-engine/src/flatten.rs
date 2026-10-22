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
    GAIN, GROUP, GROUP_INPUT, GROUP_OUTPUT, GROUP_STAGE, MUTE, SOLO, SOLO_MUTE, control_key,
    parse_control_key, parse_port_key, stage_id,
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
/// A boundary port whose controls are off their defaults (a gain, or mute,
/// including a mute that soloing another track brings) stays as a stage: a
/// real node in the flat graph under the port's [`stage_id`], as does one
/// with a wire into its gain or mute. The rest are dropped, so a group left
/// at its defaults costs nothing and renders like the flat patch.
pub fn flatten(graph: &Graph) -> Cow<'_, Graph> {
    flatten_keeping(graph, &BTreeSet::new())
}

/// [`flatten`], also keeping a stage for each boundary port in `keep` (by
/// stage ID), so an automation lane has a gain or mute port to drive.
pub(crate) fn flatten_keeping<'a>(graph: &'a Graph, keep: &BTreeSet<NodeId>) -> Cow<'a, Graph> {
    if !has_groups(graph) {
        return Cow::Borrowed(graph);
    }
    let is_structure = |id: NodeId| {
        graph
            .node(id)
            .is_some_and(|node| matches!(node.type_id.as_str(), GROUP | GROUP_INPUT | GROUP_OUTPUT))
    };
    let stages = stages(graph, keep);

    let nodes = graph
        .nodes()
        .filter(|&(id, _)| !is_structure(id))
        .map(|(id, node)| {
            let mut node = node.clone();
            node.parent = None;
            (id, node)
        })
        .chain(stages.iter().map(|(&id, stage)| (id, stage.node.clone())));
    let mut connections: Vec<Connection> = graph
        .connections()
        .filter_map(|c| {
            let to = if is_structure(c.to.node) {
                stage_input(graph, &stages, &c.to)?
            } else {
                c.to.clone()
            };
            let from = resolve(graph, &stages, &c.to)?;
            Some(Connection { from, to })
        })
        .collect();
    // A track's mute reaches its outputs without a wire: its track input's
    // mute drives the stage of each output, unless something is wired to
    // that output's mute already.
    for (node_id, node) in graph.nodes() {
        if node.type_id != GROUP_OUTPUT {
            continue;
        }
        let Some(track) = node.parent.and_then(|group| graph.track_input(group)) else {
            continue;
        };
        for (port, _) in node.boundary_ports() {
            let stage = stage_id(node_id, port);
            let wired = graph
                .source(&Endpoint::new(node_id, control_key(MUTE, port)))
                .is_some();
            if stages.contains_key(&stage) && !wired {
                connections.push(Connection {
                    from: Endpoint::new(track, noodle_core::group::TRACK_MUTE),
                    to: Endpoint::new(stage, MUTE),
                });
            }
        }
    }
    // A kept group input has nothing wired to it inside the group; its signal
    // comes from whatever feeds the group's input of that name.
    for stage in stages.values() {
        let Some(port) = &stage.input else { continue };
        if let Some(from) = resolve(
            graph,
            &stages,
            &Endpoint::new(port.group, port.name.clone()),
        ) {
            connections.push(Connection {
                from,
                to: Endpoint::new(port.stage, STAGE_IN),
            });
        }
    }
    Cow::Owned(
        Graph::from_parts(nodes, connections)
            .expect("a graph that loaded stays valid with its groups removed"),
    )
}

/// A boundary port that stays in the flat graph as a stage.
struct Stage {
    /// The stage node.
    node: Node,
    /// For a stage on a group input: where the group's input of that name
    /// comes from, since nothing is wired to the stage inside the group.
    input: Option<InputStage>,
}

struct InputStage {
    stage: NodeId,
    group: NodeId,
    name: String,
}

/// The stage of every port of every group's output node, for the meters.
pub(crate) fn output_stages(graph: &Graph) -> impl Iterator<Item = NodeId> + '_ {
    graph
        .nodes()
        .filter(|(_, node)| node.type_id == GROUP_OUTPUT && node.parent.is_some())
        .flat_map(|(id, node)| {
            node.boundary_ports()
                .into_iter()
                .map(move |(port, _)| stage_id(id, port))
        })
}

/// The boundary ports that stay in the flat graph, as the stage nodes that
/// replace them, by stage ID.
///
/// A stage stays when its gain or mute is off its default or has a wire into
/// it, and also once either has been set at all (even back to its default):
/// adding or removing a node in the audible path makes the engine fade the whole output out and
/// in, so a control that is moved again has to find its stage in place, and
/// then moving it is only a parameter change. Soloing works the same way:
/// the muted tracks get their stages at their outputs, and while solo is in
/// use on a level, every group on it keeps one.
fn stages(graph: &Graph, keep: &BTreeSet<NodeId>) -> BTreeMap<NodeId, Stage> {
    let muted_by_solo = graph.solo_muted();
    let mut found = BTreeMap::new();
    for (node_id, node) in graph.nodes() {
        let is_input = node.type_id == GROUP_INPUT;
        if !is_input && node.type_id != GROUP_OUTPUT {
            continue;
        }
        let Some(group) = node.parent else { continue };
        for (port, name) in node.boundary_ports() {
            let stage = stage_id(node_id, port);
            let controls = node.port_controls(port);
            // Solo mutes at a group's outputs, which is enough to silence
            // it. A group with none is muted at its inputs.
            let solo_here = !is_input || graph.group_ports(group).outputs.is_empty();
            let by_solo = solo_here && muted_by_solo.contains(&group);
            let keep_for_solo = solo_here && graph.solo_in_use(group);
            let mute = controls.mute || by_solo;
            // A wire into the gain or mute needs the stage's port to land on.
            let wired = [GAIN, MUTE].iter().any(|control| {
                graph
                    .source(&Endpoint::new(node_id, control_key(control, port)))
                    .is_some()
            });
            // A track's outputs keep a stage for its track input's mute to drive.
            let track_out = !is_input && graph.track_input(group).is_some();
            if !(node.has_gain_or_mute(port)
                || track_out
                || mute
                || keep_for_solo
                || wired
                || keep.contains(&stage))
            {
                continue;
            }
            let mut flat = Node::new(GROUP_STAGE);
            flat.params.insert(GAIN.into(), controls.gain_db);
            // Solo goes in its own input, so a lane on the mute can't undo it.
            flat.params
                .insert(MUTE.into(), f32::from(u8::from(controls.mute)));
            flat.params
                .insert(SOLO_MUTE.into(), f32::from(u8::from(by_solo)));
            flat.position = node.position;
            found.insert(
                stage,
                Stage {
                    node: flat,
                    input: is_input.then_some(InputStage { stage, group, name }),
                },
            );
        }
    }
    found
}

/// Where a wire into a group, group input or group output node lands in the
/// flat graph: a port of a stage, or `None` if it ends there, because
/// nothing flat is on the other side.
fn stage_input(graph: &Graph, stages: &BTreeMap<NodeId, Stage>, to: &Endpoint) -> Option<Endpoint> {
    let node = graph.node(to.node)?;
    if !matches!(node.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT) {
        return None;
    }
    // The signal port of an output node. An input node's is an output.
    if node.type_id == GROUP_OUTPUT
        && let Some(port) = parse_port_key(&to.port)
    {
        let stage = stage_id(to.node, port);
        return stages
            .contains_key(&stage)
            .then(|| Endpoint::new(stage, STAGE_IN));
    }
    let (control, port) = parse_control_key(&to.port)?;
    let stage = stage_id(to.node, port);
    // Solo has no port on a stage.
    (stages.contains_key(&stage) && control != SOLO).then(|| Endpoint::new(stage, control))
}

/// The nodes' outputs in the flat graph that feed a group's outputs: what
/// freezing the group renders. A port with nothing wired inside, or whose
/// source chain ends at a group input, has none.
pub(crate) fn group_outputs(
    graph: &Graph,
    keep: &BTreeSet<NodeId>,
    group: NodeId,
) -> Vec<Endpoint> {
    let stages = stages(graph, keep);
    let mut sources = Vec::new();
    for port in graph.group_ports(group).outputs {
        let endpoint = if stages.contains_key(&port.stage()) {
            Some(Endpoint::new(port.stage(), STAGE_OUT))
        } else {
            resolve(graph, &stages, &Endpoint::new(port.node, port.key()))
        };
        if let Some(endpoint) = endpoint
            && !sources.contains(&endpoint)
        {
            sources.push(endpoint);
        }
    }
    sources
}

/// Follows an input's wire back through group boundaries to the real node
/// that produces its signal, or `None` if the chain ends at an unconnected
/// group input or output, or loops. A stage ends the chain too: it is the
/// node that produces the signal.
fn resolve(graph: &Graph, stages: &BTreeMap<NodeId, Stage>, input: &Endpoint) -> Option<Endpoint> {
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
            // port inside, or that port's stage if it has one.
            GROUP => {
                let port = graph.group_ports(at.node).output(&at.port)?.clone();
                if stages.contains_key(&port.stage()) {
                    return Some(Endpoint::new(port.stage(), STAGE_OUT));
                }
                at = graph.source(&Endpoint::new(port.node, port.key()))?.clone();
            }
            // Signal entering a group: whatever feeds the group's input of
            // that name from outside, or the input port's stage.
            GROUP_INPUT => {
                let id = parse_port_key(&at.port)?;
                let stage = stage_id(at.node, id);
                if stages.contains_key(&stage) {
                    return Some(Endpoint::new(stage, STAGE_OUT));
                }
                let group = node.parent?;
                let name = node
                    .boundary_ports()
                    .into_iter()
                    .find(|&(port, _)| port == id)?
                    .1;
                at = graph.source(&Endpoint::new(group, name))?.clone();
            }
            GROUP_OUTPUT => return None,
            _ => return Some(at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::group::{GAIN, MUTE, SOLO, SOLO_MUTE, group_nodes, port_key, ports_config};
    use noodle_core::{Command, History, Node, Project};

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
            .with_config(ports_config(&[(1, name.to_owned())]))
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
            (input, &port_key(1)),
            (output, &port_key(1)),
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
            (input, &port_key(1)),
            (output, &port_key(1)),
        );
        wire(&mut project, &mut history, (group, "b"), (out, "in"));
        let flat = flatten(project.graph());
        assert!(flat.source(&Endpoint::new(out, "in")).is_none());
    }

    /// A chain `osc -> gain -> out` with the gain folded into a group, and the
    /// group's input and output nodes.
    fn grouped() -> (Project, History, NodeId, NodeId, NodeId) {
        let (mut project, mut history, [_, gain, _]) = chain();
        let (group, command) =
            group_nodes(&project.clone(), &[gain], || project.new_node_id()).unwrap();
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
            &control_key(GAIN, 1),
            0.0,
        );
        set(
            &mut project,
            &mut history,
            input,
            &control_key(MUTE, 1),
            0.0,
        );
        let flat = flatten(project.graph());
        for id in [input, output] {
            let stage = flat.node(stage_id(id, 1)).expect("kept");
            assert_eq!((stage.params[GAIN], stage.params[MUTE]), (0.0, 0.0));
        }
        assert_eq!(flat.nodes().count(), 5);
        // The stages are transparent: wired through, nothing else changed.
        assert_eq!(
            flat.source(&Endpoint::new(NodeId(3), "in")),
            Some(&Endpoint::new(stage_id(output, 1), "out"))
        );
    }

    #[test]
    fn a_gain_on_a_group_output_stays_as_a_stage() {
        let (mut project, mut history, _, _, output) = grouped();
        set(
            &mut project,
            &mut history,
            output,
            &control_key(GAIN, 1),
            -6.0,
        );
        let flat = flatten(project.graph());
        let stage = flat
            .node(stage_id(output, 1))
            .expect("kept under the port's stage ID");
        assert_eq!(stage.type_id, GROUP_STAGE);
        assert_eq!(stage.params[GAIN], -6.0);
        assert_eq!(stage.params[MUTE], 0.0);
        assert_eq!(stage.parent, None);
        // osc -> gain -> stage -> out
        let gain = NodeId(2);
        let out = NodeId(3);
        assert_eq!(
            flat.source(&Endpoint::new(stage_id(output, 1), "in")),
            Some(&Endpoint::new(gain, "out"))
        );
        assert_eq!(
            flat.source(&Endpoint::new(out, "in")),
            Some(&Endpoint::new(stage_id(output, 1), "out"))
        );
        assert_eq!(flat.nodes().count(), 4);
    }

    #[test]
    fn a_mute_on_a_group_input_stays_as_a_stage() {
        let (mut project, mut history, _, input, _) = grouped();
        set(
            &mut project,
            &mut history,
            input,
            &control_key(MUTE, 1),
            1.0,
        );
        let flat = flatten(project.graph());
        assert_eq!(flat.node(stage_id(input, 1)).unwrap().type_id, GROUP_STAGE);
        assert_eq!(flat.node(stage_id(input, 1)).unwrap().params[MUTE], 1.0);
        // osc -> stage -> gain
        assert_eq!(
            flat.source(&Endpoint::new(stage_id(input, 1), "in")),
            Some(&Endpoint::new(NodeId(1), "out"))
        );
        assert_eq!(
            flat.source(&Endpoint::new(NodeId(2), "in")),
            Some(&Endpoint::new(stage_id(input, 1), "out"))
        );
    }

    #[test]
    fn a_stage_with_nothing_wired_into_it_is_left_unwired() {
        let (mut project, mut history, group, _, output) = grouped();
        set(
            &mut project,
            &mut history,
            output,
            &control_key(MUTE, 1),
            1.0,
        );
        let input = Endpoint::new(group, "in1");
        history
            .apply(&mut project, Command::Disconnect { input })
            .unwrap();
        let flat = flatten(project.graph());
        assert!(flat.node(stage_id(output, 1)).is_some());
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
                (input, &port_key(1)),
                (output, &port_key(1)),
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
        set(&mut project, &mut history, a, &control_key(SOLO, 1), 1.0);
        let flat = flatten(project.graph());
        let muted = |id| flat.node(stage_id(id, 1)).map(|n| n.params[SOLO_MUTE]);
        assert_eq!(muted(b), Some(1.0), "the other track is muted");
        assert_eq!(muted(a), Some(0.0), "the soloed track has a stage, open");
        // Muting at the output is enough: the inputs have no stage.
        let inputs = [NodeId(a.0 - 1), NodeId(b.0 - 1)];
        assert!(
            inputs
                .iter()
                .all(|&id| flat.node(stage_id(id, 1)).is_none())
        );
        assert_eq!(flat.node(stage_id(b, 1)).unwrap().type_id, GROUP_STAGE);
    }

    #[test]
    fn unsoloing_keeps_the_stages_so_toggling_solo_is_a_parameter_change() {
        let (mut project, mut history, _, [a, b]) = two_tracks();
        set(&mut project, &mut history, a, &control_key(SOLO, 1), 1.0);
        let shape = |project: &Project| {
            let flat = flatten(project.graph());
            let ids: Vec<_> = flat.nodes().map(|(id, _)| id).collect();
            (ids, flat.connections().collect::<Vec<_>>())
        };
        let soloed = shape(&project);
        set(&mut project, &mut history, a, &control_key(SOLO, 1), 0.0);
        let flat = flatten(project.graph());
        assert_eq!(shape(&project), soloed);
        assert_eq!(flat.node(stage_id(b, 1)).unwrap().params[SOLO_MUTE], 0.0);
    }

    #[test]
    fn soloing_every_track_mutes_none() {
        let (mut project, mut history, _, [a, b]) = two_tracks();
        set(&mut project, &mut history, a, &control_key(SOLO, 1), 1.0);
        set(&mut project, &mut history, b, &control_key(SOLO, 1), 1.0);
        let flat = flatten(project.graph());
        assert_eq!(flat.node(stage_id(a, 1)).unwrap().params[SOLO_MUTE], 0.0);
        assert_eq!(flat.node(stage_id(b, 1)).unwrap().params[SOLO_MUTE], 0.0);
    }

    #[test]
    fn solo_and_mute_together_mute() {
        let (mut project, mut history, _, [a, _]) = two_tracks();
        set(&mut project, &mut history, a, &control_key(SOLO, 1), 1.0);
        set(&mut project, &mut history, a, &control_key(MUTE, 1), 1.0);
        let flat = flatten(project.graph());
        assert_eq!(flat.node(stage_id(a, 1)).unwrap().params[MUTE], 1.0);
    }

    #[test]
    fn each_port_of_a_boundary_node_has_its_own_stage_and_controls() {
        use noodle_core::spare::{Side, add_group_port, wire};
        // osc -> [group: in1 -> gain -> out1, and also -> out2] -> out
        let (mut project, mut history, group, _, output) = grouped();
        let inner = NodeId(2);
        let id = project.new_node_id();
        let (command, key) = add_group_port(project.graph(), group, Side::Output, "out2", id);
        history.apply(&mut project, command).unwrap();
        history
            .apply(
                &mut project,
                wire(Endpoint::new(inner, "out"), Endpoint::new(output, key)),
            )
            .unwrap();
        // Only the second port has a gain.
        set(
            &mut project,
            &mut history,
            output,
            &control_key(GAIN, 2),
            -6.0,
        );
        let flat = flatten(project.graph());
        assert!(
            flat.node(stage_id(output, 1)).is_none(),
            "port 1 costs nothing"
        );
        let stage = flat.node(stage_id(output, 2)).expect("port 2 has a stage");
        assert_eq!(stage.params[GAIN], -6.0);
        assert_eq!(
            flat.source(&Endpoint::new(stage_id(output, 2), "in")),
            Some(&Endpoint::new(inner, "out"))
        );
        // The first port's signal reaches the group's `out1` consumer directly.
        assert_eq!(
            flat.source(&Endpoint::new(NodeId(3), "in")),
            Some(&Endpoint::new(inner, "out"))
        );
        // A wire into port 2's mute lands on its stage.
        let mute_source = add(
            &mut project,
            &mut history,
            Node::new("const").in_group(group),
        );
        wire_into_mute(&mut project, &mut history, mute_source, output);
        let flat = flatten(project.graph());
        assert_eq!(
            flat.source(&Endpoint::new(stage_id(output, 2), "mute")),
            Some(&Endpoint::new(mute_source, "out"))
        );
    }

    fn wire_into_mute(project: &mut Project, history: &mut History, from: NodeId, node: NodeId) {
        let c = Connection {
            from: Endpoint::new(from, "out"),
            to: Endpoint::new(node, control_key(MUTE, 2)),
        };
        history.apply(project, Command::Connect(c)).unwrap();
    }

    #[test]
    fn a_wire_into_a_group_inputs_mute_lands_on_its_stage() {
        let (mut project, mut history, group, input, _) = grouped();
        let lfo = add(&mut project, &mut history, Node::new("lfo").in_group(group));
        let c = Connection {
            from: Endpoint::new(lfo, "out"),
            to: Endpoint::new(input, control_key(MUTE, 1)),
        };
        history.apply(&mut project, Command::Connect(c)).unwrap();
        let flat = flatten(project.graph());
        assert_eq!(
            flat.source(&Endpoint::new(stage_id(input, 1), "mute")),
            Some(&Endpoint::new(lfo, "out"))
        );
    }
}
