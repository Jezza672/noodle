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
mod library;
mod stems;
pub use export::ExportChoice;
pub use library::LibraryEdit;

use crate::filewatch::FileWatch;
use crate::freezing::{Badge, Freezing, Look, Polled};
use noodle_core::{Clip, Command, EditError, FrameId, History, MidiClip, NodeId, Project, Tick};
use noodle_engine::{
    Controller, Diagnostic, Registry, Settings, Telemetry, TempoTable, compile_replacing,
    compile_with_lanes,
};
use noodle_io::{
    AudioConfig, AudioError, DeviceError, DeviceErrorKind, Playback, RecordError, Recorder, Stream,
    Take,
};
use noodle_nodes::{ClipFeeds, ClipProblem, Library};

/// Frames per block while playing: about 2.7 ms at 48 kHz.
const MAX_FRAMES: usize = 128;

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
    /// Opens or closes a plugin node's editor window. Closing it (here or in
    /// the window) stores the plugin's state and parameter values in the
    /// node, as one undo step.
    PluginEditor {
        node: NodeId,
        open: bool,
    },
    /// Saves to or deletes from the user's library. Not an undo step: the
    /// project is untouched.
    Library(LibraryEdit),
    /// Replaces the contents of a group with its library template's current
    /// version. One undo step.
    UpdateFromTemplate(NodeId),
    /// Splits an audio clip into stems (see [`Session::split_to_stems`]). Not
    /// an edit itself: the tracks arrive as one undo step when the job is done.
    SplitToStems(noodle_core::ClipId),
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
    /// Where the Sampler nodes get their samples, by hash.
    pub samples: noodle_nodes::SampleStore,
    /// The plugins whose node types are in `registry`. Renders (freeze,
    /// export) add the same ones to their own registries.
    pub plugins: noodle_plugins::Catalog,
    /// Where a background plugin scan reports, if one is running.
    pub plugin_scan: Option<std::sync::mpsc::Receiver<noodle_plugins::Discovery>>,
    /// Every plugin file the last scan looked at, for the plugin browser.
    pub plugin_files: Vec<noodle_plugins::PluginFile>,
}

impl Nodes {
    /// Every node type the app offers.
    pub fn all() -> Self {
        let mut registry = Registry::with_builtins();
        let Library {
            telemetry,
            clips,
            midi,
            samples,
        } = noodle_nodes::register_library(&mut registry);
        Self {
            registry,
            telemetry,
            clips,
            midi,
            samples,
            plugins: noodle_plugins::Catalog::default(),
            plugin_scan: None,
            plugin_files: Vec::new(),
        }
    }

    /// [`all`](Self::all), plus the plugins found last time (read from the
    /// cache, so this is quick), and a scan of the machine in the
    /// background for new ones. The session adds what it finds.
    pub fn all_with_plugins() -> Self {
        let mut nodes = Self::all();
        let cached = noodle_plugins::ScanCache::default_path()
            .map(|path| noodle_plugins::ScanCache::load(&path))
            .unwrap_or_default();
        nodes.set_plugins(noodle_plugins::Catalog::new(&cached.files));
        nodes.plugin_files = cached.files;
        let (sender, receiver) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("noodle-plugin-scan".into())
            .spawn(move || {
                let _ = sender.send(noodle_plugins::discover());
            });
        nodes.plugin_scan = spawned.ok().map(|_| receiver);
        nodes
    }

    fn set_plugins(&mut self, catalog: noodle_plugins::Catalog) {
        catalog.register(&mut self.registry);
        self.plugins = catalog;
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
    /// Group templates and presets, system and the user's.
    library: noodle_core::library::Library,
    /// The plugin node types in `registry`.
    plugins: noodle_plugins::Catalog,
    /// Where a background plugin scan reports, until it has.
    plugin_scan: Option<std::sync::mpsc::Receiver<noodle_plugins::Discovery>>,
    plugin_files: Vec<noodle_plugins::PluginFile>,
    /// Plugin nodes whose editor was opened from here: when one closes, its
    /// state is stored.
    plugin_editors: std::collections::BTreeMap<NodeId, noodle_plugins::EditorKey>,
    /// Why a plugin's editor last failed to open, until it opens.
    plugin_editor_errors: std::collections::BTreeMap<NodeId, String>,
    /// Whether plugins run in processes of their own.
    sandbox_plugins: bool,
    /// How many restarts of each plugin host have been reported, and when
    /// the hosts were last looked at.
    host_restarts: std::collections::BTreeMap<NodeId, (u32, bool)>,
    hosts_checked: Option<Instant>,
    telemetry: Telemetry,
    clips: ClipFeeds,
    /// Where the MIDI input port's messages go, to reach the MIDI In nodes.
    midi: noodle_io::MidiBus,
    /// The samples the Samplers play, kept in `samples` beside the project.
    samples: noodle_nodes::SampleStore,
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
    /// The tracks that record audio when the record button is pressed.
    armed_audio: BTreeSet<NodeId>,
    /// The tracks that record MIDI, and play it live.
    armed_midi: BTreeSet<NodeId>,
    /// The take being recorded, if there is one.
    recording: Option<Recording>,
    /// How the MIDI of the next take is snapped. Not saved with the project.
    record_quantise: crate::midi_take::Quantise,
    /// The step, in ticks, that clips, notes and markers snap to (see
    /// [`crate::snap`]). Not saved with the project.
    snap: i64,
    /// Stands in for the input stream's recorder in tests.
    #[cfg(test)]
    fake_input: Option<Box<dyn TakeSink>>,
    /// Something the user should know, such as a failed save, shown until the
    /// next one replaces it.
    message: Option<String>,
    /// The export under way, if there is one.
    exporting: Option<export::Run>,
    /// Split to stems: the separator and the split under way, if any.
    stems: stems::Stems,
}

/// The settings frozen audio is rendered with when no stream is open.
const FREEZE_SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 128,
    channels: 2,
};
/// Seconds a render runs past the last clip, for tails.
const FREEZE_TAIL: f32 = 4.0;
/// The shortest render in seconds, for projects with little or no timeline.
const FREEZE_MINIMUM: f32 = 30.0;

/// Which input a track's arm button records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmKind {
    Audio,
    Midi,
}

/// A take being recorded, as the timeline draws it.
pub struct TakePreview {
    /// The tracks that get an audio clip of it, and those that get a MIDI
    /// clip.
    pub audio_tracks: Vec<NodeId>,
    pub midi_tracks: Vec<NodeId>,
    /// Where the take started and where the playhead is now.
    pub start: Tick,
    pub end: Tick,
    /// Whether an audio input is being recorded.
    pub audio: bool,
    /// The notes played so far, as the clip they will be, and where it starts.
    pub midi: Option<(Tick, MidiClip)>,
}

/// A take being recorded.
struct Recording {
    /// The tracks that get an audio clip of it, and those that get a MIDI
    /// clip.
    audio_tracks: Vec<NodeId>,
    midi_tracks: Vec<NodeId>,
    /// Where the take starts on the timeline.
    start: Tick,
    /// The audio file, relative to the project file, when an input is being
    /// recorded.
    relative: Option<String>,
    /// The MIDI played during the take.
    midi: crate::midi_take::MidiTake,
    /// How the MIDI is snapped when the take ends.
    quantise: crate::midi_take::Quantise,
}

/// The clip for a take of `frames` frames that began when the transport was
/// at `start`, with its first `latency` frames cut off (they were the sound
/// that was already on its way when the take began). Keeps at least a frame.
fn recorded_clip(track: NodeId, start: Tick, relative: &str, frames: u64, latency: u64) -> Clip {
    let skip = latency.min(frames.saturating_sub(1));
    let mut clip = Clip::audio(track, start, relative, frames - skip);
    if let noodle_core::ClipContent::Audio(audio) = &mut clip.content {
        audio.offset = skip;
    }
    clip
}

/// Something that can record the input to a file.
trait TakeSink {
    fn start(&mut self, path: &Path) -> Result<(), RecordError>;
    fn stop(&mut self) -> Result<Take, RecordError>;
    /// The take so far as peaks, if the sink can say. See
    /// [`Recorder::live_peaks`].
    fn live_peaks(&self, _columns: usize) -> Option<(u64, Vec<noodle_io::Peak>)> {
        None
    }
}

impl TakeSink for Playback {
    fn start(&mut self, path: &Path) -> Result<(), RecordError> {
        self.start_recording(path)
    }
    fn stop(&mut self) -> Result<Take, RecordError> {
        self.stop_recording()
    }
    fn live_peaks(&self, columns: usize) -> Option<(u64, Vec<noodle_io::Peak>)> {
        Playback::live_peaks(self, columns)
    }
}

impl TakeSink for Recorder {
    fn start(&mut self, path: &Path) -> Result<(), RecordError> {
        Recorder::start(self, path)
    }
    fn stop(&mut self) -> Result<Take, RecordError> {
        Recorder::stop(self)
    }
    fn live_peaks(&self, columns: usize) -> Option<(u64, Vec<noodle_io::Peak>)> {
        Some(Recorder::live_peaks(self, columns))
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
    /// Set when the engine runs on something other than what was chosen: the
    /// system default, or the virtual output. Lasts as long as the stream,
    /// for the toolbar to show.
    fallback: Option<String>,
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
            samples,
            plugins,
            plugin_scan,
            plugin_files,
        } = nodes;
        let mut session = Self {
            saved: Project::new(),
            project: Project::new(),
            history: History::new(),
            registry,
            library: noodle_core::library::Library::in_memory(),
            plugins,
            plugin_scan,
            plugin_files,
            plugin_editors: Default::default(),
            plugin_editor_errors: Default::default(),
            sandbox_plugins: false,
            host_restarts: Default::default(),
            hosts_checked: None,
            telemetry,
            clips,
            midi,
            samples,
            midi_connection: None,
            midi_watch: MidiWatch::default(),
            file_watch: FileWatch::default(),
            exporting: None,
            stems: stems::Stems::new(),
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
            armed_audio: BTreeSet::new(),
            armed_midi: BTreeSet::new(),
            recording: None,
            record_quantise: crate::midi_take::Quantise::default(),
            snap: crate::snap::DEFAULT,
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

    /// Whether warped clips are playing as cheap Repitch, because a warp is
    /// being dragged.
    #[cfg(test)]
    pub fn warp_preview(&self) -> bool {
        self.clips.preview()
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
            self.reopen_stream();
        }
    }

    /// Holds `key` down on the track input `node` (or lets it go): for
    /// trying the notes of a clip on the piano roll's keyboard through the
    /// synth the track feeds. It sounds whether or not the transport runs.
    pub fn audition(&self, node: NodeId, key: u8, on: bool) {
        self.clips.audition(node, key, on);
    }

    /// Where MIDI from sources other than a port, such as the computer
    /// keyboard, is sent to reach the MIDI In nodes.
    pub fn midi_bus(&self) -> &noodle_io::MidiBus {
        &self.midi
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

    /// Takes the result of the background plugin scan, once it's done: new
    /// plugins become node types, and the graph is compiled again so nodes
    /// that were unknown until now come to life.
    fn poll_plugin_scan(&mut self) {
        let Some(receiver) = &self.plugin_scan else {
            return;
        };
        match receiver.try_recv() {
            Ok(found) => {
                self.plugin_scan = None;
                self.plugin_files = found.files.clone();
                let known = self.plugins.len();
                let failed = found.files.iter().filter(|f| f.error.is_some()).count();
                if found.catalog.len() != known || found.scanned > 0 {
                    found.catalog.register(&mut self.registry);
                    self.plugins = found.catalog;
                    self.recompile();
                }
                if found.scanned > 0 {
                    self.message = Some(format!(
                        "Scanned {} plugin file(s): {} plugin(s) available{}",
                        found.scanned,
                        self.plugins.len(),
                        if failed > 0 {
                            format!(", {failed} file(s) failed")
                        } else {
                            String::new()
                        }
                    ));
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.plugin_scan = None,
        }
    }

    /// The plugins the browser lists.
    pub fn plugins(&self) -> &noodle_plugins::Catalog {
        &self.plugins
    }

    /// Plugin files the last scan couldn't read, with why.
    pub fn failed_plugin_files(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.plugin_files
            .iter()
            .filter_map(|f| Some((f.path.as_path(), f.error.as_deref()?)))
    }

    /// Whether plugins run in processes of their own, so one that crashes or
    /// hangs doesn't take the app with it.
    pub fn sandbox_plugins(&self) -> bool {
        self.sandbox_plugins
    }

    /// Runs plugins in processes of their own, or in the app. The graph
    /// compiles again so the plugins already in it move.
    pub fn set_sandbox_plugins(&mut self, on: bool) {
        if on == self.sandbox_plugins {
            return;
        }
        let command = if on {
            match noodle_plugins::SandboxCommand::current_exe() {
                Ok(command) => Some(command),
                Err(error) => {
                    self.message = Some(format!("Can't start plugin processes: {error}"));
                    return;
                }
            }
        } else {
            None
        };
        noodle_plugins::set_sandbox(command);
        self.sandbox_plugins = on;
        self.host_restarts.clear();
        if self.has_plugin_nodes() {
            self.recompile();
        }
    }

    fn has_plugin_nodes(&self) -> bool {
        self.project
            .graph()
            .nodes()
            .any(|(_, node)| self.plugins.get(&node.type_id).is_some())
    }

    /// Says so when a plugin's process died or hung and was started again
    /// (or the node gave up). Looks about twice a second.
    fn poll_plugin_hosts(&mut self, now: Instant) {
        if !self.sandbox_plugins
            || self
                .hosts_checked
                .is_some_and(|at| now.duration_since(at) < Duration::from_millis(500))
        {
            return;
        }
        self.hosts_checked = Some(now);
        let mut report = None;
        for (id, node) in self.project.graph().nodes() {
            let Some(plugin) = self.plugins.get(&node.type_id) else {
                continue;
            };
            let Some(status) = noodle_plugins::sandbox_status(id) else {
                continue;
            };
            let seen = self.host_restarts.entry(id).or_default();
            if status.restarts > seen.0 || (status.failed && !seen.1) {
                *seen = (status.restarts, status.failed);
                let why = status.last_error.unwrap_or_default();
                report = Some(if status.failed {
                    format!(
                        "{} keeps crashing and was switched off ({why}). Edit the node to try again.",
                        plugin.descriptor().name
                    )
                } else {
                    format!(
                        "{} crashed or stopped answering and was restarted ({why})",
                        plugin.descriptor().name
                    )
                });
            }
        }
        if report.is_some() {
            self.message = report;
        }
    }

    pub fn plugin_scanning(&self) -> bool {
        self.plugin_scan.is_some()
    }

    /// Looks for plugins again in the background. Without `everything`, only
    /// new and changed files are scanned; with it, every file is, which
    /// also gives ones that failed another chance.
    pub fn rescan_plugins(&mut self, everything: bool) {
        if self.plugin_scan.is_some() {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("noodle-plugin-scan".into())
            .spawn(move || {
                let _ = sender.send(noodle_plugins::discover_with(everything));
            });
        match spawned {
            Ok(_) => self.plugin_scan = Some(receiver),
            Err(error) => self.message = Some(format!("Couldn't scan for plugins: {error}")),
        }
    }

    /// The plugin a node is, if it is one.
    pub fn plugin_of(&self, node: NodeId) -> Option<&noodle_plugins::PluginNodeType> {
        let node = self.project.graph().node(node)?;
        self.plugins.get(&node.type_id)
    }

    /// Whether the node's plugin editor window is up.
    pub fn plugin_editor_open(&self, node: NodeId) -> bool {
        noodle_plugins::editor_is_open(node)
    }

    fn plugin_editor(&mut self, node: NodeId, open: bool) {
        if !open {
            // Storing the state waits for the window to be gone.
            noodle_plugins::close_editor(node);
            return;
        }
        match noodle_plugins::open_editor(node) {
            Ok(key) => {
                self.plugin_editors.insert(node, key);
                self.plugin_editor_errors.remove(&node);
            }
            Err(error) => {
                let why = format!("Couldn't open the editor: {error}");
                self.plugin_editor_errors.insert(node, why.clone());
                self.message = Some(why);
            }
        }
    }

    /// Why the node's plugin editor didn't open, if it hasn't.
    pub fn plugin_editor_error(&self, node: NodeId) -> Option<&str> {
        self.plugin_editor_errors.get(&node).map(String::as_str)
    }

    /// Stores the state of each plugin whose editor has closed. The node
    /// stays on the list until that worked, or its plugin instance is gone
    /// (the graph was built again, which loses what the editor changed).
    fn poll_plugin_editors(&mut self) {
        let closed: Vec<(NodeId, noodle_plugins::EditorKey)> = self
            .plugin_editors
            .iter()
            .map(|(n, k)| (*n, *k))
            .filter(|(_, key)| !noodle_plugins::editor_key_is_open(*key))
            .collect();
        for (node, key) in closed {
            if !noodle_plugins::editor_instance_running(key) {
                self.plugin_editors.remove(&node);
                continue;
            }
            let Some(plugin) = self.plugin_of(node) else {
                self.plugin_editors.remove(&node);
                continue;
            };
            let descriptor = plugin.descriptor().clone();
            match noodle_plugins::capture_key(key) {
                Ok(captured) => {
                    self.plugin_editors.remove(&node);
                    let unchanged = self.project.graph().node(node).is_some_and(|n| {
                        n.config.get(noodle_plugins::STATE_KEY)
                            == Some(&noodle_core::Value::Text(captured.state_text()))
                            && captured.params.iter().all(|(id, v)| {
                                n.params
                                    .get(&noodle_plugins::param_key(*id))
                                    .is_none_or(|p| (f64::from(*p) - v).abs() < 1e-6)
                            })
                    });
                    if !unchanged {
                        let config = self
                            .project
                            .graph()
                            .node(node)
                            .map(|n| n.config.clone())
                            .unwrap_or_default();
                        let command = captured.commands(node, &descriptor, &config);
                        self.edit([Edit::Apply(command)]);
                        self.message = Some(format!("Kept the state of {}", descriptor.name));
                    }
                }
                Err(error) => {
                    self.message = Some(format!("Couldn't keep the plugin's state: {error}"));
                }
            }
        }
    }

    /// The hook that gives a render's registry the plugin node types.
    pub(crate) fn extend_registry(&self) -> Option<noodle_plugins::ExtendRegistry> {
        (!self.plugins.is_empty()).then(|| self.plugins.extender())
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
                    // A warp being dragged plays as cheap Repitch: the
                    // stretch would start over with every change.
                    if edits_warp(&command) {
                        self.clips.set_preview(true);
                    }
                    command
                }
                Edit::EndDrag => {
                    self.history.end_group();
                    self.clips.set_preview(false);
                    continue;
                }
                Edit::PluginEditor { node, open } => {
                    self.plugin_editor(node, open);
                    continue;
                }
                Edit::Library(edit) => {
                    self.library_edit(edit);
                    continue;
                }
                Edit::SplitToStems(clip) => {
                    self.split_to_stems(clip);
                    continue;
                }
                Edit::UpdateFromTemplate(group) => {
                    self.history.end_group();
                    match self.update_from_template(group) {
                        Ok(command) => command,
                        Err(error) => {
                            self.message = Some(format!("Couldn't update the group: {error}"));
                            continue;
                        }
                    }
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
                self.save_samples();
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

    /// Points the store at `samples` beside the file and writes every sample
    /// a Sampler names into it, so a project saved anywhere takes its sounds
    /// along.
    fn save_samples(&mut self) {
        self.samples.set_dir(self.base().join("samples"));
        let hashes: std::collections::BTreeSet<String> = self
            .project
            .graph()
            .nodes()
            .filter(|(_, node)| node.type_id == noodle_nodes::SAMPLER_ID)
            .filter_map(|(_, node)| match node.config.get("sample") {
                Some(noodle_core::Value::Text(hash)) if !hash.is_empty() => Some(hash.clone()),
                _ => None,
            })
            .collect();
        for hash in hashes {
            if let Err(error) = self.samples.save(&hash) {
                self.message = Some(format!(
                    "Couldn't save sample {}: {error}",
                    &hash[..8.min(hash.len())]
                ));
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
        // The take belongs to the project it was recorded in, and so does
        // the arming: track IDs mean other tracks in the next project.
        self.stop_recording();
        self.armed_audio.clear();
        for track in std::mem::take(&mut self.armed_midi) {
            self.clips.set_monitoring(track, false);
        }
        self.saved = project.clone();
        self.project = project;
        self.history = History::new();
        self.path = path;
        self.dirty = false;
        self.plugin_editors.clear();
        self.message = None;
        self.parked = Tick(0);
        self.recompile();
        // A new project starts from its beginning, running or not as the
        // last one's was.
        if let Some(audio) = &self.audio {
            audio.controller.transport().seek(Tick(0));
        }
    }

    /// The tap on the virtual output when the engine plays on it, for tests
    /// that check the sound the app made.
    #[cfg(test)]
    pub fn virtual_capture(&self) -> Option<noodle_io::VirtualCapture> {
        self.audio.as_ref()?.playback.virtual_capture()
    }

    /// Whether the engine, the audio stream that runs the graph, is open.
    /// It runs whether or not the transport does.
    pub fn engine_running(&self) -> bool {
        self.audio.is_some()
    }

    /// Why the engine isn't on what was chosen, if it isn't: it fell back to
    /// the system default output, or to the virtual output, which plays to
    /// nowhere. Lasts as long as the engine runs.
    pub fn engine_warning(&self) -> Option<&str> {
        self.audio.as_ref()?.fallback.as_deref()
    }

    /// Starts the engine, with the transport stopped: the graph renders and
    /// live MIDI plays, and the timeline waits for [`Session::play`]. Does
    /// nothing if it's running.
    pub fn start_engine(&mut self) {
        self.open_stream(false);
    }

    /// Closes the audio stream, keeping the playhead for the next start.
    /// Nothing sounds, live MIDI included, and a drone falls silent.
    pub fn stop_engine(&mut self) {
        self.stop_recording();
        self.close_stream();
        self.feed_clips();
    }

    /// Starts the timeline from the playhead, starting the engine first if
    /// it's off.
    pub fn play(&mut self) {
        self.open_stream(false);
        self.set_transport_running(true);
    }

    /// Opens the audio stream if it isn't open, with the transport running
    /// or stopped. The chosen output is tried first, then the system's
    /// default, then the virtual output, which always opens. The fallbacks
    /// are not saved, and say so in [`Session::engine_warning`].
    fn open_stream(&mut self, running: bool) {
        if self.audio.is_some() {
            return;
        }
        // Output nodes tied to other devices play on those as well.
        let devices = noodle_engine::output_devices(self.project.graph());
        let open = |config: &AudioConfig| {
            noodle_io::play_with_outputs_stopped(config, &devices, MAX_FRAMES, &self.midi).map(
                |(playback, controller)| {
                    if running {
                        controller.transport().play();
                    }
                    (playback, controller)
                },
            )
        };
        let mut started = open(&self.audio_config);
        let mut fell_back = None;
        // A saved device that's been unplugged, or a rate it no longer
        // takes, shouldn't stop the app making sound. The setting is kept
        // for when the device is back.
        if let Err(error) = &started
            && let Some(defaults) = fallback(&self.audio_config)
            && let Ok(playing) = open(&defaults)
        {
            fell_back = Some(on_default_output(error));
            started = Ok(playing);
        }
        // Nothing real opens: run on the virtual output, so the app still
        // works for editing and live MIDI still reaches the graph.
        if let Err(error) = &started
            && self.audio_config.output.as_deref() != Some(noodle_io::VIRTUAL_OUTPUT)
            && let Ok(playing) = open(&virtual_fallback(&self.audio_config))
        {
            fell_back = Some(on_virtual_output(error));
            started = Ok(playing);
        }
        match started {
            Ok((playback, mut controller)) => {
                controller.set_telemetry(&self.telemetry);
                // Playback carries on without input rather than failing.
                // The status bar keeps saying so; see `input_problem`.
                self.message = fell_back
                    .clone()
                    .or_else(|| playback.input_problem().map(no_input))
                    .or_else(|| output_problems(&playback));
                self.audio = Some(Audio {
                    playback,
                    controller,
                    devices,
                    monitor: Monitor::default(),
                    fallback: fell_back,
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

    /// Closes the stream and opens it again on the settings, keeping the
    /// transport as it was. For a change of device.
    fn reopen_stream(&mut self) {
        let running = self.transport_running();
        self.close_stream();
        self.open_stream(running);
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

    /// Stops the timeline where it is. Stopping again while it's stopped
    /// rewinds, as in Logic and GarageBand. The engine keeps running, and so
    /// do the live notes held down.
    pub fn stop(&mut self) {
        if self.transport_running() {
            self.set_transport_running(false);
        } else {
            self.rewind();
        }
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
    #[cfg(test)]
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
        // The bus holds only so many messages, so a take drains it often.
        if let Some(recording) = &mut self.recording {
            recording.midi.poll(self.midi.clock().anchor());
        }
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
        self.poll_split();
        self.poll_plugin_scan();
        self.poll_plugin_editors();
        self.poll_plugin_hosts(Instant::now());
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
        self.reopen_stream();
        let Some(audio) = &self.audio else {
            // `open_stream` has said why it couldn't.
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

    /// Arms or disarms a track (by its track input node) for recording one
    /// `kind` of input. A track armed for MIDI also plays the live MIDI
    /// through its own synth, whether or not the transport runs: see
    /// [`noodle_nodes::TrackInput::with_midi`]. Audio has no live
    /// monitoring yet.
    pub fn arm_kind(&mut self, track: NodeId, kind: ArmKind, on: bool) {
        let set = match kind {
            ArmKind::Audio => &mut self.armed_audio,
            ArmKind::Midi => &mut self.armed_midi,
        };
        if on {
            set.insert(track);
        } else {
            set.remove(&track);
        }
        if kind == ArmKind::Midi {
            self.clips.set_monitoring(track, on);
        }
    }

    /// Arms or disarms a track for both kinds of input.
    #[cfg(test)]
    pub fn arm(&mut self, track: NodeId, on: bool) {
        self.arm_kind(track, ArmKind::Audio, on);
        self.arm_kind(track, ArmKind::Midi, on);
    }

    pub fn is_armed(&self, track: NodeId, kind: ArmKind) -> bool {
        match kind {
            ArmKind::Audio => self.armed_audio.contains(&track),
            ArmKind::Midi => self.armed_midi.contains(&track),
        }
    }

    /// The step, in ticks, that the arrangement, piano roll and clip editor
    /// snap to. 1 is off.
    pub fn snap(&self) -> i64 {
        self.snap
    }

    pub fn set_snap(&mut self, ticks: i64) {
        self.snap = ticks.max(1);
    }

    /// How the MIDI of the next take is snapped to the beat.
    pub fn record_quantise(&self) -> crate::midi_take::Quantise {
        self.record_quantise
    }

    pub fn set_record_quantise(&mut self, quantise: crate::midi_take::Quantise) {
        self.record_quantise = quantise;
    }

    /// Records into a stand-in for the input stream, whose end is returned,
    /// without opening any audio.
    #[cfg(test)]
    pub(crate) fn fake_recorder(&mut self) -> noodle_io::RecordTap {
        let (tap, recorder) = noodle_io::record_path(1, 48_000.0);
        self.fake_input = Some(Box::new(recorder));
        tap
    }

    /// Moves the playhead without stopping a take, as playing would.
    #[cfg(test)]
    pub(crate) fn advance_playhead(&mut self, tick: Tick) {
        self.parked = tick;
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// What records the input: the stream's recorder, or a stand-in in tests.
    fn sink(&mut self) -> Option<&mut dyn TakeSink> {
        #[cfg(test)]
        if let Some(fake) = &mut self.fake_input {
            return Some(fake.as_mut());
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
        let exist = |set: &BTreeSet<NodeId>| -> Vec<NodeId> {
            set.iter()
                .copied()
                .filter(|&track| self.project.graph().node(track).is_some())
                .collect()
        };
        let audio_tracks = exist(&self.armed_audio);
        let midi_tracks = exist(&self.armed_midi);
        if audio_tracks.is_empty() && midi_tracks.is_empty() {
            self.message = Some("Arm a track to record on (RA or RM on its header)".into());
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
        // A take of MIDI alone has no file, and no folder for one.
        let (path, relative) = if audio_tracks.is_empty() {
            (PathBuf::new(), String::new())
        } else {
            match next_take_path(&project_path) {
                Ok(found) => found,
                Err(error) => {
                    self.message = Some(format!("Couldn't make a place for the take: {error}"));
                    return;
                }
            }
        };
        // Only tracks armed for audio need the input recorded.
        let started = match self.sink() {
            _ if audio_tracks.is_empty() => Err(RecordError::NoInput),
            Some(sink) => sink.start(&path),
            None => Err(RecordError::NoInput),
        };
        // No audio input chosen is not a failure: the take is MIDI alone.
        let mut notice = None;
        let relative = match started {
            Ok(()) => Some(relative),
            Err(RecordError::NoInput) => {
                if !audio_tracks.is_empty() {
                    notice = Some("No audio input is chosen, so only MIDI is recorded".into());
                }
                None
            }
            Err(error) => {
                self.message = Some(format!("Couldn't record: {error}"));
                return;
            }
        };
        let start_sample = match &self.audio {
            Some(audio) => audio.controller.transport().position(),
            None => self
                .project
                .tempo_map()
                .sample_at(start, f64::from(FREEZE_SETTINGS.sample_rate)),
        };
        self.recording = Some(Recording {
            audio_tracks,
            midi_tracks,
            start,
            relative,
            midi: crate::midi_take::MidiTake::start(
                &self.midi,
                noodle_io::now_micros(),
                start_sample,
            ),
            quantise: self.record_quantise,
        });
        self.message = notice;
    }

    /// The sample rate to place a take with, and how long after the
    /// transport rendered something the player heard it.
    fn take_timing(&self) -> (f64, std::time::Duration) {
        match &self.audio {
            Some(audio) => {
                let rate = f64::from(audio.controller.settings().sample_rate);
                // What the player heard was also held back by plugin delay
                // compensation.
                let pdc = std::time::Duration::from_secs_f64(
                    audio.controller.output_latency() as f64 / rate,
                );
                (rate, audio.playback.output_latency() + pdc)
            }
            None => (
                f64::from(FREEZE_SETTINGS.sample_rate),
                std::time::Duration::ZERO,
            ),
        }
    }

    /// How long before the app got it the input was captured, as the driver
    /// says.
    fn input_latency(&self) -> std::time::Duration {
        self.audio
            .as_ref()
            .map_or(std::time::Duration::ZERO, |audio| {
                audio.playback.input_latency()
            })
    }

    /// The take being recorded, as far as it goes: where it is on the
    /// timeline and the notes played so far, for drawing it growing with the
    /// playhead. `None` when not recording.
    pub fn take_preview(&self) -> Option<TakePreview> {
        let recording = self.recording.as_ref()?;
        let (rate, latency) = self.take_timing();
        let midi = recording.midi.clip_so_far(&crate::midi_take::Finish {
            stop_micros: noodle_io::now_micros(),
            output_latency_micros: latency.as_micros() as u64,
            sample_rate: rate,
            tempo_map: self.project.tempo_map(),
            quantise: recording.quantise,
            anchor: self.midi.clock().anchor(),
        });
        Some(TakePreview {
            audio_tracks: recording.audio_tracks.clone(),
            midi_tracks: recording.midi_tracks.clone(),
            start: recording.start,
            end: self.playhead().max(recording.start),
            audio: recording.relative.is_some(),
            midi,
        })
    }

    /// The audio recorded so far as `columns` peaks over all of it, for
    /// [`TakePreview::audio`] takes.
    pub fn take_waveform(&self, columns: usize) -> Vec<noodle_io::Peak> {
        #[cfg(test)]
        if let Some(fake) = &self.fake_input {
            return fake.live_peaks(columns).map(|(_, p)| p).unwrap_or_default();
        }
        self.audio
            .as_ref()
            .and_then(|audio| TakeSink::live_peaks(&audio.playback, columns))
            .map(|(_, peaks)| peaks)
            .unwrap_or_default()
    }

    /// Ends the take and adds it to the project: an audio clip, if an input
    /// was recorded, and a MIDI clip, if any notes were played, on each track
    /// that was armed, all in one undo step. Does nothing if not recording.
    pub fn stop_recording(&mut self) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        let stop_micros = noodle_io::now_micros();
        let mut notes = Vec::new();
        let mut take = None;
        // What to tell the user, kept together: a failed audio take mustn't
        // be hidden by the notes that were played.
        let mut notices = Vec::new();
        if recording.relative.is_some() {
            let taken = match self.sink() {
                Some(sink) => sink.stop(),
                None => Err(RecordError::NotRecording),
            };
            match taken {
                Ok(done) if done.frames == 0 => {
                    let _ = std::fs::remove_file(&done.path);
                }
                Ok(done) => take = Some(done),
                Err(error) => notices.push(format!("The recording failed: {error}")),
            }
        }
        let (rate, latency) = self.take_timing();
        // Notes only matter if a track is armed to take them.
        if !recording.midi_tracks.is_empty()
            && let Some((start, clip)) = recording.midi.finish(&crate::midi_take::Finish {
                stop_micros,
                output_latency_micros: latency.as_micros() as u64,
                sample_rate: rate,
                tempo_map: self.project.tempo_map(),
                quantise: recording.quantise,
                anchor: self.midi.clock().anchor(),
            })
        {
            notes.push((start, clip));
        }
        if take.is_none() && notes.is_empty() {
            if notices.is_empty() {
                notices.push("Nothing was recorded".into());
            }
            self.message = Some(notices.join("; "));
            return;
        }
        let exist = |tracks: &[NodeId]| -> Vec<NodeId> {
            tracks
                .iter()
                .copied()
                .filter(|&track| self.project.graph().node(track).is_some())
                .collect()
        };
        let audio_tracks = exist(&recording.audio_tracks);
        let midi_tracks = exist(&recording.midi_tracks);
        if audio_tracks.is_empty() && midi_tracks.is_empty() {
            // An empty batch would be an undo step that does nothing.
            notices.push(match &recording.relative {
                Some(relative) if take.is_some() => {
                    format!("The armed tracks were deleted, so the take was kept as {relative}")
                }
                _ => "The armed tracks were deleted, so the take was dropped".into(),
            });
            self.message = Some(notices.join("; "));
            return;
        }
        let mut commands = Vec::new();
        // What was played arrived after the round trip through the output and
        // back in through the input, so the file starts that much late: skip
        // it and the take lines up with the project the player heard.
        let round_trip = ((latency + self.input_latency()).as_secs_f64() * rate) as u64;
        // The take's clip tempo is the timeline's where it was recorded. The
        // sections on every armed track show the same clip, so they share one
        // warp. Its seconds count from the start of the file, which is
        // `round_trip` frames before the sections begin.
        let warp = take.as_ref().filter(|_| rate > 0.0).map(|take| {
            let skip = round_trip.min(take.frames.saturating_sub(1)) as f64 / rate;
            let seconds = take.frames as f64 / rate;
            let warp = noodle_core::ClipWarp::recorded(
                self.project.tempo_map(),
                recording.start,
                skip,
                seconds,
            );
            (self.project.next_warp_id(), warp)
        });
        for &track in &audio_tracks {
            if let (Some(take), Some(relative)) = (&take, &recording.relative) {
                let mut clip =
                    recorded_clip(track, recording.start, relative, take.frames, round_trip);
                if let (Some((id, warp)), noodle_core::ClipContent::Audio(audio)) =
                    (&warp, &mut clip.content)
                {
                    if commands.is_empty() {
                        commands.push(Command::AddWarp {
                            id: *id,
                            warp: warp.clone(),
                        });
                    }
                    audio.warp = Some(*id);
                }
                commands.push(Command::AddClip {
                    id: self.project.new_clip_id(),
                    clip,
                });
            }
        }
        for &track in &midi_tracks {
            for (start, midi) in &notes {
                let mut clip = Clip::midi(track, *start, midi.length);
                if let noodle_core::ClipContent::Midi(content) = &mut clip.content {
                    content.notes = midi.notes.clone();
                }
                commands.push(Command::AddClip {
                    id: self.project.new_clip_id(),
                    clip,
                });
            }
        }
        self.edit([Edit::Apply(Command::Batch(commands))]);
        if let (Some(relative), Some(take)) = (&recording.relative, &take) {
            notices.push(match take.dropped {
                0 => format!("Recorded {relative}"),
                lost => format!("Recorded {relative}, but {lost} frames of input were lost"),
            });
        }
        for (_, midi) in &notes {
            notices.push(format!("Recorded {} notes", midi.notes.len()));
        }
        self.message = Some(notices.join("; "));
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
        // The feeds forget a node that is gone, monitoring flag and all, and
        // undoing the delete brings it back unmonitored while still armed.
        for &track in &self.armed_midi {
            if self.project.graph().node(track).is_some() {
                self.clips.set_monitoring(track, true);
            }
        }
    }

    /// The samples the Sampler nodes play, by hash.
    pub fn samples(&self) -> &noodle_nodes::SampleStore {
        &self.samples
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
            extend_registry: self.extend_registry(),
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
        // Samplers read their samples from beside the project file.
        // An unsaved project has no folder yet: its samples stay in memory
        // until the first save writes them.
        let dir = self.path.is_some().then(|| self.base().join("samples"));
        self.samples.set_dir_option(dir);
        // Devices are fixed for a stream's lifetime, so an Output node tied
        // to another one means playing afresh. `play` compiles.
        let wanted = noodle_engine::output_devices(self.project.graph());
        if self.audio.as_ref().is_some_and(|a| a.devices != wanted) {
            self.reopen_stream();
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

/// Whether a command changes how a clip is warped.
fn edits_warp(command: &Command) -> bool {
    match command {
        Command::SetWarp { .. } | Command::AddWarp { .. } => true,
        Command::Batch(commands) => commands.iter().any(edits_warp),
        _ => false,
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
            | Command::SetParamShown { .. }
            | Command::SetPreset { .. }
            | Command::SetTrackOrder(_)
            | Command::AddFrame { .. }
            | Command::RemoveFrame { .. }
            | Command::SetFrame { .. } => {}
            Command::SetParam {
                node,
                key,
                value: Some(value),
            } => self.params.push((*node, key.clone(), *value)),
            // A warp is only read when the clips are scheduled.
            Command::AddClip { .. }
            | Command::RemoveClip { .. }
            | Command::SetClip { .. }
            | Command::AddWarp { .. }
            | Command::RemoveWarp { .. }
            | Command::SetWarp { .. }
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

fn on_virtual_output(error: &AudioError) -> String {
    format!(
        "No audio output opened, so the engine runs on the virtual output and makes no sound: {error}"
    )
}

/// What to try when no real output opens: the virtual one, keeping the
/// input.
fn virtual_fallback(config: &AudioConfig) -> AudioConfig {
    AudioConfig {
        output: Some(noodle_io::VIRTUAL_OUTPUT.to_owned()),
        input: config.input.clone(),
        ..AudioConfig::default()
    }
}

/// What to try when the chosen output settings can't play: the system's
/// defaults, keeping the input. `None` if that's what was tried.
fn fallback(config: &AudioConfig) -> Option<AudioConfig> {
    let defaults = AudioConfig {
        input: config.input.clone(),
        ..AudioConfig::default()
    };
    // Neither the MIDI port nor the worker threads play a part in opening
    // the audio.
    let tried = AudioConfig {
        midi_input: None,
        threads: 0,
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

    #[test]
    fn samples_go_with_the_project_when_it_is_saved_elsewhere() {
        let mut session = Session::new(Nodes::all());
        let hash = session.samples().insert(&noodle_io::Audio {
            samples: (0..2_000).map(|i| (i as f32 * 0.05).sin()).collect(),
            channels: 1,
            sample_rate: 48_000,
        });
        let sampler = session.new_node_id();
        session.edit([
            Edit::Apply(Command::AddNode {
                id: sampler,
                node: Node::new(noodle_nodes::SAMPLER_ID),
            }),
            Edit::Apply(Command::SetConfig {
                node: sampler,
                key: "sample".into(),
                value: Some(noodle_core::Value::Text(hash.clone())),
            }),
        ]);
        // Not saved yet: nothing is written anywhere.
        assert_eq!(session.samples().dir(), None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("music").join("song.ron");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        assert!(session.save_as(&path));
        assert!(
            dir.path()
                .join("music/samples")
                .join(format!("{hash}.wav"))
                .exists()
        );

        // A fresh session that opens the file finds the sample.
        let mut fresh = Session::new(Nodes::all());
        assert!(fresh.load(&path));
        assert!(fresh.samples().get(&hash).is_some());
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

    fn fake_plugin_file(id: &str) -> noodle_plugins::PluginFile {
        noodle_plugins::PluginFile {
            path: "/no/such/plugin.clap".into(),
            len: 0,
            modified: 0,
            plugins: vec![noodle_plugins::Descriptor {
                format: noodle_plugins::PluginFormat::Clap,
                id: id.into(),
                name: "Fake".into(),
                vendor: "Nobody".into(),
                instrument: false,
                audio_inputs: vec![noodle_plugins::AudioPort {
                    name: "main".into(),
                    channels: 2,
                }],
                audio_outputs: vec![noodle_plugins::AudioPort {
                    name: "main".into(),
                    channels: 2,
                }],
                note_inputs: 0,
                note_outputs: 0,
                params: Vec::new(),
            }],
            error: None,
        }
    }

    #[test]
    fn plugins_found_by_the_background_scan_become_node_types_and_reach_renders() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut nodes = Nodes::all();
        nodes.plugin_scan = Some(receiver);
        let mut session = Session::new(nodes);
        assert!(session.registry().get("clap.fake.one").is_none());
        assert!(session.extend_registry().is_none(), "no plugins, no hook");

        let files = vec![fake_plugin_file("fake.one")];
        sender
            .send(noodle_plugins::Discovery {
                catalog: noodle_plugins::Catalog::new(&files),
                files,
                scanned: 1,
            })
            .unwrap();
        session.maintain();

        let ty = session.registry().get("clap.fake.one").expect("registered");
        assert_eq!(ty.info().category, "Plugins");
        assert!(session.message().unwrap().contains("1 plugin(s) available"));
        // A render's own registry gets them through the hook.
        let mut render_registry = Registry::new();
        session.extend_registry().expect("a hook")(&mut render_registry);
        assert!(render_registry.get("clap.fake.one").is_some());
    }

    #[test]
    fn the_scan_reports_files_that_failed_and_a_rescan_waits_for_one_in_progress() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut nodes = Nodes::all();
        nodes.plugin_scan = Some(receiver);
        let mut session = Session::new(nodes);
        assert!(session.plugin_scanning());
        // A second scan isn't started over the first.
        session.rescan_plugins(true);
        assert!(session.plugin_scanning());

        let mut files = vec![fake_plugin_file("fake.one")];
        let mut bad = fake_plugin_file("fake.bad");
        bad.plugins.clear();
        bad.path = "/plugins/bad.clap".into();
        bad.error = Some("it crashed".into());
        files.push(bad);
        sender
            .send(noodle_plugins::Discovery {
                catalog: noodle_plugins::Catalog::new(&files),
                files,
                scanned: 2,
            })
            .unwrap();
        session.maintain();
        assert!(!session.plugin_scanning());
        assert_eq!(session.plugins().len(), 1);
        let failed: Vec<_> = session.failed_plugin_files().collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].1, "it crashed");
    }

    #[test]
    fn opening_the_editor_of_a_plugin_that_is_not_running_says_so() {
        let mut session = Session::new(Nodes::all());
        let id = session.new_node_id();
        session.edit([
            Edit::Apply(Command::AddNode {
                id,
                node: noodle_core::Node::new("clap.fake.one"),
            }),
            Edit::PluginEditor {
                node: id,
                open: true,
            },
        ]);
        assert!(
            session
                .message()
                .is_some_and(|m| m.starts_with("Couldn't open the editor"))
        );
        assert!(!session.plugin_editor_open(id));
        // Nothing was waiting to be kept.
        session.maintain();
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
            threads: 3,
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
        assert!(!session.engine_running());
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
        session.fake_input = Some(Box::new(recorder));
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

    /// Stands in for a machine with no audio input chosen.
    struct NoInput;

    impl TakeSink for NoInput {
        fn start(&mut self, _: &Path) -> Result<(), RecordError> {
            Err(RecordError::NoInput)
        }
        fn stop(&mut self) -> Result<Take, RecordError> {
            Err(RecordError::NotRecording)
        }
    }

    /// Per MIDI clip, its track and its notes as (key, start, length).
    type Notes = Vec<(u8, Tick, Tick)>;

    fn midi_clips(session: &Session) -> Vec<(NodeId, Notes)> {
        session
            .project()
            .clips()
            .filter_map(|(_, clip)| match &clip.content {
                noodle_core::ClipContent::Midi(midi) => Some((
                    clip.node,
                    midi.notes
                        .iter()
                        .map(|n| (n.key, n.start, n.length))
                        .collect(),
                )),
                noodle_core::ClipContent::Audio(_) => None,
            })
            .collect()
    }

    #[test]
    fn midi_played_during_a_take_becomes_a_midi_clip_without_an_audio_input() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _tap, tracks) = recording_session(dir.path(), 2);
        session.fake_input = Some(Box::new(NoInput));
        session.arm(tracks[0], true);
        session.arm(tracks[1], true);
        session.record();
        assert!(session.is_recording(), "{:?}", session.message());
        session.midi_bus().send([0x90, 60, 100]);
        session.midi_bus().send([0x80, 60, 0]);
        session.stop_recording();
        let clips = midi_clips(&session);
        assert_eq!(clips.len(), 2);
        for (_, notes) in &clips {
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].0, 60);
        }
        assert!(
            session
                .project()
                .clips()
                .all(|(_, c)| c.as_audio().is_none()),
            "no audio input, no audio clip"
        );
        // One undo takes the whole take away.
        session.undo();
        assert_eq!(session.project().clips().count(), 0);
    }

    /// An input that starts but fails when the take ends.
    struct FailsAtTheEnd;

    impl TakeSink for FailsAtTheEnd {
        fn start(&mut self, _: &Path) -> Result<(), RecordError> {
            Ok(())
        }
        fn stop(&mut self) -> Result<Take, RecordError> {
            Err(RecordError::NotRecording)
        }
    }

    #[test]
    fn a_failed_audio_take_is_still_reported_when_notes_were_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _tap, tracks) = recording_session(dir.path(), 1);
        session.fake_input = Some(Box::new(FailsAtTheEnd));
        session.arm(tracks[0], true);
        session.record();
        session.midi_bus().send([0x90, 60, 100]);
        session.stop_recording();
        assert_eq!(midi_clips(&session).len(), 1);
        let message = session.message().unwrap();
        assert!(message.contains("The recording failed"), "{message}");
        assert!(message.contains("Recorded 1 notes"), "{message}");
    }

    #[test]
    fn a_take_with_audio_and_notes_makes_both_in_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        session.record();
        tap.push(&[0.5; 480]);
        session.midi_bus().send([0x90, 64, 90]);
        session.stop_recording();
        assert_eq!(session.project().clips().count(), 2);
        assert_eq!(midi_clips(&session).len(), 1);
        session.undo();
        assert_eq!(session.project().clips().count(), 0);
    }

    #[test]
    fn audio_and_midi_arms_choose_what_each_track_records() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 2);
        session.arm_kind(tracks[0], ArmKind::Audio, true);
        session.arm_kind(tracks[1], ArmKind::Midi, true);
        session.record();
        tap.push(&[0.5; 480]);
        session.midi_bus().send([0x90, 64, 90]);
        session.stop_recording();
        // Audio on the first track alone, notes on the second alone.
        let audio: Vec<NodeId> = session
            .project()
            .clips()
            .filter(|(_, clip)| clip.as_audio().is_some())
            .map(|(_, clip)| clip.node)
            .collect();
        assert_eq!(audio, [tracks[0]]);
        let midi = midi_clips(&session);
        assert_eq!(midi.len(), 1);
        assert_eq!(midi[0].0, tracks[1]);
    }

    #[test]
    fn only_a_midi_arm_monitors_and_only_an_audio_arm_records_audio() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 1);
        session.arm_kind(tracks[0], ArmKind::Midi, true);
        session.record();
        // No audio file is started for a take with nothing armed for audio.
        assert!(!session.take_preview().unwrap().audio);
        tap.push(&[0.5; 480]);
        session.stop_recording();
        assert_eq!(session.project().clips().count(), 0);
        assert!(!dir.path().join("song recordings").exists());
    }

    #[test]
    fn arming_audio_with_no_input_chosen_says_so_when_recording_starts() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _tap, tracks) = recording_session(dir.path(), 1);
        session.fake_input = Some(Box::new(NoInput));
        session.arm_kind(tracks[0], ArmKind::Audio, true);
        session.record();
        assert!(session.is_recording());
        assert!(session.message().unwrap().contains("No audio input"));
    }

    #[test]
    fn the_take_in_progress_can_be_previewed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 2);
        assert!(session.take_preview().is_none());
        session.arm(tracks[1], true);
        session.record();
        let preview = session.take_preview().unwrap();
        assert_eq!(preview.audio_tracks, [tracks[1]]);
        assert_eq!(preview.midi_tracks, [tracks[1]]);
        assert!(preview.audio);
        assert!(preview.midi.is_none());
        // Notes show up as they are played, a held one included.
        session.midi_bus().send([0x90, 62, 100]);
        session.maintain();
        session.advance_playhead(Tick(1920));
        let preview = session.take_preview().unwrap();
        assert_eq!((preview.start, preview.end), (Tick(0), Tick(1920)));
        let (_, clip) = preview.midi.unwrap();
        assert_eq!(clip.notes.len(), 1);
        assert_eq!(clip.notes[0].key, 62);
        // So does the audio, up to a block behind.
        tap.push(&[0.5; 4096]);
        let mut peaks = Vec::new();
        for _ in 0..500 {
            peaks = session.take_waveform(4);
            if peaks.iter().any(|p| p.max > 0.0) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(peaks.len(), 4);
        assert!(peaks.iter().all(|p| p.max == 0.5), "{peaks:?}");
        // Nothing is left to draw once the take is done.
        session.stop_recording();
        assert!(session.take_preview().is_none());
    }

    #[test]
    fn a_take_with_neither_notes_nor_audio_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _tap, tracks) = recording_session(dir.path(), 1);
        session.fake_input = Some(Box::new(NoInput));
        session.arm(tracks[0], true);
        session.record();
        session.stop_recording();
        assert_eq!(session.project().clips().count(), 0);
        assert_eq!(session.message(), Some("Nothing was recorded"));
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

        // Both sections show one clip, whose tempo is the timeline's where it
        // was recorded; it sounds as recorded until the conversion is on.
        let warps: Vec<_> = session
            .project()
            .clips()
            .filter_map(|(id, _)| session.project().warp_of(id))
            .collect();
        assert_eq!(warps.len(), 2);
        assert_eq!(warps[0].0, warps[1].0);
        assert_eq!(warps[0].1.tempo.points()[0].bpm, 120.0);
        assert!(warps[0].1.is_identity());

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

    use crate::testkit::virtual_audio as virtual_config;

    #[test]
    fn the_engine_runs_without_the_transport_and_stop_leaves_it_running() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(virtual_config());
        session.start_engine();
        assert!(session.engine_running(), "{:?}", session.message());
        assert_eq!(session.engine_warning(), None);
        // Started stopped: the timeline waits.
        assert!(!session.transport_running());
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(session.playhead(), Tick(0));

        session.play();
        assert!(session.transport_running());
        std::thread::sleep(Duration::from_millis(150));
        assert!(session.playhead() > Tick(0));

        // Stop pauses, in place, and the engine stays up.
        session.stop();
        assert!(session.engine_running());
        assert!(!session.transport_running());
        std::thread::sleep(Duration::from_millis(50));
        let held = session.playhead();
        assert!(held > Tick(0));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(session.playhead(), held);

        // Stopping again rewinds.
        session.stop();
        std::thread::sleep(Duration::from_millis(100));
        assert!(session.engine_running());
        assert_eq!(session.playhead(), Tick(0));
    }

    #[test]
    fn playing_starts_a_stopped_engine_and_turning_it_off_keeps_the_playhead() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(virtual_config());
        assert!(!session.engine_running());
        session.seek(Tick(1920));
        session.play();
        assert!(session.engine_running(), "{:?}", session.message());
        assert!(session.transport_running());
        session.set_transport_running(false);
        std::thread::sleep(Duration::from_millis(100));
        let at = session.playhead();
        session.stop_engine();
        assert!(!session.engine_running());
        assert_eq!(session.playhead(), at);
        // It comes back stopped, from where it was.
        session.start_engine();
        assert!(session.engine_running());
        assert!(!session.transport_running());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(session.playhead(), at);
    }

    #[test]
    fn a_project_plays_audible_sound_through_the_virtual_output() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vibrato.ron");
        let mut session = Session::open(Nodes::all(), &path).expect("the example opens");
        session.set_audio_config(virtual_config());
        session.start_engine();
        let tap = session
            .virtual_capture()
            .expect("playing on the virtual output");
        tap.start();
        session.play();
        std::thread::sleep(Duration::from_millis(400));
        let heard = tap.take();
        assert!(heard.len() > 4_000, "{} samples", heard.len());
        assert!(heard.iter().all(|x| x.is_finite()));
        let peak = heard.iter().fold(0.0f32, |a, &x| a.max(x.abs()));
        assert!(peak > 0.01 && peak <= 1.0, "peak {peak}");
    }

    #[test]
    fn a_change_of_device_keeps_the_transport_as_it_was() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(virtual_config());
        session.start_engine();
        // Stopped stays stopped.
        session.set_audio_config(AudioConfig {
            sample_rate: Some(44_100),
            ..virtual_config()
        });
        assert!(session.engine_running());
        assert!(!session.transport_running());
        // Running stays running.
        session.play();
        session.set_audio_config(virtual_config());
        assert!(session.transport_running());
    }

    #[test]
    fn with_no_real_output_the_engine_falls_back_and_says_so() {
        let mut session = Session::new(Nodes::all());
        let chosen = AudioConfig {
            output: Some("alsa:no such device".into()),
            ..AudioConfig::default()
        };
        session.set_audio_config(chosen.clone());
        session.start_engine();
        assert!(session.engine_running(), "{:?}", session.message());
        // The system default if there is one (a dev machine), else the
        // virtual output.
        let warning = session.engine_warning().expect("a warning").to_owned();
        assert!(
            warning.contains("default output") || warning.contains("virtual output"),
            "{warning}"
        );
        // The choice is kept for when the device is back.
        assert_eq!(session.audio_config(), &chosen);
        assert_eq!(session.message(), Some(warning.as_str()));
    }

    #[test]
    fn choosing_the_virtual_output_is_no_fallback() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(virtual_config());
        session.start_engine();
        assert!(session.engine_running(), "{:?}", session.message());
        assert_eq!(session.engine_warning(), None);
        assert_eq!(session.message(), None);
    }

    /// track input → mono → saw → VCA ← ADSR, → meter and output. Returns the
    /// track and the meter.
    fn track_synth(session: &mut Session) -> (NodeId, NodeId) {
        let ids: Vec<NodeId> = [
            noodle_nodes::TRACK_INPUT_ID,
            "noodle.event.mono",
            "noodle.osc.saw",
            "noodle.mod.adsr",
            "noodle.util.vca",
            "noodle.view.meter",
            OUTPUT_ID,
        ]
        .into_iter()
        .map(|type_id| add(session, Node::new(type_id)))
        .collect();
        for (from, port, to, to_port) in [
            (0, "midi", 1, "in"),
            (1, "pitch", 2, "frequency"),
            (1, "gate", 3, "gate"),
            (2, "out", 4, "in"),
            (3, "out", 4, "level"),
            (4, "out", 5, "in"),
            (4, "out", 6, "in"),
        ] {
            session.edit([Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(ids[from], port),
                to: Endpoint::new(ids[to], to_port),
            }))]);
        }
        (ids[0], ids[5])
    }

    /// How loud the meter is, after giving the engine up to two seconds to
    /// get there.
    fn loudness(session: &mut Session, meter: NodeId, loud: bool) -> f32 {
        let reader = session.telemetry().meter_reader();
        let mut level = 0.0;
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(10));
            session.maintain();
            level = reader.meter(meter).map_or(0.0, |levels| levels[0].rms);
            if (level > 0.01) == loud {
                break;
            }
        }
        level
    }

    #[test]
    fn an_armed_track_plays_its_synth_from_the_keyboard_without_the_transport() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(virtual_config());
        let (track, meter) = track_synth(&mut session);
        session.start_engine();
        assert!(!session.transport_running());
        let keys = session.midi_bus().clone();

        // Not armed: the keyboard does nothing.
        keys.send([0x90, 69, 100]);
        assert!(loudness(&mut session, meter, true) < 0.01);
        keys.send([0x80, 69, 0]);

        // Armed: the same keys sound, with the transport stopped.
        session.arm(track, true);
        keys.send([0x90, 69, 100]);
        assert!(loudness(&mut session, meter, true) > 0.01);
        assert!(!session.transport_running());

        // Play and stop leave the held note alone. (A rewind is a seek, which
        // resets every node, so it ends the note.)
        session.play();
        session.stop();
        assert!(loudness(&mut session, meter, true) > 0.01);

        keys.send([0x80, 69, 0]);
        assert!(loudness(&mut session, meter, false) < 0.01);
    }

    #[test]
    fn disarming_a_track_lets_go_of_the_notes_it_holds() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(virtual_config());
        let (track, meter) = track_synth(&mut session);
        session.start_engine();
        session.arm(track, true);
        session.midi_bus().send([0x90, 60, 100]);
        assert!(loudness(&mut session, meter, true) > 0.01);
        session.arm(track, false);
        assert!(loudness(&mut session, meter, false) < 0.01);
    }

    #[test]
    fn a_new_project_does_not_inherit_the_arming() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(virtual_config());
        let (track, _) = track_synth(&mut session);
        session.arm(track, true);
        session.new_project();
        assert!(!session.is_armed(track, ArmKind::Midi));
    }

    #[test]
    fn an_armed_track_still_sounds_after_deleting_it_and_undoing() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(virtual_config());
        let (track, meter) = track_synth(&mut session);
        session.start_engine();
        session.arm(track, true);
        session.edit([Edit::Apply(Command::RemoveNode { id: track })]);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(10));
            session.maintain();
        }
        // Another edit makes the feeds forget the node.
        add(&mut session, Node::new("noodle.osc.sine"));
        session.undo();
        session.undo();
        assert!(session.project().graph().node(track).is_some());
        assert!(session.is_armed(track, ArmKind::Midi));
        session.midi_bus().send([0x90, 69, 100]);
        assert!(loudness(&mut session, meter, true) > 0.01);
    }

    #[test]
    fn an_armed_track_inside_a_group_plays_its_synth_too() {
        let mut session = Session::new(Nodes::all());
        session.set_audio_config(virtual_config());
        let (track, meter) = track_synth(&mut session);
        // A real track's input and its instrument live inside a group.
        let nodes: Vec<NodeId> = (1..=7).map(NodeId).collect();
        let mut next = session.project().next_node_id().0;
        let (_, command) = noodle_core::group::group_nodes(session.project(), &nodes[..5], || {
            next += 1;
            NodeId(next - 1)
        })
        .unwrap();
        session.edit([Edit::Apply(command)]);
        session.start_engine();
        session.arm(track, true);
        session.midi_bus().send([0x90, 69, 100]);
        assert!(loudness(&mut session, meter, true) > 0.01);
    }
}

#[cfg(test)]
mod recorded_clip_tests {
    use super::*;

    fn audio(clip: &Clip) -> (u64, u64) {
        let audio = clip.as_audio().unwrap();
        (audio.offset, audio.length)
    }

    #[test]
    fn a_take_skips_the_round_trip_latency() {
        let clip = recorded_clip(NodeId(1), Tick(960), "t.wav", 48_000, 1_200);
        assert_eq!(clip.start, Tick(960));
        assert_eq!(audio(&clip), (1_200, 46_800));
    }

    #[test]
    fn no_latency_leaves_the_take_whole() {
        let clip = recorded_clip(NodeId(1), Tick(0), "t.wav", 48_000, 0);
        assert_eq!(audio(&clip), (0, 48_000));
    }

    #[test]
    fn a_take_shorter_than_the_latency_keeps_a_frame() {
        let clip = recorded_clip(NodeId(1), Tick(0), "t.wav", 100, 5_000);
        assert_eq!(audio(&clip), (99, 1));
    }
}
