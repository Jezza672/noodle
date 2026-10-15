//! Noodle, the desktop app. `noodle-app [project.ron]` opens a project, or an
//! empty one.

#[cfg(test)]
mod acceptance;
mod app;
mod clip_editor;
mod clip_panel;
mod devices;
mod editor;
mod export_dialog;
mod filewatch;
mod freezing;
mod keyboard;
mod metronome;
mod midi_take;
mod mixer;
mod outputs;
mod piano_roll;
mod pill_buttons;
mod plugin_browser;
mod prefs;
mod properties;
mod session;
mod snap;
mod tempo;
#[cfg(test)]
mod testkit;
mod theme;
mod timeline;
mod toolbar;
mod widgets;

use std::process::ExitCode;

use crate::app::App;
use crate::session::{Nodes, Session};

/// The window's context, for waking the UI thread when another thread asks
/// something of the plugins it hosts.
static UI: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

fn main() -> ExitCode {
    // Plugins are scanned in a child process, which is this program started
    // with a hidden argument.
    noodle_plugins::run_scan_child_if_requested();
    // Plugin editors need the UI thread on macOS (Cocoa) and a thread that
    // pumps messages on Windows, so there it hosts the plugins: `App::update`
    // runs what other threads ask of them. Elsewhere they get a thread of
    // their own.
    if cfg!(any(target_os = "macos", windows)) {
        noodle_plugins::adopt_current_thread(|| {
            if let Some(ctx) = UI.get() {
                ctx.request_repaint();
            }
        });
    }
    let mut session = match std::env::args_os().nth(1) {
        Some(path) => match Session::open(Nodes::all_with_plugins(), path.as_ref()) {
            Ok(session) => session,
            Err(error) => {
                eprintln!("noodle: {error}");
                return ExitCode::FAILURE;
            }
        },
        None => Session::new(Nodes::all_with_plugins()),
    };
    if let Some(path) = prefs::default_path() {
        let prefs = prefs::init(path);
        session.set_audio_config(prefs.audio);
        session.set_sandbox_plugins(prefs.sandbox_plugins);
    }
    // The engine runs from the start, as in Ableton, so live MIDI plays
    // before anything else is done. The toolbar turns it off.
    session.start_engine();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Noodle")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Noodle",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            let _ = UI.set(cc.egui_ctx.clone());
            Ok(Box::new(App::new(session)))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("noodle: {error}");
            ExitCode::FAILURE
        }
    }
}
