//! More buttons on the transport pill. Like the metronome's, each one is a
//! [`Button`](noodle_nodes::Button) node in the graph, so pressing it is an
//! ordinary undoable edit and the same wiring can be changed in the node
//! editor. The pill shows every top-level Button except the metronome's,
//! and its menu binds a new one to a switch on a node you pick: it adds a
//! Button wired into that parameter.

use noodle_core::{Command, Connection, Endpoint, Node, NodeId, Project};
use noodle_engine::{InputKind, ParamKind, Registry};
use noodle_nodes::{BUTTON_ID, BUTTON_STATE};

use crate::metronome::{self, Binding};

/// A Button in the pill.
#[derive(Clone, Debug, PartialEq)]
pub struct Extra {
    pub button: NodeId,
    pub on: bool,
    /// What it drives, for its tooltip; empty if nothing is wired to it.
    pub target: String,
}

/// The buttons the pill shows after the metronome's, in node order.
pub fn extras(project: &Project, registry: &Registry) -> Vec<Extra> {
    let graph = project.graph();
    let metronome = match metronome::binding(project) {
        Binding::Bound { button, .. } => Some(button),
        _ => None,
    };
    graph
        .nodes()
        .filter(|(id, node)| {
            node.type_id == BUTTON_ID && node.parent.is_none() && Some(*id) != metronome
        })
        .map(|(id, node)| {
            let target = graph
                .connections()
                .find(|c| c.from.node == id)
                .map(|c| describe(project, registry, &c.to))
                .unwrap_or_default();
            Extra {
                button: id,
                on: node.params.get(BUTTON_STATE).copied().unwrap_or(0.0) >= 0.5,
                target,
            }
        })
        .collect()
}

/// "Node · Port" for the tooltip.
fn describe(project: &Project, registry: &Registry, at: &Endpoint) -> String {
    let Some(node) = project.graph().node(at.node) else {
        return String::new();
    };
    let Some(node_type) = registry.get(&node.type_id) else {
        return node.type_id.clone();
    };
    let port = node_type
        .layout(&node.config)
        .ok()
        .and_then(|layout| {
            layout
                .inputs
                .iter()
                .find(|input| input.key == at.port)
                .map(|input| input.name.to_string())
        })
        .unwrap_or_else(|| at.port.clone());
    format!("{} · {port}", node_type.info().name)
}

/// A switch a new button can be bound to.
#[derive(Clone, Debug, PartialEq)]
pub struct Switch {
    pub node: NodeId,
    pub key: String,
    /// "Node · Parameter", for the menu.
    pub label: String,
}

/// The two-valued parameters of top-level nodes that nothing is wired into.
pub fn switches(project: &Project, registry: &Registry) -> Vec<Switch> {
    let graph = project.graph();
    let mut found = Vec::new();
    for (id, node) in graph.nodes() {
        if node.parent.is_some() || node.type_id == BUTTON_ID {
            continue;
        }
        let Some(node_type) = registry.get(&node.type_id) else {
            continue;
        };
        let Ok(layout) = node_type.layout(&node.config) else {
            continue;
        };
        for input in &layout.inputs {
            let InputKind::Param(param) = &input.kind else {
                continue;
            };
            let ParamKind::Stepped { labels } = &param.kind else {
                continue;
            };
            if labels.len() != 2
                || param.min != 0.0
                || param.max != 1.0
                || graph
                    .source(&Endpoint::new(id, input.key.to_string()))
                    .is_some()
            {
                continue;
            }
            found.push(Switch {
                node: id,
                key: input.key.to_string(),
                label: format!("{} · {}", node_type.info().name, input.name),
            });
        }
    }
    found
}

/// Adds a Button, set to the switch's current value and left of its node,
/// and wires it in, as one undo step.
pub fn bind(project: &Project, registry: &Registry, switch: &Switch, button: NodeId) -> Command {
    let graph = project.graph();
    let node = graph.node(switch.node);
    let current = node
        .and_then(|node| node.params.get(&switch.key).copied())
        .or_else(|| default_of(registry, node, &switch.key))
        .unwrap_or(0.0);
    let (x, y) = node.map_or((0.0, 0.0), |node| {
        (node.position.x - 200.0, node.position.y)
    });
    Command::Batch(vec![
        Command::AddNode {
            id: button,
            node: Node::new(BUTTON_ID)
                .at(x, y)
                .with_param(BUTTON_STATE, current),
        },
        Command::Connect(Connection {
            from: Endpoint::new(button, "out"),
            to: Endpoint::new(switch.node, switch.key.clone()),
        }),
    ])
}

fn default_of(registry: &Registry, node: Option<&Node>, key: &str) -> Option<f32> {
    let node = node?;
    let layout = registry.get(&node.type_id)?.layout(&node.config).ok()?;
    layout
        .inputs
        .iter()
        .find(|input| input.key == key)
        .map(|input| input.default_value())
}

/// Presses a button: flips its state.
pub fn press(project: &Project, button: NodeId) -> Option<Command> {
    let on = project
        .graph()
        .node(button)?
        .params
        .get(BUTTON_STATE)
        .copied()
        .unwrap_or(0.0)
        >= 0.5;
    Some(Command::SetParam {
        node: button,
        key: BUTTON_STATE.to_owned(),
        value: Some(if on { 0.0 } else { 1.0 }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Edit, Nodes, Session};
    use noodle_nodes::{METRONOME_ID, METRONOME_ON};

    fn session_with_metronome() -> (Session, NodeId) {
        let mut session = Session::new(Nodes::all());
        let metronome = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id: metronome,
            node: Node::new(METRONOME_ID).at(300.0, 40.0),
        })]);
        (session, metronome)
    }

    #[test]
    fn only_unwired_two_valued_parameters_can_be_bound() {
        let (session, metronome) = session_with_metronome();
        let found = switches(session.project(), session.registry());
        assert!(
            found
                .iter()
                .any(|s| s.node == metronome && s.key == METRONOME_ON),
            "{found:?}"
        );
        // Level is continuous, so it isn't a switch.
        assert!(found.iter().all(|s| s.key != "level"), "{found:?}");
    }

    #[test]
    fn binding_adds_a_wired_button_in_one_undo_step_and_it_shows_in_the_pill() {
        let (mut session, metronome) = session_with_metronome();
        let switch = switches(session.project(), session.registry())
            .into_iter()
            .find(|s| s.node == metronome && s.key == "accent")
            .unwrap();
        let button = session.new_node_id();
        let command = bind(session.project(), session.registry(), &switch, button);
        session.edit([Edit::Apply(command)]);
        let shown = extras(session.project(), session.registry());
        // Taken from the metronome's own value (on), and named for it.
        assert_eq!(
            shown,
            vec![Extra {
                button,
                on: true,
                target: "Metronome · Accent".into()
            }]
        );
        // The switch is wired now, so it isn't offered again.
        assert!(
            switches(session.project(), session.registry())
                .iter()
                .all(|s| s.key != "accent")
        );
        session.undo();
        assert!(extras(session.project(), session.registry()).is_empty());
    }

    #[test]
    fn pressing_flips_the_button() {
        let (mut session, metronome) = session_with_metronome();
        let switch = switches(session.project(), session.registry())
            .into_iter()
            .find(|s| s.node == metronome && s.key == "accent")
            .unwrap();
        let button = session.new_node_id();
        let command = bind(session.project(), session.registry(), &switch, button);
        session.edit([Edit::Apply(command)]);
        let flip = press(session.project(), button).unwrap();
        session.edit([Edit::Apply(flip)]);
        assert!(!extras(session.project(), session.registry())[0].on);
    }

    #[test]
    fn the_metronomes_own_button_is_not_repeated() {
        let mut session = Session::new(Nodes::all());
        let ids: [NodeId; 3] = std::array::from_fn(|_| session.new_node_id());
        let command = metronome::press(session.project(), || ids).unwrap();
        session.edit([Edit::Apply(command)]);
        assert!(extras(session.project(), session.registry()).is_empty());
    }
}
