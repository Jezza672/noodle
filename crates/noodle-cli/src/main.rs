//! `noodle`: command-line tools for Noodle projects.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand};
use noodle_core::Project;
use noodle_engine::{Diagnostic, Registry, Settings, render};
use noodle_io::{AudioConfig, InputChoice, Stream};

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
    /// Play a project on an audio device until Ctrl-C. Saving the file
    /// while it plays swaps the new version in.
    Play {
        /// The project file (.ron).
        project: PathBuf,
        #[command(flatten)]
        device: DeviceArgs,
    },
    /// List the audio hosts, and the devices on one, with their IDs for
    /// `play`.
    Devices {
        /// The host to list (default: the platform's default host).
        #[arg(long)]
        host: Option<String>,
    },
}

/// Which device to play on, and how. Each defaults to the system's choice.
#[derive(clap::Args)]
struct DeviceArgs {
    /// The audio host, by ID (see `noodle devices`).
    #[arg(long)]
    host: Option<String>,
    /// The output device, by ID (see `noodle devices`).
    #[arg(long)]
    output: Option<String>,
    /// Record into Input nodes from this device, by ID, or `default`.
    /// Off unless given.
    #[arg(long)]
    input: Option<String>,
    #[arg(long)]
    sample_rate: Option<u32>,
    /// Frames per device callback.
    #[arg(long)]
    buffer_size: Option<u32>,
}

impl DeviceArgs {
    fn config(self) -> AudioConfig {
        AudioConfig {
            host: self.host,
            output: self.output,
            input: match self.input.as_deref() {
                None => InputChoice::Off,
                Some("default") => InputChoice::Default,
                Some(id) => InputChoice::Device(id.to_owned()),
            },
            sample_rate: self.sample_rate,
            buffer_size: self.buffer_size,
        }
    }
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
        Command::Play { project, device } => play(&project, &device.config()),
        Command::Devices { host } => list_devices(host.as_deref()),
    }
}

fn list_devices(host: Option<&str>) -> Result<(), String> {
    println!("Hosts:");
    for info in noodle_io::hosts() {
        let default = if info.is_default { " (default)" } else { "" };
        println!("  {}: {}{default}", info.id, info.name);
    }
    let list = noodle_io::devices(host).map_err(|error| error.to_string())?;
    for (title, devices) in [("Outputs", &list.outputs), ("Inputs", &list.inputs)] {
        println!("{title}:");
        if devices.is_empty() {
            println!("  none");
        }
        for device in devices {
            let default = if device.is_default { " (default)" } else { "" };
            let caps = &device.capabilities;
            println!("  {}{default}", device.name);
            println!("    id: {}", device.id);
            println!("    channels: up to {}", caps.max_channels);
            let rates: Vec<String> = caps.sample_rates.iter().map(u32::to_string).collect();
            println!("    sample rates: {}", rates.join(", "));
            if let Some((min, max)) = caps.buffer_sizes {
                println!("    buffer sizes: {min} to {max} frames");
            }
        }
    }
    Ok(())
}

/// Frames per block for live playback: about 11 ms at 48 kHz.
const MAX_FRAMES: usize = 512;
/// How often to check the project file for changes.
const POLL: Duration = Duration::from_millis(100);

fn play(path: &Path, config: &AudioConfig) -> Result<(), String> {
    let text = read(path)?;
    let project = parse(path, &text)?;
    // Watch from the text that was loaded, so an edit made while the device
    // opens isn't missed.
    let mut watch = Watch::new(path, text);
    let registry = registry();
    let (mut playback, mut controller) =
        noodle_io::play(config, MAX_FRAMES).map_err(|error| error.to_string())?;
    report(&controller.update(project.graph(), &registry));
    let settings = playback.settings();
    eprintln!(
        "Playing {} on {} ({} Hz, {} channels). Press Ctrl-C to stop.",
        path.display(),
        playback.device(),
        settings.sample_rate,
        settings.channels,
    );
    if let Some((device, channels)) = playback.input() {
        eprintln!("Recording from {device} ({channels} channels).");
    }
    if let Some(problem) = playback.input_problem() {
        eprintln!("warning: playing without input: {problem}");
    }

    let mut underruns = 0;
    let mut input_glitches = 0;
    loop {
        thread::sleep(POLL);
        controller.maintain();

        let health = playback.health();
        for (stream, error) in health.errors() {
            match (stream, noodle_io::is_fatal(&error)) {
                (Stream::Output, true) => return Err(format!("playback stopped: {error}")),
                // The sound goes on; Input nodes just go quiet.
                (Stream::Input, true) => eprintln!("warning: input lost: {error}"),
                (_, false) => eprintln!("warning: {error}"),
            }
        }
        // Summarised, since a struggling machine can underrun constantly.
        let total = health.underruns();
        if total > underruns {
            eprintln!("warning: {} underruns", total - underruns);
            underruns = total;
        }
        let total = health.input_glitches();
        if total > input_glitches {
            eprintln!("warning: {} input glitches", total - input_glitches);
            input_glitches = total;
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
    // Nothing displays meters or scopes yet, so their hub isn't needed.
    let _telemetry = noodle_nodes::register_all(&mut registry);
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
