//! The Settings window: one place, with a tab each for the General
//! preferences, the Audio devices, the Display switches, the separation
//! Models and the Sources of sounds. Everything in it
//! is saved in the user's settings file (see [`crate::prefs`]); the audio
//! devices and the plugin sandbox belong to this machine and are left out of
//! exported and project-embedded settings.

use noodle_io::{AudioConfig, LatencyReport};

use crate::devices::DevicePicker;
use crate::display::Display;
use crate::session::Session;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    General,
    Audio,
    Display,
    Models,
    Sources,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Tab::General,
        Tab::Audio,
        Tab::Display,
        Tab::Models,
        Tab::Sources,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Audio => "Audio",
            Tab::Display => "Display",
            Tab::Models => "Models",
            Tab::Sources => "Sources",
        }
    }
}

/// What the user did in the window this frame.
#[derive(Default)]
pub struct Outcome {
    /// New audio settings, applied.
    pub audio: Option<AudioConfig>,
    /// A display switch changed.
    pub display_changed: bool,
    /// Import User Settings was pressed.
    pub import: bool,
    /// Export User Settings was pressed.
    pub export: bool,
}

#[derive(Default)]
pub struct Settings {
    open: bool,
    tab: Tab,
}

impl Settings {
    /// Opens the window on `tab`.
    pub fn open(&mut self, tab: Tab) {
        self.open = true;
        self.tab = tab;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Draws the window while it is open.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        devices: &mut DevicePicker,
        latency: Option<LatencyReport>,
        display: &mut Display,
        session: &mut Session,
        models: &mut crate::models::Models,
        sounds: &mut crate::sounds::Sounds,
    ) -> Outcome {
        let mut out = Outcome::default();
        if !self.open {
            return out;
        }
        let modal = crate::dialog::modal(ctx, "settings", |ui| {
            ui.heading("Settings");
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    ui.selectable_value(&mut self.tab, tab, tab.label());
                }
            });
            ui.separator();
            match self.tab {
                Tab::General => general(ui, session, &mut out),
                Tab::Audio => out.audio = devices.body(ui, latency),
                Tab::Display => out.display_changed = crate::display::body(ui, display),
                Tab::Models => models.body(ui),
                Tab::Sources => egui::ScrollArea::vertical()
                    .max_height(480.0)
                    .show(ui, |ui| sounds.body(ui))
                    .inner,
            }
            ui.add_space(8.0);
            ui.separator();
            if ui.button("Close").clicked() {
                self.open = false;
            }
        });
        if modal.should_close() {
            self.open = false;
        }
        out
    }
}

fn general(ui: &mut egui::Ui, session: &mut Session, out: &mut Outcome) {
    let mut sandboxed = session.sandbox_plugins();
    if ui
        .checkbox(&mut sandboxed, "Run plugins in separate processes")
        .on_hover_text(
            "A plugin that crashes or hangs then plays silence and is started \
             again, instead of taking the app with it. Costs a little CPU for each \
             plugin. Editors open as before. Kept on this machine only.",
        )
        .changed()
    {
        session.set_sandbox_plugins(sandboxed);
        crate::prefs::remember_sandbox(sandboxed);
    }
    ui.add_space(8.0);
    ui.weak(
        "Your settings are saved on this machine. Export them to take to another machine \
         or to share; the audio devices and the plugin sandbox stay local.",
    );
    ui.horizontal(|ui| {
        out.import |= ui.button("Import User Settings…").clicked();
        out.export |= ui.button("Export User Settings…").clicked();
    });
}
