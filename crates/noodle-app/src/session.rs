//! The open project and everything attached to it: its undo history, its file,
//! the diagnostics from compiling it, and the engine playing it.
//!
//! The UI never changes the project directly. Views return [`Edit`]s, and
//! [`Session::edit`] applies them through the history and keeps the audio in
//! step: structural changes recompile, and parameter changes go straight to
//! the engine's parameter cells.

use std::cell::Cell;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use noodle_core::{Command, EditError, FrameId, History, NodeId, Project, Tick};
use noodle_engine::{Controller, Diagnostic, Registry, Telemetry, TempoTable, compile_with_lanes};
use noodle_io::{AudioConfig, AudioError, DeviceError, DeviceErrorKind, Playback, Stream};
use noodle_nodes::{ClipFeeds, ClipProblem, Library};

/// Frames per block while playing: about 11 ms at 48 kHz.
const MAX_FRAMES: usize = 512;

/// More reroutes than this within [`REROUTE_WINDOW`] means the output is
/// flapping, and playback stops instead of restarting again.
const MAX_REROUTES: usize = 3;
const REROUTE_WINDOW: Duration = Duration::from_secs(10);

/// What [`Session::save`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    Yes,
    /// The write failed. The reason is in [`Session::message`].
    Failed,
    /// The project has no file yet, so the caller should ask for one.
    NoFile,
}

/// A change a view wants made.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// One undo step.
    Apply(Command),
    /// Part of a continuous gesture, such as dragging a node or a knob. Every
    /// `Drag` until the next [`Edit::EndDrag`] is one undo step.
    Drag(Command),
    EndDrag,
}

/// The node types a session can use, and the hub their telemetry (meters,
/// scopes) reports to.
pub struct Nodes {
    pub registry: Registry,
    pub telemetry: Telemetry,
    /// Where the track input nodes get their clips.
    pub clips: ClipFeeds,
}

impl Nodes {
    /// Every node type the app offers.
    pub fn all() -> Self {
        let mut registry = Registry::with_builtins();
        let Library { telemetry, clips } = noodle_nodes::register_library(&mut registry);
        Self {
            registry,
            telemetry,
            clips,
        }
    }
}

pub struct Session {
    project: Project,
    history: History,
    registry: Registry,
    telemetry: Telemetry,
    clips: ClipFeeds,
    /// Clips the track inputs couldn't schedule, as of the last feed.
    clip_problems: Vec<ClipProblem>,
    /// Where the project was loaded from or last saved to.
    path: Option<PathBuf>,
    /// The project as it was last saved or loaded, to tell whether it has
    /// unsaved changes.
    saved: Project,
    dirty: bool,
    /// The next ID [`Session::new_node_id`] can hand out. Views only get
    /// `&Session`, so it's a `Cell`.
    next_id: Cell<u64>,
    /// The same, for [`Session::new_frame_id`].
    next_frame_id: Cell<u64>,
    diagnostics: Vec<Diagnostic>,
    /// The device to play on.
    audio_config: AudioConfig,
    audio: Option<Audio>,
    reroutes: Reroutes,
    /// Something the user should know, such as a failed save, shown until the
    /// next one replaces it.
    message: Option<String>,
}

struct Audio {
    playback: Playback,
    controller: Controller,
    /// What the user has been told about the devices.
    monitor: Monitor,
}

/// Running totals of audible trouble since playback started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Glitches {
    /// The output device ran dry.
    underruns: u64,
    /// Input arrived late or early, or the engine wasn't taking it.
    input: u64,
}

impl Glitches {
    /// A message about whatever has grown since `self`, which then catches
    /// up to `now`.
    fn report(&mut self, now: Self) -> Option<String> {
        let mut parts = Vec::new();
        if now.underruns > self.underruns {
            parts.push(count(now.underruns, "underrun", "underruns"));
        }
        if now.input > self.input {
            parts.push(count(now.input, "input glitch", "input glitches"));
        }
        *self = now;
        (!parts.is_empty()).then(|| format!("{} since playback started", parts.join(", ")))
    }
}

impl Session {
    /// An empty, unsaved project.
    pub fn new(nodes: Nodes) -> Self {
        Self::with_project(nodes, Project::new(), None)
    }

    pub fn open(nodes: Nodes, path: &Path) -> Result<Self, FileError> {
        let project = load(path)?;
        Ok(Self::with_project(nodes, project, Some(path.to_owned())))
    }

    fn with_project(nodes: Nodes, project: Project, path: Option<PathBuf>) -> Self {
        let Nodes {
            registry,
            telemetry,
            clips,
        } = nodes;
        let mut session = Self {
            saved: Project::new(),
            project: Project::new(),
            history: History::new(),
            registry,
            telemetry,
            clips,
            clip_problems: Vec::new(),
            path: None,
            dirty: false,
            next_id: Cell::new(0),
            next_frame_id: Cell::new(0),
            diagnostics: Vec::new(),
            audio_config: AudioConfig::default(),
            audio: None,
            reroutes: Reroutes::default(),
            message: None,
        };
        session.replace(project, path);
        session
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Where meter and scope nodes report what they measure.
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    pub fn audio_config(&self) -> &AudioConfig {
        &self.audio_config
    }

    /// Chooses the device to play on. If playing, playback restarts there.
    pub fn set_audio_config(&mut self, config: AudioConfig) {
        self.audio_config = config;
        if self.audio.take().is_some() {
            self.play();
        }
    }

    /// The folder the project file is in, which audio files are relative to.
    pub fn directory(&self) -> Option<&Path> {
        self.path.as_deref()?.parent()
    }

    /// The window title's name for the project.
    pub fn name(&self) -> String {
        match &self.path {
            Some(path) => path.file_stem().map_or_else(
                || path.display().to_string(),
                |s| s.to_string_lossy().into(),
            ),
            None => "Untitled".into(),
        }
    }

    /// An ID for a node a view is about to add, e.g. by adding, duplicating
    /// or pasting. Each call gives a different ID, even before the nodes are
    /// added, so a view can wire up several new nodes in one batch. IDs that
    /// end up unused are harmless.
    pub fn new_node_id(&self) -> NodeId {
        let id = self.next_id.get().max(self.project.next_node_id().0);
        self.next_id.set(id + 1);
        NodeId(id)
    }

    /// Like [`new_node_id`](Self::new_node_id), for a frame.
    pub fn new_frame_id(&self) -> FrameId {
        let id = self.next_frame_id.get().max(self.project.next_frame_id().0);
        self.next_frame_id.set(id + 1);
        FrameId(id)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Problems found compiling the project, for the views to show where they
    /// happened.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Applies edits from a view. An edit that fails, e.g. because a node was
    /// removed in the meantime, is skipped and reported in the status bar.
    pub fn edit(&mut self, edits: impl IntoIterator<Item = Edit>) {
        let mut edits = edits.into_iter().peekable();
        if edits.peek().is_none() {
            return;
        }
        let mut effect = Effect::default();
        for edit in edits {
            let command = match edit {
                Edit::Apply(command) => {
                    self.history.end_group();
                    command
                }
                Edit::Drag(command) => {
                    self.history.begin_group();
                    command
                }
                Edit::EndDrag => {
                    self.history.end_group();
                    continue;
                }
            };
            let this = Effect::of(&command);
            match self.history.apply(&mut self.project, command) {
                Ok(()) => effect.merge(this),
                Err(error) => self.message = Some(format!("Couldn't edit: {error}")),
            }
        }
        if effect.structural {
            self.recompile();
        } else {
            if let Some(audio) = &mut self.audio {
                for (node, key, value) in effect.params {
                    audio.controller.set_param(node, &key, value);
                }
            }
            if effect.clips {
                self.feed_clips();
            }
        }
        self.update_dirty();
    }

    pub fn undo(&mut self) {
        self.step(History::undo, "undo");
    }

    pub fn redo(&mut self) {
        self.step(History::redo, "redo");
    }

    fn step(
        &mut self,
        step: fn(&mut History, &mut Project) -> Result<bool, EditError>,
        name: &str,
    ) {
        match step(&mut self.history, &mut self.project) {
            // Recompiling also writes the project's parameter values into the
            // engine, so undoing a parameter change reaches the audio.
            Ok(true) => {
                self.recompile();
                self.update_dirty();
            }
            Ok(false) => {}
            Err(error) => self.message = Some(format!("Couldn't {name}: {error}")),
        }
    }

    /// Saves to the project's own file.
    pub fn save(&mut self) -> Saved {
        match self.path.clone() {
            Some(path) if self.save_as(&path) => Saved::Yes,
            Some(_) => Saved::Failed,
            None => Saved::NoFile,
        }
    }

    /// Returns whether it saved. If not, the reason is in [`Self::message`].
    pub fn save_as(&mut self, path: &Path) -> bool {
        match write_atomically(path, &self.project.to_ron()) {
            Ok(()) => {
                self.path = Some(path.to_owned());
                self.saved = self.project.clone();
                self.dirty = false;
                self.message = Some(format!("Saved {}", path.display()));
                true
            }
            Err(error) => {
                self.message = Some(format!("Couldn't save {}: {error}", path.display()));
                false
            }
        }
    }

    /// Replaces the project with one from a file, keeping the current one if
    /// it can't be loaded. Playback carries on with the new project. Returns
    /// whether it loaded.
    pub fn load(&mut self, path: &Path) -> bool {
        match load(path) {
            Ok(project) => {
                self.replace(project, Some(path.to_owned()));
                true
            }
            Err(error) => {
                self.message = Some(error.to_string());
                false
            }
        }
    }

    /// Starts a new, empty project. Playback carries on.
    pub fn new_project(&mut self) {
        self.replace(Project::new(), None);
    }

    fn replace(&mut self, project: Project, path: Option<PathBuf>) {
        self.saved = project.clone();
        self.project = project;
        self.history = History::new();
        self.path = path;
        self.dirty = false;
        self.message = None;
        self.recompile();
        // A new project starts from its beginning, running, whatever the
        // last one's playhead was doing.
        if let Some(audio) = &self.audio {
            let transport = audio.controller.transport();
            transport.seek(Tick(0));
            transport.play();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.audio.is_some()
    }

    pub fn play(&mut self) {
        if self.audio.is_some() {
            return;
        }
        let mut started = noodle_io::play(&self.audio_config, MAX_FRAMES);
        let mut fell_back = None;
        // A saved device that's been unplugged, or a rate it no longer
        // takes, shouldn't stop the app making sound. The setting is kept
        // for when the device is back.
        if let Err(error) = &started
            && let Some(defaults) = fallback(&self.audio_config)
            && let Ok(playing) = noodle_io::play(&defaults, MAX_FRAMES)
        {
            fell_back = Some(on_default_output(error));
            started = Ok(playing);
        }
        match started {
            Ok((playback, controller)) => {
                // Playback carries on without input rather than failing.
                // The status bar keeps saying so; see `input_problem`.
                self.message = fell_back.or_else(|| playback.input_problem().map(no_input));
                self.audio = Some(Audio {
                    playback,
                    controller,
                    monitor: Monitor::default(),
                });
                self.recompile();
                if let Some(audio) = &mut self.audio {
                    audio.controller.set_tempo_map(self.project.tempo_map());
                }
            }
            Err(error) => self.message = Some(play_error(&error)),
        }
    }

    /// Why playback is going on without the input that was asked for. It
    /// lasts as long as playback, unlike [`Session::message`].
    pub fn input_problem(&self) -> Option<String> {
        let audio = self.audio.as_ref()?;
        match audio.playback.input_problem() {
            Some(problem) => Some(no_input(problem)),
            None => audio.monitor.input_lost.clone(),
        }
    }

    pub fn stop(&mut self) {
        self.audio = None;
    }

    /// Whether the transport is running. The audio stream can be open with
    /// the transport paused.
    pub fn transport_running(&self) -> bool {
        self.audio
            .as_ref()
            .is_some_and(|audio| audio.controller.transport().is_playing())
    }

    /// Pauses or resumes the timeline. Needs the stream open; does nothing
    /// otherwise.
    pub fn set_transport_running(&self, running: bool) {
        if let Some(audio) = &self.audio {
            let transport = audio.controller.transport();
            if running {
                transport.play();
            } else {
                transport.stop();
            }
        }
    }

    /// Moves the playhead to the start of the timeline.
    pub fn rewind(&self) {
        if let Some(audio) = &self.audio {
            audio.controller.transport().seek(Tick(0));
        }
    }

    /// The tempo map the audio thread is using, if the stream is open.
    #[cfg(test)]
    pub fn tempo_in_engine(&self) -> Option<&noodle_core::TempoMap> {
        self.audio
            .as_ref()
            .map(|audio| audio.controller.tempo_map())
    }

    /// Where the playhead is, if the stream is open.
    pub fn playhead(&self) -> Option<Tick> {
        let audio = self.audio.as_ref()?;
        let samples = audio.controller.transport().position();
        let rate = f64::from(audio.controller.settings().sample_rate);
        let tick = audio.controller.tempo_map().tick_at_sample(samples, rate);
        Some(Tick(tick.round() as i64))
    }

    /// Housekeeping to do every frame: frees plans the audio thread is done
    /// with, and checks the device's health.
    pub fn maintain(&mut self) {
        let Some(audio) = &mut self.audio else {
            return;
        };
        audio.controller.maintain();
        if audio.controller.tempo_map() != self.project.tempo_map() {
            audio.controller.set_tempo_map(self.project.tempo_map());
        }
        let health = audio.playback.health();
        let now = Glitches {
            underruns: health.underruns(),
            input: health.input_glitches(),
        };
        // Which error the backend sent decides what happens next, and
        // backends differ, so say what arrived.
        let errors: Vec<_> = health.errors().collect();
        for (stream, error) in &errors {
            eprintln!(
                "noodle: audio error on the {stream:?}: {:?} ({error})",
                error.kind()
            );
        }
        let check = audio.monitor.check(errors, now);
        if let Some(message) = check.message {
            self.message = Some(message);
        }
        if check.stopped {
            self.audio = None;
        } else if check.restart {
            if self.reroutes.allow(Instant::now()) {
                self.restart_on_new_output();
            } else {
                self.audio = None;
                self.message = Some("Playback stopped: the audio output keeps changing".into());
            }
        }
    }

    /// The output was rerouted, say when headphones are unplugged. Some
    /// backends reroute the stream by themselves but leave it silent, so
    /// start again on whatever is now the default, and say so.
    fn restart_on_new_output(&mut self) {
        self.audio = None;
        self.play();
        let Some(audio) = &self.audio else {
            // `play` has said why it couldn't.
            return;
        };
        let changed = format!(
            "Audio output changed: playing on {}",
            audio.playback.device()
        );
        self.message = Some(match self.message.take() {
            Some(also) => format!("{changed}. {also}"),
            None => changed,
        });
    }

    /// Clips the track inputs couldn't play, with the reason. Empty while
    /// nothing is playing, since nothing has been scheduled.
    pub fn clip_problems(&self) -> &[ClipProblem] {
        &self.clip_problems
    }

    /// What a track input is doing with its clips.
    #[cfg(test)]
    pub fn clip_status(&self, track: NodeId) -> noodle_nodes::ClipStatus {
        self.clips.status(track)
    }

    /// Schedules the project's clips on the track inputs, at the positions the
    /// tempo map gives. Only matters while a stream is open.
    fn feed_clips(&mut self) {
        let Some(audio) = &self.audio else {
            self.clip_problems.clear();
            return;
        };
        let rate = audio.controller.settings().sample_rate;
        let table = TempoTable::new(self.project.tempo_map(), rate);
        let base = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        self.clip_problems = self.clips.update(&self.project, &table, rate as u32, base);
    }

    fn recompile(&mut self) {
        self.diagnostics = match &mut self.audio {
            Some(audio) => audio
                .controller
                .update_project(&self.project, &self.registry),
            None => {
                let lanes: Vec<_> = self.project.lanes().collect();
                compile_with_lanes(self.project.graph(), &lanes, &self.registry).1
            }
        };
        self.feed_clips();
    }

    fn update_dirty(&mut self) {
        self.dirty = self.project != self.saved;
    }
}

/// What a command means for the engine.
#[derive(Default, Debug, PartialEq)]
struct Effect {
    /// Needs a recompile.
    structural: bool,
    /// Parameter values the engine can take without recompiling.
    params: Vec<(NodeId, String, f32)>,
    /// Clips or the tempo map changed, so the track inputs need their
    /// schedules again. The graph is unchanged.
    clips: bool,
}

impl Effect {
    fn of(command: &Command) -> Self {
        let mut effect = Self::default();
        effect.add(command);
        effect
    }

    fn add(&mut self, command: &Command) {
        match command {
            // Changes nothing the engine sees.
            Command::MoveNode { .. }
            | Command::AddFrame { .. }
            | Command::RemoveFrame { .. }
            | Command::SetFrame { .. } => {}
            Command::SetParam {
                node,
                key,
                value: Some(value),
            } => self.params.push((*node, key.clone(), *value)),
            Command::AddClip { .. }
            | Command::RemoveClip { .. }
            | Command::SetClip { .. }
            | Command::SetTempoMap(_) => self.clips = true,
            Command::Batch(commands) => commands.iter().for_each(|c| self.add(c)),
            // Includes resetting a parameter to its default, which needs the
            // default from the node type; recompiling reads it.
            _ => self.structural = true,
        }
    }

    fn merge(&mut self, other: Self) {
        self.structural |= other.structural;
        self.clips |= other.clips;
        self.params.extend(other.params);
    }
}

fn play_error(error: &AudioError) -> String {
    format!("Can't play: {error}")
}

fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What the user has been told about the devices since playback started.
#[derive(Debug, Default)]
struct Monitor {
    /// The counts last reported.
    reported: Glitches,
    /// Why the input stopped after playback began. The output carries on.
    input_lost: Option<String>,
}

/// When playback last restarted after a reroute, so a flapping output can't
/// make it restart on every frame.
#[derive(Debug, Default)]
struct Reroutes {
    recent: Vec<Instant>,
}

impl Reroutes {
    /// Whether a restart at `now` is allowed, and if so, counts it.
    fn allow(&mut self, now: Instant) -> bool {
        self.recent
            .retain(|&at| now.saturating_duration_since(at) < REROUTE_WINDOW);
        if self.recent.len() >= MAX_REROUTES {
            return false;
        }
        self.recent.push(now);
        true
    }
}

/// What a check of the devices found.
#[derive(Debug, Default, PartialEq, Eq)]
struct Check {
    /// Something to tell the user.
    message: Option<String>,
    /// Playback has stopped for good.
    stopped: bool,
    /// The output was rerouted: playback should restart on the new device.
    restart: bool,
}

impl Monitor {
    /// Takes the errors reported since the last check, and the running
    /// glitch totals.
    fn check(
        &mut self,
        errors: impl IntoIterator<Item = (Stream, DeviceError)>,
        mut now: Glitches,
    ) -> Check {
        let verdict = judge(errors);
        let mut message = verdict.message;
        // Said once when it happens, and the first cause is kept; the status
        // bar keeps "No input" up.
        if let Some(lost) = verdict.input_lost
            && self.input_lost.is_none()
        {
            message = Some(lost.clone());
            self.input_lost = Some(lost);
        }
        // A lost input is already reported, and every block after it would
        // count as another glitch.
        if self.input_lost.is_some() {
            now.input = self.reported.input;
        }
        if let Some(glitches) = self.reported.report(now) {
            message = Some(glitches);
        }
        let stopped = verdict.stopped.is_some();
        if stopped {
            message = verdict.stopped;
        }
        Check {
            message,
            stopped,
            // There's no device to restart on if the output was lost.
            restart: verdict.output_changed && !stopped,
        }
    }
}

/// What a batch of device errors means for playback.
#[derive(Debug, Default, PartialEq, Eq)]
struct Verdict {
    /// An output error that stopped playback for good.
    stopped: Option<String>,
    /// An input error that stopped the input. The output plays on.
    input_lost: Option<String>,
    /// A survivable error worth telling the user about.
    message: Option<String>,
    /// The output was rerouted to another device. The stream may not have
    /// carried on, so playback should start afresh on the new one.
    output_changed: bool,
}

/// Sorts errors by what they mean: only the output stream's fatal errors
/// stop playback, since Input nodes just go quiet without their input.
fn judge(errors: impl IntoIterator<Item = (Stream, DeviceError)>) -> Verdict {
    let mut verdict = Verdict::default();
    for (stream, error) in errors {
        match (stream, noodle_io::is_fatal(&error)) {
            // The first error is the cause; a backend may follow it with more.
            (Stream::Output, true) => {
                verdict
                    .stopped
                    .get_or_insert_with(|| format!("Playback stopped: {error}"));
            }
            (Stream::Input, true) => {
                verdict
                    .input_lost
                    .get_or_insert_with(|| format!("Input lost: {error}"));
            }
            (Stream::Output, false) if error.kind() == DeviceErrorKind::DeviceChanged => {
                verdict.output_changed = true;
            }
            (_, false) => verdict.message = Some(error.to_string()),
        }
    }
    verdict
}

fn no_input(error: &AudioError) -> String {
    format!("Playing without input: {error}")
}

fn on_default_output(error: &AudioError) -> String {
    format!("Playing on the default output: {error}")
}

/// What to try when the chosen output settings can't play: the system's
/// defaults, keeping the input. `None` if that's what was tried.
fn fallback(config: &AudioConfig) -> Option<AudioConfig> {
    let defaults = AudioConfig {
        input: config.input.clone(),
        ..AudioConfig::default()
    };
    (defaults != *config).then_some(defaults)
}

#[derive(Debug)]
pub enum FileError {
    Read(PathBuf, std::io::Error),
    Load(PathBuf, Box<noodle_core::LoadError>),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(path, error) => write!(f, "Can't read {}: {error}", path.display()),
            Self::Load(path, error) => write!(f, "Can't load {}: {error}", path.display()),
        }
    }
}

impl std::error::Error for FileError {}

/// Writes to a temporary file next to `path`, then renames it into place, so
/// a failed save (a full disk, say) never destroys the last good copy.
///
/// Saving through a symlink writes the file it points to and keeps the link,
/// and the file keeps its permissions.
pub(crate) fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    // A path that doesn't exist yet can't be resolved; it's created as it is.
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let mut temp = path.as_os_str().to_owned();
    temp.push(".saving");
    let temp = PathBuf::from(temp);
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        std::io::Write::write_all(&mut file, contents.as_bytes())?;
        if let Ok(old) = std::fs::metadata(path) {
            file.set_permissions(old.permissions())?;
        }
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn load(path: &Path) -> Result<Project, FileError> {
    let text =
        std::fs::read_to_string(path).map_err(|error| FileError::Read(path.to_owned(), error))?;
    Project::from_ron(&text).map_err(|error| FileError::Load(path.to_owned(), Box::new(error)))
}

#[cfg(test)]
mod tests {
    use noodle_core::{Connection, Endpoint, Node, Position};
    use noodle_engine::OUTPUT_ID;

    use super::*;

    fn error(kind: noodle_io::DeviceErrorKind) -> DeviceError {
        kind.into()
    }

    #[test]
    fn a_fatal_input_error_loses_the_input_but_not_the_playback() {
        use noodle_io::DeviceErrorKind::*;
        let verdict = judge([(Stream::Input, error(DeviceNotAvailable))]);
        assert_eq!(verdict.stopped, None);
        assert!(verdict.input_lost.unwrap().starts_with("Input lost: "));
        // Playback only stops for the output.
        let verdict = judge([
            (Stream::Input, error(BackendError)),
            (Stream::Output, error(DeviceNotAvailable)),
        ]);
        assert!(verdict.stopped.unwrap().starts_with("Playback stopped: "));
        assert!(verdict.input_lost.is_some());
    }

    #[test]
    fn the_first_fatal_error_is_the_cause() {
        use noodle_io::DeviceErrorKind::*;
        let verdict = judge([
            (Stream::Input, error(DeviceNotAvailable)),
            (Stream::Input, error(BackendError)),
            (Stream::Output, error(PermissionDenied)),
            (Stream::Output, error(BackendError)),
        ]);
        let said = |kind, prefix| format!("{prefix}: {}", error(kind));
        assert_eq!(
            verdict.input_lost,
            Some(said(DeviceNotAvailable, "Input lost"))
        );
        assert_eq!(
            verdict.stopped,
            Some(said(PermissionDenied, "Playback stopped"))
        );
    }

    fn glitches(underruns: u64, input: u64) -> Glitches {
        Glitches { underruns, input }
    }

    #[test]
    fn a_lost_input_is_reported_once_and_playback_goes_on() {
        use noodle_io::DeviceErrorKind::*;
        let mut monitor = Monitor::default();
        let check = monitor.check([(Stream::Input, error(DeviceNotAvailable))], glitches(0, 0));
        assert!(!check.stopped);
        assert!(check.message.unwrap().starts_with("Input lost: "));
        let cause = monitor.input_lost.clone().unwrap();

        // A follow-up error neither repeats the message nor replaces the cause.
        let check = monitor.check([(Stream::Input, error(BackendError))], glitches(0, 0));
        assert_eq!(check, Check::default());
        assert_eq!(monitor.input_lost, Some(cause));
    }

    #[test]
    fn after_the_input_is_lost_its_glitches_stop_being_reported() {
        use noodle_io::DeviceErrorKind::*;
        let mut monitor = Monitor::default();
        // Before the loss, input glitches are reported as usual.
        let check = monitor.check([], glitches(0, 2));
        assert!(check.message.unwrap().contains("2 input glitches"));
        monitor.check([(Stream::Input, error(DeviceNotAvailable))], glitches(0, 2));

        // Every block now comes up dry, but that's the loss, not new news.
        let check = monitor.check([], glitches(0, 500));
        assert_eq!(check, Check::default());
        // Underruns are still the output's business.
        let check = monitor.check([], glitches(3, 900));
        let message = check.message.unwrap();
        assert!(message.contains("3 underruns"), "{message}");
        assert!(!message.contains("input glitch"), "{message}");
        assert!(!check.stopped);
    }

    #[test]
    fn only_a_lost_output_stops_playback() {
        use noodle_io::DeviceErrorKind::*;
        let mut monitor = Monitor::default();
        let check = monitor.check(
            [
                (Stream::Input, error(DeviceNotAvailable)),
                (Stream::Output, error(DeviceNotAvailable)),
            ],
            glitches(0, 0),
        );
        assert!(check.stopped);
        assert!(check.message.unwrap().starts_with("Playback stopped: "));
    }

    #[test]
    fn a_rerouted_output_restarts_playback_but_a_rerouted_input_does_not() {
        use noodle_io::DeviceErrorKind::*;
        let mut monitor = Monitor::default();
        let check = monitor.check([(Stream::Input, error(DeviceChanged))], glitches(0, 0));
        assert!(!check.restart);
        assert!(check.message.is_some(), "still worth a message");

        // However many times it's rerouted, it restarts once.
        let check = monitor.check(
            [
                (Stream::Output, error(DeviceChanged)),
                (Stream::Output, error(DeviceChanged)),
            ],
            glitches(0, 0),
        );
        assert!(check.restart);
        assert!(!check.stopped);
    }

    #[test]
    fn a_flapping_output_stops_being_restarted() {
        let start = Instant::now();
        let mut reroutes = Reroutes::default();
        let second = Duration::from_secs(1);
        for i in 0..MAX_REROUTES as u32 {
            assert!(reroutes.allow(start + second * i), "restart {i}");
        }
        assert!(!reroutes.allow(start + second * MAX_REROUTES as u32));
        // Once the window has passed, an ordinary reroute is fine again.
        assert!(reroutes.allow(start + REROUTE_WINDOW + second * MAX_REROUTES as u32));
    }

    #[test]
    fn no_restart_when_the_output_is_lost_for_good() {
        use noodle_io::DeviceErrorKind::*;
        let mut monitor = Monitor::default();
        let check = monitor.check(
            [
                (Stream::Output, error(DeviceChanged)),
                (Stream::Output, error(DeviceNotAvailable)),
            ],
            glitches(0, 0),
        );
        assert!(check.stopped);
        assert!(!check.restart);
    }

    #[test]
    fn survivable_errors_only_get_a_message() {
        use noodle_io::DeviceErrorKind::*;
        for stream in [Stream::Output, Stream::Input] {
            let verdict = judge([(stream, error(RealtimeDenied))]);
            assert_eq!(verdict.stopped, None);
            assert_eq!(verdict.input_lost, None);
            assert!(!verdict.output_changed);
            assert!(verdict.message.is_some());
        }
        assert_eq!(judge([]), Verdict::default());
    }

    #[cfg(unix)]
    #[test]
    fn saving_through_a_symlink_keeps_the_link_and_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.ron");
        let link = dir.path().join("link.ron");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_atomically(&link, "new").unwrap();

        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[test]
    fn falling_back_keeps_the_input_and_drops_the_output_choices() {
        let chosen = AudioConfig {
            host: Some("jack".into()),
            output: Some("jack:system".into()),
            input: noodle_io::InputChoice::Default,
            sample_rate: Some(96_000),
            buffer_size: Some(64),
        };
        assert_eq!(
            fallback(&chosen),
            Some(AudioConfig {
                input: noodle_io::InputChoice::Default,
                ..AudioConfig::default()
            })
        );
        let defaults = AudioConfig {
            input: noodle_io::InputChoice::Default,
            ..AudioConfig::default()
        };
        assert_eq!(fallback(&defaults), None);
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noodle-app-{}-{name}", std::process::id()))
    }

    /// Adds a node as one undo step.
    fn add(session: &mut Session, node: Node) -> NodeId {
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode { id, node })]);
        id
    }

    fn param(session: &Session, node: NodeId, key: &str) -> Option<f32> {
        session
            .project()
            .graph()
            .node(node)?
            .params
            .get(key)
            .copied()
    }

    #[test]
    fn edits_can_be_undone_and_redone() {
        let mut session = Session::new(Nodes::all());
        let sine = add(&mut session, Node::new("noodle.osc.sine"));
        assert!(session.can_undo());
        session.undo();
        assert!(session.project().graph().node(sine).is_none());
        session.redo();
        assert!(session.project().graph().node(sine).is_some());
    }

    #[test]
    fn a_nan_parameter_does_not_keep_the_project_dirty() {
        let mut session = Session::new(Nodes::all());
        let sine = add(
            &mut session,
            Node::new("noodle.osc.sine").with_param("frequency", f32::NAN),
        );
        let path = temp("nan.ron");
        session.save_as(&path);
        assert!(!session.is_dirty());
        session.edit([Edit::Drag(Command::SetParam {
            node: sine,
            key: "frequency".into(),
            value: Some(1.0),
        })]);
        session.edit([Edit::EndDrag]);
        assert!(session.is_dirty());
        session.undo();
        assert!(!session.is_dirty(), "undone back to the saved state");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_drag_is_one_undo_step() {
        let mut session = Session::new(Nodes::all());
        let sine = add(&mut session, Node::new("noodle.osc.sine"));
        let set = |value| {
            Edit::Drag(Command::SetParam {
                node: sine,
                key: "frequency".into(),
                value: Some(value),
            })
        };
        session.edit([set(100.0), set(200.0)]);
        session.edit([set(300.0), Edit::EndDrag]);
        assert_eq!(param(&session, sine, "frequency"), Some(300.0));

        session.undo();
        assert_eq!(param(&session, sine, "frequency"), None);
        assert!(session.project().graph().node(sine).is_some());
    }

    #[test]
    fn diagnostics_follow_structural_edits() {
        let mut session = Session::new(Nodes::all());
        assert!(session.diagnostics().is_empty());
        add(&mut session, Node::new("no.such.type"));
        assert_eq!(session.diagnostics().len(), 1);
        session.undo();
        assert!(session.diagnostics().is_empty());
    }

    #[test]
    fn a_failed_edit_is_reported_and_skipped() {
        let mut session = Session::new(Nodes::all());
        session.edit([Edit::Apply(Command::RemoveNode { id: NodeId(42) })]);
        assert!(
            session
                .message()
                .is_some_and(|m| m.contains("Couldn't edit"))
        );
        assert!(!session.can_undo());
    }

    #[test]
    fn tracks_unsaved_changes() {
        let path = temp("dirty.ron");
        let mut session = Session::new(Nodes::all());
        assert!(!session.is_dirty());
        assert_eq!(session.save(), Saved::NoFile);

        let sine = add(&mut session, Node::new("noodle.osc.sine"));
        assert!(session.is_dirty());
        session.save_as(&path);
        assert!(!session.is_dirty());
        assert_eq!(
            session.name(),
            format!("noodle-app-{}-dirty", std::process::id())
        );

        session.edit([Edit::Apply(Command::MoveNode {
            node: sine,
            position: Position { x: 10.0, y: 0.0 },
        })]);
        assert!(session.is_dirty());
        // Back to how it was saved.
        session.undo();
        assert!(!session.is_dirty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn saved_projects_load_back() {
        let path = temp("round-trip.ron");
        let mut session = Session::new(Nodes::all());
        let sine = add(&mut session, Node::new("noodle.osc.sine"));
        let output = add(&mut session, Node::new(OUTPUT_ID));
        session.edit([Edit::Apply(Command::Connect(Connection {
            from: Endpoint::new(sine, "out"),
            to: Endpoint::new(output, "in"),
        }))]);
        session.save_as(&path);

        let opened = Session::open(Nodes::all(), &path).unwrap();
        assert_eq!(opened.project(), session.project());
        assert!(!opened.is_dirty() && !opened.can_undo());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_bad_file_keeps_the_current_project() {
        let path = temp("bad.ron");
        std::fs::write(&path, "not a project").unwrap();
        let mut session = Session::new(Nodes::all());
        let sine = add(&mut session, Node::new("noodle.osc.sine"));
        assert!(!session.load(&path));
        assert!(session.project().graph().node(sine).is_some());
        assert!(session.message().is_some_and(|m| m.contains("Can't load")));
        assert!(Session::open(Nodes::all(), &path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn node_ids_are_unique_even_before_use() {
        let mut session = Session::new(Nodes::all());
        let existing = add(&mut session, Node::new("noodle.osc.sine"));
        let (a, b) = (session.new_node_id(), session.new_node_id());
        assert!(a != b && a != existing && b != existing);
        let (f, g) = (session.new_frame_id(), session.new_frame_id());
        assert_ne!(f, g);

        // Both added in one step, wired together.
        session.edit([Edit::Apply(Command::Batch(vec![
            Command::AddNode {
                id: b,
                node: Node::new("noodle.osc.sine"),
            },
            Command::AddNode {
                id: a,
                node: Node::new(OUTPUT_ID),
            },
            Command::Connect(Connection {
                from: Endpoint::new(b, "out"),
                to: Endpoint::new(a, "in"),
            }),
        ]))]);
        assert_eq!(session.project().graph().nodes().count(), 3);

        // A removed node's ID isn't handed out again, so undoing the removal
        // can't clash with a node added since.
        session.edit([Edit::Apply(Command::RemoveNode { id: b })]);
        let c = session.new_node_id();
        assert!(c != a && c != b && c != existing);
    }

    #[test]
    fn a_new_project_starts_clean() {
        let mut session = Session::new(Nodes::all());
        add(&mut session, Node::new("no.such.type"));
        session.new_project();
        assert_eq!(session.project(), &Project::new());
        assert!(!session.is_dirty() && !session.can_undo());
        assert!(session.diagnostics().is_empty());
    }

    #[test]
    fn effects_of_commands() {
        let node = NodeId(1);
        let moved = Command::MoveNode {
            node,
            position: Position::default(),
        };
        let set = |value| Command::SetParam {
            node,
            key: "gain".into(),
            value,
        };
        let param = |value| (node, "gain".to_owned(), value);

        assert_eq!(Effect::of(&moved), Effect::default());
        assert_eq!(
            Effect::of(&set(Some(-6.0))),
            Effect {
                structural: false,
                params: vec![param(-6.0)],
                clips: false,
            }
        );
        // Parameters in a batch still skip the recompile.
        assert_eq!(
            Effect::of(&Command::Batch(vec![
                moved.clone(),
                set(Some(-6.0)),
                Command::Batch(vec![set(Some(-3.0))]),
            ])),
            Effect {
                structural: false,
                params: vec![param(-6.0), param(-3.0)],
                clips: false,
            }
        );
        assert!(Effect::of(&set(None)).structural);
        // Clips and the tempo map only need the track inputs' schedules.
        let tempo = Command::SetTempoMap(noodle_core::TempoMap::default());
        assert_eq!(
            Effect::of(&tempo),
            Effect {
                clips: true,
                ..Effect::default()
            }
        );
        assert!(
            Effect::of(&Command::RemoveClip {
                id: noodle_core::ClipId(1)
            })
            .clips
        );
        assert!(
            Effect::of(&Command::Batch(vec![
                moved,
                Command::RemoveNode { id: node }
            ]))
            .structural
        );
    }

    #[test]
    fn saving_replaces_the_file_whole() {
        let path = temp("atomic.ron");
        std::fs::write(&path, "old").unwrap();
        let mut session = Session::new(Nodes::all());
        session.save_as(&path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            session.project().to_ron()
        );
        let mut leftover = path.clone().into_os_string();
        leftover.push(".saving");
        assert!(!PathBuf::from(leftover).exists());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_failed_save_keeps_the_old_file() {
        // Saving into a directory that doesn't exist fails before touching
        // anything.
        let path = temp("missing-dir").join("project.ron");
        let mut session = Session::new(Nodes::all());
        add(&mut session, Node::new("noodle.osc.sine"));
        session.save_as(&path);
        assert!(session.is_dirty());
        assert!(
            session
                .message()
                .is_some_and(|m| m.contains("Couldn't save"))
        );
    }

    #[test]
    fn choosing_a_device_while_stopped_does_not_play() {
        let mut session = Session::new(Nodes::all());
        let config = AudioConfig {
            output: Some("missing".into()),
            ..AudioConfig::default()
        };
        session.set_audio_config(config.clone());
        assert_eq!(session.audio_config(), &config);
        assert!(!session.is_playing());
        assert_eq!(session.message(), None);
    }

    #[test]
    fn glitches_are_reported_once_each_time_they_grow() {
        let mut reported = Glitches::default();
        assert_eq!(reported.report(Glitches::default()), None);
        let one = Glitches {
            underruns: 1,
            input: 0,
        };
        assert_eq!(
            reported.report(one).as_deref(),
            Some("1 underrun since playback started")
        );
        let underruns = Glitches {
            underruns: 2,
            input: 0,
        };
        assert_eq!(
            reported.report(underruns).as_deref(),
            Some("2 underruns since playback started")
        );
        assert_eq!(reported.report(underruns), None, "already reported");
        let both = Glitches {
            underruns: 3,
            input: 1,
        };
        assert_eq!(
            reported.report(both).as_deref(),
            Some("3 underruns, 1 input glitch since playback started")
        );
        let input = Glitches {
            underruns: 3,
            input: 4,
        };
        assert_eq!(
            reported.report(input).as_deref(),
            Some("4 input glitches since playback started")
        );
    }
}
