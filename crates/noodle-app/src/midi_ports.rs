//! The MIDI ports view: which port each MIDI In, MIDI Control and MIDI Out
//! node uses, changed from one place instead of node by node.
//!
//! Like the outputs view, it keeps no project state. A node's port is its
//! `port` setting, so a choice here is a `SetConfig`, one undo step.
//! Listing ports talks to the system, so it happens when the view opens and
//! on Refresh, never every frame. Unlike an audio device a MIDI port can be
//! shared, so a port another node uses is named but stays choosable.

use crate::widgets::Dropdown;
use egui::{RichText, Ui};
use noodle_core::{Command, Config, Node, NodeId, Project, Value};
use noodle_io::{MidiError, same_port};
use noodle_nodes::{MIDI_CONTROL_ID, MIDI_IN_ID, MIDI_OUT_ID, MIDI_OUT_PORT_KEY, MIDI_PORT_KEY};

use crate::session::Edit;

/// What a row's node does with its port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// MIDI In or MIDI Control: listens to an input port.
    Input,
    /// MIDI Out: sends to an output port.
    Output,
}

/// One MIDI node and its port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub node: NodeId,
    pub kind: Kind,
    /// The node's type name, for the row's label.
    pub name: &'static str,
    /// The saved port name; empty for every port (input) or none (output).
    pub port: String,
}

fn port_key(kind: Kind) -> &'static str {
    match kind {
        Kind::Input => MIDI_PORT_KEY,
        Kind::Output => MIDI_OUT_PORT_KEY,
    }
}

/// Every MIDI In, MIDI Control and MIDI Out node in the project, in ID
/// order.
pub fn rows(project: &Project) -> Vec<Row> {
    project
        .graph()
        .nodes()
        .filter_map(|(node, n)| {
            let (kind, name) = match n.type_id.as_str() {
                MIDI_IN_ID => (Kind::Input, "MIDI In"),
                MIDI_CONTROL_ID => (Kind::Input, "MIDI Control"),
                MIDI_OUT_ID => (Kind::Output, "MIDI Out"),
                _ => return None,
            };
            let port = match n.config.get(port_key(kind)) {
                Some(Value::Text(text)) => text.clone(),
                _ => String::new(),
            };
            Some(Row {
                node,
                kind,
                name,
                port,
            })
        })
        .collect()
}

/// The edit that sets `node`'s port; an empty name clears it.
pub fn assign(row: &Row, port: &str) -> Edit {
    Edit::Apply(Command::SetConfig {
        node: row.node,
        key: port_key(row.kind).to_owned(),
        value: (!port.is_empty()).then(|| Value::Text(port.to_owned())),
    })
}

/// The edit that adds a MIDI Out node on `port`.
pub fn add_out(id: NodeId, port: &str) -> Edit {
    let mut config = Config::new();
    if !port.is_empty() {
        config.set(MIDI_OUT_PORT_KEY, Value::Text(port.to_owned()));
    }
    Edit::Apply(Command::AddNode {
        id,
        node: Node::new(MIDI_OUT_ID).with_config(config),
    })
}

type Lister = fn() -> Result<Vec<String>, MidiError>;

/// The view's state: the ports it last listed.
pub struct MidiPortsView {
    inputs: Option<Result<Vec<String>, String>>,
    outputs: Option<Result<Vec<String>, String>>,
    list_inputs: Lister,
    list_outputs: Lister,
}

#[cfg(not(test))]
fn system_listers() -> (Lister, Lister) {
    (noodle_io::midi_inputs, noodle_io::midi_outputs)
}

/// Tests (and the story routes) see two ports of each kind, whatever the
/// machine has.
#[cfg(test)]
fn system_listers() -> (Lister, Lister) {
    (
        || Ok(vec!["Test Keys".into(), "Test Pads".into()]),
        || Ok(vec!["Test Synth".into(), "Test Drum Machine".into()]),
    )
}

impl Default for MidiPortsView {
    fn default() -> Self {
        let (inputs, outputs) = system_listers();
        Self::with_listers(inputs, outputs)
    }
}

/// The ports the system lists, kept in the UI context so a node's panel can
/// offer them without asking the system every frame.
#[derive(Clone, Default)]
struct PortCache {
    inputs: Vec<String>,
    outputs: Vec<String>,
    error: Option<String>,
    listed: bool,
}

fn port_cache_id() -> egui::Id {
    egui::Id::new("midi port cache")
}

fn refresh_cache(ctx: &egui::Context) -> PortCache {
    let (list_inputs, list_outputs) = system_listers();
    let mut cache = PortCache {
        listed: true,
        ..Default::default()
    };
    match list_inputs() {
        Ok(ports) => cache.inputs = ports,
        Err(error) => cache.error = Some(error.to_string()),
    }
    match list_outputs() {
        Ok(ports) => cache.outputs = ports,
        Err(error) => cache.error = Some(error.to_string()),
    }
    ctx.data_mut(|d| d.insert_temp(port_cache_id(), cache.clone()));
    cache
}

/// The port drop-down on a MIDI node's own panel, so the choice is made
/// where the node is. It returns the new port name (empty for "every port"
/// or "none") when one is picked. The listing is taken when the panel first
/// shows it and again on Refresh.
pub fn port_picker(ui: &mut Ui, id: NodeId, kind: Kind, current: &str) -> Option<String> {
    let cache = ui
        .ctx()
        .data(|d| d.get_temp::<PortCache>(port_cache_id()))
        .filter(|c| c.listed)
        .unwrap_or_else(|| refresh_cache(ui.ctx()));
    let ports = match kind {
        Kind::Input => &cache.inputs,
        Kind::Output => &cache.outputs,
    };
    let any = match kind {
        Kind::Input => "Every port",
        Kind::Output => "None",
    };
    let unset = current.is_empty() || current == "*";
    let connected = ports.iter().any(|p| same_port(p, current));
    let selected = if unset {
        any.to_owned()
    } else if connected {
        current.to_owned()
    } else {
        format!("{current} (not connected)")
    };
    let mut chosen = None;
    ui.horizontal(|ui| {
        let name = ui.label("Port");
        let combo = Dropdown::from_id_salt(("node midi port", id))
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if ui.selectable_label(unset, any).clicked() {
                    chosen = Some(String::new());
                }
                for port in ports {
                    if ui
                        .selectable_label(same_port(current, port), port)
                        .clicked()
                    {
                        chosen = Some(port.clone());
                    }
                }
                if !unset && !connected {
                    let _ = ui.selectable_label(true, format!("{current} (not connected)"));
                }
            });
        combo.response.labelled_by(name.id);
        if ui
            .small_button("Refresh")
            .on_hover_text("List the ports again, e.g. after plugging a device in")
            .clicked()
        {
            refresh_cache(ui.ctx());
        }
    });
    if ports.is_empty() {
        ui.weak(match &cache.error {
            Some(error) => format!("MIDI isn't available here: {error}"),
            None => "No MIDI ports found. Plug a device in and press Refresh.".to_owned(),
        });
    }
    chosen.filter(|port| port != current)
}

impl MidiPortsView {
    /// A view that lists ports with the given functions, so tests can supply
    /// their own.
    pub(crate) fn with_listers(list_inputs: Lister, list_outputs: Lister) -> Self {
        Self {
            inputs: None,
            outputs: None,
            list_inputs,
            list_outputs,
        }
    }

    /// Lists the ports again.
    pub fn refresh(&mut self) {
        self.inputs = Some((self.list_inputs)().map_err(|e| e.to_string()));
        self.outputs = Some((self.list_outputs)().map_err(|e| e.to_string()));
    }

    fn listed(&self, kind: Kind) -> &[String] {
        match match kind {
            Kind::Input => &self.inputs,
            Kind::Output => &self.outputs,
        } {
            Some(Ok(ports)) => ports,
            _ => &[],
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        project: &Project,
        new_id: impl Fn() -> NodeId,
    ) -> Vec<Edit> {
        if self.inputs.is_none() {
            self.refresh();
        }
        let mut edits = Vec::new();
        let rows = rows(project);
        ui.horizontal(|ui| {
            ui.label(RichText::new("MIDI ports").strong());
            if ui
                .button("Refresh")
                .on_hover_text("List the ports again, e.g. after plugging a device in")
                .clicked()
            {
                self.refresh();
            }
            if ui
                .button("Add MIDI Out")
                .on_hover_text("A MIDI Out node on a port that has none yet")
                .clicked()
            {
                let free = self.listed(Kind::Output).iter().find(|p| {
                    rows.iter()
                        .all(|r| r.kind != Kind::Output || !same_port(&r.port, p))
                });
                edits.push(add_out(new_id(), free.map_or("", String::as_str)));
            }
        });
        for list in [&self.inputs, &self.outputs] {
            if let Some(Err(error)) = list {
                ui.colored_label(ui.visuals().warn_fg_color, error);
            }
        }
        if rows.is_empty() {
            ui.weak("No MIDI In, MIDI Control or MIDI Out nodes yet.");
            return edits;
        }
        egui::Grid::new("midi ports grid")
            .num_columns(3)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for row in &rows {
                    let name = ui.label(format!("{} {}", row.name, row.node.0));
                    if let Some(edit) = self.port_combo(ui, row, &rows, name.id) {
                        edits.push(edit);
                    }
                    self.status(ui, row);
                    ui.end_row();
                }
            });
        edits
    }

    /// Whether the row's saved port is among the listed ones.
    fn connected(&self, row: &Row) -> bool {
        self.listed(row.kind)
            .iter()
            .any(|p| same_port(p, &row.port))
    }

    fn status(&self, ui: &mut Ui, row: &Row) {
        let text = match (row.kind, row.port.is_empty() || row.port == "*") {
            (Kind::Input, true) => "Listening to every port".to_owned(),
            (Kind::Output, true) => "No port chosen".to_owned(),
            _ if self.connected(row) => "Connected".to_owned(),
            _ => "Not connected".to_owned(),
        };
        if text == "Not connected" {
            ui.colored_label(ui.visuals().warn_fg_color, text);
        } else {
            ui.weak(text);
        }
    }

    fn port_combo(&self, ui: &mut Ui, row: &Row, rows: &[Row], name: egui::Id) -> Option<Edit> {
        let any = match row.kind {
            Kind::Input => "Every port",
            Kind::Output => "None",
        };
        let ports = self.listed(row.kind);
        let selected = if row.port.is_empty() || row.port == "*" {
            any.to_owned()
        } else if self.connected(row) {
            row.port.clone()
        } else {
            format!("{} (not connected)", row.port)
        };
        let mut chosen = None;
        let combo = Dropdown::from_id_salt(("midi port", row.node))
            .selected_text(selected)
            .width(280.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(row.port.is_empty() || row.port == "*", any)
                    .clicked()
                {
                    chosen = Some(String::new());
                }
                for port in ports {
                    let others: Vec<String> = rows
                        .iter()
                        .filter(|r| r.node != row.node && r.kind == row.kind)
                        .filter(|r| same_port(&r.port, port))
                        .map(|r| format!("{} {}", r.name, r.node.0))
                        .collect();
                    let label = if others.is_empty() {
                        port.clone()
                    } else {
                        format!("{port} ({})", others.join(", "))
                    };
                    if ui
                        .selectable_label(same_port(&row.port, port), label)
                        .clicked()
                    {
                        chosen = Some(port.clone());
                    }
                }
                // A saved port that isn't connected stays chosen until
                // another is picked.
                if !row.port.is_empty() && row.port != "*" && !self.connected(row) {
                    let _ = ui.selectable_label(true, format!("{} (not connected)", row.port));
                }
            });
        combo.response.labelled_by(name);
        chosen
            .filter(|port| *port != row.port)
            .map(|port| assign(row, &port))
    }
}
