//! `noodle`: command-line tools for Noodle projects.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand};
use noodle_core::Project;
use noodle_engine::{Diagnostic, Registry, Settings, render};

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
    /// Play a project on the default audio device until Ctrl-C. Saving the
    /// file while it plays swaps the new version in.
    Play {
        /// The project file (.ron).
        project: PathBuf,
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
            report(&rendered.diagnostics);
            noodle_io::write_wav(&output, &rendered.samples, channels, sample_rate)
                .map_err(|error| format!("can't write {}: {error}", output.display()))
        }
        Command::Play { project } => play(&project),
    }
}

/// Frames per block for live playback: about 11 ms at 48 kHz.
const MAX_FRAMES: usize = 512;
/// How often to check the project file for changes.
const POLL: Duration = Duration::from_millis(100);

fn play(path: &Path) -> Result<(), String> {
    let text = read(path)?;
    let project = parse(path, &text)?;
    // Watch from the text that was loaded, so an edit made while the device
    // opens isn't missed.
    let mut watch = Watch::new(path, text);
    let registry = registry();
    let (mut playback, mut controller) =
        noodle_io::play(MAX_FRAMES).map_err(|error| error.to_string())?;
    report(&controller.update(project.graph(), &registry));
    let settings = playback.settings();
    eprintln!(
        "Playing {} on {} ({} Hz, {} channels). Press Ctrl-C to stop.",
        path.display(),
        playback.device(),
        settings.sample_rate,
        settings.channels,
    );

    let mut underruns = 0;
    loop {
        thread::sleep(POLL);
        controller.maintain();

        let health = playback.health();
        for error in health.errors() {
            if noodle_io::is_fatal(&error) {
                return Err(format!("playback stopped: {error}"));
            }
            eprintln!("warning: {error}");
        }
        // Summarised, since a struggling machine can underrun constantly.
        let total = health.underruns();
        if total > underruns {
            eprintln!("warning: {} underruns", total - underruns);
            underruns = total;
        }

        if let Some(text) = watch.changed() {
            // A bad edit keeps the last good version playing.
            match parse(path, &text) {
                Ok(project) => {
                    report(&controller.update(project.graph(), &registry));
                    eprintln!("Reloaded {}.", path.display());
                }
                Err(message) => eprintln!("noodle: {message}"),
            }
        }
    }
}

fn report(diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        eprintln!("warning: {diagnostic}");
    }
}

/// Notices when a file's contents change, by reading it. Project files are
/// small, and comparing contents rather than modification times catches
/// saves that land within the timestamp resolution, such as a save that
/// finishes after a half-written version was read.
struct Watch {
    path: PathBuf,
    seen: String,
}

impl Watch {
    fn new(path: &Path, seen: String) -> Self {
        Self {
            path: path.to_owned(),
            seen,
        }
    }

    /// The file's contents, if they've changed since the last call. A file
    /// that can't be read, e.g. mid-save, counts as unchanged.
    fn changed(&mut self) -> Option<String> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        if text == self.seen {
            return None;
        }
        self.seen.clone_from(&text);
        Some(text)
    }
}

fn load(path: &Path) -> Result<Project, String> {
    parse(path, &read(path)?)
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("can't read {}: {error}", path.display()))
}

fn parse(path: &Path, text: &str) -> Result<Project, String> {
    Project::from_ron(text).map_err(|error| format!("can't load {}: {error}", path.display()))
}

fn registry() -> Registry {
    let mut registry = Registry::with_builtins();
    noodle_nodes::register_all(&mut registry);
    registry
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn a_watch_notices_each_change_once() {
        let path = std::env::temp_dir().join(format!("noodle-watch-{}.ron", std::process::id()));
        fs::write(&path, "one").unwrap();
        let mut watch = Watch::new(&path, "one".into());
        assert_eq!(watch.changed(), None);

        // Same length and, on a coarse filesystem, the same timestamp.
        fs::write(&path, "two").unwrap();
        assert_eq!(watch.changed().as_deref(), Some("two"));
        assert_eq!(watch.changed(), None);

        fs::remove_file(&path).unwrap();
        assert_eq!(watch.changed(), None, "a missing file is mid-save");
        fs::write(&path, "two").unwrap();
        assert_eq!(watch.changed(), None, "back as it was");
        fs::remove_file(&path).unwrap();
    }
}
