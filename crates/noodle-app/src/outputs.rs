//! The outputs view: which audio device each Output node plays on.
//!
//! Like the mixer, it keeps no project state. An Output node's device is its
//! `device` setting (see [`noodle_engine::OUTPUT_DEVICE`]), so a choice here
//! is a `SetConfig` on the node, one undo step, and the session restarts
//! playback on the devices the project asks for. Listing devices talks to the
//! system and can take a moment, so it happens when the view opens and on
//! Refresh, never every frame.

use egui::{ComboBox, RichText, Ui};
use noodle_core::{Command, Config, Node, NodeId, Project, Value};
use noodle_engine::{OUTPUT_DEVICE, OUTPUT_DEVICE_KEY, OUTPUT_ID, Problem};
use noodle_io::{AudioError, DeviceInfo, DeviceList, OutputStatus};

use crate::session::{Edit, Session};

/// The settings key an Output node's device is kept under.
const DEVICE: &str = OUTPUT_DEVICE_KEY;

/// One Output node and where it plays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub node: NodeId,
    /// A device ID, or empty for the main output.
    pub device: String,
}

/// Every Output node in the project, in ID order.
pub fn rows(project: &Project) -> Vec<Row> {
    project
        .graph()
        .nodes()
        .filter(|(_, node)| node.type_id == OUTPUT_ID)
        .map(|(node, n)| Row {
            node,
            device: OUTPUT_DEVICE.get_text(&n.config),
        })
        .collect()
}

/// The edit that ties `node` to `device`, or back to the main output if it's
/// empty.
pub fn assign(node: NodeId, device: &str) -> Edit {
    Edit::Apply(Command::SetConfig {
        node,
        key: DEVICE.to_owned(),
        value: (!device.is_empty()).then(|| Value::Text(device.to_owned())),
    })
}

/// The edit that adds an Output node tied to `device`.
pub fn add(id: NodeId, device: &str) -> Edit {
    let mut config = Config::new();
    if !device.is_empty() {
        config.set(DEVICE, Value::Text(device.to_owned()));
    }
    Edit::Apply(Command::AddNode {
        id,
        node: Node::new(OUTPUT_ID).with_config(config),
    })
}

/// The Output node that has `device`, other than `except`. At most one may.
pub fn user_of<'a>(rows: &'a [Row], device: &str, except: NodeId) -> Option<&'a Row> {
    rows.iter()
        .find(|row| row.device == device && row.node != except)
}

/// The view's state: the devices it last listed.
pub struct OutputsView {
    /// The output devices on the audio settings' host, or why they couldn't
    /// be listed. `None` until the first listing.
    devices: Option<Result<Vec<DeviceInfo>, String>>,
    list_devices: fn(Option<&str>) -> Result<DeviceList, AudioError>,
}

impl Default for OutputsView {
    fn default() -> Self {
        Self::with_lister(noodle_io::devices)
    }
}

impl OutputsView {
    /// A view that lists devices with the given function, so tests can
    /// supply their own.
    pub(crate) fn with_lister(
        list_devices: fn(Option<&str>) -> Result<DeviceList, AudioError>,
    ) -> Self {
        Self {
            devices: None,
            list_devices,
        }
    }

    /// Lists the devices again, on `host` (`None` for the platform's).
    pub fn refresh(&mut self, host: Option<&str>) {
        self.devices = Some(
            (self.list_devices)(host)
                .map(|list| list.outputs)
                .map_err(|error| error.to_string()),
        );
    }

    /// The device the main output plays on: the one chosen in the audio
    /// settings, else the host's default, if it's been listed.
    fn main_device(&self, chosen: Option<&str>) -> Option<String> {
        chosen.map(str::to_owned).or_else(|| {
            self.listed()
                .iter()
                .find(|d| d.is_default)
                .map(|d| d.id.clone())
        })
    }

    /// The output devices last listed.
    fn listed(&self) -> &[DeviceInfo] {
        match &self.devices {
            Some(Ok(devices)) => devices,
            _ => &[],
        }
    }

    pub fn show(&mut self, ui: &mut Ui, session: &Session) -> Vec<Edit> {
        let host = session.audio_config().host.clone();
        if self.devices.is_none() {
            self.refresh(host.as_deref());
        }
        let mut edits = Vec::new();
        let rows = rows(session.project());
        let main = self.main_device(session.audio_config().output.as_deref());
        let status = session.output_devices();

        ui.horizontal(|ui| {
            ui.label(RichText::new("Outputs").strong());
            if ui
                .button("Refresh")
                .on_hover_text("List the devices again, e.g. after plugging one in")
                .clicked()
            {
                self.refresh(host.as_deref());
            }
            if ui
                .button("Add output")
                .on_hover_text("An Output node on a device that has none yet")
                .clicked()
            {
                let free = self.listed().iter().find(|d| {
                    Some(&d.id) != main.as_ref() && rows.iter().all(|r| r.device != d.id)
                });
                edits.push(add(
                    session.new_node_id(),
                    free.map_or("", |d| d.id.as_str()),
                ));
            }
        });
        if let Some(Err(error)) = &self.devices {
            ui.colored_label(ui.visuals().warn_fg_color, error);
        }
        if rows.is_empty() {
            ui.weak("No Output nodes yet. Add one here, or in the node editor.");
            return edits;
        }
        egui::Grid::new("outputs grid")
            .num_columns(3)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for row in &rows {
                    let name = ui.label(format!("Output {}", row.node.0));
                    if let Some(edit) = self.device_combo(ui, row, &rows, main.as_deref(), name.id)
                    {
                        edits.push(edit);
                    }
                    let problem = session.diagnostics().iter().find_map(|d| {
                        (d.location == noodle_engine::Location::Node(row.node)
                            && matches!(
                                d.problem,
                                Problem::DeviceTaken(_) | Problem::DeviceUnavailable(_)
                            ))
                        .then(|| d.problem.to_string())
                    });
                    status_label(
                        ui,
                        row,
                        status,
                        session.is_playing(),
                        main.as_deref(),
                        problem,
                    );
                    ui.end_row();
                }
            });
        edits
    }

    /// The drop-down choosing `row`'s device. A device another Output node
    /// has can't be chosen, since there can be only one per device.
    fn device_combo(
        &self,
        ui: &mut Ui,
        row: &Row,
        rows: &[Row],
        main: Option<&str>,
        name: egui::Id,
    ) -> Option<Edit> {
        let devices = self.listed();
        let name_of = |id: &str| {
            devices
                .iter()
                .find(|d| d.id == id)
                .map_or_else(|| format!("{id} (not connected)"), |d| d.name.clone())
        };
        let main_name = main.map_or("system default".to_owned(), name_of);
        let selected = if row.device.is_empty() {
            format!("Main output ({main_name})")
        } else {
            name_of(&row.device)
        };
        let mut chosen = None;
        let combo = ComboBox::from_id_salt(("output device", row.node))
            .selected_text(selected)
            .width(260.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(row.device.is_empty(), format!("Main output ({main_name})"))
                    .clicked()
                {
                    chosen = Some(String::new());
                }
                let mut shown_current = row.device.is_empty();
                for device in devices {
                    // The main device is the main output; offering it twice
                    // would let two nodes share it.
                    if Some(device.id.as_str()) == main {
                        continue;
                    }
                    shown_current |= device.id == row.device;
                    let taken = user_of(rows, &device.id, row.node);
                    let label = match taken {
                        Some(other) => format!("{} (Output {})", device.name, other.node.0),
                        None => device.name.clone(),
                    };
                    let response = ui.add_enabled(
                        taken.is_none(),
                        egui::Button::selectable(row.device == device.id, label),
                    );
                    if response.clicked() {
                        chosen = Some(device.id.clone());
                    }
                }
                // A saved device that isn't connected stays chosen until the
                // user picks another.
                if !shown_current {
                    let _ = ui.selectable_label(true, name_of(&row.device));
                }
            });
        combo.response.labelled_by(name);
        chosen
            .filter(|device| *device != row.device)
            .map(|device| assign(row.node, &device))
    }
}

/// What became of `row`'s device while playing.
fn status_label(
    ui: &mut Ui,
    row: &Row,
    status: &[OutputStatus],
    playing: bool,
    main: Option<&str>,
    problem: Option<String>,
) {
    if let Some(problem) = problem.filter(|_| playing) {
        ui.colored_label(ui.visuals().warn_fg_color, problem);
        return;
    }
    // The main device's own ID is the main output.
    if row.device.is_empty() || Some(row.device.as_str()) == main {
        ui.weak(if playing { "Playing" } else { "Not playing" });
        return;
    }
    match status.iter().find(|s| s.device == row.device) {
        Some(OutputStatus {
            result: Ok(opened), ..
        }) => {
            ui.label(format!("Playing on {} channels", opened.channels));
        }
        Some(OutputStatus {
            result: Err(error), ..
        }) => {
            ui.colored_label(ui.visuals().warn_fg_color, format!("Can't play: {error}"));
        }
        None => {
            ui.weak("Not playing");
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use noodle_io::{Capabilities, DeviceInfo};

    use super::*;
    use crate::session::Nodes;

    fn project(devices: &[&str]) -> Project {
        let mut project = Project::new();
        for device in devices {
            let id = project.new_node_id();
            match add(id, device) {
                Edit::Apply(command) => {
                    command.apply(&mut project).unwrap();
                }
                _ => unreachable!(),
            }
        }
        project
    }

    #[test]
    fn rows_list_the_output_nodes_with_their_devices() {
        let rows = rows(&project(&["", "alsa:b"]));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].device, "");
        assert_eq!(rows[1].device, "alsa:b");
    }

    #[test]
    fn assigning_a_device_sets_it_and_the_main_output_clears_it() {
        let mut project = project(&[""]);
        let node = rows(&project)[0].node;
        for (device, expected) in [("alsa:x", "alsa:x"), ("", "")] {
            match assign(node, device) {
                Edit::Apply(command) => {
                    command.apply(&mut project).unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(rows(&project)[0].device, expected);
        }
        assert!(
            project
                .graph()
                .node(node)
                .unwrap()
                .config
                .get(DEVICE)
                .is_none(),
            "the main output is the default, so it isn't stored"
        );
    }

    #[test]
    fn a_device_is_used_by_one_other_node_at_most() {
        let rows = rows(&project(&["alsa:a", "alsa:b"]));
        assert_eq!(user_of(&rows, "alsa:a", rows[1].node), Some(&rows[0]));
        assert_eq!(user_of(&rows, "alsa:a", rows[0].node), None);
        assert_eq!(user_of(&rows, "alsa:c", rows[0].node), None);
    }

    fn device(id: &str, name: &str, is_default: bool) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            name: name.into(),
            is_default,
            capabilities: Capabilities {
                max_channels: 2,
                default_sample_rate: Some(48_000),
                sample_rates: vec![48_000],
                buffer_sizes: None,
            },
        }
    }

    fn fake_devices(_host: Option<&str>) -> Result<DeviceList, AudioError> {
        Ok(DeviceList {
            outputs: vec![
                device("alsa:speakers", "Speakers", true),
                device("alsa:usb", "USB Interface", false),
                device("alsa:hdmi", "HDMI", false),
            ],
            inputs: vec![],
        })
    }

    struct State {
        view: OutputsView,
        session: Session,
    }

    fn harness(devices: &[&str]) -> Harness<'static, State> {
        let session = Session::with_project(Nodes::all(), project(devices), None);
        let state = State {
            view: OutputsView::with_lister(fake_devices),
            session,
        };
        let mut harness = Harness::new_ui_state(
            |ui, state: &mut State| {
                let edits = state.view.show(ui, &state.session);
                state.session.edit(edits);
            },
            state,
        );
        harness.run();
        harness
    }

    fn shown(harness: &Harness<'_, State>, label: &str) -> String {
        harness.get_by_label(label).value().unwrap_or_default()
    }

    fn choose(harness: &mut Harness<'_, State>, row: &str, item: &str) {
        harness.get_by_label(row).click();
        harness.run();
        harness.get_by_label(item).click();
        harness.run();
    }

    fn devices_of(harness: &Harness<'_, State>) -> Vec<String> {
        rows(harness.state().session.project())
            .into_iter()
            .map(|row| row.device)
            .collect()
    }

    #[test]
    fn each_output_node_shows_where_it_plays() {
        let harness = harness(&["", "alsa:usb"]);
        assert_eq!(shown(&harness, "Output 1"), "Main output (Speakers)");
        assert_eq!(shown(&harness, "Output 2"), "USB Interface");
    }

    #[test]
    fn choosing_a_device_ties_the_node_to_it_in_one_undo_step() {
        let mut harness = harness(&[""]);
        choose(&mut harness, "Output 1", "HDMI");
        assert_eq!(devices_of(&harness), ["alsa:hdmi"]);
        harness.state_mut().session.undo();
        assert_eq!(devices_of(&harness), [""]);
    }

    #[test]
    fn choosing_the_main_output_unties_the_node() {
        let mut harness = harness(&["alsa:usb"]);
        choose(&mut harness, "Output 1", "Main output (Speakers)");
        assert_eq!(devices_of(&harness), [""]);
    }

    #[test]
    fn a_device_another_node_has_cannot_be_chosen() {
        let mut harness = harness(&["alsa:usb", ""]);
        harness.get_by_label("Output 2").click();
        harness.run();
        // Greyed out, and says who has it.
        harness.get_by_label("USB Interface (Output 1)").click();
        harness.run();
        assert_eq!(devices_of(&harness), ["alsa:usb", ""]);
    }

    #[test]
    fn the_main_device_is_not_offered_twice() {
        let mut harness = harness(&[""]);
        harness.get_by_label("Output 1").click();
        harness.run();
        assert!(harness.query_by_label("Speakers").is_none());
        assert!(harness.query_by_label("HDMI").is_some());
    }

    #[test]
    fn a_device_that_is_not_connected_stays_chosen() {
        let harness = harness(&["alsa:gone"]);
        assert_eq!(shown(&harness, "Output 1"), "alsa:gone (not connected)");
        assert_eq!(devices_of(&harness), ["alsa:gone"]);
    }

    #[test]
    fn add_output_ties_the_new_node_to_a_free_device() {
        let mut harness = harness(&["alsa:usb"]);
        harness.get_by_label("Add output").click();
        harness.run();
        // Not the main device (Speakers) and not USB, which is taken.
        assert_eq!(devices_of(&harness), ["alsa:usb", "alsa:hdmi"]);
        harness.get_by_label("Add output").click();
        harness.run();
        // Nothing free is left, so it plays on the main output.
        assert_eq!(devices_of(&harness), ["alsa:usb", "alsa:hdmi", ""]);
    }
}
