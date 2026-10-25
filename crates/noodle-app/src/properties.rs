//! The properties panel: the active node's config and parameters, and any
//! problems compiling it.
//!
//! Config comes first, because changing it can add or remove parameters.
//! A parameter with a wire into it is shown greyed out, since the wire's
//! signal replaces its value until the wire is removed.

use egui::{RichText, Ui};
use noodle_core::group::GROUP;
use noodle_core::library::{SYSTEM, USER};
use noodle_core::{Command, Endpoint, Node, NodeId};
use noodle_engine::{InputKind, Location, NodeType, NodeView, ParamReading};

use crate::session::{Edit, LibraryEdit, Session};
use crate::widgets::format::format_value;
use crate::widgets::{
    ConfigField, CurveEditor, EnvelopeEditor, GraphEditor, GraphKind, Live, ParamField,
};

/// What the wired parameter `key` of a node has been doing lately, if known:
/// the editor reads it once a frame.
pub type LiveParams<'a> = &'a dyn Fn(NodeId, &str) -> Option<Live>;

/// What a Spectrum node last measured, if it has.
pub type Spectra<'a> = &'a dyn Fn(NodeId) -> Option<&'a crate::editor::SpectrumTrace>;

pub fn show<'a>(
    ui: &mut Ui,
    session: &Session,
    active: Option<NodeId>,
    live: LiveParams<'_>,
    spectra: Spectra<'a>,
) -> Vec<Edit> {
    let graph = session.project().graph();
    let mut edits = pending_scripts(ui, session, active);
    let Some((id, node)) = active.and_then(|id| Some((id, graph.node(id)?))) else {
        ui.weak("Select a node to see its properties.");
        return edits;
    };
    let node_type = session.registry().get(&node.type_id);

    ui.horizontal(|ui| {
        // The freeze button sits at the right of the name, in the same row.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            freeze_button(ui, session, id, &mut edits);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let name = node_type.map_or_else(
                    || crate::editor::friendly_type_name(&node.type_id),
                    |t| t.info().name.to_owned(),
                );
                ui.add(egui::Label::new(RichText::new(name).heading()).truncate())
                    .on_hover_text(&node.type_id);
            });
        });
    });
    freeze_status(ui, session, id);
    problems(ui, session, id);
    presets(ui, session, id, node, &mut edits);
    plugin_editor(ui, session, id, &mut edits);
    if node_type.is_some_and(|t| t.view(&node.config) == NodeView::Spectrum) {
        spectrum_views(ui, session, spectra(id));
    }
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

/// A Spectrum node's curve and spectrogram at full width, with the frequency
/// axis marked.
fn spectrum_views(ui: &mut Ui, session: &Session, trace: Option<&crate::editor::SpectrumTrace>) {
    if !session.engine_running() {
        ui.weak("The engine is off, so there is nothing to measure.");
    }
    let width = ui.available_width();
    let (response, painter) = ui.allocate_painter(egui::vec2(width, 150.0), egui::Sense::hover());
    crate::editor::draw_spectrum(&painter, response.rect, trace);
    response.on_hover_text("Loudness by frequency (20 Hz to 20 kHz, log), 0 dB at the top. The thin line holds recent peaks.");
    ui.label(
        RichText::new("20 Hz  ·  100  ·  1 k  ·  10 k  ·  20 kHz")
            .small()
            .weak(),
    );
    let (response, painter) = ui.allocate_painter(egui::vec2(width, 100.0), egui::Sense::hover());
    crate::editor::draw_spectrogram(&painter, response.rect, trace);
    response.on_hover_text(
        "Spectrogram: the last few seconds, newest on the right, low frequencies at the bottom.",
    );
    ui.add_space(4.0);
}

/// The Presets control: pick a preset that fits, save the values as a user
/// preset, or overwrite the user preset applied (when it has been edited).
/// For a group the presets are for everything inside it.
fn presets(ui: &mut Ui, session: &Session, id: NodeId, node: &Node, edits: &mut Vec<Edit>) {
    let is_group = node.type_id == GROUP;
    if !is_group && session.registry().get(&node.type_id).is_none() {
        return;
    }
    let available = session.presets_for(id);
    let status = session.preset_status(id);
    let current = match &status {
        Some(s) if s.modified => format!("{} (modified)", s.name),
        Some(s) => s.name.clone(),
        None => "None".to_string(),
    };
    ui.horizontal(|ui| {
        ui.label("Presets");
        crate::widgets::Dropdown::from_id_salt(("presets", id))
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (heading, store) in [("System", SYSTEM), ("User", USER)] {
                    let mut listed = available
                        .iter()
                        .filter(|(entry, _)| entry.starts_with(store))
                        .peekable();
                    if listed.peek().is_none() {
                        continue;
                    }
                    ui.weak(heading);
                    for (entry, name) in listed {
                        let applied = status.as_ref().is_some_and(|s| s.id == *entry);
                        if ui.selectable_label(applied, *name).clicked() {
                            edits.extend(session.apply_preset(id, entry));
                        }
                    }
                }
                if available.is_empty() {
                    ui.weak("None fit yet");
                }
            });
    });

    let naming = ui.id().with(("naming-preset", id));
    let mut name: Option<String> = ui.data(|d| d.get_temp(naming));
    let mut just_opened = false;
    let save = |name: String, overwrite: Option<String>| {
        Edit::Library(if is_group {
            LibraryEdit::GroupPreset {
                group: id,
                name,
                overwrite,
            }
        } else {
            LibraryEdit::NodePreset {
                node: id,
                name,
                overwrite,
            }
        })
    };
    ui.horizontal(|ui| {
        let overwritable = status.as_ref().filter(|s| s.user && s.modified);
        let button = ui.add_enabled(overwritable.is_some(), egui::Button::new("Save"));
        if let (true, Some(s)) = (button.clicked(), overwritable) {
            edits.push(save(s.name.clone(), Some(s.id.clone())));
        }
        if ui.button("Save as...").clicked() {
            just_opened = true;
            name = Some(status.as_ref().map_or_else(String::new, |s| s.name.clone()));
        }
    });
    if let Some(text) = &mut name {
        let mut done = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(text)
                    .hint_text("Preset name")
                    .desired_width(140.0),
            );
            if just_opened {
                edit.request_focus();
            }
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let named = !text.trim().is_empty();
            let clicked = ui.add_enabled(named, egui::Button::new("OK")).clicked();
            if (clicked || enter) && named {
                edits.push(save(text.trim().to_string(), None));
                done = true;
            }
            if ui.button("Cancel").clicked() {
                done = true;
            }
        });
        if done {
            name = None;
        }
    }
    match name {
        Some(name) => ui.data_mut(|d| {
            d.insert_temp(naming, name);
        }),
        None => ui.data_mut(|d| d.remove::<String>(naming)),
    }
    ui.separator();
}

/// A plugin node's editor button, and why it didn't open if it didn't.
fn plugin_editor(ui: &mut Ui, session: &Session, id: NodeId, edits: &mut Vec<Edit>) {
    let Some(plugin) = session.plugin_of(id) else {
        return;
    };
    if plugin.descriptor().format == noodle_plugins::PluginFormat::Au {
        ui.label(
            RichText::new(
                "Audio Units have no editor window yet; set them through their parameters.",
            )
            .weak(),
        );
        return;
    }
    let open = session.plugin_editor_open(id);
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
    // The session says why an editor couldn't open; keep it where the
    // button is rather than only in the status line.
    if let Some(why) = session.plugin_editor_error(id) {
        ui.add(egui::Label::new(RichText::new(why).color(ui.visuals().error_fg_color)).wrap());
    }
}

/// Whether the node can be frozen: a node or group with audio out.
fn freezable(session: &Session, id: NodeId) -> bool {
    let node = session.project().graph().node(id);
    node.is_some_and(|node| {
        if node.type_id == noodle_core::group::GROUP {
            return !session.project().graph().group_ports(id).outputs.is_empty();
        }
        session
            .registry()
            .get(&node.type_id)
            .and_then(|t| t.layout(&node.config).ok())
            .is_some_and(|layout| !layout.outputs.is_empty())
    })
}

/// The Freeze / Unfreeze button, for the right of the node's name. A node
/// that isn't frozen and can't be gets none.
fn freeze_button(ui: &mut Ui, session: &Session, id: NodeId, edits: &mut Vec<Edit>) {
    let frozen = session.project().is_frozen(id);
    let badge = session.freeze_badges().get(&id);
    if !frozen && badge.is_none() && !freezable(session, id) {
        return;
    }
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
}

/// How the node's render is doing, under its name. A failure wraps to the
/// panel's width: a long message must never widen the panel.
fn freeze_status(ui: &mut Ui, session: &Session, id: NodeId) {
    use crate::freezing::BadgeState;

    let Some(badge) = session.freeze_badges().get(&id) else {
        return;
    };
    match &badge.state {
        BadgeState::Ready => {
            ui.weak(if badge.frozen { "Frozen" } else { "Rendered" });
        }
        BadgeState::Rendering(fraction) => {
            ui.add(
                egui::ProgressBar::new(*fraction)
                    .desired_width(ui.available_width().min(160.0))
                    .show_percentage(),
            );
        }
        BadgeState::Waiting => {
            ui.weak("Waiting to render");
        }
        BadgeState::Failed(why) => {
            ui.add(egui::Label::new(RichText::new(why).color(ui.visuals().error_fg_color)).wrap());
        }
    }
}

fn problems(ui: &mut Ui, session: &Session, id: NodeId) {
    for diagnostic in session.diagnostics() {
        let here = match &diagnostic.location {
            Location::Node(node) => *node == id,
            Location::Wire(input) => input.node == id,
        };
        if here {
            let color = ui.visuals().error_fg_color;
            ui.add(
                egui::Label::new(RichText::new(diagnostic.problem.to_string()).color(color)).wrap(),
            );
        }
    }
}

/// The Sampler's sample: what is loaded, and a button to load another. The
/// audio goes into the sample store (and is saved in `samples` beside the
/// project), and the node keeps its hash.
fn sample_picker(
    ui: &mut Ui,
    session: &Session,
    id: NodeId,
    info: &noodle_engine::ConfigInfo,
    node: &Node,
    edits: &mut Vec<Edit>,
) {
    use noodle_nodes::{SAMPLE_INPUT_ID, SampleRef};
    let sampler = node.type_id != SAMPLE_INPUT_ID;
    let top = ui.cursor().top();
    let store = session.samples();
    // A Sample node's option can name a window of the sample.
    let reference = SampleRef::parse(&info.get_text(&node.config));
    let hash = reference.hash.clone();
    ui.label(info.name);
    let mut status = if hash.is_empty() {
        "No sample loaded".to_owned()
    } else if let Some(sample) = store.get(&hash) {
        format!(
            "{:.2} s at {} Hz ({})",
            sample.frames() as f32 / sample.sample_rate.max(1) as f32,
            sample.sample_rate,
            &hash[..8.min(hash.len())]
        )
    } else {
        format!(
            "Sample {} is missing from the samples folder",
            &hash[..8.min(hash.len())]
        )
    };
    if !reference.is_whole() {
        status.push_str(&format!(
            ", the part from {:.0}% to {:.0}%",
            reference.start * 100.0,
            reference.end * 100.0
        ));
    }
    ui.label(RichText::new(status).weak());
    crate::sample_view::preview(ui, session, id, 64.0);
    if node.type_id == noodle_nodes::SAMPLER_ID {
        crate::clip_panel::sampler_warp(ui, session, id, edits);
    }
    let error_id = egui::Id::new((id, "sample_error"));
    if let Some(error) = ui.data(|d| d.get_temp::<String>(error_id)) {
        ui.colored_label(crate::theme::MUTE, error);
    }
    if ui.button("Load sample…").clicked()
        && let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "Audio",
                &["wav", "flac", "mp3", "ogg", "aif", "aiff", "m4a"],
            )
            .pick_file()
    {
        // Kept in memory; saving the project writes it beside the file.
        match store.import(&path) {
            Ok(hash) => {
                ui.data_mut(|d| d.remove_temp::<String>(error_id));
                edits.push(Edit::Apply(if sampler {
                    crate::sample_drop::choose(session, id, &hash)
                } else {
                    Command::SetConfig {
                        node: id,
                        key: info.key.to_owned(),
                        value: Some(noodle_core::Value::Text(hash)),
                    }
                }));
            }
            Err(error) => {
                ui.data_mut(|d| d.insert_temp(error_id, error.to_string()));
            }
        }
    }
    // A clip carried here from the timeline sets the sample.
    let area = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top());
    let over = crate::carry::carried(ui.ctx())
        .is_some_and(|(item, at)| matches!(item, crate::carry::Item::Clip(_)) && area.contains(at));
    if over {
        ui.painter().rect_stroke(
            area,
            4.0,
            egui::Stroke::new(2.0, crate::theme::editor::SAMPLE_SOCKET),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(drop) = crate::carry::drop_in(ui.ctx(), area)
        && let crate::carry::Item::Clip(clip) = drop.item
    {
        crate::carry::take_drop(ui.ctx());
        match crate::sample_drop::from_clip(session, clip) {
            Ok(dropped) if sampler => {
                edits.push(Edit::Apply(crate::sample_drop::onto_sampler(id, &dropped)));
            }
            Ok(dropped) => edits.push(Edit::Apply(Command::SetConfig {
                node: id,
                key: info.key.to_owned(),
                value: Some(noodle_core::Value::Text(dropped.reference.to_text())),
            })),
            Err(error) => {
                ui.data_mut(|d| d.insert_temp(error_id, error));
            }
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
            && info.key == noodle_engine::OUTPUT_ROUTING_KEY
        {
            routing_editor(ui, session, id, info, node, edits);
            continue;
        }
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
        if node_type.info().id == noodle_nodes::ENVELOPE_ID
            && info.key == noodle_nodes::ENVELOPE_CURVE_KEY
        {
            let text = info.get_text(&node.config);
            ui.label(info.name);
            let curve = noodle_nodes::EnvelopeCurve::from_text(&text);
            edits.extend(EnvelopeEditor::new(id, info.key, &curve).show(ui));
            ui.label(
                RichText::new(
                    "Left to right is the Length, bottom to top the height. Drag a point to \
                     move it, drag the line between two points to bend it, and click the \
                     line to add a point.",
                )
                .weak()
                .small(),
            );
            if ui.small_button("Reset to the default shape").clicked() {
                edits.push(Edit::Apply(Command::SetConfig {
                    node: id,
                    key: info.key.to_owned(),
                    value: None,
                }));
            }
            continue;
        }
        // Edited in the sample editor and the Warp section above.
        if node_type.info().id == noodle_nodes::SAMPLER_ID
            && matches!(
                info.key,
                noodle_nodes::GAIN_ENV_KEY | noodle_nodes::WARP_KEY
            )
        {
            continue;
        }
        if info.key == noodle_nodes::LAYOUT_KEY
            && let Some(default) = noodle_nodes::default_layout(node_type.info().id)
        {
            layout_picker(ui, id, info, node, default, edits);
            continue;
        }
        if node_type.info().id == noodle_nodes::SCRIPT_ID && info.key == noodle_nodes::SCRIPT_KEY {
            script_editor(ui, id, info, node, edits);
            continue;
        }
        if matches!(
            node_type.info().id,
            noodle_nodes::SAMPLER_ID | noodle_nodes::SAMPLE_INPUT_ID
        ) && info.key == noodle_nodes::SAMPLE_KEY
        {
            sample_picker(ui, session, id, info, node, edits);
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

/// The drop-down for a node's channel layout: one undo step per choice.
/// An Output node's speaker routing: for each channel of the signal (named by
/// the layout chosen here, which only decides how many rows there are), the
/// device channel it plays on. The setting holds the numbers, in order.
fn routing_editor(
    ui: &mut Ui,
    session: &Session,
    id: NodeId,
    info: &noodle_engine::ConfigInfo,
    node: &Node,
    edits: &mut Vec<Edit>,
) {
    use noodle_engine::ChannelLayout;
    let text = info.get_text(&node.config);
    let routes = noodle_engine::parse_routing(&text).unwrap_or_default();
    let device_channels = usize::from(session.audio_config().channels.unwrap_or(2)).max(1);
    let shown = device_channels.max(routes.iter().flatten().map(|d| d + 1).max().unwrap_or(0));
    let layout_id = egui::Id::new((id, "routing layout"));
    let default_layout = ChannelLayout::from_channels(routes.len().max(device_channels))
        .unwrap_or(ChannelLayout::Stereo);
    let mut layout = ui
        .data(|d| d.get_temp::<ChannelLayout>(layout_id))
        .unwrap_or(default_layout);
    ui.label(RichText::new("Speaker routing").strong());
    ui.horizontal(|ui| {
        ui.label("Signal");
        crate::widgets::Dropdown::from_id_salt((id, "routing layout"))
            .selected_text(crate::export_dialog::layout_label(layout))
            .show_ui(ui, |ui| {
                for choice in ChannelLayout::ALL {
                    ui.selectable_value(
                        &mut layout,
                        choice,
                        crate::export_dialog::layout_label(choice),
                    );
                }
            });
    });
    ui.data_mut(|d| d.insert_temp(layout_id, layout));
    // With no routing set, channel n plays on channel n.
    let current = |row: usize| {
        if routes.is_empty() {
            (row < device_channels).then_some(row)
        } else {
            routes.get(row).copied().flatten()
        }
    };
    let mut chosen: Vec<Option<usize>> = (0..layout.channels()).map(current).collect();
    egui::Grid::new((id, "routing grid")).show(ui, |ui| {
        for (row, speaker) in layout.speakers().iter().enumerate() {
            let name = ui.label(speaker.name());
            let label = match chosen[row] {
                Some(channel) => format!("Output {}", channel + 1),
                None => "Off".to_owned(),
            };
            crate::widgets::Dropdown::from_id_salt((id, "route", row))
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut chosen[row], None, "Off");
                    for channel in 0..shown {
                        ui.selectable_value(
                            &mut chosen[row],
                            Some(channel),
                            format!("Output {}", channel + 1),
                        );
                    }
                })
                .response
                .labelled_by(name.id);
            ui.end_row();
        }
    });
    let new_text = chosen
        .iter()
        .map(|c| c.map_or("-".to_owned(), |c| (c + 1).to_string()))
        .collect::<Vec<_>>()
        .join(" ");
    let before: Vec<Option<usize>> = (0..layout.channels()).map(current).collect();
    if chosen != before {
        edits.push(Edit::Apply(Command::SetConfig {
            node: id,
            key: info.key.to_owned(),
            value: Some(noodle_core::Value::Text(new_text)),
        }));
    }
    ui.label(
        RichText::new(
            "Which output channel each channel of the signal plays on. Channels that \
             are off, or past the device's channels, play nowhere, and a routing \
             replaces the usual fold-down of a wide signal onto a narrow device.",
        )
        .weak()
        .small(),
    );
    if !text.is_empty() && ui.small_button("Back to channel n on channel n").clicked() {
        edits.push(Edit::Apply(Command::SetConfig {
            node: id,
            key: info.key.to_owned(),
            value: None,
        }));
    }
}

fn layout_picker(
    ui: &mut Ui,
    id: NodeId,
    info: &noodle_engine::ConfigInfo,
    node: &Node,
    default: noodle_engine::ChannelLayout,
    edits: &mut Vec<Edit>,
) {
    use noodle_engine::ChannelLayout;
    let current = ChannelLayout::from_name(&info.get_text(&node.config)).unwrap_or(default);
    let mut chosen = current;
    ui.horizontal(|ui| {
        let label = ui.label(info.name);
        crate::widgets::Dropdown::from_id_salt((id, info.key))
            .selected_text(crate::export_dialog::layout_label(current))
            .show_ui(ui, |ui| {
                for layout in ChannelLayout::ALL {
                    ui.selectable_value(
                        &mut chosen,
                        layout,
                        crate::export_dialog::layout_label(layout),
                    );
                }
            })
            .response
            .labelled_by(label.id)
            .on_hover_text("Changing this rebuilds the node, and changes its output's channels.");
    });
    if chosen != current {
        edits.push(Edit::Apply(Command::SetConfig {
            node: id,
            key: info.key.to_owned(),
            value: Some(noodle_core::Value::Text(chosen.name().to_owned())),
        }));
    }
}

/// Script text typed into a node that is no longer the active one (the user
/// clicked another node while the box had focus, so the box never saw the focus
/// leave): committed now, so it isn't lost.
fn pending_scripts(ui: &Ui, session: &Session, active: Option<NodeId>) -> Vec<Edit> {
    let mut edits = Vec::new();
    for (id, node) in session.project().graph().nodes() {
        if Some(id) == active || node.type_id != noodle_nodes::SCRIPT_ID {
            continue;
        }
        let pending_id = egui::Id::new((id, "script_pending"));
        let Some(text) = ui.data_mut(|d| d.remove_temp::<String>(pending_id)) else {
            continue;
        };
        let committed = match node.config.get(noodle_nodes::SCRIPT_KEY) {
            Some(noodle_core::Value::Text(t)) if !t.trim().is_empty() => t.as_str(),
            _ => noodle_nodes::DEFAULT_SCRIPT,
        };
        if text != committed {
            edits.push(Edit::Apply(Command::SetConfig {
                node: id,
                key: noodle_nodes::SCRIPT_KEY.to_owned(),
                value: Some(noodle_core::Value::Text(text)),
            }));
        }
    }
    edits
}

/// The Script node's text: a code box that commits once it loses focus (the
/// node is rebuilt then, and may get other ports), with the ports the text
/// makes or what is wrong with it shown as it is typed.
fn script_editor(
    ui: &mut Ui,
    id: NodeId,
    info: &noodle_engine::ConfigInfo,
    node: &Node,
    edits: &mut Vec<Edit>,
) {
    let committed = match info.get_text(&node.config) {
        text if text.trim().is_empty() => noodle_nodes::DEFAULT_SCRIPT.to_owned(),
        text => text,
    };
    let pending_id = egui::Id::new((id, "script_pending"));
    let mut text: String = ui
        .data(|d| d.get_temp(pending_id))
        .unwrap_or_else(|| committed.clone());
    ui.label(info.name);
    let response = ui.add(
        egui::TextEdit::multiline(&mut text)
            .id_salt((id, "script_text"))
            .code_editor()
            .desired_rows(10)
            .desired_width(f32::INFINITY),
    );
    if response.has_focus() {
        ui.data_mut(|d| d.insert_temp(pending_id, text.clone()));
    } else {
        ui.data_mut(|d| d.remove::<String>(pending_id));
        if response.lost_focus() && text != committed {
            edits.push(Edit::Apply(Command::SetConfig {
                node: id,
                key: info.key.to_owned(),
                value: Some(noodle_core::Value::Text(text.clone())),
            }));
        }
    }
    match noodle_nodes::check_script(&text) {
        Ok(ports) => {
            let mut summary = format!(
                "Inputs: {}. Outputs: {}.",
                ports.inputs.join(", "),
                ports.outputs.join(", ")
            );
            if !ports.event_inputs.is_empty() || !ports.event_outputs.is_empty() {
                summary += &format!(
                    " Events in: {}. Events out: {}.",
                    ports.event_inputs.join(", "),
                    ports.event_outputs.join(", ")
                );
            }
            ui.label(RichText::new(summary).weak().small());
        }
        Err(error) => {
            ui.colored_label(crate::theme::MUTE, error);
        }
    }
    egui::CollapsingHeader::new("Script reference")
        .id_salt(("script-help", id))
        .default_open(false)
        .show(ui, |ui| {
            ui.label(RichText::new(SCRIPT_HELP).small().monospace());
        });
    if ui.small_button("Reset to a pass-through").clicked() {
        edits.push(Edit::Apply(Command::SetConfig {
            node: id,
            key: info.key.to_owned(),
            value: None,
        }));
    }
}

const SCRIPT_HELP: &str = "\
input x            audio input
param p = 1 [0, 4] log hz   parameter
output y           output
state z = 0        keeps its value
let k = x * 2      this sample only
y = k + z          (also += -= *= /=)
if c { } else { }  comparisons give 1/0
array a[64] = 0    memory; a[i] wraps round
for i in 0 to 8 { } fixed length; len(a)
events in n        n_on n_off n_key n_velocity
events out m       note_on(m, key, vel)
                   note_off(m, key)
sr bpm beat time pi tau e
(time is coarse late on: keep phase in state)
rand()  select(c,a,b)  db()  mtof()
sin cos tan tanh abs sqrt exp ln
log2 log10 floor ceil round fract
sign min max pow atan2 clamp mix
// comment; lines or ; end statements";

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
    // A wire or automation lane into a parameter keeps its port: hiding it
    // would leave them pointing at nothing, which fails the compile.
    let wired = |param: u32| {
        let target = Endpoint::new(id, noodle_plugins::param_key(param));
        session.project().graph().source(&target).is_some()
            || session
                .project()
                .lanes()
                .any(|(_, lane)| lane.target == target && !lane.points.is_empty())
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
        // Changing the setting builds the plugin again from its saved state,
        // so what the open editor changed is kept first, in the same undo
        // step. A parameter that leaves the node forgets its stored value, so
        // that showing it again doesn't send a stale one.
        let set = |ids: Vec<u32>| {
            let mut commands = Vec::new();
            if session.plugin_editor_open(id)
                && let Ok(captured) = noodle_plugins::capture(id)
            {
                commands.push(captured.commands(id, descriptor, &node.config));
            }
            commands.push(Command::SetConfig {
                node: id,
                key: noodle_plugins::PARAMS_KEY.to_owned(),
                value: Some(noodle_core::Value::Text(params_text(&ids))),
            });
            for hidden in chosen.iter().filter(|c| !ids.contains(c)) {
                let key = noodle_plugins::param_key(*hidden);
                if node.params.contains_key(&key) {
                    commands.push(Command::SetParam {
                        node: id,
                        key,
                        value: None,
                    });
                }
            }
            Edit::Apply(Command::Batch(commands))
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
    match node_type.view(&node.config) {
        NodeView::Graph(kind) => graph(ui, session, kind.into(), id, node, &params, live, edits),
        NodeView::Panner => panner(ui, session, id, node, &params, live, edits),
        _ => {}
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
        let shown_value = reading.map_or(value, |reading| reading.value);
        let lane = session
            .project()
            .lane_for(&Endpoint::new(id, key))
            .is_some();
        let shown = layout.param_shown(&node.param_shown, key);
        ui.horizontal(|ui| {
            if let Some(edit) = shown_dot(ui, id, key, shown, source.is_some() || lane) {
                edits.push(edit);
            }
            ui.add_enabled_ui(source.is_none(), |ui| {
                let out = ParamField::new(&input.name, info, shown_value)
                    .id_salt((id, key))
                    .width(ui.available_width())
                    .show(ui);
                if let Some(source) = &source {
                    out.response
                        .on_disabled_hover_text(format!("Driven by {}", describe(session, source)));
                } else {
                    edits.extend(out.edits(id, key));
                }
            });
        });
        ui.label(RichText::new(range_text(info, reading)).weak().small());
    }
}

/// The dot beside a parameter: filled while the parameter is drawn on the
/// node in the graph. Clicking it shows or hides the parameter there, which
/// keeps a node with many controls a reasonable size without taking any away.
/// A parameter with a wire or a lane stays on the node, so its dot is fixed.
fn shown_dot(ui: &mut Ui, node: NodeId, key: &str, shown: bool, fixed: bool) -> Option<Edit> {
    let size = egui::vec2(16.0, ui.spacing().interact_size.y);
    let sense = if fixed {
        egui::Sense::hover()
    } else {
        egui::Sense::click()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let on = shown || fixed;
    let colour = if on {
        crate::theme::ACCENT
    } else {
        ui.visuals().widgets.noninteractive.fg_stroke.color
    };
    let centre = rect.center();
    let radius = if response.hovered() && !fixed {
        5.5
    } else {
        4.5
    };
    if on {
        ui.painter().circle_filled(centre, radius, colour);
    } else {
        ui.painter()
            .circle_stroke(centre, radius, egui::Stroke::new(1.5, colour));
    }
    let tip = if fixed {
        "Shown on the node: a wire or an automation lane goes into it"
    } else if on {
        "Shown on the node. Click to hide it there"
    } else {
        "Hidden on the node. Click to show it there"
    };
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, !fixed, on, "Show on node")
    });
    let response = response.on_hover_text(tip);
    (response.clicked()).then(|| {
        Edit::Apply(Command::SetParamShown {
            node,
            key: key.to_owned(),
            shown: Some(!on),
        })
    })
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
        let default = info_of(key).map_or(0.0, |info| info.default);
        crate::widgets::graph::graph_value(
            key,
            node,
            wired(key),
            &|channel| live(id, channel).map(|live| live.value),
            default,
        )
    };
    let range = |key: &str| info_of(key).map_or((0.0, 1.0), |info| (info.min, info.max));
    let editable = |key: &str| !wired(key);
    edits.extend(GraphEditor::new(id, kind, &values, &range, &editable).show(ui));
}

/// The Surround Pan node's panner: the speakers around the listener and the
/// source, which can be dragged.
fn panner(
    ui: &mut Ui,
    session: &Session,
    id: NodeId,
    node: &Node,
    params: &[(&noodle_engine::InputPort, &noodle_engine::ParamInfo)],
    live: LiveParams<'_>,
    edits: &mut Vec<Edit>,
) {
    use noodle_engine::ChannelLayout;
    let wired = |key: &str| {
        session
            .project()
            .graph()
            .source(&Endpoint::new(id, key))
            .is_some()
    };
    let value = |key: &str| {
        let default = params
            .iter()
            .find(|(input, _)| input.key == key)
            .map_or(0.0, |p| p.1.default);
        let own = node.params.get(key).copied().unwrap_or(default);
        if wired(key) {
            live(id, key).map_or(own, |live| live.value)
        } else {
            own
        }
    };
    let range = |key: &str| {
        params
            .iter()
            .find(|(input, _)| input.key == key)
            .map_or((0.0, 1.0), |p| (p.1.min, p.1.max))
    };
    let default = noodle_nodes::default_layout(&node.type_id).unwrap_or(ChannelLayout::Stereo);
    let layout = node
        .config
        .get(noodle_nodes::LAYOUT_KEY)
        .and_then(|v| match v {
            noodle_core::Value::Text(name) => ChannelLayout::from_name(name),
            _ => None,
        })
        .unwrap_or(default);
    edits.extend(
        crate::widgets::panner::PannerEditor::new(
            id,
            layout,
            value("azimuth"),
            value("spread"),
            (!wired("azimuth"), !wired("spread")),
        )
        .ranges(range("azimuth"), range("spread"))
        .show(ui),
    );
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
    use crate::session::LibraryEdit;
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
        crate::testkit::harness(state, vec2(300.0, 400.0), |ui, state: &mut State| {
            let edits = show(ui, &state.session, state.active, &|_, _| None, &|_| None);
            state.session.edit(edits);
        })
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
    fn dragging_the_dynamics_points_sets_threshold_and_attack() {
        let mut harness = harness([Node::new("noodle.effect.dynamics")]);
        harness.set_size(vec2(300.0, 700.0));
        harness.run();
        let outer = harness.get_by_label("Dynamics graph").rect();
        let plot = outer.shrink2(egui::vec2(2.0, 8.0));
        let at = |x: f32, y: f32| {
            egui::pos2(
                plot.left() + x * plot.width(),
                plot.bottom() - y * plot.height(),
            )
        };
        // The threshold, -18 dB, sits 0.5625 dB below the line at the soft
        // knee, on a plot of -60 to 0 dB across 46% of the width.
        let y = (60.0 - 18.5625) / 72.0;
        drag(&mut harness, at(0.46 * 42.0 / 60.0, y), at(0.46 * 0.5, y));
        let threshold = param(&harness, "threshold").unwrap();
        assert!((threshold + 30.0).abs() < 1.0, "{threshold}");

        // The attack point: 10 ms in, over a span of (0.04 + 0.4 + 0.01) · 1.5.
        let span = 0.45 * 1.5;
        // 63% of the way to the reduction at 0 dB (22.5 dB over the -30 dB
        // threshold at 4:1), on a scale of 1.5 times the most it could be (30).
        let height = 1.0 - 22.5 * 0.632 / 45.0;
        let from = at(0.54 + 0.46 * 0.01 / span, height);
        drag(&mut harness, from, at(0.54 + 0.46 * 0.05 / span, 0.5));
        let attack = param(&harness, "attack").unwrap();
        assert!((attack - 0.05).abs() < 0.01, "{attack}");
        // Each drag is its own undo step.
        harness.state_mut().session.undo();
        assert_eq!(param(&harness, "attack"), None);
        assert!(param(&harness, "threshold").is_some());
    }

    #[test]
    fn dragging_an_eq_band_sets_its_frequency_and_gain() {
        let mut harness = harness([Node::new("noodle.effect.eq")]);
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
        // Band 1 is at 100 Hz, 0 dB: halfway up.
        let x = (100.0f32 / 20.0).ln() / 1_000.0f32.ln();
        drag(&mut harness, at(x, 0.5), at(x + 0.2, 0.75));
        assert!(param(&harness, "freq1").unwrap() > 300.0);
        let gain = param(&harness, "gain1").unwrap();
        assert!((gain - 12.0).abs() < 1.5, "{gain}");
    }

    #[test]
    fn the_freeze_button_is_in_the_name_row_at_the_right() {
        let harness = harness([Node::new("noodle.offline.reverse")]);
        let name = harness.get_by_label("Reverse").rect();
        let freeze = harness.get_by_label("Freeze").rect();
        assert!(
            (name.center().y - freeze.center().y).abs() < 4.0,
            "{name:?} {freeze:?}"
        );
        assert!(freeze.left() > name.right(), "{name:?} {freeze:?}");
        assert!(freeze.right() > 250.0, "at the right edge: {freeze:?}");
        // And the type's ID is no longer printed under it.
        assert!(harness.query_by_label("noodle.offline.reverse").is_none());
    }

    #[test]
    fn a_long_freeze_error_wraps_instead_of_widening_the_panel() {
        let mut harness = harness([
            Node::new("noodle.offline.reverse"),
            Node::new(noodle_engine::INPUT_ID),
        ]);
        harness
            .state_mut()
            .session
            .edit([Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(NodeId(2), "out"),
                to: Endpoint::new(NodeId(1), "in"),
            }))]);
        harness.run();
        let messages: Vec<_> = harness
            .get_all_by_label_contains("same every time")
            .map(|m| m.rect())
            .collect();
        assert!(!messages.is_empty());
        for why in messages {
            assert!(why.right() <= 300.0, "{why:?}");
        }
        // The controls below it are still on screen.
        let freeze = harness.get_by_label("Freeze").rect();
        assert!(freeze.right() <= 300.0, "{freeze:?}");
    }

    #[test]
    fn a_dot_beside_each_parameter_shows_or_hides_it_on_the_node() {
        let mut harness = harness([Node::new("noodle.effect.chorus")]);
        harness.set_size(vec2(300.0, 900.0));
        harness.run();
        let dots: Vec<_> = harness.get_all_by_label("Show on node").collect();
        assert_eq!(dots.len(), 5, "one for each parameter");
        // The third is Delay, which a node with so many ports leaves off.
        dots[2].click();
        harness.run();
        let shown = harness
            .state()
            .session
            .project()
            .graph()
            .node(NodeId(1))
            .unwrap()
            .param_shown
            .clone();
        assert_eq!(shown.get("delay"), Some(&true));
        // The first is Rate, which it shows; clicking turns it off.
        harness
            .get_all_by_label("Show on node")
            .next()
            .unwrap()
            .click();
        harness.run();
        let shown = harness
            .state()
            .session
            .project()
            .graph()
            .node(NodeId(1))
            .unwrap()
            .param_shown
            .clone();
        assert_eq!(shown.get("rate"), Some(&false));
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
    fn a_spectrum_node_shows_its_views_and_says_when_the_engine_is_off() {
        let harness = harness([Node::new(noodle_nodes::SPECTRUM_ID)]);
        harness.get_by_label("Spectrum");
        harness.get_by_label_contains("nothing to measure");
        harness.get_by_label_contains("20 kHz");
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

    fn script_of(harness: &Harness<'_, State>) -> Option<String> {
        let graph = harness.state().session.project().graph();
        match graph.node(NodeId(1))?.config.get("script") {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        }
    }

    #[test]
    fn script_typed_into_a_node_is_kept_when_another_node_is_selected() {
        let mut harness = harness([
            Node::new("noodle.util.script"),
            Node::new("noodle.osc.sine"),
        ]);
        harness.set_size(vec2(300.0, 700.0));
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .click();
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.run();
        harness.event(egui::Event::Text("output o\no = 0.5".into()));
        harness.run();
        assert_eq!(script_of(&harness), None, "still being typed");
        harness.state_mut().active = Some(NodeId(2));
        harness.run();
        harness.run();
        assert_eq!(script_of(&harness).as_deref(), Some("output o\no = 0.5"));
    }

    #[test]
    fn the_script_is_edited_as_text_and_commits_when_the_box_loses_focus() {
        let mut harness = harness([Node::new("noodle.util.script")]);
        harness.set_size(vec2(300.0, 700.0));
        harness.run();
        harness.get_by_label_contains("Inputs: in. Outputs: out.");

        // Typing alone changes nothing yet, but the ports line follows.
        harness
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .click();
        harness.run();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.run();
        harness.event(egui::Event::Text(
            "input x\nparam amount = 2 [0, 4]\noutput a\noutput b\na = x * amount\nb = -a".into(),
        ));
        harness.run();
        harness.get_by_label_contains("Inputs: x, amount. Outputs: a, b.");
        assert_eq!(script_of(&harness), None, "still being typed");

        // Leaving the box commits it, and the node gets the new ports.
        harness.key_press(Key::Escape);
        harness.run();
        harness.run();
        let text = script_of(&harness).expect("committed");
        assert!(text.starts_with("input x\nparam amount"), "{text}");
        let layout = harness
            .state()
            .session
            .registry()
            .get("noodle.util.script")
            .unwrap()
            .layout(
                &harness
                    .state()
                    .session
                    .project()
                    .graph()
                    .node(NodeId(1))
                    .unwrap()
                    .config,
            )
            .unwrap();
        assert_eq!(layout.outputs.len(), 2);
        harness.get_by_label("Amount");

        // A mistake is shown, and the button puts the pass-through back.
        harness
            .state_mut()
            .session
            .edit([Edit::Apply(Command::SetConfig {
                node: NodeId(1),
                key: "script".to_owned(),
                value: Some(Value::Text("output o\no = nope".into())),
            })]);
        harness.run();
        assert!(
            harness
                .query_all_by_label_contains("unknown name `nope`")
                .count()
                >= 1
        );
        harness.get_by_label("Reset to a pass-through").click();
        harness.run();
        harness.run();
        assert_eq!(script_of(&harness), None);
        harness.get_by_label_contains("Inputs: in. Outputs: out.");
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

    fn envelope_of(harness: &Harness<'_, State>) -> noodle_nodes::EnvelopeCurve {
        let graph = harness.state().session.project().graph();
        let config = &graph.node(NodeId(1)).unwrap().config;
        match config.get("curve") {
            Some(Value::Text(text)) => noodle_nodes::EnvelopeCurve::from_text(text),
            _ => noodle_nodes::EnvelopeCurve::default(),
        }
    }

    #[test]
    fn the_envelope_curve_is_drawn_with_the_mouse() {
        let mut harness = harness([Node::new("noodle.mod.envelope")]);
        harness.set_size(vec2(300.0, 700.0));
        harness.run();
        // The editor is `width` wide and 120 high, under the "Curve" label.
        let label = harness.get_by_label("Curve").rect();
        let width = 320.0f32.min(300.0 - 2.0 * label.min.x);
        let at = |x: f32, y: f32| {
            egui::pos2(
                label.min.x + x * width,
                label.max.y + 4.0 + 120.0 * (1.0 - y),
            )
        };
        let before = envelope_of(&harness);
        assert_eq!(before.points().len(), 3);

        // Drag the peak to the right and down: one undo step.
        let peak = before.points()[1];
        let from = at(peak.x, peak.y);
        harness.hover_at(from);
        harness.run();
        harness.drag_at(from);
        harness.run();
        harness.hover_at(at(0.4, 0.6));
        harness.run();
        harness.drop_at(at(0.4, 0.6));
        harness.run();
        let moved = envelope_of(&harness).points()[1];
        assert!(
            (moved.x - 0.4).abs() < 0.05 && (moved.y - 0.6).abs() < 0.05,
            "{moved:?}"
        );
        harness.state_mut().session.undo();
        harness.run();
        assert_eq!(envelope_of(&harness), before);

        // Drag the falling stretch upward: it bends instead of moving.
        let x = 0.7;
        let on_line = at(x, before.eval(x));
        harness.hover_at(on_line);
        harness.run();
        harness.drag_at(on_line);
        harness.run();
        harness.hover_at(at(x, 0.9));
        harness.run();
        harness.drop_at(at(x, 0.9));
        harness.run();
        let bent = envelope_of(&harness);
        assert_eq!(bent.points().len(), 3);
        assert_ne!(bent.points()[1].bend, before.points()[1].bend);

        // The button puts the default shape back.
        harness.get_by_label("Reset to the default shape").click();
        harness.run();
        harness.run();
        assert_eq!(
            envelope_of(&harness),
            noodle_nodes::EnvelopeCurve::default()
        );
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
    fn the_layout_of_a_surround_node_is_chosen_from_a_list() {
        let mut harness = harness([
            Node::new("noodle.util.surround_pan"),
            Node::new("noodle.util.convert_layout"),
        ]);
        let layout = |harness: &Harness<'_, State>, node| {
            let graph = harness.state().session.project().graph();
            graph
                .node(NodeId(node))
                .unwrap()
                .config
                .get("layout")
                .cloned()
        };
        // An unset layout shows the node's default.
        assert_eq!(
            harness.get_by_label("Layout").value().unwrap_or_default(),
            "5.1 (6 channels)"
        );
        harness.get_by_label("Layout").click();
        harness.run();
        harness.get_by_label("7.1 (8 channels)").click();
        harness.run();
        assert_eq!(layout(&harness, 1), Some(Value::Text("7.1".to_owned())));

        // The next node's default is its own.
        harness.state_mut().active = Some(NodeId(2));
        harness.run_steps(2);
        harness.get_by_label("Layout").click();
        harness.run();
        harness.get_by_label("Quad (4 channels)").click();
        harness.run();
        assert_eq!(layout(&harness, 2), Some(Value::Text("Quad".to_owned())));
    }

    #[test]
    fn dragging_the_surround_panner_sets_the_azimuth_and_spread() {
        let mut harness = harness([Node::new("noodle.util.surround_pan")]);
        let rect = harness.get_by_label("Surround panner").rect();
        let value = |harness: &Harness<'_, State>, key| param(harness, key).unwrap_or(0.0);
        // Out to the right on the ring's edge: 90 degrees, no spread.
        harness.drag_at(rect.center());
        harness.run();
        let radius = (rect.width().min(rect.height()) * 0.5 - 8.0).max(4.0);
        let right = rect.center() + vec2(radius, 0.0);
        harness.hover_at(right);
        harness.run();
        harness.drop_at(right);
        harness.run();
        let azimuth = value(&harness, "azimuth");
        assert!((azimuth - 90.0).abs() < 2.0, "{azimuth}");
        assert!(value(&harness, "spread") < 0.05);
        // It was one undo step.
        harness.state_mut().session.undo();
        assert_eq!(param(&harness, "azimuth"), None);
    }

    #[test]
    fn an_outputs_speakers_are_routed_from_a_list() {
        let mut harness = harness([Node::new(noodle_engine::OUTPUT_ID)]);
        let routing = |harness: &Harness<'_, State>| {
            let graph = harness.state().session.project().graph();
            graph
                .node(NodeId(1))
                .unwrap()
                .config
                .get("routing")
                .cloned()
        };
        assert_eq!(routing(&harness), None);
        // Stereo by default: the left channel is switched off.
        harness.get_by_label("L").click();
        harness.run();
        harness.get_by_label("Off").click();
        harness.run();
        assert_eq!(routing(&harness), Some(Value::Text("- 2".to_owned())));
        harness
            .get_by_label("Back to channel n on channel n")
            .click();
        harness.run();
        assert_eq!(routing(&harness), None);
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

    fn saved(session: &mut Session, node: NodeId, name: &str) {
        session.edit([Edit::Library(LibraryEdit::NodePreset {
            node,
            name: name.into(),
            overwrite: None,
        })]);
    }

    fn set_cutoff(h: &mut Harness<'_, State>, value: f32) {
        h.state_mut().session.edit([Edit::Apply(Command::SetParam {
            node: NodeId(1),
            key: "cutoff".into(),
            value: Some(value),
        })]);
    }

    #[test]
    fn a_saved_node_preset_is_applied_from_the_presets_control_in_one_undo_step() {
        let mut h = harness([Node::new("noodle.filter.svf").with_param("cutoff", 900.0)]);
        saved(&mut h.state_mut().session, NodeId(1), "Mellow");
        set_cutoff(&mut h, 4000.0);
        h.run();
        h.get_by_role(egui::accesskit::Role::ComboBox).click();
        h.run();
        h.get_by_label("User");
        h.get_by_label("Mellow").click();
        h.run();
        assert_eq!(param(&h, "cutoff"), Some(900.0));
        let node = h.state().session.project().graph().node(NodeId(1)).unwrap();
        assert_eq!(node.preset.as_deref(), Some("user:node:mellow"));
        let shown = |h: &Harness<'_, State>| {
            let combo = h.get_by_role(egui::accesskit::Role::ComboBox);
            let node = combo.accesskit_node();
            node.value().unwrap_or_default()
        };
        assert_eq!(shown(&h), "Mellow");
        // Edited since: it says so.
        set_cutoff(&mut h, 1000.0);
        h.run();
        assert_eq!(shown(&h), "Mellow (modified)");
        // One undo step undid the apply, not part of it.
        h.state_mut().session.undo();
        assert_eq!(param(&h, "cutoff"), Some(900.0));
        h.state_mut().session.undo();
        assert_eq!(param(&h, "cutoff"), Some(4000.0));
    }

    #[test]
    fn save_as_names_a_new_user_preset_and_save_overwrites_the_one_applied() {
        let mut h = harness([Node::new("noodle.filter.svf").with_param("cutoff", 900.0)]);
        h.run();
        h.get_by_label("Save as...").click();
        h.run();
        h.event(egui::Event::Text("Dark".into()));
        h.run();
        h.get_by_label("OK").click();
        h.run();
        let session = &mut h.state_mut().session;
        let preset = session
            .library()
            .node_preset("user:node:dark")
            .expect("saved");
        assert_eq!(preset.values.params["cutoff"], 900.0);
        // Apply it, change the cutoff, and Save overwrites it.
        let apply = session.apply_preset(NodeId(1), "user:node:dark").unwrap();
        session.edit([apply]);
        set_cutoff(&mut h, 2500.0);
        h.run();
        h.get_by_label("Save").click();
        h.run();
        let preset = h
            .state()
            .session
            .library()
            .node_preset("user:node:dark")
            .unwrap();
        assert_eq!(preset.values.params["cutoff"], 2500.0);
        assert_eq!(
            h.state()
                .session
                .library()
                .node_presets()
                .filter(|(id, _)| id.starts_with("user"))
                .count(),
            1,
            "overwritten, not added"
        );
    }

    #[test]
    fn a_group_made_from_a_template_offers_its_system_presets() {
        let mut session = Session::new(crate::session::Nodes::all());
        let (group, command) = session
            .library()
            .template("system:template:kick")
            .unwrap()
            .instantiate(
                "system:template:kick",
                None,
                noodle_core::Position::default(),
                || session.new_node_id(),
            );
        session.edit([Edit::Apply(command)]);
        let state = State {
            session,
            active: Some(group),
        };
        let mut h = crate::testkit::harness(state, vec2(300.0, 400.0), |ui, state: &mut State| {
            let edits = show(ui, &state.session, state.active, &|_, _| None, &|_| None);
            state.session.edit(edits);
        });
        h.run();
        h.get_by_role(egui::accesskit::Role::ComboBox).click();
        h.run();
        h.get_by_label("System");
        h.get_by_label("Deep Kick").click();
        h.run();
        let session = &h.state().session;
        let node = session.project().graph().node(group).unwrap();
        assert_eq!(node.preset.as_deref(), Some("system:group:kick-deep-kick"));
        assert!(!session.preset_status(group).unwrap().modified);
        // A control's value inside changed: the decay of 0.5 seconds.
        let graph = session.project().graph();
        assert!(
            graph
                .descendants(group)
                .into_iter()
                .any(|id| { graph.node(id).unwrap().params.get("decay") == Some(&0.5) })
        );
    }

    #[test]
    fn nothing_fits_a_node_no_preset_was_made_for() {
        let mut h = harness([Node::new("noodle.noise.white")]);
        h.get_by_role(egui::accesskit::Role::ComboBox).click();
        h.run();
        h.get_by_label("None fit yet");
    }
}
