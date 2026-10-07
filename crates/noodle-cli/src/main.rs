//! `noodle`: command-line tools for Noodle projects.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use noodle_core::Project;
use noodle_engine::{Registry, Settings, render};

#[derive(Parser)]
#[command(
    name = "noodle",
    version,
    about = "Command-line tools for Noodle projects"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render a project to a 32-bit float WAV file, without an audio device.
    Render {
        /// The project file (.ron).
        project: PathBuf,
        /// The WAV file to write.
        output: PathBuf,
        /// How long to render, in seconds.
        #[arg(long, default_value_t = 5.0)]
        seconds: f64,
        #[arg(long, default_value_t = 48_000)]
        sample_rate: u32,
        #[arg(long, default_value_t = 2)]
        channels: usize,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("noodle: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Render {
            project,
            output,
            seconds,
            sample_rate,
            channels,
        } => {
            if !(seconds.is_finite() && seconds >= 0.0) {
                return Err(format!("can't render {seconds} seconds"));
            }
            let project = load(&project)?;
            let settings = Settings {
                sample_rate: sample_rate as f32,
                max_frames: 512,
                channels,
            };
            // `as` saturates, so a huge length would quietly be truncated.
            // `usize::MAX as f64` rounds up to 2^64, which doesn't fit.
            let frames = (seconds * f64::from(sample_rate)).round();
            if frames >= usize::MAX as f64 {
                return Err(format!("can't render {seconds} seconds: too long"));
            }
            let frames = frames as usize;
            let rendered = render(project.graph(), &registry(), settings, frames)
                .map_err(|error| error.to_string())?;
            for diagnostic in &rendered.diagnostics {
                eprintln!("warning: {diagnostic}");
            }
            noodle_io::write_wav(&output, &rendered.samples, channels, sample_rate)
                .map_err(|error| format!("can't write {}: {error}", output.display()))
        }
    }
}

fn load(path: &Path) -> Result<Project, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("can't read {}: {error}", path.display()))?;
    Project::from_ron(&text).map_err(|error| format!("can't load {}: {error}", path.display()))
}

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    noodle_nodes::register_all(&mut registry);
    registry
}
