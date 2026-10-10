//! The open project and everything attached to it: its undo history, its file,
//! the diagnostics from compiling it, and the engine playing it.
//!
//! The UI never changes the project directly. Views return [`Edit`]s, and
//! [`Session::edit`] applies them through the history and keeps the audio in
//! step: structural changes recompile, and parameter changes go straight to
//! the engine's parameter cells.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod export;
pub use export::ExportChoice;

use crate::filewatch::FileWatch;
use crate::freezing::{Badge, Freezing, Look, Polled};
use noodle_core::{Clip, Command, EditError, FrameId, History, NodeId, Project, Tick};
use noodle_engine::{
    Controller, Diagnostic, Registry, Settings, Telemetry, TempoTable, compile_replacing,
    compile_with_lanes,
};
use noodle_io::{
    AudioConfig, AudioError, DeviceError, DeviceErrorKind, Playback, RecordError, Recorder, Stream,
    Take,
};
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
    /// Where the MIDI In nodes get their messages.
    pub midi: noodle_io::MidiBus,
}

impl Nodes {
    /// Every node type the app offers.
    pub fn all() -> Self {
        let mut registry = Registry::with_builtins();
        let Library {
            telemetry,
            clips,
            midi,
        } = noodle_nodes::register_library(&mut registry);
        Self {
            registry,
            telemetry,
            clips,
            midi,
        }
    }
}

/// How often to look for the chosen MIDI port coming or going.
pub const MIDI_WATCH_INTERVAL: Duration = Duration::from_secs(1);

type ListMidi = Box<dyn FnMut() -> Result<Vec<String>, noodle_io::MidiError>>;
type OpenMidi =
    fn(&str, &noodle_io::MidiBus) -> Result<noodle_io::MidiConnection, noodle_io::MidiError>;

/// What `watch_midi` needs from the system, replaceable in tests.
struct MidiWatch {
    checked: Option<Instant>,
    list: ListMidi,
    open: OpenMidi,
}

impl Default for MidiWatch {
    fn default() -> Self {
        // One client for all the looks, made the first time.
        let mut lister: Option<noodle_io::MidiLister> = None;
        Self {
            checked: None,
            list: Box::new(move || {
                if lister.is_none() {
                    lister = Some(noodle_io::MidiLister::new()?);
                }
                lister.as_ref().expect("just made").names()
            }),
            open: noodle_io::connect_midi,
        }
    }
}

pub struct Session {
    project: Project,
    history: History,
    registry: Registry,
    telemetry: Telemetry,
    clips: ClipFeeds,
    /// Where the MIDI input port's messages go, to reach the MIDI In nodes.
    midi: noodle_io::MidiBus,
    /// The open MIDI input port, if one is chosen and opened.
    midi_connection: Option<noodle_io::MidiConnection>,
    /// Watches for the chosen MIDI port being unplugged and plugged back in.
    midi_watch: MidiWatch,
    /// Watches the audio files clips play for being replaced on disk.
    file_watch: FileWatch,
    /// Clips the track inputs couldn't schedule, as of the last feed.
    clip_problems: Vec<ClipProblem>,
    /// The renders behind frozen nodes and offline nodes.
    freezing: Freezing,
    /// Where the playhead is while no stream is open, so it can be set and
    /// read when stopped, and playing starts from it.
    parked: Tick,
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
    /// The tracks that record when the record button is pressed.
    armed: BTreeSet<NodeId>,
    /// The take being recorded, if there is one.
    recording: Option<Recording>,
    /// Stands in for the input stream's recorder in tests.
    #[cfg(test)]
    fake_input: Option<Recorder>,
    /// Something the user should know, such as a failed save, shown until the
    /// next one replaces it.
    message: Option<String>,
    /// The export under way, if there is one.
    exporting: Option<export::Run>,
}

/// The settings frozen audio is rendered with when no stream is open.
const FREEZE_SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 512,
    channels: 2,
};
/// Seconds a render runs past the last clip, for tails.
const FREEZE_TAIL: f32 = 4.0;
/// The shortest render in seconds, for projects with little or no timeline.
const FREEZE_MINIMUM: f32 = 30.0;

/// A take being recorded.
struct Recording {
    /// The tracks that get a clip of it.
    tracks: Vec<NodeId>,
    /// Where the take starts on the timeline.
    start: Tick,
    /// The file, relative to the project file.
    relative: String,
}

/// Something that can record the input to a file.
trait TakeSink {
    fn start(&mut self, path: &Path) -> Result<(), RecordError>;
    fn stop(&mut self) -> Result<Take, RecordError>;
}

impl TakeSink for Playback {
    fn start(&mut self, path: &Path) -> Result<(), RecordError> {
        self.start_recording(path)
    }
    fn stop(&mut self) -> Result<Take, RecordError> {
        self.stop_recording()
    }
}

impl TakeSink for Recorder {
    fn start(&mut self, path: &Path) -> Result<(), RecordError> {
        Recorder::start(self, path)
    }
    fn stop(&mut self) -> Result<Take, RecordError> {
        Recorder::stop(self)
    }
}

/// A free file name for the next take, in a folder beside the project file
/// named after it. Returns the path and the same relative to the project.
fn next_take_path(project: &Path) -> std::io::Result<(PathBuf, String)> {
    let dir = project.parent().unwrap_or(Path::new("."));
    let stem = project
        .file_stem()
        .map_or_else(|| "project".into(), |s| s.to_string_lossy().into_owned());
    let folder = format!("{stem} recordings");
    std::fs::create_dir_all(dir.join(&folder))?;
    let mut number = 1;
    loop {
        let relative = format!("{folder}/take-{number:03}.wav");
        let path = dir.join(&relative);
        if !path.exists() {
            return Ok((path, relative));
        }
        number += 1;
    }
}

struct Audio {
    playback: Playback,
    controller: Controller,
    /// The devices the project's Output nodes are tied to, which playback
    /// was started with. Playback restarts when the project asks for others.
    devices: Vec<String>,
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

    pub(crate) fn with_project(nodes: Nodes, project: Project, path: Option<PathBuf>) -> Self {
        let Nodes {
            registry,
            telemetry,
            clips,
            midi,
        } = nodes;
        let mut session = Self {
            saved: Project::new(),
            project: Project::new(),
            history: History::new(),
            registry,
            telemetry,
            clips,
            midi,
            midi_connection: None,
            midi_watch: MidiWatch::default(),
            file_watch: FileWatch::default(),
            exporting: None,
            clip_problems: Vec::new(),
            freezing: Freezing::new(),
            parked: Tick(0),
            path: None,
            dirty: false,
            next_id: Cell::new(0),
            next_frame_id: Cell::new(0),
            diagnostics: Vec::new(),
            audio_config: AudioConfig::default(),
            audio: None,
            reroutes: Reroutes::default(),
            armed: BTreeSet::new(),
            recording: None,
            #[cfg(test)]
            fake_input: None,
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

    /// How each extra output device fared, when playing: the devices the
    /// project's Output nodes are tied to, and why any didn't open.
    pub fn output_devices(&self) -> &[noodle_io::OutputStatus] {
        self.audio
            .as_ref()
            .map_or(&[], |audio| audio.playback.outputs())
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
        let without_midi = |c: &AudioConfig| AudioConfig {
            midi_input: None,
            ..c.clone()
        };
        let audio_changed = without_midi(&self.audio_config) != without_midi(&config);
        let midi_changed = self.audio_config.midi_input != config.midi_input;
        self.audio_config = config;
        if midi_changed {
            self.connect_midi();
        }
        if audio_changed && self.audio.is_some() {
            self.close_stream();
            self.play();
        }
    }

    /// Holds `key` down on the track input `node` (or lets it go): for
    /// trying the notes of a clip on the piano roll's keyboard through the
    /// synth the track feeds. It sounds whether or not the transport runs.
    pub fn audition(&self, node: NodeId, key: u8, on: bool) {
        self.clips.audition(node, key, on);
    }

    /// The MIDI input port that is open, if any.
    #[cfg(test)]
    pub fn midi_input(&self) -> Option<&str> {
        self.midi_connection.as_ref().map(|c| c.name())
    }

    /// Opens the chosen MIDI input port, closing the one before. A port that
    /// won't open leaves none open and says why.
    fn connect_midi(&mut self) {
        self.close_midi();
        let Some(name) = self.audio_config.midi_input.clone() else {
            return;
        };
        match noodle_io::connect_midi(&name, &self.midi) {
            Ok(connection) => self.midi_connection = Some(connection),
            Err(error) => self.message = Some(format!("MIDI input: {error}")),
        }
    }

    /// Whether a MIDI port is chosen, so the window should look at the
    /// ports now and then even when nothing else makes it redraw.
    pub fn watching_midi(&self) -> bool {
        self.audio_config.midi_input.is_some()
    }

    /// Lets go of the MIDI port, releasing the notes it held down (they
    /// would never see their note-offs).
    fn close_midi(&mut self) {
        if self.midi_connection.take().is_some() {
            self.midi.all_notes_off();
        }
    }

    /// The chosen MIDI port can be unplugged and plugged back in, and the
    /// drivers don't say when, so look at the list of ports about once a
    /// second: let go of the port when it is gone, and open it again when it
    /// returns (or when it was missing from the start). Ports are matched by
    /// name without ALSA's numbers, which change when a device comes back.
    /// An unplug and replug between two looks that leaves the port's name
    /// as it was isn't seen.
    fn watch_midi(&mut self, now: Instant) {
        let Some(name) = self.audio_config.midi_input.clone() else {
            return;
        };
        if self
            .midi_watch
            .checked
            .is_some_and(|at| now.duration_since(at) < MIDI_WATCH_INTERVAL)
        {
            return;
        }
        self.midi_watch.checked = Some(now);
        let Ok(ports) = (self.midi_watch.list)() else {
            return;
        };
        if self
            .midi_connection
            .as_ref()
            .is_some_and(|c| !ports.iter().any(|p| p == c.name()))
        {
            self.close_midi();
            self.message = Some(format!("MIDI input {name:?} was unplugged"));
        }
        if self.midi_connection.is_none()
            && ports.iter().any(|p| noodle_io::same_port(p, &name))
            // Quietly when it fails: a port that is listed but won't open
            // would otherwise say so every second.
            && let Ok(connection) = (self.midi_watch.open)(&name, &self.midi)
        {
            self.midi_connection = Some(connection);
            self.message = Some(format!("MIDI input {name:?} is connected again"));
        }
    }

    /// Looks about once a second at whether an audio file a clip plays has
    /// been replaced on disk (re-exported from another program, say). If so,
    /// the clips are scheduled again so their streams read the new file, and
    /// anything frozen or offline that depends on it is keyed afresh, which
    /// renders it again.
    fn watch_files(&mut self, now: Instant) {
        let base = self.base().to_owned();
        let files = self
            .project
            .clips()
            .filter_map(|(_, clip)| match &clip.content {
                noodle_core::ClipContent::Audio(audio) => Some(base.join(&audio.source)),
                noodle_core::ClipContent::Midi(_) => None,
            });
        let mut changed = self.file_watch.changed(now, files);
        if changed.is_empty() {
            return;
        }
        changed.sort();
        changed.dedup();
        let names: Vec<String> = changed
            .iter()
            .filter_map(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
            .collect();
        self.message = Some(format!("{} changed on disk", names.join(", ")));
        self.feed_clips();
        if Freezing::in_use(&self.project, &self.registry) {
            self.recompile();
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

    /// Shows the user `text` until the next message replaces it.
    pub fn notify(&mut self, text: String) {
        self.message = Some(text);
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
                for (node, key, value) in &effect.params {
                    audio.controller.set_param(*node, key, *value);
                }
            }
            if effect.clips {
                self.feed_clips();
            }
            // A parameter or clip upstream of a frozen node makes its render
            // stale. Its key changes, so compiling again takes the stale audio
            // off at once and plays that part live. The new render waits for
            // the editing to pause.
            if (effect.clips || !effect.params.is_empty())
                && Freezing::in_use(&self.project, &self.registry)
            {
                self.freezing.touch();
                self.recompile_with(false);
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
                // Relative clip paths resolve against the file's directory.
                self.path = Some(path.to_owned());
                self.feed_clips();
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
        // The take belongs to the project it was recorded in.
        self.stop_recording();
        self.saved = project.clone();
        self.project = project;
        self.history = History::new();
        self.path = path;
        self.dirty = false;
        self.message = None;
        self.parked = Tick(0);
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
        // Output nodes tied to other devices play on those as well.
        let devices = noodle_engine::output_devices(self.project.graph());
        let mut started = noodle_io::play_with_outputs(&self.audio_config, &devices, MAX_FRAMES);
        let mut fell_back = None;
        // A saved device that's been unplugged, or a rate it no longer
        // takes, shouldn't stop the app making sound. The setting is kept
        // for when the device is back.
        if let Err(error) = &started
            && let Some(defaults) = fallback(&self.audio_config)
            && let Ok(playing) = noodle_io::play_with_outputs(&defaults, &devices, MAX_FRAMES)
        {
            fell_back = Some(on_default_output(error));
            started = Ok(playing);
        }
        match started {
            Ok((playback, mut controller)) => {
                controller.set_telemetry(&self.telemetry);
                // Playback carries on without input rather than failing.
                // The status bar keeps saying so; see `input_problem`.
                self.message = fell_back
                    .or_else(|| playback.input_problem().map(no_input))
                    .or_else(|| output_problems(&playback));
                self.audio = Some(Audio {
                    playback,
                    controller,
                    devices,
                    monitor: Monitor::default(),
                });
                self.recompile();
                if let Some(audio) = &mut self.audio {
                    audio.controller.set_tempo_map(self.project.tempo_map());
                    // Carry on from where the playhead was left.
                    if self.parked != Tick(0) {
                        audio.controller.transport().seek(self.parked);
                    }
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

    /// Closes the stream, keeping the playhead for the next play. Stopping
    /// again while stopped rewinds, as in Logic and GarageBand.
    pub fn stop(&mut self) {
        self.stop_recording();
        if self.audio.is_none() {
            self.parked = Tick(0);
        }
        self.close_stream();
        self.feed_clips();
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
    pub fn set_transport_running(&mut self, running: bool) {
        // A take is laid down against the running timeline, so pausing it
        // ends the take.
        if !running {
            self.stop_recording();
        }
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
    pub fn rewind(&mut self) {
        self.seek(Tick(0));
    }

    /// Moves the playhead. Works stopped too: playing starts from there.
    pub fn seek(&mut self, tick: Tick) {
        // The clip would start where the take began, no longer where the
        // audio is on the timeline.
        self.stop_recording();
        let tick = Tick(tick.0.max(0));
        self.parked = tick;
        if let Some(audio) = &self.audio {
            audio.controller.transport().seek(tick);
        }
    }

    /// Closes the stream, keeping the playhead where it was.
    fn close_stream(&mut self) {
        // The take so far is kept, since the stream it came from is going.
        if self.recording.is_some() {
            self.stop_recording();
            let kept = "Recording stopped and the take so far was kept";
            self.message = Some(match self.message.take() {
                Some(also) => format!("{also}. {kept}"),
                None => kept.into(),
            });
        }
        self.parked = self.playhead();
        self.audio = None;
    }

    /// The tempo map the audio thread is using, if the stream is open.
    #[cfg(all(test, target_os = "linux"))]
    pub fn tempo_in_engine(&self) -> Option<&noodle_core::TempoMap> {
        self.audio
            .as_ref()
            .map(|audio| audio.controller.tempo_map())
    }

    /// Where the playhead is: the transport's while playing, else where it
    /// was left or set.
    pub fn playhead(&self) -> Tick {
        let Some(audio) = &self.audio else {
            return self.parked;
        };
        let samples = audio.controller.transport().position();
        let rate = f64::from(audio.controller.settings().sample_rate);
        let tick = audio.controller.tempo_map().tick_at_sample(samples, rate);
        Tick(tick.round() as i64)
    }

    /// Housekeeping to do every frame: frees plans the audio thread is done
    /// with, and checks the device's health.
    pub fn maintain(&mut self) {
        match self.freezing.poll() {
            Polled::Nothing => {
                if self.freezing.due() {
                    self.recompile();
                }
            }
            Polled::Finished => self.recompile(),
            Polled::Failed(why) => {
                self.message = Some(format!("Couldn't render: {why}"));
                self.recompile();
            }
        }
        self.watch_midi(Instant::now());
        self.watch_files(Instant::now());
        self.poll_export();
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
            self.close_stream();
        } else if check.restart {
            if self.reroutes.allow(Instant::now()) {
                self.restart_on_new_output();
            } else {
                self.close_stream();
                self.message = Some("Playback stopped: the audio output keeps changing".into());
            }
        }
    }

    /// The output was rerouted, say when headphones are unplugged. Some
    /// backends reroute the stream by themselves but leave it silent, so
    /// start again on whatever is now the default, and say so.
    fn restart_on_new_output(&mut self) {
        self.close_stream();
        self.play();
        let Some(audio) = &self.audio else {
            // `play` has said why it couldn't.
            self.feed_clips();
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

    /// Arms or disarms a track (by its track input node) for recording.
    pub fn arm(&mut self, track: NodeId, on: bool) {
        if on {
            self.armed.insert(track);
        } else {
            self.armed.remove(&track);
        }
    }

    pub fn is_armed(&self, track: NodeId) -> bool {
        self.armed.contains(&track)
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// What records the input: the stream's recorder, or a stand-in in tests.
    fn sink(&mut self) -> Option<&mut dyn TakeSink> {
        #[cfg(test)]
        if let Some(fake) = &mut self.fake_input {
            return Some(fake);
        }
        self.audio
            .as_mut()
            .map(|audio| &mut audio.playback as &mut dyn TakeSink)
    }

    /// Starts recording the input into a new audio file next to the project,
    /// from the playhead, on every armed track. Starts playing if it isn't.
    /// The take becomes a clip on each armed track when it ends; see
    /// [`Session::stop_recording`].
    pub fn record(&mut self) {
        if self.recording.is_some() {
            return;
        }
        let tracks: Vec<NodeId> = self
            .armed
            .iter()
            .copied()
            .filter(|&track| self.project.graph().node(track).is_some())
            .collect();
        if tracks.is_empty() {
            self.message = Some("Arm a track to record on (R on its header)".into());
            return;
        }
        let Some(project_path) = self.path.clone() else {
            self.message = Some("Save the project first: takes are stored next to it".into());
            return;
        };
        #[cfg(test)]
        let faked = self.fake_input.is_some();
        #[cfg(not(test))]
        let faked = false;
        if !faked {
            self.play();
            if self.audio.is_none() {
                // `play` has said why.
                return;
            }
            self.set_transport_running(true);
        }
        let start = self.playhead();
        let (path, relative) = match next_take_path(&project_path) {
            Ok(found) => found,
            Err(error) => {
                self.message = Some(format!("Couldn't make a place for the take: {error}"));
                return;
            }
        };
        let started = match self.sink() {
            Some(sink) => sink.start(&path),
            None => Err(RecordError::NoInput),
        };
        match started {
            Ok(()) => {
                self.recording = Some(Recording {
                    tracks,
                    start,
                    relative,
                });
                self.message = None;
            }
            Err(error) => self.message = Some(format!("Couldn't record: {error}")),
        }
    }

    /// Ends the take and adds it to the project: one audio clip on each track
    /// that was armed, all in one undo step. Does nothing if not recording.
    pub fn stop_recording(&mut self) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        let taken = match self.sink() {
            Some(sink) => sink.stop(),
            None => Err(RecordError::NotRecording),
        };
        let take = match taken {
            Ok(take) => take,
            Err(error) => {
                self.message = Some(format!("The recording failed: {error}"));
                return;
            }
        };
        if take.frames == 0 {
            let _ = std::fs::remove_file(&take.path);
            self.message = Some("Nothing was recorded".into());
            return;
        }
        let tracks: Vec<NodeId> = recording
            .tracks
            .iter()
            .copied()
            .filter(|&track| self.project.graph().node(track).is_some())
            .collect();
        if tracks.is_empty() {
            // An empty batch would be an undo step that does nothing.
            self.message = Some(format!(
                "The armed tracks were deleted, so the take was kept as {}",
                recording.relative
            ));
            return;
        }
        let commands = tracks
            .into_iter()
            .map(|track| Command::AddClip {
                id: self.project.new_clip_id(),
                clip: Clip::audio(
                    track,
                    recording.start,
                    recording.relative.clone(),
                    take.frames,
                ),
            })
            .collect();
        self.edit([Edit::Apply(Command::Batch(commands))]);
        self.message = Some(match take.dropped {
            0 => format!("Recorded {}", recording.relative),
            lost => format!(
                "Recorded {}, but {lost} frames of input were lost",
                recording.relative
            ),
        });
    }

    /// Clips the track inputs couldn't play, with the reason. Empty while
    /// nothing is playing, since nothing has been scheduled.
    pub fn clip_problems(&self) -> &[ClipProblem] {
        &self.clip_problems
    }

    /// What a track input is doing with its clips.
    #[cfg(all(test, target_os = "linux"))]
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

    /// The folder the project's audio files are relative to.
    fn base(&self) -> &Path {
        self.path
            .as_deref()
            .and_then(Path::parent)
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
    }

    /// Where the timeline ends, in frames at `rate`: the last clip,
    /// automation point or tempo change, so nothing that is set up on it is
    /// cut off. Looks at every audio file.
    fn timeline_end(&self, rate: f32) -> u64 {
        let table = TempoTable::new(self.project.tempo_map(), rate);
        let marks = self
            .project
            .lanes()
            .flat_map(|(_, lane)| lane.points.iter().map(|p| p.tick))
            .chain(self.project.tempo_map().tempos().iter().map(|t| t.tick))
            .map(|tick| table.sample_at_tick(tick))
            .max()
            .unwrap_or(0);
        self.clips
            .project_frames(&self.project, &table, rate as u32, self.base())
            .max(marks)
    }

    /// How long the renders of frozen and offline nodes run: the timeline
    /// and a tail, and at least `FREEZE_MINIMUM`. Playback and export use
    /// the same length, so they use the same renders.
    fn freeze_frames(&self, rate: f32) -> usize {
        (self.timeline_end(rate) as usize + (FREEZE_TAIL * rate) as usize)
            .max((FREEZE_MINIMUM * rate) as usize)
    }

    /// Looks at the cache of renders for the project and starts the ones it
    /// lacks. Returns what plays in place of the cached nodes, if anything is
    /// frozen or offline.
    fn refresh_freezing(&mut self, start: bool) -> Option<noodle_nodes::FreezePlan> {
        // Frozen audio is keyed by the rate it was rendered at, so use the
        // stream's, or a common one before a stream is open.
        let settings = self
            .audio
            .as_ref()
            .map_or(FREEZE_SETTINGS, |audio| audio.controller.settings());
        let rate = settings.sample_rate;
        let base = self.base().to_owned();
        // Only measured when something needs it: it looks at every audio file.
        let frames = if Freezing::in_use(&self.project, &self.registry) {
            self.freeze_frames(rate)
        } else {
            0
        };
        let look = Look {
            project: &self.project,
            registry: &self.registry,
            settings,
            base: &base,
            frames,
            start,
        };
        match self.freezing.refresh(&look) {
            Ok(plan) => plan,
            Err(message) => {
                self.message = Some(message);
                None
            }
        }
    }

    /// Whether the cache of renders wants another look soon, so the app
    /// should keep running even if nothing else moves.
    pub fn freeze_pending(&self) -> bool {
        self.freezing.progress().is_some()
            || self.freezing.is_waiting()
            || self.freezing.is_hashing()
    }

    /// What to show on frozen and offline nodes.
    pub fn freeze_badges(&self) -> &std::collections::BTreeMap<NodeId, Badge> {
        self.freezing.badges()
    }

    /// Keeps renders in `dir` instead of the user's cache folder.
    #[cfg(test)]
    pub fn use_cache_dir(&mut self, dir: PathBuf) {
        self.freezing.use_cache_dir(dir);
        self.recompile();
    }

    fn recompile(&mut self) {
        self.recompile_with(true);
    }

    /// Compiles the project. `start` is whether renders that are missing may
    /// be started now.
    fn recompile_with(&mut self, start: bool) {
        // Devices are fixed for a stream's lifetime, so an Output node tied
        // to another one means playing afresh. `play` compiles.
        let wanted = noodle_engine::output_devices(self.project.graph());
        if self.audio.as_ref().is_some_and(|a| a.devices != wanted) {
            // A new engine starts running, so keep a pause.
            let paused = !self.transport_running();
            self.close_stream();
            self.play();
            if paused && let Some(audio) = &self.audio {
                audio.controller.transport().stop();
            }
            if self.audio.is_some() {
                return;
            }
        }
        let plan = self.refresh_freezing(start);
        self.diagnostics = match (&mut self.audio, &plan) {
            (Some(audio), Some(plan)) => audio.controller.update_project_replacing(
                &self.project,
                &self.registry,
                &plan.replacements,
            ),
            (Some(audio), None) => audio
                .controller
                .update_project(&self.project, &self.registry),
            (None, plan) => {
                let lanes: Vec<_> = self.project.lanes().collect();
                match plan {
                    Some(plan) => {
                        compile_replacing(
                            self.project.graph(),
                            &lanes,
                            &self.registry,
                            &plan.replacements,
                        )
                        .1
                    }
                    None => compile_with_lanes(self.project.graph(), &lanes, &self.registry).1,
                }
            }
        };
        if let Some(plan) = plan {
            self.diagnostics.extend(plan.diagnostics);
        }
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
            | Command::SetPortOrder { .. }
            | Command::SetTrackOrder(_)
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
            // Only the Output nodes tied to that device go quiet.
            (Stream::ExtraOutput, true) => {
                verdict
                    .message
                    .get_or_insert_with(|| format!("Output device lost: {error}"));
            }
            (Stream::Output, false) if error.kind() == DeviceErrorKind::DeviceChanged => {
                verdict.output_changed = true;
            }
            (_, false) => verdict.message = Some(error.to_string()),
        }
    }
    verdict
}

/// Which extra output devices didn't open, if any.
fn output_problems(playback: &Playback) -> Option<String> {
    let failed: Vec<String> = playback
        .outputs()
        .iter()
        .filter_map(|status| {
            let error = status.result.as_ref().err()?;
            Some(format!("{} ({error})", status.device))
        })
        .collect();
    (!failed.is_empty()).then(|| format!("Can't play on {}", failed.join(", ")))
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
    // The MIDI port plays no part in opening the audio.
    let tried = AudioConfig {
        midi_input: None,
        ..config.clone()
    };
    (defaults != tried).then_some(defaults)
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

    /// A sine into an output, with the sine in a group.
    fn grouped_sine(session: &mut Session) -> (NodeId, NodeId) {
        let osc = session.new_node_id();
        let out = session.new_node_id();
        session.edit([
            Edit::Apply(Command::AddNode {
                id: osc,
                node: Node::new("noodle.osc.sine"),
            }),
            Edit::Apply(Command::AddNode {
                id: out,
                node: Node::new(OUTPUT_ID),
            }),
            Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(osc, "out"),
                to: Endpoint::new(out, "in"),
            })),
        ]);
        let mut next = session.new_node_id().0;
        let (group, command) = noodle_core::group::group_nodes(session.project(), &[osc], || {
            next += 1;
            NodeId(next)
        })
        .unwrap();
        session.edit([Edit::Apply(command)]);
        (group, osc)
    }

    /// Runs the housekeeping until `done` holds, or fails after a while.
    fn until(session: &mut Session, what: &str, done: impl Fn(&Session) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(session) {
            assert!(Instant::now() < deadline, "never happened: {what}");
            std::thread::sleep(Duration::from_millis(5));
            session.maintain();
        }
    }

    #[test]
    fn freezing_a_group_renders_it_in_the_background_and_undoes() {
        use crate::freezing::BadgeState;

        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().to_owned());
        let (group, osc) = grouped_sine(&mut session);
        assert!(session.freeze_badges().is_empty());

        session.edit([Edit::Apply(Command::SetFrozen {
            node: group,
            frozen: true,
        })]);
        let badge = session
            .freeze_badges()
            .get(&group)
            .expect("a badge")
            .clone();
        assert!(badge.frozen);
        assert!(
            matches!(badge.state, BadgeState::Rendering(_) | BadgeState::Ready),
            "{badge:?}"
        );
        until(&mut session, "the freeze finishes", |s| {
            s.freeze_badges().get(&group).map(|b| &b.state) == Some(&BadgeState::Ready)
        });

        // Changing a parameter inside the group makes the render stale. The
        // session notices once the editing pauses, and renders again.
        session.edit([Edit::Apply(Command::SetParam {
            node: osc,
            key: "frequency".into(),
            value: Some(220.0),
        })]);
        // The stale audio is off the moment the edit lands, with no wait for
        // the editing to pause, and no render has started yet.
        assert_eq!(
            session.freeze_badges().get(&group).map(|b| &b.state),
            Some(&BadgeState::Waiting)
        );
        until(&mut session, "it renders again", |s| {
            s.freeze_badges().get(&group).map(|b| &b.state) == Some(&BadgeState::Ready)
        });

        // Unfreezing, and undoing that.
        session.undo();
        session.undo();
        assert!(
            !session.project().is_frozen(group) || session.freeze_badges().contains_key(&group)
        );
        session.edit([Edit::Apply(Command::SetFrozen {
            node: group,
            frozen: false,
        })]);
        assert!(session.freeze_badges().is_empty());
        assert!(!session.freeze_pending());
    }

    #[test]
    fn an_offline_node_renders_itself_and_a_node_that_cant_be_cached_says_why() {
        use crate::freezing::BadgeState;

        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().to_owned());
        let osc = session.new_node_id();
        let reverse = session.new_node_id();
        let out = session.new_node_id();
        let wire = |from: NodeId, to: NodeId| {
            Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(from, "out"),
                to: Endpoint::new(to, "in"),
            }))
        };
        session.edit([
            Edit::Apply(Command::AddNode {
                id: osc,
                node: Node::new("noodle.osc.sine"),
            }),
            Edit::Apply(Command::AddNode {
                id: reverse,
                node: Node::new("noodle.offline.reverse"),
            }),
            Edit::Apply(Command::AddNode {
                id: out,
                node: Node::new(OUTPUT_ID),
            }),
            wire(osc, reverse),
            wire(reverse, out),
        ]);
        until(&mut session, "the offline node renders", |s| {
            s.freeze_badges().get(&reverse).map(|b| &b.state) == Some(&BadgeState::Ready)
        });
        assert!(!session.freeze_badges()[&reverse].frozen);

        // Live input in front of it can't be cached.
        let input = session.new_node_id();
        session.edit([
            Edit::Apply(Command::AddNode {
                id: input,
                node: Node::new(noodle_engine::INPUT_ID),
            }),
            wire(input, reverse),
        ]);
        let badge = session.freeze_badges().get(&reverse).unwrap();
        assert!(matches!(&badge.state, BadgeState::Failed(why) if why.contains("same every time")));
        assert!(
            session
                .diagnostics()
                .iter()
                .any(|d| matches!(d.problem, noodle_engine::Problem::NotCacheable(_)))
        );
    }

    #[test]
    fn audio_files_are_hashed_off_the_ui_thread_and_a_replaced_file_renders_again() {
        use crate::freezing::BadgeState;
        use noodle_core::Clip;

        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let tone = |frames: usize, step: f32| -> Vec<f32> {
            (0..frames * 2)
                .map(|i| (i as f32 * step).sin() * 0.5)
                .collect()
        };
        noodle_io::write_wav(&dir.path().join("a.wav"), &tone(24_000, 0.01), 2, 48_000).unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(cache.path().to_owned());
        assert!(session.save_as(&dir.path().join("p.noodle")));
        let track = session.new_node_id();
        let reverse = session.new_node_id();
        let out = session.new_node_id();
        let clip = session.project().next_clip_id();
        let wire = |from: NodeId, from_port: &str, to: NodeId| {
            Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(from, from_port),
                to: Endpoint::new(to, "in"),
            }))
        };
        session.edit([
            Edit::Apply(Command::AddNode {
                id: track,
                node: Node::new(noodle_nodes::TRACK_INPUT_ID),
            }),
            Edit::Apply(Command::AddNode {
                id: reverse,
                node: Node::new("noodle.offline.reverse"),
            }),
            Edit::Apply(Command::AddNode {
                id: out,
                node: Node::new(OUTPUT_ID),
            }),
            wire(track, "audio", reverse),
            wire(reverse, "out", out),
            Edit::Apply(Command::AddClip {
                id: clip,
                clip: Clip::audio(track, Tick(0), "a.wav", 24_000),
            }),
        ]);
        // The file hasn't been read for its key yet: that happens on a
        // thread of its own, so nothing renders or fails in the meantime.
        assert_eq!(
            session.freeze_badges().get(&reverse).map(|b| &b.state),
            Some(&BadgeState::Waiting)
        );
        assert!(session.freeze_pending());
        let renders = || std::fs::read_dir(cache.path()).unwrap().count();
        until(&mut session, "the offline node renders", |s| {
            s.freeze_badges().get(&reverse).map(|b| &b.state) == Some(&BadgeState::Ready)
        });
        let first = renders();
        assert!(first > 0);

        // The file is re-exported with other contents: the keys change, and
        // the render is made again.
        noodle_io::write_wav(&dir.path().join("a.wav"), &tone(30_000, 0.02), 2, 48_000).unwrap();
        until(&mut session, "the change is seen and rendered again", |s| {
            renders() > first
                && s.freeze_badges().get(&reverse).map(|b| &b.state) == Some(&BadgeState::Ready)
        });
        assert!(
            session
                .message()
                .is_some_and(|m| m.contains("a.wav changed on disk")),
            "{:?}",
            session.message()
        );
    }

    /// A sine into an Output, optionally through `middle`.
    fn sine_chain(session: &mut Session, middle: Option<&str>) {
        let osc = session.new_node_id();
        let out = session.new_node_id();
        let wire = |from: NodeId, to: NodeId| {
            Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(from, "out"),
                to: Endpoint::new(to, "in"),
            }))
        };
        let mut edits = vec![
            Edit::Apply(Command::AddNode {
                id: osc,
                node: Node::new("noodle.osc.sine"),
            }),
            Edit::Apply(Command::AddNode {
                id: out,
                node: Node::new(OUTPUT_ID),
            }),
        ];
        match middle {
            Some(kind) => {
                let mid = session.new_node_id();
                edits.push(Edit::Apply(Command::AddNode {
                    id: mid,
                    node: Node::new(kind),
                }));
                edits.push(wire(osc, mid));
                edits.push(wire(mid, out));
            }
            None => edits.push(wire(osc, out)),
        }
        session.edit(edits);
    }

    #[test]
    fn a_range_is_exported_in_the_background_and_the_session_says_so() {
        use noodle_io::ExportFormat;

        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().join("cache"));
        // Nothing on the timeline: the whole project is nothing.
        sine_chain(&mut session, None);
        let whole = ExportChoice {
            format: ExportFormat::Wav16,
            range: None,
        };
        assert!(
            session
                .start_export(dir.path().join("x.wav"), whole)
                .is_err()
        );
        let empty = ExportChoice {
            format: ExportFormat::Wav16,
            range: Some((2.0, 2.0)),
        };
        assert!(
            session
                .start_export(dir.path().join("x.wav"), empty)
                .is_err()
        );

        let path = dir.path().join("half.wav");
        let choice = ExportChoice {
            format: ExportFormat::Wav16,
            range: Some((0.0, 0.5)),
        };
        session.start_export(path.clone(), choice).unwrap();
        assert!(
            session
                .start_export(dir.path().join("two.wav"), choice)
                .is_err(),
            "one at a time"
        );
        assert!(session.export_progress().is_some());
        until(&mut session, "the export finishes", |s| {
            s.export_progress().is_none()
        });
        assert_eq!(session.message(), Some("Exported 0.5 s to half.wav"));
        let audio = noodle_io::decode_file(&path).unwrap();
        assert_eq!(audio.channels, 2);
        assert_eq!(audio.samples.len(), 24_000 * 2);
        assert!(audio.samples.iter().any(|&x| x.abs() > 0.05));
    }

    #[test]
    fn an_export_includes_offline_nodes_and_can_be_cancelled() {
        use noodle_io::ExportFormat;

        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().join("cache"));
        sine_chain(&mut session, Some("noodle.offline.reverse"));
        let choice = ExportChoice {
            format: ExportFormat::Flac16,
            // Inside the 30 s the offline node is rendered over.
            range: Some((1.0, 2.0)),
        };
        let path = dir.path().join("reversed.flac");
        session.start_export(path.clone(), choice).unwrap();
        until(&mut session, "the export finishes", |s| {
            s.export_progress().is_none()
        });
        assert_eq!(session.message(), Some("Exported 1.0 s to reversed.flac"));
        let audio = noodle_io::decode_file(&path).unwrap();
        assert!(audio.samples.iter().any(|&x| x.abs() > 0.05), "silent");

        // Cancelled, it leaves no file.
        let path = dir.path().join("cancelled.flac");
        session.start_export(path.clone(), choice).unwrap();
        session.cancel_export();
        until(&mut session, "the export stops", |s| {
            s.export_progress().is_none()
        });
        assert!(!path.exists());
        assert!(matches!(
            session.message(),
            Some("Export cancelled") | Some("Exported 1.0 s to cancelled.flac")
        ));
    }

    #[test]
    fn the_playhead_can_be_set_and_read_while_stopped() {
        let mut session = Session::new(Nodes::all());
        assert_eq!(session.playhead(), Tick(0));
        session.seek(Tick(1920));
        assert_eq!(session.playhead(), Tick(1920));
        session.seek(Tick(-5));
        assert_eq!(session.playhead(), Tick(0));
        session.stop();
        assert_eq!(session.playhead(), Tick(0), "stopping again rewinds");
        session.seek(Tick(960));
        session.new_project();
        assert_eq!(session.playhead(), Tick(0), "a new project starts over");
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
    fn a_midi_port_that_will_not_open_is_reported_and_leaves_none_open() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(AudioConfig {
            midi_input: Some("No Such Port".into()),
            ..AudioConfig::default()
        });
        assert_eq!(session.midi_input(), None);
        assert!(
            session
                .message()
                .is_some_and(|m| m.starts_with("MIDI input:"))
        );
        // Turning it off again needs no port and says nothing new.
        session.set_audio_config(AudioConfig::default());
        assert_eq!(session.midi_input(), None);
    }

    thread_local! {
        static PORTS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn fake_ports() -> Result<Vec<String>, noodle_io::MidiError> {
        Ok(PORTS.with(|p| p.borrow().clone()))
    }

    fn fake_open(
        name: &str,
        _bus: &noodle_io::MidiBus,
    ) -> Result<noodle_io::MidiConnection, noodle_io::MidiError> {
        // Like the real one, named as the port is now listed.
        let listed = PORTS.with(|p| {
            p.borrow()
                .iter()
                .find(|p| noodle_io::same_port(p, name))
                .cloned()
        });
        Ok(noodle_io::MidiConnection::detached(
            &listed.unwrap_or_else(|| name.to_string()),
        ))
    }

    #[test]
    fn an_unplugged_midi_port_is_let_go_and_reopened_when_it_returns() {
        let mut session = Session::new(Nodes::all());
        session.midi_watch.list = Box::new(fake_ports);
        session.midi_watch.open = fake_open;
        PORTS.with(|p| *p.borrow_mut() = vec!["Keys".into()]);
        session.audio_config.midi_input = Some("Keys".into());
        let mut now = Instant::now();
        let mut step = |session: &mut Session| {
            now += Duration::from_secs(2);
            session.watch_midi(now);
        };
        // Chosen but not open (it was missing when chosen): opens once listed.
        step(&mut session);
        assert_eq!(session.midi_input(), Some("Keys"));
        // Unplugged.
        PORTS.with(|p| p.borrow_mut().clear());
        step(&mut session);
        assert_eq!(session.midi_input(), None);
        assert!(session.message().is_some_and(|m| m.contains("unplugged")));
        // Still gone: nothing changes.
        step(&mut session);
        assert_eq!(session.midi_input(), None);
        // Plugged back in.
        PORTS.with(|p| *p.borrow_mut() = vec!["Keys".into()]);
        step(&mut session);
        assert_eq!(session.midi_input(), Some("Keys"));
        // Looks at most once a second.
        PORTS.with(|p| p.borrow_mut().clear());
        session.watch_midi(now + Duration::from_millis(100));
        assert_eq!(session.midi_input(), Some("Keys"));
    }

    #[test]
    fn a_port_that_comes_back_with_other_numbers_is_reopened_and_held_notes_released() {
        let mut session = Session::new(Nodes::all());
        session.midi_watch.list = Box::new(fake_ports);
        session.midi_watch.open = fake_open;
        PORTS.with(|p| *p.borrow_mut() = vec!["Keys MIDI 1 24:0".into()]);
        session.audio_config.midi_input = Some("Keys MIDI 1 24:0".into());
        let mut receiver = session.midi.subscribe();
        let mut now = Instant::now();
        let mut step = |session: &mut Session| {
            now += Duration::from_secs(2);
            session.watch_midi(now);
        };
        step(&mut session);
        assert_eq!(session.midi_input(), Some("Keys MIDI 1 24:0"));
        // Unplugged and plugged in again between two looks, as number 28.
        PORTS.with(|p| *p.borrow_mut() = vec!["Keys MIDI 1 28:0".into()]);
        step(&mut session);
        assert_eq!(session.midi_input(), Some("Keys MIDI 1 28:0"));
        // The notes it held were let go.
        assert_eq!(receiver.pop(), Some([0xB0, 123, 0]));
        // And it stays open on later looks.
        step(&mut session);
        assert_eq!(session.midi_input(), Some("Keys MIDI 1 28:0"));
    }

    #[test]
    fn falling_back_keeps_the_input_and_drops_the_output_choices() {
        let chosen = AudioConfig {
            host: Some("jack".into()),
            output: Some("jack:system".into()),
            input: noodle_io::InputChoice::Default,
            sample_rate: Some(96_000),
            buffer_size: Some(64),
            midi_input: Some("Keys".into()),
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

    /// A saved project with `count` tracks, a recorder standing in for the
    /// input stream, and the track input nodes.
    fn recording_session(dir: &Path, count: usize) -> (Session, noodle_io::RecordTap, Vec<NodeId>) {
        let mut session = Session::new(Nodes::all());
        for _ in 0..count {
            let (_, create) =
                noodle_core::group::create_track(None, Position { x: 0.0, y: 0.0 }, || {
                    session.new_node_id()
                });
            session.edit([Edit::Apply(create)]);
        }
        let tracks: Vec<NodeId> = session
            .project()
            .graph()
            .nodes()
            .filter(|(_, node)| node.type_id == noodle_core::group::TRACK_INPUT)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(tracks.len(), count);
        assert!(session.save_as(&dir.join("song.ron")));
        let (tap, recorder) = noodle_io::record_path(1, 48_000.0);
        session.fake_input = Some(recorder);
        (session, tap, tracks)
    }

    fn clips(session: &Session) -> Vec<(NodeId, Tick, String, u64)> {
        session
            .project()
            .clips()
            .map(|(_, clip)| {
                let audio = clip.as_audio().unwrap();
                (clip.node, clip.start, audio.source.clone(), audio.length)
            })
            .collect()
    }

    #[test]
    fn a_take_becomes_a_clip_on_each_armed_track_in_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 3);
        session.arm(tracks[0], true);
        session.arm(tracks[2], true);
        session.seek(Tick(960));
        session.record();
        assert!(session.is_recording());
        tap.push(&vec![0.25_f32; 4_800]);
        session.stop_recording();
        assert!(!session.is_recording());

        let source = "song recordings/take-001.wav";
        assert_eq!(
            clips(&session),
            [
                (tracks[0], Tick(960), source.into(), 4_800),
                (tracks[2], Tick(960), source.into(), 4_800),
            ]
        );
        let audio = noodle_io::read_wav(&dir.path().join(source)).unwrap();
        assert_eq!(audio.samples.len() / audio.channels, 4_800);
        assert!(session.is_dirty());

        session.undo();
        assert!(
            clips(&session).is_empty(),
            "one undo removes the whole take"
        );
        session.redo();
        assert_eq!(clips(&session).len(), 2);
    }

    #[test]
    fn each_take_gets_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        for _ in 0..2 {
            session.record();
            tap.push(&vec![0.1_f32; 480]);
            session.stop_recording();
        }
        let sources: Vec<String> = clips(&session).into_iter().map(|c| c.2).collect();
        assert_eq!(
            sources,
            [
                "song recordings/take-001.wav",
                "song recordings/take-002.wav"
            ]
        );
    }

    #[test]
    fn recording_needs_an_armed_track_and_a_saved_project() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _tap, tracks) = recording_session(dir.path(), 1);
        session.record();
        assert!(!session.is_recording());
        assert!(session.message().unwrap().contains("Arm a track"));

        session.arm(tracks[0], true);
        session.arm(tracks[0], false);
        session.record();
        assert!(!session.is_recording(), "disarming undoes arming");

        session.arm(tracks[0], true);
        session.new_project();
        let mut unsaved = Session::new(Nodes::all());
        unsaved.arm(tracks[0], true);
        unsaved.record();
        assert!(!unsaved.is_recording());
        // The armed track doesn't exist in the empty project either; arming
        // is checked first.
        assert!(unsaved.message().unwrap().contains("Arm a track"));

        let (mut saved_later, _tap, tracks) = recording_session(dir.path(), 1);
        saved_later.path = None;
        saved_later.arm(tracks[0], true);
        saved_later.record();
        assert!(!saved_later.is_recording());
        assert!(saved_later.message().unwrap().contains("Save the project"));
    }

    #[test]
    fn a_take_with_nothing_in_it_adds_no_clip_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        session.record();
        session.stop_recording();
        assert!(clips(&session).is_empty());
        assert_eq!(session.message(), Some("Nothing was recorded"));
        let folder = dir.path().join("song recordings");
        assert_eq!(std::fs::read_dir(folder).unwrap().count(), 0);
    }

    #[test]
    fn stopping_the_transport_keeps_the_take() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        session.record();
        tap.push(&vec![0.5_f32; 960]);
        session.stop();
        assert!(!session.is_recording());
        assert_eq!(clips(&session).len(), 1);
    }

    #[test]
    fn a_track_removed_during_the_take_gets_no_clip() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 2);
        session.arm(tracks[0], true);
        session.arm(tracks[1], true);
        session.record();
        tap.push(&vec![0.5_f32; 960]);
        let group = session
            .project()
            .graph()
            .node(tracks[1])
            .unwrap()
            .parent
            .unwrap();
        session.edit([Edit::Apply(Command::RemoveNode { id: group })]);
        session.stop_recording();
        let on: Vec<NodeId> = clips(&session).into_iter().map(|c| c.0).collect();
        assert_eq!(on, [tracks[0]]);
    }

    #[test]
    fn deleting_every_armed_track_keeps_the_take_without_an_empty_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        session.record();
        tap.push(&vec![0.5_f32; 960]);
        let group = session
            .project()
            .graph()
            .node(tracks[0])
            .unwrap()
            .parent
            .unwrap();
        session.edit([Edit::Apply(Command::RemoveNode { id: group })]);
        session.stop_recording();
        assert!(clips(&session).is_empty());
        assert!(session.message().unwrap().contains("take was kept"));
        assert!(dir.path().join("song recordings/take-001.wav").exists());
        // The next undo undoes the deletion, not a no-op.
        session.undo();
        assert!(session.project().graph().node(tracks[0]).is_some());
    }

    #[test]
    fn seeking_ends_the_take_where_it_was_laid_down() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        session.seek(Tick(480));
        session.record();
        tap.push(&vec![0.5_f32; 960]);
        session.seek(Tick(0));
        assert!(!session.is_recording());
        assert_eq!(clips(&session)[0].1, Tick(480));
    }
}
