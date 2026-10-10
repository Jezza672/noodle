//! `noodle`: command-line tools for Noodle projects.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand};
use noodle_core::Project;
use noodle_engine::{Diagnostic, Job, Registry, Settings, StreamError};
use noodle_io::{AudioConfig, InputChoice, Stream};
use noodle_nodes::{RenderRequest, render_streaming};

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
            midi_input: None,
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
            let project_path = project;
            let project = load(&project_path)?;
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
            // Clip files are looked up next to the project file.
            let base = project_path.parent().unwrap_or(Path::new(""));
            let request = RenderRequest {
                project,
                base: base.to_owned(),
                settings,
                frames,
                extend_registry: None,
            };
            let mut wav = noodle_io::WavStreamWriter::create(&output, channels, sample_rate)
                .map_err(|error| format!("can't write {}: {error}", output.display()))?;
            // Rendered in chunks on a background thread, so a long render
            // needn't fit in memory and can report how far it has got.
            let mut job = Job::spawn(move |progress| {
                let report = render_streaming(&request, progress, |chunk| wav.write(chunk))?;
                wav.finish().map_err(StreamError::Sink)?;
                Ok::<_, StreamError<noodle_io::WavError>>(report)
            });
            let mut shown = 0;
            let rendered = loop {
                if let Some(done) = job.poll() {
                    break done.map_err(|error| error.to_string())?;
                }
                let percent = (job.fraction() * 100.0) as u32;
                if percent >= shown + 10 {
                    shown = percent / 10 * 10;
                    eprintln!("rendering: {shown}%");
                }
                thread::sleep(Duration::from_millis(20));
            }
            .map_err(|error| format!("can't render to {}: {error}", output.display()))?;
            for problem in &rendered.problems {
                eprintln!("warning: clip {}: {}", problem.clip.0, problem.message);
            }
            for error in &rendered.errors {
                eprintln!("warning: can't read {error}");
            }
            if rendered.underruns > 0 {
                eprintln!(
                    "warning: {} blocks of clip audio could not be read",
                    rendered.underruns
                );
            }
            report(&rendered.diagnostics);
            Ok(())
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
    // Output nodes tied to other devices play on those too. The devices are
    // opened once, so a reload that changes them takes a restart.
    let mut devices = noodle_engine::output_devices(project.graph());
    let (mut playback, mut controller) = noodle_io::play_with_outputs(config, &devices, MAX_FRAMES)
        .map_err(|error| error.to_string())?;
    for status in playback.outputs() {
        match &status.result {
            Ok(opened) => eprintln!(
                "Also playing on {} ({} channels).",
                opened.name, opened.channels
            ),
            Err(error) => eprintln!("warning: can't play on {}: {error}", status.device),
        }
    }
    report(&controller.update_project(&project, &registry));
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
    let mut input_lost = false;
    loop {
        thread::sleep(POLL);
        controller.maintain();

        let health = playback.health();
        for (stream, error) in health.errors() {
            match (stream, noodle_io::is_fatal(&error)) {
                (Stream::Output, true) => return Err(format!("playback stopped: {error}")),
                // The sound goes on; Input nodes just go quiet.
                (Stream::Input, true) if !input_lost => {
                    eprintln!("warning: input lost: {error}");
                    input_lost = true;
                }
                // A backend may follow the cause with more.
                (Stream::Input, true) => {}
                // An extra device failing silences only its own outputs.
                (Stream::ExtraOutput, true) => eprintln!("warning: output device lost: {error}"),
                (_, false) => eprintln!("warning: {error}"),
            }
        }
        // Summarised, since a struggling machine can underrun constantly.
        let total = health.underruns();
        if total > underruns {
            eprintln!("warning: {} underruns", total - underruns);
            underruns = total;
        }
        // Once the input is lost, every block comes up dry and counts again.
        let total = health.input_glitches();
        if total > input_glitches && !input_lost {
            eprintln!("warning: {} input glitches", total - input_glitches);
            input_glitches = total;
        }

        if let Some(text) = watch.changed() {
            // A bad edit keeps the last good version playing.
            match parse(path, &text) {
                Ok(project) => {
                    let wanted = noodle_engine::output_devices(project.graph());
                    if wanted != devices {
                        eprintln!("warning: restart to play on the outputs' new devices.");
                        devices = wanted;
                    }
                    report(&controller.update_project(&project, &registry));
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
    fn render_plays_a_projects_audio_clips_from_next_to_the_project_file() {
        use noodle_core::{
            Clip, Command as Edit, Connection, Endpoint, History, Node, NodeId, Tick,
        };

        let dir = std::env::temp_dir().join(format!("noodle-render-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let samples = vec![0.25f32; 2 * 48_000];
        noodle_io::write_wav(&dir.join("tone.wav"), &samples, 2, 48_000).unwrap();
        let mut project = Project::new();
        let mut history = History::new();
        let (track, out) = (NodeId(1), NodeId(2));
        for (id, kind) in [(track, "noodle.track.input"), (out, "noodle.io.output")] {
            let node = Node::new(kind);
            history
                .apply(&mut project, Edit::AddNode { id, node })
                .unwrap();
        }
        let connection = Connection {
            from: Endpoint::new(track, "audio"),
            to: Endpoint::new(out, "in"),
        };
        history
            .apply(&mut project, Edit::Connect(connection))
            .unwrap();
        let id = project.new_clip_id();
        let clip = Clip::audio(track, Tick(0), "tone.wav", 48_000);
        history
            .apply(&mut project, Edit::AddClip { id, clip })
            .unwrap();
        fs::write(dir.join("song.ron"), project.to_ron()).unwrap();

        let output = dir.join("out.wav");
        run(Command::Render {
            project: dir.join("song.ron"),
            output: output.clone(),
            seconds: 1.0,
            sample_rate: 48_000,
            channels: 2,
        })
        .unwrap();
        let audio = noodle_io::decode_file(&output).unwrap();
        // The clip fades in over a few milliseconds, then holds its level.
        let held = &audio.samples[2 * 1_000..2 * 47_000];
        assert!(
            held.iter().all(|&s| (s - 0.25).abs() < 1e-6),
            "the clip isn\'t in the render"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

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
