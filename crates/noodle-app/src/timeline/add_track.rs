//! The Add track button's command: a track group, wired to an Output node so
//! it can be heard.

use noodle_core::group::create_track;
use noodle_core::{Command, Connection, Endpoint, Graph, Node, NodeId, Position};
use noodle_engine::OUTPUT_ID;

/// How far below the lowest node in the graph a new track goes, so its nodes
/// don't land on top of another track's.
const GAP: f32 = 200.0;

/// Creates a track below everything in the graph and connects its output to
/// an Output node, as one command. A top level Output node whose input is
/// free is used; otherwise the command adds one, since Output nodes are mixed
/// together and a project may have several.
pub fn command(graph: &Graph, mut new_id: impl FnMut() -> NodeId) -> Command {
    let below = graph.nodes().map(|(_, n)| n.position.y).fold(0.0, f32::max);
    let position = Position {
        x: 0.0,
        y: below + GAP,
    };
    let (group, create) = create_track(None, position, &mut new_id);
    let free = graph.nodes().find_map(|(id, node)| {
        let at_top = node.type_id == OUTPUT_ID && node.parent.is_none();
        (at_top && graph.source(&Endpoint::new(id, "in")).is_none()).then_some(id)
    });
    let mut commands = vec![create];
    let output = free.unwrap_or_else(|| {
        let id = new_id();
        let node = Node::new(OUTPUT_ID).at(position.x + 500.0, position.y);
        commands.push(Command::AddNode { id, node });
        id
    });
    commands.push(Command::Connect(Connection {
        from: Endpoint::new(group, "out"),
        to: Endpoint::new(output, "in"),
    }));
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

    #[test]
    fn the_first_track_gets_an_output_and_is_wired_to_it() {
        let (mut project, mut history) = (Project::new(), History::new());
        let group = apply(&mut project, &mut history);
        let outputs = outputs(&project);
        assert_eq!(outputs.len(), 1);
        let source = project.graph().source(&Endpoint::new(outputs[0], "in"));
        assert_eq!(source, Some(&Endpoint::new(group, "out")));
        // One undo step takes all of it away.
        history.undo(&mut project).unwrap();
        assert_eq!(project.graph().nodes().count(), 0);
    }

    #[test]
    fn a_free_output_is_reused_and_a_busy_one_is_not() {
        let (mut project, mut history) = (Project::new(), History::new());
        let free = project.new_node_id();
        let add = Command::AddNode {
            id: free,
            node: Node::new(OUTPUT_ID),
        };
        history.apply(&mut project, add).unwrap();
        let first = apply(&mut project, &mut history);
        assert_eq!(outputs(&project), [free]);
        let source = project.graph().source(&Endpoint::new(free, "in"));
        assert_eq!(source, Some(&Endpoint::new(first, "out")));
        // Now it's taken, so the next track brings its own.
        let second = apply(&mut project, &mut history);
        let outputs = outputs(&project);
        assert_eq!(outputs.len(), 2);
        let other = outputs.into_iter().find(|&id| id != free).unwrap();
        let source = project.graph().source(&Endpoint::new(other, "in"));
        assert_eq!(source, Some(&Endpoint::new(second, "out")));
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
        assert_eq!(reached, 2, "each track feeds its own Output");
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
