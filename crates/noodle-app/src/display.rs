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

/// One switch of [`Display`], for the dialog.
pub struct Switch {
    pub label: &'static str,
    pub hint: &'static str,
    get: fn(&Display) -> bool,
    set: fn(&mut Display, bool),
}

macro_rules! switch {
    ($label:expr, $hint:expr, $field:ident) => {
        Switch {
            label: $label,
            hint: $hint,
            get: |d| d.$field,
            set: |d, on| d.$field = on,
        }
    };
}

static SWITCHES: [Switch; 10] = [
    switch!(
        "Event wires light up",
        "MIDI and other event wires brighten while notes flow. The audio thread counts events.",
        event_wires
    ),
    switch!(
        "Audio wires glow with level",
        "Wires brighten with the signal in them. The audio thread measures each wired input.",
        audio_wires
    ),
    switch!(
        "Level bars on inputs",
        "A small meter on each wired audio input. The audio thread measures each wired input.",
        input_meters
    ),
    switch!(
        "Level bars on outputs",
        "A small meter on each audio output. The audio thread measures each output.",
        output_meters
    ),
    switch!(
        "Flash red on clipping",
        "Meters and wires flash when the signal goes over 0 dB.",
        clip_flash
    ),
    switch!(
        "Fade silent nodes",
        "Nodes that make no sound, or that nothing listens to, are faded.",
        silent_dim
    ),
    switch!(
        "Show modulation motion",
        "Modulation wires pulse, and wired parameters show their live range.",
        param_motion
    ),
    switch!(
        "Voice lights",
        "Dots on polyphonic nodes for the voices that sound.",
        voice_leds
    ),
    switch!(
        "Pulse on the beat",
        "Clocks, arpeggiators, patterns and the metronome pulse while playing.",
        beat_pulse
    ),
    switch!(
        "Heat tint for CPU use",
        "Nodes are outlined hotter the more of the block time they take.",
        cpu_heat
    ),
];

/// Draws the Display tab of the Settings window: the switches take effect as
/// they are clicked, so there is nothing to apply. Says whether one changed.
pub fn body(ui: &mut egui::Ui, display: &mut Display) -> bool {
    let mut changed = false;
    ui.weak("What the graph shows of the sound as it plays.");
    ui.add_space(6.0);
    changed |= ui
        .checkbox(&mut display.performance, "Performance mode")
        .on_hover_text("Turns all of the below off, and keeps your choices for later")
        .changed();
    ui.add_space(4.0);
    ui.add_enabled_ui(!display.performance, |ui| {
        for switch in Display::switches() {
            let mut on = display.get(switch);
            if ui
                .checkbox(&mut on, switch.label)
                .on_hover_text(switch.hint)
                .changed()
            {
                display.set(switch, on);
                changed = true;
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
