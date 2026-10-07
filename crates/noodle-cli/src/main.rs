//! `noodle`: command-line tools for Noodle projects.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime};

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
    let project = load(path)?;
    let registry = registry();
    let (playback, mut controller) =
        noodle_io::play(MAX_FRAMES, |error| eprintln!("noodle: {error}"))
            .map_err(|error| error.to_string())?;
    report(&controller.update(project.graph(), &registry));
    let settings = playback.settings();
    eprintln!(
        "Playing {} on {} ({} Hz, {} channels). Press Ctrl-C to stop.",
        path.display(),
        playback.device(),
        settings.sample_rate,
        settings.channels,
    );

    let mut watch = Watch::new(path);
    loop {
        thread::sleep(POLL);
        controller.maintain();
        if !watch.changed() {
            continue;
        }
        // A bad edit keeps the last good version playing.
        match load(path) {
            Ok(project) => {
                report(&controller.update(project.graph(), &registry));
                eprintln!("Reloaded {}.", path.display());
            }
            Err(message) => eprintln!("noodle: {message}"),
        }
    }
}

fn report(diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        eprintln!("warning: {diagnostic}");
    }
}

/// Notices when a file is modified, by polling its modification time. That's
/// coarse but needs nothing platform-specific, and it catches editors that
/// save by replacing the file.
struct Watch {
    path: PathBuf,
    modified: Option<SystemTime>,
}

impl Watch {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            modified: modified(path),
        }
    }

    /// Whether the file has changed since the last call, or since `new`.
    fn changed(&mut self) -> bool {
        let modified = modified(&self.path);
        let changed = modified != self.modified;
        self.modified = modified;
        changed
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
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

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    #[test]
    fn a_watch_notices_changes_once() {
        let path = std::env::temp_dir().join("noodle-cli-watch-test.ron");
        let file = File::create(&path).unwrap();
        let start = SystemTime::now();
        file.set_modified(start).unwrap();
        let mut watch = Watch::new(&path);
        assert!(!watch.changed());

        file.set_modified(start + Duration::from_secs(1)).unwrap();
        assert!(watch.changed());
        assert!(!watch.changed());

        std::fs::remove_file(&path).unwrap();
        assert!(watch.changed(), "a deleted file has changed");
        assert!(!watch.changed());
    }
}
