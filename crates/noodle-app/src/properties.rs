//! The properties panel: the active node's config and parameters, and any
//! problems compiling it.
//!
//! Config comes first, because changing it can add or remove parameters.
//! A parameter with a wire into it is shown greyed out, since the wire's
//! signal replaces its value until the wire is removed.

use egui::{RichText, Ui};
use noodle_core::{Endpoint, Node, NodeId};
use noodle_engine::{InputKind, Location, NodeType};

use crate::session::{Edit, Session};
use crate::widgets::{ConfigField, ParamField};

pub fn show(ui: &mut Ui, session: &Session, active: Option<NodeId>) -> Vec<Edit> {
    let graph = session.project().graph();
    let Some((id, node)) = active.and_then(|id| Some((id, graph.node(id)?))) else {
        ui.weak("Select a node to see its properties.");
        return Vec::new();
    };
    let node_type = session.registry().get(&node.type_id);

    ui.heading(node_type.map_or(node.type_id.as_str(), |t| t.info().name));
    ui.label(RichText::new(&node.type_id).weak().small());
    problems(ui, session, id);

    let mut edits = Vec::new();
    match node_type {
        Some(node_type) => {
            config(ui, node_type.as_ref(), id, node, &mut edits);
            params(ui, session, node_type.as_ref(), id, node, &mut edits);
        }
        None => {
            // Nothing describes the values, so they can only be shown.
            for (key, value) in &node.params {
                ui.label(format!("{key}: {value}"));
            }
        }
    }
    edits
}

fn problems(ui: &mut Ui, session: &Session, id: NodeId) {
    for diagnostic in session.diagnostics() {
        let here = match &diagnostic.location {
            Location::Node(node) => *node == id,
            Location::Wire(input) => input.node == id,
        };
        if here {
            let color = ui.visuals().error_fg_color;
            ui.label(RichText::new(diagnostic.problem.to_string()).color(color));
        }
    }
}

fn config(ui: &mut Ui, node_type: &dyn NodeType, id: NodeId, node: &Node, edits: &mut Vec<Edit>) {
    let settings = node_type.config();
    if settings.is_empty() {
        return;
    }
    ui.separator();
    ui.label(RichText::new("Settings").strong());
    for info in settings {
        let out = ConfigField::new(info, node.config.get(info.key))
            .id_salt((id, info.key))
            .show(ui);
        edits.extend(out.edit(id, info.key));
        out.response
            .on_hover_text("Changing this rebuilds the node, and may change its ports.");
    }
}

fn params(
    ui: &mut Ui,
    session: &Session,
    node_type: &dyn NodeType,
    id: NodeId,
    node: &Node,
    edits: &mut Vec<Edit>,
) {
    // A config the node type rejects is reported by `problems`.
    let Ok(layout) = node_type.layout(&node.config) else {
        return;
    };
    let mut params = layout.inputs.iter().filter_map(|input| match &input.kind {
        InputKind::Param(info) => Some((input, info)),
        InputKind::Audio => None,
    });
    let Some(first) = params.next() else {
        return;
    };
    ui.separator();
    ui.label(RichText::new("Parameters").strong());
    for (input, info) in std::iter::once(first).chain(params) {
        let key = input.key.as_ref();
        let value = node.params.get(key).copied().unwrap_or(info.default);
        let source = session
            .project()
            .graph()
            .source(&Endpoint::new(id, key))
            .cloned();
        ui.add_enabled_ui(source.is_none(), |ui| {
            let out = ParamField::new(&input.name, info, value)
                .id_salt((id, key))
                .show(ui);
            if let Some(source) = &source {
                out.response
                    .on_disabled_hover_text(format!("Driven by {}", describe(session, source)));
            } else {
                edits.extend(out.edits(id, key));
            }
        });
    }
}

/// e.g. "Sine › Out".
fn describe(session: &Session, endpoint: &Endpoint) -> String {
    let Some(node) = session.project().graph().node(endpoint.node) else {
        return endpoint.to_string();
    };
    let Some(node_type) = session.registry().get(&node.type_id) else {
        return format!("{} › {}", node.type_id, endpoint.port);
    };
    let port = node_type
        .layout(&node.config)
        .ok()
        .and_then(|layout| {
            let port = layout
                .outputs
                .into_iter()
                .find(|p| p.key == endpoint.port)?;
            Some(port.name.into_owned())
        })
        .unwrap_or_else(|| endpoint.port.clone());
    format!("{} › {port}", node_type.info().name)
}

#[cfg(test)]
mod tests {
    use egui::{Key, Modifiers, vec2};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use noodle_core::{Command, Config, Connection, Value};

    use super::*;

    struct State {
        session: Session,
        active: Option<NodeId>,
    }

    fn harness(nodes: impl IntoIterator<Item = Node>) -> Harness<'static, State> {
        let mut session = Session::new(crate::registry());
        let edits: Vec<_> = nodes
            .into_iter()
            .enumerate()
            .map(|(i, node)| {
                Edit::Apply(Command::AddNode {
                    id: NodeId(i as u64 + 1),
                    node,
                })
            })
            .collect();
        session.edit(edits);
        let state = State {
            session,
            active: Some(NodeId(1)),
        };
        let mut harness = Harness::new_ui_state(
            |ui, state: &mut State| {
                let edits = show(ui, &state.session, state.active);
                state.session.edit(edits);
            },
            state,
        );
        harness.set_size(vec2(300.0, 400.0));
        harness.run();
        harness
    }

    fn param(harness: &Harness<'_, State>, key: &str) -> Option<f32> {
        let graph = harness.state().session.project().graph();
        graph.node(NodeId(1))?.params.get(key).copied()
    }

    #[test]
    fn nothing_selected() {
        let mut harness = harness([]);
        harness.state_mut().active = None;
        harness.run();
        harness.get_by_label("Select a node to see its properties.");
    }

    #[test]
    fn shows_the_parameters_with_their_units() {
        let harness = harness([Node::new("noodle.osc.sine").with_param("frequency", 220.0)]);
        harness.get_by_label("Sine");
        let field = harness.get_by_label("Frequency");
        assert_eq!(field.value().as_deref(), Some("220 Hz"));
    }

    #[test]
    fn a_drag_is_one_undo_step() {
        let mut harness = harness([Node::new("noodle.osc.sine")]);
        let rect = harness.get_by_label("Frequency").rect();
        harness.drag_at(rect.center());
        harness.run();
        for i in 1..=5 {
            harness.hover_at(rect.center() + vec2(10.0 * i as f32, 0.0));
            harness.run();
        }
        harness.drop_at(rect.center() + vec2(50.0, 0.0));
        harness.run();

        let dragged = param(&harness, "frequency").expect("the drag set the frequency");
        assert!(dragged > 440.0, "{dragged}");
        harness.state_mut().session.undo();
        assert_eq!(param(&harness, "frequency"), None);
    }

    #[test]
    fn backspace_resets_to_the_default() {
        let mut harness = harness([Node::new("noodle.osc.sine").with_param("frequency", 220.0)]);
        harness.get_by_label("Frequency").hover();
        harness.run();
        harness.key_press(Key::Backspace);
        harness.run();
        assert_eq!(param(&harness, "frequency"), None);

        harness.state_mut().session.undo();
        assert_eq!(param(&harness, "frequency"), Some(220.0));
    }

    #[test]
    fn connected_parameters_are_greyed_out() {
        let mut harness = harness([Node::new("noodle.osc.sine"), Node::new("noodle.osc.sine")]);
        harness
            .state_mut()
            .session
            .edit([Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(NodeId(2), "out"),
                to: Endpoint::new(NodeId(1), "frequency"),
            }))]);
        harness.run();
        let field = harness.get_by_label("Frequency");
        assert!(field.accesskit_node().is_disabled());

        // Dragging it does nothing.
        let rect = field.rect();
        harness.drag_at(rect.center());
        harness.run();
        harness.hover_at(rect.center() + vec2(50.0, 0.0));
        harness.run();
        harness.drop_at(rect.center() + vec2(50.0, 0.0));
        harness.run();
        assert_eq!(param(&harness, "frequency"), None);
    }

    #[test]
    fn shows_config_and_its_problems() {
        let mix = |inputs| {
            Node::new("noodle.util.mix")
                .with_config(Config::new().with("inputs", Value::Int(inputs)))
        };
        let harness = harness([mix(3)]);
        harness.get_by_label("Inputs");
        harness.get_by_value("3");

        let harness = super::tests::harness([mix(0)]);
        harness.get_by_label_contains("inputs");
        harness.get_by_value("0");
    }

    /// Replaces the text in the focused field.
    fn type_over(harness: &Harness<'_, State>, text: &str) {
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.event(egui::Event::Text(text.into()));
    }

    #[test]
    fn a_config_edit_in_progress_stays_with_its_node() {
        let mix = || Node::new("noodle.util.mix");
        let mut harness = harness([mix(), mix()]);
        let inputs = |harness: &Harness<'_, State>, node| {
            let graph = harness.state().session.project().graph();
            graph
                .node(NodeId(node))
                .unwrap()
                .config
                .get("inputs")
                .cloned()
        };

        // Start typing into the first Mix's Inputs, then select the second.
        harness.get_by_value("2").click();
        harness.run();
        type_over(&harness, "8");
        harness.run();
        harness.state_mut().active = Some(NodeId(2));
        harness.run();
        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(
            inputs(&harness, 2),
            None,
            "the edit moved to the other node"
        );

        // Going back doesn't commit the abandoned edit either.
        harness.state_mut().active = Some(NodeId(1));
        harness.run();
        harness.run();
        assert_eq!(inputs(&harness, 1), None);

        // But finishing an edit does.
        harness.get_by_value("2").click();
        harness.run();
        type_over(&harness, "4");
        harness.run();
        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(inputs(&harness, 1), Some(Value::Int(4)));
    }

    #[test]
    fn shows_problems() {
        let harness = harness([Node::new("no.such.type").with_param("x", 1.0)]);
        harness.get_by_label("unknown node type `no.such.type`");
        harness.get_by_label("x: 1");
    }
}
