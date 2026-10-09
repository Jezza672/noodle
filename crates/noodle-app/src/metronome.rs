//! The transport bar's metronome button. It isn't special-cased: it presses a
//! [`Button`](noodle_nodes::Button) node that is wired into the `on` input of
//! a [`Metronome`](noodle_nodes::Metronome) node, so the same setup can be
//! rewired in the node editor (to a lane, say, or a different button).
//!
//! If the project has no metronome, the first press adds the default setup:
//! button → metronome → output, as one undo step.

use noodle_core::{Command, Connection, Endpoint, Node, NodeId, Project};
use noodle_engine::OUTPUT_ID;
use noodle_nodes::{BUTTON_ID, BUTTON_STATE, METRONOME_ID, METRONOME_ON};

/// What the button is bound to.
#[derive(Clone, Debug, PartialEq)]
pub enum Binding {
    /// There is no metronome yet; pressing the button adds one.
    Missing,
    /// A metronome whose `on` input is fed by a button.
    Bound { button: NodeId, on: bool },
    /// A metronome exists, but nothing the button can press feeds it. The
    /// text says why.
    Broken(String),
}

/// Finds the metronome the button controls: the first top-level one whose
/// `on` input is fed by a Button. If there is none, the first metronome's
/// problem is reported.
pub fn binding(project: &Project) -> Binding {
    let graph = project.graph();
    let mut first_problem = None;
    for (metronome, _) in graph
        .nodes()
        .filter(|(_, node)| node.type_id == METRONOME_ID && node.parent.is_none())
    {
        let input = Endpoint::new(metronome, METRONOME_ON);
        let Some(source) = graph.source(&input) else {
            first_problem.get_or_insert("Nothing is wired into the metronome's On input");
            continue;
        };
        match graph.node(source.node) {
            Some(node) if node.type_id == BUTTON_ID => {
                return Binding::Bound {
                    button: source.node,
                    on: node.params.get(BUTTON_STATE).copied().unwrap_or(0.0) >= 0.5,
                };
            }
            _ => {
                first_problem
                    .get_or_insert("The metronome's On input isn't driven by a Button node");
            }
        }
    }
    first_problem.map_or(Binding::Missing, |why| Binding::Broken(why.to_owned()))
}

/// The command for pressing the button: flip a bound button, or add the
/// default setup, with `ids` for the three new nodes. `None` when the
/// binding is broken and there is nothing to press.
pub fn press(project: &Project, ids: impl FnOnce() -> [NodeId; 3]) -> Option<Command> {
    match binding(project) {
        Binding::Bound { button, on } => Some(Command::SetParam {
            node: button,
            key: BUTTON_STATE.to_owned(),
            value: Some(if on { 0.0 } else { 1.0 }),
        }),
        Binding::Missing => Some(default_setup(project, ids())),
        Binding::Broken(_) => None,
    }
}

/// Button → Metronome → Output, switched on, below everything else at the top
/// level.
fn default_setup(project: &Project, [button, metronome, output]: [NodeId; 3]) -> Command {
    let below = project
        .graph()
        .nodes()
        .filter(|(_, node)| node.parent.is_none())
        .map(|(_, node)| node.position.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let y = if below.is_finite() {
        below + 160.0
    } else {
        0.0
    };
    let wire = |from, from_port: &str, to, to_port: &str| {
        Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        })
    };
    Command::Batch(vec![
        Command::AddNode {
            id: button,
            node: Node::new(BUTTON_ID)
                .at(0.0, y)
                .with_param(BUTTON_STATE, 1.0),
        },
        Command::AddNode {
            id: metronome,
            node: Node::new(METRONOME_ID).at(220.0, y),
        },
        Command::AddNode {
            id: output,
            node: Node::new(OUTPUT_ID).at(440.0, y),
        },
        wire(button, "out", metronome, METRONOME_ON),
        wire(metronome, "out", output, "in"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};

    fn session() -> Session {
        Session::new(Nodes::all())
    }

    fn press_it(session: &mut Session) -> bool {
        let command = press(session.project(), || {
            std::array::from_fn(|_| session.new_node_id())
        });
        command
            .map(|command| session.edit([Edit::Apply(command)]))
            .is_some()
    }

    #[test]
    fn a_project_without_a_metronome_is_missing_one() {
        assert_eq!(binding(&Project::new()), Binding::Missing);
    }

    #[test]
    fn the_first_press_adds_the_default_setup_switched_on_as_one_undo_step() {
        let mut session = session();
        assert!(press_it(&mut session));
        let Binding::Bound { on, .. } = binding(session.project()) else {
            panic!("{:?}", binding(session.project()));
        };
        assert!(on);
        assert_eq!(session.project().graph().nodes().count(), 3);
        assert!(
            session.diagnostics().is_empty(),
            "{:?}",
            session.diagnostics()
        );

        session.undo();
        assert_eq!(binding(session.project()), Binding::Missing);
        assert_eq!(session.project().graph().nodes().count(), 0);
    }

    #[test]
    fn later_presses_flip_the_button() {
        let mut session = session();
        press_it(&mut session);
        press_it(&mut session);
        assert!(matches!(
            binding(session.project()),
            Binding::Bound { on: false, .. }
        ));
        press_it(&mut session);
        assert!(matches!(
            binding(session.project()),
            Binding::Bound { on: true, .. }
        ));
        // Still the same three nodes.
        assert_eq!(session.project().graph().nodes().count(), 3);
    }

    #[test]
    fn a_metronome_not_driven_by_a_button_is_broken_and_the_press_does_nothing() {
        let mut session = session();
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new(METRONOME_ID),
        })]);
        assert!(matches!(binding(session.project()), Binding::Broken(_)));
        assert!(!press_it(&mut session));

        // Wired to something that isn't a button.
        let lfo = session.new_node_id();
        session.edit([
            Edit::Apply(Command::AddNode {
                id: lfo,
                node: Node::new("noodle.osc.sine"),
            }),
            Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(lfo, "out"),
                to: Endpoint::new(id, METRONOME_ON),
            })),
        ]);
        assert!(matches!(binding(session.project()), Binding::Broken(_)));
    }

    #[test]
    fn the_new_setup_is_placed_below_existing_nodes() {
        let mut session = session();
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new("noodle.osc.sine").at(10.0, 300.0),
        })]);
        press_it(&mut session);
        for (_, node) in session.project().graph().nodes() {
            if node.type_id != "noodle.osc.sine" {
                assert!(node.position.y > 300.0);
            }
        }
    }

    #[test]
    fn a_bound_pair_wins_over_a_loose_metronome() {
        let mut session = session();
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new(METRONOME_ID),
        })]);
        assert!(matches!(binding(session.project()), Binding::Broken(_)));
        // The press is refused while it's broken, so add a pair by hand.
        let command = default_setup(
            session.project(),
            std::array::from_fn(|_| session.new_node_id()),
        );
        session.edit([Edit::Apply(command)]);
        assert!(matches!(binding(session.project()), Binding::Bound { .. }));
    }
}
