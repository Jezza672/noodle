//! Noodle, the desktop app. `noodle-app [project.ron]` opens a project, or an
//! empty one. This is the desktop shell: it opens the window with `eframe`
//! and runs `noodle_app::App` in it.

use std::process::ExitCode;

use noodle_app::app::App;
use noodle_app::session::{Nodes, Session};
use noodle_app::{autosave, prefs, theme};

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
    if let Some(dir) = prefs::library_dir() {
        session.set_library(noodle_core::library::Library::open(Some(dir)));
    }
    session.set_templates_dir(prefs::templates_dir());
    let mut engine_on = true;
    if let Some(path) = prefs::default_path() {
        let prefs = prefs::init(path);
        engine_on = prefs.engine_on;
        session.set_audio_config(prefs.audio.clone());
        session.set_sandbox_plugins(prefs.sandbox_plugins);
        session.set_monitor_device(&prefs.monitor_device);
        session.apply_user_settings(&prefs);
    }
    // The engine runs from the start, as in Ableton, so live MIDI plays
    // before anything else is done. The toolbar turns it off, and stays off
    // until it is turned on again.
    if engine_on {
        session.start_engine();
    }
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
            let ctx = cc.egui_ctx.clone();
            // For the life of the process.
            std::mem::forget(noodle_engine::wake::register(std::sync::Arc::new(
                move || ctx.request_repaint(),
            )));
            let mut app = App::new(session);
            if let Some(dir) = autosave::default_dir() {
                app.set_autosave(autosave::Autosave::new(dir));
            }
            Ok(Box::new(app))
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
