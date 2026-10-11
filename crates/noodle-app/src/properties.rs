//! The properties panel: the active node's config and parameters, and any
//! problems compiling it.
//!
//! Config comes first, because changing it can add or remove parameters.
//! A parameter with a wire into it is shown greyed out, since the wire's
//! signal replaces its value until the wire is removed.

use egui::{RichText, Ui};
use noodle_core::{Command, Endpoint, Node, NodeId};
use noodle_engine::{InputKind, Location, NodeType, ParamReading};

use crate::session::{Edit, Session};
use crate::widgets::format::format_value;
use crate::widgets::{ConfigField, CurveEditor, GraphEditor, GraphKind, Live, ParamField};

/// What the wired parameter `key` of a node has been doing lately, if known:
/// the editor reads it once a frame.
pub type LiveParams<'a> = &'a dyn Fn(NodeId, &str) -> Option<Live>;

pub fn show(
    ui: &mut Ui,
    session: &Session,
    active: Option<NodeId>,
    live: LiveParams<'_>,
) -> Vec<Edit> {
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
    plugin_editor(ui, session, id, &mut edits);
    freezing(ui, session, id, &mut edits);
    match node_type {
        Some(node_type) => {
            config(ui, session, node_type.as_ref(), id, node, &mut edits);
            params(ui, session, node_type.as_ref(), id, node, live, &mut edits);
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

/// A plugin node's editor button.
fn plugin_editor(ui: &mut Ui, session: &Session, id: NodeId, edits: &mut Vec<Edit>) {
    let Some(plugin) = session.plugin_of(id) else {
        return;
    };
    let open = session.plugin_editor_open(id);
    ui.horizontal(|ui| {
        let label = if open { "Close editor" } else { "Open editor" };
        if ui
            .button(label)
            .on_hover_text(format!(
                "{}'s own window. Closing it keeps what you changed in the project.",
                plugin.descriptor().name
            ))
            .clicked()
        {
            edits.push(Edit::PluginEditor {
                node: id,
                open: !open,
            });
        }
    });
}

/// Whether the node is frozen, how its render is doing, and the button to
/// change it.
fn freezing(ui: &mut Ui, session: &Session, id: NodeId, edits: &mut Vec<Edit>) {
    use crate::freezing::BadgeState;

    let frozen = session.project().is_frozen(id);
    let badge = session.freeze_badges().get(&id);
    if !frozen && badge.is_none() {
        // Offer to freeze only what can be: a node or group with audio out.
        let node = session.project().graph().node(id);
        let has_audio = node.is_some_and(|node| {
            if node.type_id == noodle_core::group::GROUP {
                return !session.project().graph().group_ports(id).outputs.is_empty();
            }
            session
                .registry()
                .get(&node.type_id)
                .and_then(|t| t.layout(&node.config).ok())
                .is_some_and(|layout| !layout.outputs.is_empty())
        });
        if !has_audio {
            return;
        }
    }
    ui.horizontal(|ui| {
        let label = if frozen { "Unfreeze" } else { "Freeze" };
        if ui
            .button(label)
            .on_hover_text("Render this once and play the result, so it costs no CPU (Shift+F)")
            .clicked()
        {
            edits.push(Edit::Apply(Command::SetFrozen {
                node: id,
                frozen: !frozen,
            }));
        }
        if let Some(badge) = badge {
            match &badge.state {
                BadgeState::Ready => {
                    ui.weak(if badge.frozen { "Frozen" } else { "Rendered" });
                }
                BadgeState::Rendering(fraction) => {
                    ui.add(
                        egui::ProgressBar::new(*fraction)
                            .desired_width(120.0)
                            .show_percentage(),
                    );
                }
                BadgeState::Waiting => {
                    ui.weak("Waiting to render");
                }
                BadgeState::Failed(why) => {
                    ui.colored_label(ui.visuals().error_fg_color, why);
                }
            }
        }
    });
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

fn config(
    ui: &mut Ui,
    session: &Session,
    node_type: &dyn NodeType,
    id: NodeId,
    node: &Node,
    edits: &mut Vec<Edit>,
) {
    let settings = node_type.config();
    if settings.is_empty() {
        return;
    }
    ui.separator();
    ui.label(RichText::new("Settings").strong());
    for info in settings {
        // Chosen in the outputs view, which knows what devices there are.
        if node_type.info().id == noodle_engine::OUTPUT_ID
            && info.key == noodle_engine::OUTPUT_DEVICE_KEY
        {
            let device = info.get_text(&node.config);
            ui.label(if device.is_empty() {
                "Plays on the main output".to_owned()
            } else {
                format!("Plays on {device}")
            })
            .on_hover_text("Choose the device in View → Outputs.");
            continue;
        }
        if node_type.info().id == noodle_nodes::REMAP_ID
            && info.key == noodle_nodes::REMAP_CURVE_KEY
        {
            let text = info.get_text(&node.config);
            ui.label(info.name);
            let curve = noodle_nodes::Curve::from_text(&text);
            edits.extend(CurveEditor::new(id, info.key, &curve).show(ui));
            ui.label(
                RichText::new(
                    "Left to right is the input range, bottom to top the output range. \
                     Click the line to add a point.",
                )
                .weak()
                .small(),
            );
            if ui.small_button("Reset to straight line").clicked() {
                edits.push(Edit::Apply(Command::SetConfig {
                    node: id,
                    key: info.key.to_owned(),
                    value: None,
                }));
            }
            continue;
        }
        if info.key == noodle_plugins::PARAMS_KEY
            && let Some(plugin) = session.plugin_of(id)
        {
            param_picker(ui, session, plugin.descriptor(), id, node, edits);
            continue;
        }
        let out = ConfigField::new(info, node.config.get(info.key))
            .id_salt((id, info.key))
            .show(ui);
        edits.extend(out.edit(id, info.key));
        out.response
            .on_hover_text("Changing this rebuilds the node, and may change its ports.");
    }
}

/// The most parameters the picker lists at once; narrow the search for more.
const PICKER_ROWS: usize = 200;

/// Which of a plugin's parameters get ports on its node: a searchable list
/// with a checkbox each, grouped by the plugin's own modules.
fn param_picker(
    ui: &mut Ui,
    session: &Session,
    descriptor: &noodle_plugins::Descriptor,
    id: NodeId,
    node: &Node,
    edits: &mut Vec<Edit>,
) {
    use noodle_plugins::{exposed_params, params_text};

    let chosen: Vec<u32> = exposed_params(descriptor, &node.config)
        .iter()
        .map(|p| p.id)
        .collect();
    // A wire into a parameter keeps its port: hiding it would leave the wire
    // pointing at nothing, which fails the compile.
    let wired = |param: u32| {
        session
            .project()
            .graph()
            .source(&Endpoint::new(id, noodle_plugins::param_key(param)))
            .is_some()
    };
    let search_id = ui.make_persistent_id(("param-search", id));
    let mut query: String = ui.data(|d| d.get_temp(search_id)).unwrap_or_default();
    egui::CollapsingHeader::new(format!(
        "Parameters on the node ({} of {})",
        chosen.len(),
        descriptor.params.len()
    ))
    .id_salt(("param-picker", id))
    .default_open(false)
    .show(ui, |ui| {
        ui.add(egui::TextEdit::singleline(&mut query).hint_text("Search parameters"));
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let matches: Vec<&noodle_plugins::ParamDesc> = descriptor
            .params
            .iter()
            .filter(|p| {
                let text = format!("{} {}", p.module, p.name).to_lowercase();
                words.iter().all(|w| text.contains(w))
            })
            .collect();
        let set = |ids: Vec<u32>| {
            Edit::Apply(Command::SetConfig {
                node: id,
                key: noodle_plugins::PARAMS_KEY.to_owned(),
                value: Some(noodle_core::Value::Text(params_text(&ids))),
            })
        };
        ui.horizontal(|ui| {
            if ui.small_button("Show matches").clicked() {
                let mut ids = chosen.clone();
                ids.extend(matches.iter().map(|p| p.id).filter(|p| !chosen.contains(p)));
                edits.push(set(ids));
            }
            if ui.small_button("Hide matches").clicked() {
                let ids = chosen
                    .iter()
                    .copied()
                    .filter(|c| wired(*c) || !matches.iter().any(|p| p.id == *c))
                    .collect();
                edits.push(set(ids));
            }
        });
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .id_salt(("param-picker-list", id))
            .show(ui, |ui| {
                let mut module = None;
                for param in matches.iter().take(PICKER_ROWS) {
                    if module != Some(&param.module) {
                        module = Some(&param.module);
                        if !param.module.is_empty() {
                            ui.label(RichText::new(&param.module).weak().small());
                        }
                    }
                    let mut on = chosen.contains(&param.id);
                    let locked = on && wired(param.id);
                    let response = ui
                        .add_enabled(!locked, egui::Checkbox::new(&mut on, &param.name))
                        .on_disabled_hover_text("A wire goes into this parameter.");
                    if response.changed() {
                        let mut ids = chosen.clone();
                        if on {
                            ids.push(param.id);
                        } else {
                            ids.retain(|c| *c != param.id);
                        }
                        edits.push(set(ids));
                    }
                }
                if matches.len() > PICKER_ROWS {
                    ui.weak(format!(
                        "{} more: narrow the search.",
                        matches.len() - PICKER_ROWS
                    ));
                }
            });
        ui.label(
            RichText::new(
                "Only these get ports. A parameter left off keeps the value in the \
                 plugin's saved state; one with a wire into it stays.",
            )
            .weak()
            .small(),
        );
    });
    ui.data_mut(|d| d.insert_temp(search_id, query));
}

fn params(
    ui: &mut Ui,
    session: &Session,
    node_type: &dyn NodeType,
    id: NodeId,
    node: &Node,
    live: LiveParams<'_>,
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
    let params: Vec<_> = std::iter::once(first).chain(params).collect();
    if let Some(kind) = GraphKind::of(&node.type_id) {
        graph(ui, session, kind, id, node, &params, live, edits);
    }
    for (input, info) in params {
        let key = input.key.as_ref();
        let value = node.params.get(key).copied().unwrap_or(info.default);
        let source = session
            .project()
            .graph()
            .source(&Endpoint::new(id, key))
            .cloned();
        // A wired parameter shows what the wire is delivering right now.
        let reading = source.as_ref().and_then(|_| {
            live(id, key).map(|live| ParamReading {
                value: live.value,
                min: live.min,
                max: live.max,
            })
        });
        let shown = reading.map_or(value, |reading| reading.value);
        ui.add_enabled_ui(source.is_none(), |ui| {
            let out = ParamField::new(&input.name, info, shown)
                .id_salt((id, key))
                .show(ui);
            if let Some(source) = &source {
                out.response
                    .on_disabled_hover_text(format!("Driven by {}", describe(session, source)));
            } else {
                edits.extend(out.edits(id, key));
            }
        });
        ui.label(RichText::new(range_text(info, reading)).weak().small());
    }
}

/// The graph of what the node does, with its points to drag. A wired
/// parameter is drawn at the value its wire delivers, and can't be dragged.
#[allow(clippy::too_many_arguments)]
fn graph(
    ui: &mut Ui,
    session: &Session,
    kind: GraphKind,
    id: NodeId,
    node: &Node,
    params: &[(&noodle_engine::InputPort, &noodle_engine::ParamInfo)],
    live: LiveParams<'_>,
    edits: &mut Vec<Edit>,
) {
    let info_of = |key: &str| {
        params
            .iter()
            .find(|(input, _)| input.key == key)
            .map(|p| p.1)
    };
    let wired = |key: &str| {
        session
            .project()
            .graph()
            .source(&Endpoint::new(id, key))
            .is_some()
    };
    let values = |key: &str| {
        let own = || {
            let default = info_of(key).map_or(0.0, |info| info.default);
            node.params.get(key).copied().unwrap_or(default)
        };
        if wired(key) {
            live(id, key).map_or_else(own, |live| live.value)
        } else {
            own()
        }
    };
    let range = |key: &str| info_of(key).map_or((0.0, 1.0), |info| (info.min, info.max));
    let editable = |key: &str| !wired(key);
    edits.extend(GraphEditor::new(id, kind, &values, &range, &editable).show(ui));
}

/// What a parameter accepts, and for a wired one the range its signal has
/// covered lately: "Range 20 Hz to 20 kHz, default 440 Hz".
fn range_text(info: &noodle_engine::ParamInfo, reading: Option<ParamReading>) -> String {
    let at = |value| format_value(info, value);
    let mut text = format!(
        "Range {} to {}, default {}",
        at(info.min),
        at(info.max),
        at(info.default)
    );
    if let Some(reading) = reading {
        text.push_str(&format!(
            "\nLive {}, recently {} to {}",
            at(reading.value),
            at(reading.min),
            at(reading.max)
        ));
    }
    text
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
        let mut session = Session::new(crate::session::Nodes::all());
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
                let edits = show(ui, &state.session, state.active, &|_, _| None);
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

    /// The ADSR's graph and the screen position of its plot at `(x, y)`.
    fn graph_at(harness: &Harness<'_, State>) -> impl Fn(f32, f32) -> egui::Pos2 + use<> {
        let outer = harness.get_by_label("Envelope graph").rect();
        let plot = outer.shrink2(egui::vec2(2.0, 8.0));
        move |x, y| {
            egui::pos2(
                plot.left() + x * plot.width(),
                plot.bottom() - y * plot.height(),
            )
        }
    }

    fn drag(harness: &mut Harness<'_, State>, from: egui::Pos2, to: egui::Pos2) {
        harness.hover_at(from);
        harness.run();
        harness.drag_at(from);
        harness.run();
        harness.hover_at(to);
        harness.run();
        harness.drop_at(to);
        harness.run();
    }

    #[test]
    fn dragging_the_envelope_graph_sets_the_adsr_parameters() {
        let mut harness = harness([Node::new("noodle.mod.adsr")]);
        harness.set_size(vec2(300.0, 700.0));
        harness.run();
        // The defaults: 10 ms, 200 ms, 0.7, 300 ms, drawn over 0.765 s.
        let span = (0.01 + 0.2 + 0.1275 + 0.3) * 1.2;
        let at = graph_at(&harness);

        // The decay point is where the decay ends, at the sustain level.
        let from = at(0.21 / span, 0.7);
        let to = at(0.51 / span, 0.3);
        drag(&mut harness, from, to);
        let decay = param(&harness, "decay").unwrap();
        let sustain = param(&harness, "sustain").unwrap();
        assert!((decay - 0.5).abs() < 0.03, "{decay}");
        assert!((sustain - 0.3).abs() < 0.03, "{sustain}");

        // The release point is the right-hand end.
        let at = graph_at(&harness);
        let span = (0.01 + decay + 0.25 * (0.01 + decay + 0.3) + 0.3) * 1.2;
        let held = 0.01 + decay + 0.25 * (0.01 + decay + 0.3);
        let from = at((held + 0.3) / span, 0.0);
        let to = at((held + 0.6) / span, 0.0);
        drag(&mut harness, from, to);
        let release = param(&harness, "release").unwrap();
        assert!((release - 0.6).abs() < 0.05, "{release}");

        // A whole drag is one undo step.
        harness.state_mut().session.undo();
        harness.run();
        assert_eq!(param(&harness, "release"), None);
        assert!((param(&harness, "decay").unwrap() - decay).abs() < 1e-6);
    }

    #[test]
    fn a_wired_parameter_is_not_changed_by_dragging_the_graph() {
        let mut harness = harness([Node::new("noodle.mod.adsr"), Node::new("noodle.mod.lfo")]);
        harness
            .state_mut()
            .session
            .edit([Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(NodeId(2), "out"),
                to: Endpoint::new(NodeId(1), "sustain"),
            }))]);
        harness.set_size(vec2(300.0, 700.0));
        harness.run();
        let span = (0.01 + 0.2 + 0.1275 + 0.3) * 1.2;
        let at = graph_at(&harness);
        // Sustain has a wire, so the decay-and-sustain point moves in time
        // only, and sustain is untouched.
        drag(&mut harness, at(0.21 / span, 0.7), at(0.51 / span, 0.3));
        assert!((param(&harness, "decay").unwrap() - 0.5).abs() < 0.03);
        assert_eq!(param(&harness, "sustain"), None);
    }

    #[test]
    fn filters_and_lfos_have_graphs_too() {
        for (type_id, label) in [
            ("noodle.filter.svf", "Frequency response graph"),
            ("noodle.filter.ladder", "Frequency response graph"),
            ("noodle.mod.lfo", "Wave graph"),
        ] {
            let harness = harness([Node::new(type_id)]);
            harness.get_by_label(label);
        }
    }

    #[test]
    fn dragging_the_filter_point_sets_cutoff_and_resonance() {
        let mut harness = harness([Node::new("noodle.filter.svf")]);
        harness.set_size(vec2(300.0, 700.0));
        harness.run();
        let outer = harness.get_by_label("Frequency response graph").rect();
        let plot = outer.shrink2(egui::vec2(2.0, 8.0));
        let at = |x: f32, y: f32| {
            egui::pos2(
                plot.left() + x * plot.width(),
                plot.bottom() - y * plot.height(),
            )
        };
        // At 1 kHz, no resonance: the low-pass is 6 dB down.
        let x = (1_000.0f32 / 20.0).ln() / 1_000.0f32.ln();
        let y = ((-6.02 + 48.0) / 84.0) as f32;
        // Drag it up and right.
        drag(&mut harness, at(x, y), at(x + 0.2, y + 0.3));
        assert!(param(&harness, "cutoff").unwrap() > 2_000.0);
        assert!(param(&harness, "resonance").unwrap() > 0.5);
    }

    #[test]
    fn nothing_selected() {
        let mut harness = harness([]);
        harness.state_mut().active = None;
        harness.run();
        harness.get_by_label("Select a node to see its properties.");
    }

    #[test]
    fn each_parameter_shows_the_range_it_accepts() {
        let harness = harness([Node::new("noodle.osc.sine")]);
        harness.get_all_by_label_contains("Range ").next().unwrap();
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

    fn curve_of(harness: &Harness<'_, State>) -> noodle_nodes::Curve {
        let graph = harness.state().session.project().graph();
        let config = &graph.node(NodeId(1)).unwrap().config;
        match config.get("curve") {
            Some(Value::Text(text)) => noodle_nodes::Curve::from_text(text),
            _ => noodle_nodes::Curve::linear(),
        }
    }

    #[test]
    fn the_remap_curve_is_edited_with_the_mouse() {
        let mut harness = harness([Node::new("noodle.util.remap")]);
        harness.set_size(vec2(300.0, 600.0));
        harness.run();
        // The editor is a square under the "Curve" label, `side` wide.
        let label = harness.get_by_label("Curve").rect();
        let side = 240.0f32.min(300.0 - 2.0 * label.min.x);
        let at = |x: f32, y: f32| {
            egui::pos2(label.min.x + x * side, label.max.y + 4.0 + side * (1.0 - y))
        };
        assert_eq!(curve_of(&harness).points().len(), 2);

        // Click the middle of the straight line: a point appears.
        harness.hover_at(at(0.5, 0.5));
        harness.run();
        harness.drag_at(at(0.5, 0.5));
        harness.run();
        harness.drop_at(at(0.5, 0.5));
        harness.run();
        let curve = curve_of(&harness);
        assert_eq!(curve.points().len(), 3, "{curve:?}");
        assert!((curve.points()[1].x - 0.5).abs() < 0.1);

        // Clicking away from the line adds nothing.
        harness.hover_at(at(0.5, 0.9));
        harness.run();
        harness.drag_at(at(0.5, 0.9));
        harness.run();
        harness.drop_at(at(0.5, 0.9));
        harness.run();
        assert_eq!(curve_of(&harness).points().len(), 3);

        // Drag the new point up.
        let from = at(curve.points()[1].x, curve.points()[1].y);
        harness.hover_at(from);
        harness.run();
        harness.drag_at(from);
        harness.run();
        harness.hover_at(at(0.5, 0.9));
        harness.run();
        harness.drop_at(at(0.5, 0.9));
        harness.run();
        let curve = curve_of(&harness);
        assert!(curve.points()[1].y > 0.8, "{curve:?}");
        // One undo step for the whole drag.
        harness.state_mut().session.undo();
        harness.run();
        let undone = curve_of(&harness);
        assert_eq!(undone.points().len(), 3);
        assert!((undone.points()[1].y - 0.5).abs() < 0.05, "{undone:?}");

        // The button puts it back to a line.
        harness.get_by_label("Reset to straight line").click();
        harness.run();
        harness.run();
        assert_eq!(curve_of(&harness), noodle_nodes::Curve::linear());
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
    fn a_param_abandoned_mid_typing_is_a_slider_again() {
        let mut harness = harness([Node::new("noodle.osc.sine")]);
        harness.get_by_label("Frequency").click();
        harness.run();
        type_over(&harness, "700");
        harness.run();

        // The panel goes away while the text box has focus (e.g. the node is
        // deselected or removed by undo), then comes back.
        harness.state_mut().active = None;
        harness.run_steps(4);
        harness.state_mut().active = Some(NodeId(1));
        harness.run_steps(3);
        assert_eq!(
            param(&harness, "frequency"),
            None,
            "abandoned text committed"
        );

        let rect = harness.get_by_label("Frequency").rect();
        harness.drag_at(rect.center());
        harness.run();
        for i in 1..=5 {
            harness.hover_at(rect.center() + vec2(10.0 * i as f32, 0.0));
            harness.run();
        }
        harness.drop_at(rect.center() + vec2(50.0, 0.0));
        harness.run();
        assert!(param(&harness, "frequency").is_some(), "the field is dead");
    }

    #[test]
    fn shows_problems() {
        let harness = harness([Node::new("no.such.type").with_param("x", 1.0)]);
        harness.get_by_label("unknown node type `no.such.type`");
        harness.get_by_label("x: 1");
    }
}
