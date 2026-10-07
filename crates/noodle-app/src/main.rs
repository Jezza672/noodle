//! Noodle, the desktop app. `noodle-app [project.ron]` opens a project, or an
//! empty one.

mod app;
mod editor;
mod properties;
mod session;
mod theme;
mod widgets;

use std::process::ExitCode;

use crate::app::App;
use crate::session::{Nodes, Session};

fn main() -> ExitCode {
    let session = match std::env::args_os().nth(1) {
        Some(path) => match Session::open(Nodes::all(), path.as_ref()) {
            Ok(session) => session,
            Err(error) => {
                eprintln!("noodle: {error}");
                return ExitCode::FAILURE;
            }
        },
        None => Session::new(Nodes::all()),
    };
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
