//! The Add track button's command: a track group, wired into the default
//! mixer so it can be heard.

use noodle_core::group::create_track;
use noodle_core::spare::{self, MIXER};
use noodle_core::{Command, Config, Endpoint, Graph, Node, NodeId, Position, Value};
use noodle_engine::OUTPUT_ID;

/// How far below the lowest node in the graph a new track goes, so its nodes
/// don't land on top of another track's.
const GAP: f32 = 200.0;

/// The default mixer: the first Mix node at the top level.
pub fn default_mixer(graph: &Graph) -> Option<NodeId> {
    graph
        .nodes()
        .find(|(_, node)| node.type_id == MIXER && node.parent.is_none())
        .map(|(id, _)| id)
}

/// Creates a track below everything in the graph and wires its output to the
/// next spare input of the default mixer, as one command. The only nodes it
/// adds are the group and its own, except that a project with no mixer gets
/// one, wired to an Output node (a top level one whose input is free if
/// there is one).
pub fn command(graph: &Graph, mut new_id: impl FnMut() -> NodeId) -> Command {
    let below = graph.nodes().map(|(_, n)| n.position.y).fold(0.0, f32::max);
    let position = Position {
        x: 0.0,
        y: below + GAP,
    };
    let (group, create) = create_track(None, position, &mut new_id);
    let mut commands = vec![create];
    let (mixer, key) = match default_mixer(graph) {
        Some(mixer) => {
            let key = spare::mixer_spare_key(graph, mixer);
            commands.extend(spare::prepare_mixer_input(graph, mixer, &key));
            (mixer, key)
        }
        None => {
            let mixer = new_id();
            let config = Config::new().with(spare::MIXER_INPUTS, Value::Int(1));
            let node = Node::new(MIXER)
                .with_config(config)
                .at(position.x + 500.0, position.y);
            commands.push(Command::AddNode { id: mixer, node });
            let free = graph.nodes().find_map(|(id, node)| {
                let at_top = node.type_id == OUTPUT_ID && node.parent.is_none();
                (at_top && graph.source(&Endpoint::new(id, "in")).is_none()).then_some(id)
            });
            let output = free.unwrap_or_else(|| {
                let id = new_id();
                let node = Node::new(OUTPUT_ID).at(position.x + 800.0, position.y);
                commands.push(Command::AddNode { id, node });
                id
            });
            commands.push(spare::wire(
                Endpoint::new(mixer, "out"),
                Endpoint::new(output, "in"),
            ));
            (mixer, spare::mixer_input_key(1))
        }
    };
    commands.push(spare::wire(
        Endpoint::new(group, "out"),
        Endpoint::new(mixer, key),
    ));
    Command::Batch(commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::group::GROUP;
    use noodle_core::{History, Project};
    use noodle_engine::{Registry, Settings, flatten, render};

    fn apply(project: &mut Project, history: &mut History) -> NodeId {
        let graph = project.graph().clone();
        let command = command(&graph, || project.new_node_id());
        history.apply(project, command).unwrap();
        project
            .graph()
            .nodes()
            .filter(|(_, n)| n.type_id == GROUP)
            .map(|(id, _)| id)
            .max()
            .unwrap()
    }

    fn outputs(project: &Project) -> Vec<NodeId> {
        let nodes = project.graph().nodes();
        let found = nodes.filter(|(_, n)| n.type_id == OUTPUT_ID);
        found.map(|(id, _)| id).collect()
    }

    fn mixers(project: &Project) -> Vec<NodeId> {
        let nodes = project.graph().nodes();
        let found = nodes.filter(|(_, n)| n.type_id == MIXER);
        found.map(|(id, _)| id).collect()
    }

    #[test]
    fn the_first_track_brings_a_mixer_and_an_output() {
        let (mut project, mut history) = (Project::new(), History::new());
        let group = apply(&mut project, &mut history);
        let (outputs, mixers) = (outputs(&project), mixers(&project));
        assert_eq!((outputs.len(), mixers.len()), (1, 1));
        let graph = project.graph();
        assert_eq!(
            graph.source(&Endpoint::new(mixers[0], "in1")),
            Some(&Endpoint::new(group, "out"))
        );
        assert_eq!(
            graph.source(&Endpoint::new(outputs[0], "in")),
            Some(&Endpoint::new(mixers[0], "out"))
        );
        // One undo step takes all of it away.
        history.undo(&mut project).unwrap();
        assert_eq!(project.graph().nodes().count(), 0);
    }

    #[test]
    fn later_tracks_only_add_a_group_and_a_wire_to_the_mixers_spare() {
        let (mut project, mut history) = (Project::new(), History::new());
        let first = apply(&mut project, &mut history);
        let nodes_before = project.graph().nodes().count();
        let second = apply(&mut project, &mut history);
        let third = apply(&mut project, &mut history);
        let mixer = mixers(&project)[0];
        assert_eq!(mixers(&project).len(), 1);
        assert_eq!(outputs(&project).len(), 1, "no new output node");
        // Each track adds a group and its three nodes inside, nothing else.
        assert_eq!(project.graph().nodes().count(), nodes_before + 2 * 4);
        let graph = project.graph();
        for (i, track) in [first, second, third].into_iter().enumerate() {
            assert_eq!(
                graph.source(&Endpoint::new(mixer, format!("in{}", i + 1))),
                Some(&Endpoint::new(track, "out"))
            );
        }
        let node = graph.node(mixer).unwrap();
        assert_eq!(spare::mixer_inputs(node), 3);
    }

    #[test]
    fn a_free_output_is_reused_for_the_new_mixer() {
        let (mut project, mut history) = (Project::new(), History::new());
        let free = project.new_node_id();
        let add = Command::AddNode {
            id: free,
            node: Node::new(OUTPUT_ID),
        };
        history.apply(&mut project, add).unwrap();
        apply(&mut project, &mut history);
        assert_eq!(outputs(&project), [free]);
        let mixer = mixers(&project)[0];
        let source = project.graph().source(&Endpoint::new(free, "in"));
        assert_eq!(source, Some(&Endpoint::new(mixer, "out")));
    }

    #[test]
    fn an_existing_mixer_with_gaps_is_filled_after_the_last_wired_input() {
        let (mut project, mut history) = (Project::new(), History::new());
        let mixer = project.new_node_id();
        let node = Node::new(MIXER);
        history
            .apply(&mut project, Command::AddNode { id: mixer, node })
            .unwrap();
        let src = project.new_node_id();
        let add = Command::AddNode {
            id: src,
            node: Node::new("noodle.osc.sine"),
        };
        history.apply(&mut project, add).unwrap();
        let wire = spare::wire(Endpoint::new(src, "out"), Endpoint::new(mixer, "in4"));
        history.apply(&mut project, wire).unwrap();
        let group = apply(&mut project, &mut history);
        assert_eq!(
            project.graph().source(&Endpoint::new(mixer, "in5")),
            Some(&Endpoint::new(group, "out"))
        );
    }

    /// The track input reaches the Output once the groups are flattened, and
    /// the engine accepts the result.
    #[test]
    fn a_new_track_reaches_the_output_in_the_compiled_graph() {
        let (mut project, mut history) = (Project::new(), History::new());
        apply(&mut project, &mut history);
        apply(&mut project, &mut history);
        let flat = flatten(project.graph());
        let mut reached = 0;
        for connection in flat.connections() {
            let to = flat.node(connection.to.node).unwrap();
            let from = flat.node(connection.from.node).unwrap();
            // Through the boundary stages, back to the track input.
            if to.type_id == OUTPUT_ID {
                let mut at = connection.from.node;
                let mut kind = from.type_id.clone();
                while kind != noodle_core::group::TRACK_INPUT {
                    let feed = flat
                        .connections()
                        .find(|c| c.to.node == at)
                        .expect("a chain back to a track input");
                    at = feed.from.node;
                    kind = flat.node(at).unwrap().type_id.clone();
                }
                reached += 1;
            }
        }
        assert_eq!(reached, 1, "the mixer feeds the Output");
        let mut registry = Registry::with_builtins();
        let _library = noodle_nodes::register_library(&mut registry);
        let settings = Settings {
            sample_rate: 48_000.0,
            max_frames: 256,
            channels: 2,
        };
        let rendered = render(project.graph(), &registry, settings, 512).unwrap();
        assert!(
            rendered.diagnostics.is_empty(),
            "{:?}",
            rendered.diagnostics
        );
    }
}
