//! The audio settings dialog: which host, output and input devices to use,
//! and at what sample rate and buffer size.
//!
//! It edits a copy of the session's [`AudioConfig`] and hands it back when
//! the user applies it. Listing devices talks to the system and can take a
//! moment (ALSA probes every device), so it happens when the dialog opens,
//! when the host changes, and on Refresh, never every frame.

use noodle_io::{
    AudioConfig, AudioError, Capabilities, DeviceInfo, DeviceList, HostInfo, InputChoice,
};

/// Buffer sizes the dialog offers, where the device takes them.
const BUFFER_SIZES: [u32; 8] = [32, 64, 128, 256, 512, 1024, 2048, 4096];

pub struct DevicePicker {
    open: bool,
    /// The settings being edited.
    draft: AudioConfig,
    /// The settings when the dialog opened, to tell whether there's anything
    /// to apply.
    original: AudioConfig,
    hosts: Vec<HostInfo>,
    /// The devices on the draft's host, or why they couldn't be listed.
    devices: Result<DeviceList, String>,
    list_hosts: fn() -> Vec<HostInfo>,
    list_devices: fn(Option<&str>) -> Result<DeviceList, AudioError>,
}

impl Default for DevicePicker {
    fn default() -> Self {
        Self::with_lister(noodle_io::hosts, noodle_io::devices)
    }
}

impl DevicePicker {
    /// A picker that lists devices with the given functions, so tests can
    /// supply their own.
    fn with_lister(
        list_hosts: fn() -> Vec<HostInfo>,
        list_devices: fn(Option<&str>) -> Result<DeviceList, AudioError>,
    ) -> Self {
        Self {
            open: false,
            draft: AudioConfig::default(),
            original: AudioConfig::default(),
            hosts: Vec::new(),
            devices: Ok(DeviceList::default()),
            list_hosts,
            list_devices,
        }
    }

    /// Opens the dialog on `current`, listing the devices afresh.
    pub fn open(&mut self, current: &AudioConfig) {
        self.open = true;
        self.draft = current.clone();
        self.original = current.clone();
        self.refresh();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Draws the dialog while it's open. Returns the new settings when the
    /// user applies them.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<AudioConfig> {
        if !self.open {
            return None;
        }
        let mut applied = None;
        let modal = egui::Modal::new(egui::Id::new("audio settings")).show(ctx, |ui| {
            ui.heading("Audio Settings");
            ui.add_space(6.0);
            egui::Grid::new("audio settings grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| self.fields(ui));
            if let Err(error) = &self.devices {
                ui.add_space(4.0);
                ui.colored_label(ui.visuals().warn_fg_color, error);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .button("Refresh")
                    .on_hover_text("List the devices again, e.g. after plugging one in")
                    .clicked()
                {
                    self.refresh();
                }
                let changed = self.draft != self.original;
                if ui
                    .add_enabled(changed, egui::Button::new("Apply"))
                    .clicked()
                {
                    applied = Some(self.draft.clone());
                    self.open = false;
                }
                if ui.button("Cancel").clicked() {
                    self.open = false;
                }
            });
        });
        if modal.should_close() {
            self.open = false;
        }
        applied
    }

    fn fields(&mut self, ui: &mut egui::Ui) {
        let list = self.devices.as_ref().ok();
        let outputs = list.map_or(&[][..], |l| &l.outputs[..]);
        let inputs = list.map_or(&[][..], |l| &l.inputs[..]);

        let mut host = self.draft.host.clone();
        combo(ui, "Host", host_label(&self.hosts, host.as_deref()), |ui| {
            ui.selectable_value(&mut host, None, host_label(&self.hosts, None));
            for info in &self.hosts {
                ui.selectable_value(&mut host, Some(info.id.clone()), info.name);
            }
        });
        ui.end_row();

        let mut output = self.draft.output.clone();
        combo(
            ui,
            "Output",
            device_label(outputs, output.as_deref()),
            |ui| {
                ui.selectable_value(&mut output, None, device_label(outputs, None));
                for device in outputs {
                    ui.selectable_value(&mut output, Some(device.id.clone()), &device.name);
                }
            },
        );
        ui.end_row();

        let mut input = self.draft.input.clone();
        combo(ui, "Input", input_label(inputs, &input), |ui| {
            ui.selectable_value(&mut input, InputChoice::Off, "Off");
            ui.selectable_value(
                &mut input,
                InputChoice::Default,
                input_label(inputs, &InputChoice::Default),
            );
            for device in inputs {
                ui.selectable_value(
                    &mut input,
                    InputChoice::Device(device.id.clone()),
                    &device.name,
                );
            }
        })
        .on_hover_text("Records into Input nodes, at the output's sample rate");
        ui.end_row();

        let output_caps = chosen(outputs, self.draft.output.as_deref());
        let input_caps = match &self.draft.input {
            InputChoice::Off => None,
            InputChoice::Default => chosen(inputs, None),
            InputChoice::Device(id) => chosen(inputs, Some(id)),
        };

        let mut rate = self.draft.sample_rate;
        let default_rate = output_caps.and_then(|c| c.default_sample_rate);
        combo(ui, "Sample rate", rate_label(rate, default_rate), |ui| {
            ui.selectable_value(&mut rate, None, rate_label(None, default_rate));
            for offered in sample_rates(output_caps, input_caps) {
                ui.selectable_value(&mut rate, Some(offered), format!("{offered} Hz"));
            }
        });
        ui.end_row();

        let mut buffer = self.draft.buffer_size;
        combo(ui, "Buffer size", buffer_label(buffer), |ui| {
            ui.selectable_value(&mut buffer, None, buffer_label(None));
            for size in buffer_sizes(output_caps) {
                ui.selectable_value(&mut buffer, Some(size), buffer_label(Some(size)));
            }
        })
        .on_hover_text("Smaller is quicker to respond, larger is safer from dropouts");
        ui.end_row();

        self.draft.output = output;
        self.draft.input = input;
        self.draft.sample_rate = rate;
        self.draft.buffer_size = buffer;
        if host != self.draft.host {
            self.change_host(host);
        }
    }

    /// Switches host. Devices chosen on the old host can't be used on the new
    /// one, so they go back to its defaults.
    fn change_host(&mut self, host: Option<String>) {
        self.draft.host = host;
        self.draft.output = None;
        if let InputChoice::Device(_) = self.draft.input {
            self.draft.input = InputChoice::Default;
        }
        self.refresh_devices();
    }

    fn refresh(&mut self) {
        self.hosts = (self.list_hosts)();
        self.refresh_devices();
    }

    fn refresh_devices(&mut self) {
        self.devices = (self.list_devices)(self.draft.host.as_deref())
            .map_err(|error| format!("Couldn't list devices: {error}"));
    }
}

/// A row of the settings grid: a label, and a combo box named by it.
fn combo<R>(
    ui: &mut egui::Ui,
    label: &str,
    selected: impl Into<egui::WidgetText>,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Response {
    let label = ui.label(label);
    egui::ComboBox::from_id_salt(label.id)
        .selected_text(selected)
        .width(260.0)
        .show_ui(ui, contents)
        .response
        .labelled_by(label.id)
}

fn host_label(hosts: &[HostInfo], id: Option<&str>) -> String {
    match id {
        None => match hosts.iter().find(|h| h.is_default) {
            Some(host) => format!("Default ({})", host.name),
            None => "Default".into(),
        },
        Some(id) => match hosts.iter().find(|h| h.id == id) {
            Some(host) => host.name.into(),
            None => format!("{id} (not available)"),
        },
    }
}

/// The device's name, or for the default, which device that is.
fn device_label(devices: &[DeviceInfo], id: Option<&str>) -> String {
    match id {
        None => match devices.iter().find(|d| d.is_default) {
            Some(device) => format!("Default ({})", device.name),
            None => "Default".into(),
        },
        Some(id) => match devices.iter().find(|d| d.id == id) {
            Some(device) => device.name.clone(),
            None => format!("{id} (not found)"),
        },
    }
}

fn input_label(devices: &[DeviceInfo], choice: &InputChoice) -> String {
    match choice {
        InputChoice::Off => "Off".into(),
        InputChoice::Default => device_label(devices, None),
        InputChoice::Device(id) => device_label(devices, Some(id)),
    }
}

fn rate_label(rate: Option<u32>, default: Option<u32>) -> String {
    match (rate, default) {
        (Some(rate), _) => format!("{rate} Hz"),
        (None, Some(default)) => format!("Device default ({default} Hz)"),
        (None, None) => "Device default".into(),
    }
}

fn buffer_label(frames: Option<u32>) -> String {
    match frames {
        Some(frames) => format!("{frames} frames"),
        None => "Device default".into(),
    }
}

/// What the chosen device (`None` for the default) supports, if it's listed.
fn chosen<'a>(devices: &'a [DeviceInfo], id: Option<&str>) -> Option<&'a Capabilities> {
    devices
        .iter()
        .find(|d| match id {
            Some(id) => d.id == id,
            None => d.is_default,
        })
        .map(|d| &d.capabilities)
}

/// The rates to offer: the output's, and when input is on, only those the
/// input supports too, since it runs at the output's rate. With the output
/// unknown, any rate the input supports.
fn sample_rates(output: Option<&Capabilities>, input: Option<&Capabilities>) -> Vec<u32> {
    match (output, input) {
        (Some(output), Some(input)) => output
            .sample_rates
            .iter()
            .copied()
            .filter(|rate| input.sample_rates.contains(rate))
            .collect(),
        (Some(caps), None) | (None, Some(caps)) => caps.sample_rates.clone(),
        (None, None) => Vec::new(),
    }
}

/// The buffer sizes to offer: the usual powers of two, within the output's
/// range when it says.
fn buffer_sizes(output: Option<&Capabilities>) -> Vec<u32> {
    let range = output.and_then(|caps| caps.buffer_sizes);
    BUFFER_SIZES
        .into_iter()
        .filter(|size| range.is_none_or(|(min, max)| (min..=max).contains(size)))
        .collect()
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn caps(rates: &[u32], buffers: Option<(u32, u32)>) -> Capabilities {
        Capabilities {
            max_channels: 2,
            default_sample_rate: rates.first().copied(),
            sample_rates: rates.to_vec(),
            buffer_sizes: buffers,
        }
    }

    fn device(id: &str, name: &str, is_default: bool, capabilities: Capabilities) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            name: name.into(),
            is_default,
            capabilities,
        }
    }

    fn fake_hosts() -> Vec<HostInfo> {
        vec![
            HostInfo {
                id: "alsa".into(),
                name: "ALSA",
                is_default: true,
            },
            HostInfo {
                id: "jack".into(),
                name: "JACK",
                is_default: false,
            },
        ]
    }

    /// Speakers and a USB interface on ALSA; just JACK's system ports on JACK.
    fn fake_devices(host: Option<&str>) -> Result<DeviceList, AudioError> {
        let stereo = caps(&[44_100, 48_000, 96_000], Some((64, 4096)));
        Ok(match host {
            None | Some("alsa") => DeviceList {
                outputs: vec![
                    device("alsa:speakers", "Speakers", true, stereo.clone()),
                    device("alsa:usb", "USB Interface", false, stereo),
                ],
                inputs: vec![
                    device("alsa:mic", "Microphone", true, caps(&[44_100], None)),
                    device("alsa:usb", "USB Interface", false, caps(&[48_000], None)),
                ],
            },
            Some("jack") => DeviceList {
                outputs: vec![device("jack:system", "system", true, caps(&[48_000], None))],
                inputs: vec![],
            },
            Some(other) => return Err(AudioError::NoHost(other.into())),
        })
    }

    struct State {
        picker: DevicePicker,
        applied: Option<AudioConfig>,
    }

    fn harness(current: AudioConfig) -> Harness<'static, State> {
        let mut picker = DevicePicker::with_lister(fake_hosts, fake_devices);
        picker.open(&current);
        let state = State {
            picker,
            applied: None,
        };
        let mut harness = Harness::new_ui_state(
            |ui, state: &mut State| {
                if let Some(config) = state.picker.show(ui.ctx()) {
                    state.applied = Some(config);
                }
            },
            state,
        );
        harness.run();
        harness
    }

    /// Opens the combo box on the row called `row` and picks an item.
    fn choose(harness: &mut Harness<'_, State>, row: &str, item: &str) {
        harness.get_by_label(row).click();
        harness.run();
        harness.get_by_label(item).click();
        harness.run();
    }

    /// What the combo box on the row called `row` shows.
    fn shown(harness: &Harness<'_, State>, row: &str) -> String {
        harness.get_by_label(row).value().unwrap_or_default()
    }

    #[test]
    fn shows_which_devices_the_defaults_are() {
        let harness = harness(AudioConfig::default());
        assert_eq!(shown(&harness, "Host"), "Default (ALSA)");
        assert_eq!(shown(&harness, "Output"), "Default (Speakers)");
        assert_eq!(shown(&harness, "Input"), "Off");
        assert_eq!(shown(&harness, "Sample rate"), "Device default (44100 Hz)");
        assert_eq!(shown(&harness, "Buffer size"), "Device default");
    }

    #[test]
    fn choosing_devices_and_applying_hands_back_the_settings() {
        let mut harness = harness(AudioConfig::default());
        choose(&mut harness, "Output", "USB Interface");
        choose(&mut harness, "Input", "Microphone");
        harness.get_by_label("Apply").click();
        harness.run();
        let state = harness.state();
        assert!(!state.picker.is_open());
        assert_eq!(
            state.applied,
            Some(AudioConfig {
                output: Some("alsa:usb".into()),
                input: InputChoice::Device("alsa:mic".into()),
                ..AudioConfig::default()
            })
        );
    }

    #[test]
    fn cancel_hands_back_nothing() {
        let mut harness = harness(AudioConfig::default());
        choose(&mut harness, "Output", "USB Interface");
        harness.get_by_label("Cancel").click();
        harness.run();
        assert!(!harness.state().picker.is_open());
        assert_eq!(harness.state().applied, None);
    }

    #[test]
    fn changing_host_resets_devices_from_the_old_one() {
        let mut harness = harness(AudioConfig {
            output: Some("alsa:usb".into()),
            input: InputChoice::Device("alsa:mic".into()),
            ..AudioConfig::default()
        });
        choose(&mut harness, "Host", "JACK");
        let draft = &harness.state().picker.draft;
        assert_eq!(draft.host.as_deref(), Some("jack"));
        assert_eq!(draft.output, None);
        assert_eq!(draft.input, InputChoice::Default);
        assert_eq!(shown(&harness, "Output"), "Default (system)");
    }

    #[test]
    fn a_missing_device_is_named_rather_than_dropped() {
        let harness = harness(AudioConfig {
            output: Some("alsa:unplugged".into()),
            ..AudioConfig::default()
        });
        assert_eq!(shown(&harness, "Output"), "alsa:unplugged (not found)");
        assert_eq!(
            harness.state().picker.draft.output.as_deref(),
            Some("alsa:unplugged")
        );
    }

    #[test]
    fn a_listing_error_is_shown() {
        let harness = harness(AudioConfig {
            host: Some("asio".into()),
            ..AudioConfig::default()
        });
        assert_eq!(shown(&harness, "Host"), "asio (not available)");
        harness.get_by_label("Couldn't list devices: no audio host called \"asio\" is available");
    }

    #[test]
    fn with_input_on_only_rates_both_devices_take_are_offered() {
        let output = caps(&[44_100, 48_000, 96_000], None);
        let input = caps(&[48_000, 96_000, 192_000], None);
        assert_eq!(sample_rates(Some(&output), Some(&input)), [48_000, 96_000]);
        assert_eq!(sample_rates(Some(&output), None), [44_100, 48_000, 96_000]);
        assert_eq!(sample_rates(None, Some(&input)), [48_000, 96_000, 192_000]);
        assert!(sample_rates(None, None).is_empty());
    }

    #[test]
    fn buffer_sizes_stay_within_the_device_range() {
        assert_eq!(
            buffer_sizes(Some(&caps(&[], Some((100, 1024))))),
            [128, 256, 512, 1024]
        );
        assert_eq!(buffer_sizes(Some(&caps(&[], None))), BUFFER_SIZES);
        assert_eq!(buffer_sizes(None), BUFFER_SIZES);
    }
}
