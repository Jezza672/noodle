//! What the editor shows of the audio as it plays: wires that light when
//! signal or notes flow, meters on outputs, and so on. Each is a switch in
//! the Display tab of Settings, saved in the preferences, and each costs a
//! little: the audio thread measures levels and counts events for the ones
//! that need it, and the editor redraws while they move. The engine stops
//! the measuring when its switch is off (see
//! [`noodle_engine::Gather`]), and [`Display::performance`] turns all of it
//! off at once, for a user who wants the raw performance.

use serde::{Deserialize, Serialize};

/// The switches. Missing fields in a saved file take their defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// Everything below is off while this is on, without forgetting what
    /// the user chose.
    pub performance: bool,
    /// Event (MIDI) wires light up while notes flow through them.
    /// Audio thread: one count per event output per block.
    pub event_wires: bool,
    /// Audio wires brighten and glow with the level of the signal in them.
    /// Audio thread: the input level measurement, shared with `input_meters`.
    pub audio_wires: bool,
    /// A small level bar on each wired audio input.
    /// Audio thread: one peak and RMS pass over each wired input.
    pub input_meters: bool,
    /// A small level bar on each audio output, wired or not.
    /// Audio thread: one peak and RMS pass over each output.
    pub output_meters: bool,
    /// Meters and wires flash red when the signal goes over 0 dBFS.
    /// No cost beyond the meters it marks.
    pub clip_flash: bool,
    /// Nodes that make no sound, or that nothing listens to, are drawn
    /// faded. Audio thread: reads the silent flags the engine already keeps.
    pub silent_dim: bool,
    /// Modulation wires pulse when they move, and wired parameters show
    /// their live range. No audio-thread cost: the engine reports wired
    /// parameters for the properties panel anyway.
    pub param_motion: bool,
    /// Dots on polyphonic nodes for the voices that are sounding.
    /// Audio thread: reads the silent flags, shared with `silent_dim`.
    pub voice_leds: bool,
    /// Clocks, arpeggiators, patterns and the metronome pulse on each beat
    /// while the transport runs. UI only.
    pub beat_pulse: bool,
    /// Nodes are outlined hotter the more of the block time they take. Off
    /// unless asked for. UI only: the engine times nodes anyway.
    pub cpu_heat: bool,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            performance: false,
            event_wires: true,
            audio_wires: true,
            input_meters: true,
            output_meters: true,
            clip_flash: true,
            silent_dim: true,
            param_motion: true,
            voice_leds: true,
            beat_pulse: true,
            cpu_heat: false,
        }
    }
}

impl Display {
    /// What is shown now: the switches, or nothing while `performance` is on.
    pub fn effective(self) -> Self {
        if self.performance {
            Self {
                performance: true,
                event_wires: false,
                audio_wires: false,
                input_meters: false,
                output_meters: false,
                clip_flash: false,
                silent_dim: false,
                param_motion: false,
                voice_leds: false,
                beat_pulse: false,
                cpu_heat: false,
            }
        } else {
            self
        }
    }

    /// Tells the engine which reports to make.
    pub fn apply(self, gather: &noodle_engine::Gather) {
        let shown = self.effective();
        gather.set_input_levels(shown.audio_wires || shown.input_meters);
        gather.set_output_levels(shown.output_meters);
        gather.set_events(shown.event_wires);
        gather.set_state(shown.silent_dim || shown.voice_leds);
    }

    /// Every switch but `performance`, with the label and hint the dialog
    /// shows, in the order it shows them.
    pub fn switches() -> &'static [Switch] {
        &SWITCHES
    }

    /// Reads one switch.
    pub fn get(&self, switch: &Switch) -> bool {
        (switch.get)(self)
    }

    /// Sets one switch.
    pub fn set(&mut self, switch: &Switch, on: bool) {
        (switch.set)(self, on);
    }
}

/// Where a switch's cost falls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cost {
    /// The audio thread measures something for it.
    AudioThread,
    /// Only the editor redraws.
    UiOnly,
    /// Nothing beyond the switch it marks.
    None,
}

impl Cost {
    fn label(self) -> &'static str {
        match self {
            Cost::AudioThread => "audio thread",
            Cost::UiOnly => "UI only",
            Cost::None => "free",
        }
    }
}

/// The groups the dialog lays the switches out in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Wires,
    Meters,
    Nodes,
}

impl Group {
    const ALL: [Group; 3] = [Group::Wires, Group::Meters, Group::Nodes];

    fn label(self) -> &'static str {
        match self {
            Group::Wires => "Wires",
            Group::Meters => "Meters",
            Group::Nodes => "Nodes",
        }
    }
}

/// A ready-made set of switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Full,
    Light,
    Performance,
    Custom,
}

impl Display {
    /// What the preset sets. `Custom` is not a thing to set.
    pub fn from_preset(preset: Preset) -> Option<Self> {
        match preset {
            Preset::Full => Some(Self::default()),
            Preset::Light => Some(Self {
                performance: false,
                event_wires: false,
                audio_wires: false,
                input_meters: false,
                output_meters: true,
                clip_flash: true,
                silent_dim: false,
                param_motion: true,
                voice_leds: false,
                beat_pulse: true,
                cpu_heat: false,
            }),
            Preset::Performance => Some(Self {
                performance: true,
                ..Self::default()
            }),
            Preset::Custom => None,
        }
    }

    /// Which preset the switches match, or Custom.
    pub fn preset(&self) -> Preset {
        [Preset::Full, Preset::Light, Preset::Performance]
            .into_iter()
            .find(|p| {
                Self::from_preset(*p).is_some_and(|d| {
                    if d.performance {
                        self.performance
                    } else {
                        !self.performance && d == *self
                    }
                })
            })
            .unwrap_or(Preset::Custom)
    }
}

/// One switch of [`Display`], for the dialog.
pub struct Switch {
    pub label: &'static str,
    pub hint: &'static str,
    pub group: Group,
    pub cost: Cost,
    get: fn(&Display) -> bool,
    set: fn(&mut Display, bool),
}

macro_rules! switch {
    ($label:expr, $hint:expr, $group:ident, $cost:ident, $field:ident) => {
        Switch {
            label: $label,
            hint: $hint,
            group: Group::$group,
            cost: Cost::$cost,
            get: |d| d.$field,
            set: |d, on| d.$field = on,
        }
    };
}

static SWITCHES: [Switch; 10] = [
    switch!(
        "Event wires light up",
        "MIDI and other event wires brighten while notes flow. The audio thread counts events.",
        Wires,
        AudioThread,
        event_wires
    ),
    switch!(
        "Audio wires glow with level",
        "Wires brighten with the signal in them. The audio thread measures each wired input.",
        Wires,
        AudioThread,
        audio_wires
    ),
    switch!(
        "Level bars on inputs",
        "A small meter on each wired audio input. The audio thread measures each wired input.",
        Meters,
        AudioThread,
        input_meters
    ),
    switch!(
        "Level bars on outputs",
        "A small meter on each audio output. The audio thread measures each output.",
        Meters,
        AudioThread,
        output_meters
    ),
    switch!(
        "Flash red on clipping",
        "Meters and wires flash when the signal goes over 0 dB.",
        Meters,
        None,
        clip_flash
    ),
    switch!(
        "Fade silent nodes",
        "Nodes that make no sound, or that nothing listens to, are faded.",
        Nodes,
        AudioThread,
        silent_dim
    ),
    switch!(
        "Show modulation motion",
        "Modulation wires pulse, and wired parameters show their live range.",
        Wires,
        UiOnly,
        param_motion
    ),
    switch!(
        "Voice lights",
        "Dots on polyphonic nodes for the voices that sound.",
        Nodes,
        AudioThread,
        voice_leds
    ),
    switch!(
        "Pulse on the beat",
        "Clocks, arpeggiators, patterns and the metronome pulse while playing.",
        Nodes,
        UiOnly,
        beat_pulse
    ),
    switch!(
        "Heat tint for CPU use",
        "Nodes are outlined hotter the more of the block time they take.",
        Nodes,
        UiOnly,
        cpu_heat
    ),
];

/// Draws the Display tab of the Settings window: a preset row over the
/// switches, grouped and tagged with what each costs. They take effect as
/// they are clicked, so there is nothing to apply. Says whether one changed.
pub fn body(ui: &mut egui::Ui, display: &mut Display) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Preset");
        let current = display.preset();
        for (preset, name) in [
            (Preset::Full, "Full"),
            (Preset::Light, "Light"),
            (Preset::Performance, "Performance"),
        ] {
            let hint = match preset {
                Preset::Full => "Everything on",
                Preset::Light => "Only the output meters and what costs the audio thread nothing",
                _ => "Everything off, keeping your choices for later. Also in View > Overlays",
            };
            if ui
                .selectable_label(current == preset, name)
                .on_hover_text(hint)
                .clicked()
            {
                if preset == Preset::Performance {
                    // A toggle: it keeps the switches as they were.
                    display.performance = !display.performance;
                    changed = true;
                } else if current != preset
                    && let Some(d) = Display::from_preset(preset)
                {
                    *display = d;
                    changed = true;
                }
            }
        }
        ui.add_enabled(
            false,
            egui::Button::selectable(current == Preset::Custom, "Custom"),
        );
    });
    ui.add_space(4.0);
    ui.add_enabled_ui(!display.performance, |ui| {
        for group in Group::ALL {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(group.label()).strong());
            for switch in Display::switches().iter().filter(|s| s.group == group) {
                let mut on = display.get(switch);
                let nested = switch.label == "Flash red on clipping";
                ui.horizontal(|ui| {
                    if nested {
                        ui.add_space(20.0);
                    }
                    if ui
                        .checkbox(&mut on, switch.label)
                        .on_hover_text(switch.hint)
                        .changed()
                    {
                        display.set(switch, on);
                        changed = true;
                    }
                    ui.weak(switch.cost.label());
                });
            }
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_on_but_the_cpu_tint() {
        let d = Display::default();
        for switch in Display::switches() {
            assert_eq!(d.get(switch), switch.label != "Heat tint for CPU use");
        }
        assert!(!d.performance);
    }

    #[test]
    fn presets_are_recognised_and_anything_else_is_custom() {
        assert_eq!(Display::default().preset(), Preset::Full);
        let light = Display::from_preset(Preset::Light).unwrap();
        assert_eq!(light.preset(), Preset::Light);
        let performance = Display {
            performance: true,
            ..light
        };
        assert_eq!(performance.preset(), Preset::Performance);
        let custom = Display {
            cpu_heat: true,
            ..Display::default()
        };
        assert_eq!(custom.preset(), Preset::Custom);
    }

    #[test]
    fn performance_mode_shows_nothing_and_keeps_the_choices() {
        let d = Display {
            performance: true,
            ..Display::default()
        };
        let shown = d.effective();
        assert!(Display::switches().iter().all(|s| !shown.get(s)));
        // The choices are still there for when it is switched off.
        assert!(d.event_wires);
        let off = Display {
            performance: false,
            ..d
        };
        assert_eq!(off.effective(), off);
    }

    #[test]
    fn the_engine_measures_only_what_is_shown() {
        let telemetry = noodle_engine::Telemetry::new();
        let gather = telemetry.gather();
        Display::default().apply(gather);
        assert!(gather.input_levels() && gather.output_levels());
        assert!(gather.events() && gather.state());
        Display {
            performance: true,
            ..Display::default()
        }
        .apply(gather);
        assert!(!gather.input_levels() && !gather.output_levels());
        assert!(!gather.events() && !gather.state());
        // Levels on wires and bars share one measurement, and silence and
        // voice lights share another: it stays on while either is shown.
        let mut d = Display {
            audio_wires: false,
            input_meters: true,
            silent_dim: false,
            ..Display::default()
        };
        d.apply(gather);
        assert!(gather.input_levels() && gather.state());
        d.input_meters = false;
        d.voice_leds = false;
        d.apply(gather);
        assert!(!gather.input_levels() && !gather.state());
    }

    #[test]
    fn a_saved_file_without_the_fields_takes_the_defaults() {
        let d: Display = ron::from_str("(event_wires: false)").unwrap();
        assert!(!d.event_wires && d.audio_wires && !d.cpu_heat);
    }
}
