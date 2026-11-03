//! The open project and everything attached to it: its undo history, its file,
//! the diagnostics from compiling it, and the engine playing it.
//!
//! The UI never changes the project directly. Views return [`Edit`]s, and
//! [`Session::edit`] applies them through the history and keeps the audio in
//! step: structural changes recompile, and parameter changes go straight to
//! the engine's parameter cells.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub mod automate;
mod export;
pub mod learn_midi;
mod library;
pub mod record_automation;
mod render_clip;
pub mod replace;
mod stems;
pub use export::{ExportChoice, ExportSummary};
pub use library::LibraryEdit;

use crate::freezing::{Badge, Freezing, Look, Polled};
use noodle_core::{
    Clip, ClipId, Command, EditError, FrameId, History, MidiClip, NodeId, Project,
    TICKS_PER_QUARTER, Tick,
};
use noodle_engine::{
    Controller, Diagnostic, Pass, Registry, Settings, Telemetry, TempoTable, compile_replacing,
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

/// How often an open plugin editor is looked at for edited parameters.
const PLUGIN_LIVE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// An open plugin editor, as the session last saw it.
#[derive(Default)]
struct LiveEditor {
    polled: Option<Instant>,
    /// Each parameter's plugin ID and the value last read.
    seen: std::collections::BTreeMap<u32, f64>,
    /// A gesture's undo step is open.
    dragging: bool,
}

/// How often "learn" looks at the plugin's parameters.
const PARAM_LEARN_INTERVAL: Duration = Duration::from_millis(250);

/// A node waiting for its plugin's parameter to be moved.
struct ParamLearn {
    node: NodeId,
    key: noodle_plugins::EditorKey,
    /// Every parameter's value when learning started.
    baseline: Vec<(u32, f64)>,
    checked: Instant,
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
    /// Starts or stops "learn" on a plugin node: with it on, the next
    /// parameter moved in the plugin's editor gets a port on the node. Opens
    /// the editor if it isn't up.
    ParamLearn {
        node: NodeId,
        on: bool,
    },
    /// Saves to or deletes from the user's library. Not an undo step: the
    /// project is untouched.
    Library(LibraryEdit),
    /// Replaces the contents of a group with its library template's current
    /// version. One undo step.
    UpdateFromTemplate(NodeId),
    /// The Automate shortcut on a parameter: an Automation node, its lane
    /// and the wire into the parameter, as one undo step (see [`automate`]).
    Automate(noodle_core::Endpoint),
    /// Learn MIDI on a parameter: the next control moved on any open MIDI
    /// port becomes a MIDI Control node wired into it (see [`learn_midi`]).
    /// Choosing it again on the same parameter stops listening.
    LearnMidi(noodle_core::Endpoint),
    /// Stops listening for MIDI learn.
    CancelLearnMidi,
    /// Arms or disarms an Automation node for recording (see
    /// [`record_automation`]). Session state: not an undo step, not saved.
    ArmAutomation {
        node: NodeId,
        on: bool,
    },
    /// Replace node: changes `node` to `type_id`, keeping its ID, wires,
    /// lanes and clips where ports match (see [`replace`]). One undo step.
    ReplaceNode {
        node: NodeId,
        type_id: String,
    },
    /// Splits an audio clip into stems (see [`Session::split_to_stems`]). Not
    /// an edit itself: the tracks arrive as one undo step when the job is done.
    SplitToStems(noodle_core::ClipId),
    /// Renders a track or group to a clip (see [`Session::render_to_clip`]):
    /// the loop's span if `in_loop`, else the whole timeline. Not an edit
    /// itself: the track arrives as one undo step when the render is done.
    RenderToClip {
        group: NodeId,
        in_loop: bool,
    },
    /// A choice from the monitor button's right-click menu: monitor `node`
    /// in `mode` (alone, or added to the others), or stop. Not an undo step.
    MonitorAs {
        node: NodeId,
        mode: Option<MonitorMode>,
        add: bool,
    },
    /// A click on a node's monitor button. Not an undo step: the project is
    /// untouched.
    Monitor {
        node: NodeId,
        /// Ctrl or Cmd: exclusive, or with `shift` silencing every other
        /// Output.
        command: bool,
        /// Shift: sum into what is monitored already.
        shift: bool,
    },
}

/// How the nodes being monitored meet the project's Output nodes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MonitorMode {
    /// Summed into them.
    #[default]
    Sum,
    /// The Output nodes on the monitor's device are silenced.
    Exclusive,
    /// Every Output node is silenced.
    SilenceOthers,
}

/// What the monitor buttons have set. Not part of the project: it isn't
/// saved or undone.
#[derive(Clone, Debug, Default)]
struct NodeMonitor {
    nodes: Vec<NodeId>,
    mode: MonitorMode,
    device: String,
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
            // The MIDI Out nodes hold the hub; the clock it shares with
            // `midi` is how the device tells them when blocks are heard.
            midi_out: _,
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
                noodle_engine::wake::wake();
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

/// How often the stream's health is looked at while the window is idle.
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

/// How often sandboxed plugin hosts are looked at.
const HOST_WATCH_INTERVAL: Duration = Duration::from_millis(500);

/// A frame at about 60 Hz.
const FRAME: Duration = Duration::from_millis(16);

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
    /// What each open plugin editor last showed, to turn changes into edits.
    plugin_live: std::collections::BTreeMap<NodeId, LiveEditor>,
    /// Why a plugin's editor last failed to open, until it opens.
    plugin_editor_errors: std::collections::BTreeMap<NodeId, String>,
    /// A plugin node waiting for the user to move one of its parameters.
    param_learn: Option<ParamLearn>,
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
    /// The open MIDI input ports, each with the name it was chosen by.
    midi_connections: Vec<(String, noodle_io::MidiConnection)>,
    /// MIDI learn in progress: the parameter waiting for a control, and
    /// what listens for it.
    learning: Option<learn_midi::Learning>,
    /// Watches for the chosen MIDI port being unplugged and plugged back in.
    midi_watch: MidiWatch,
    /// Watches the audio files clips play for being replaced on disk.
    /// The stamps of the clips' files as of the last look, to notice a file
    /// that was replaced (see [`noodle_io::AudioFiles`]).
    file_stamps: HashMap<PathBuf, Option<noodle_io::FileStamp>>,
    /// Clips the track inputs couldn't schedule, as of the last feed.
    clip_problems: Vec<ClipProblem>,
    /// The renders behind frozen nodes and offline nodes, and the playback
    /// cache's own.
    freezing: Freezing,
    /// Whether the playback cache picks groups to cache by itself. On by
    /// default.
    auto_cache: bool,
    /// Where the current pass started: the last seek, or where the playhead
    /// was when the stream opened. Pausing and playing again carries on the
    /// same pass; only a jump begins a new one (ADR 0010).
    pass_start: Tick,
    /// The pass start whose cached entries the playing plan was built from,
    /// if it was built from any.
    serving: Option<Tick>,
    /// The loop as of the last look, to notice it changing.
    seen_loop: Option<(Tick, Tick)>,
    /// Where the playhead is while no stream is open, so it can be set and
    /// read when stopped, and playing starts from it.
    parked: Tick,
    /// Whether loop edits are sharing an undo step (see `set_loop`).
    loop_open: bool,
    /// Where the project was loaded from or last saved to.
    path: Option<PathBuf>,
    /// The history position when the project was last saved or loaded, to
    /// tell whether it has unsaved changes.
    saved_position: u64,
    /// The next ID [`Session::new_node_id`] can hand out. Views only get
    /// `&Session`, so it's a `Cell`.
    next_id: Cell<u64>,
    /// The same, for [`Session::new_frame_id`].
    next_frame_id: Cell<u64>,
    diagnostics: Vec<Diagnostic>,
    /// The device to play on.
    audio_config: AudioConfig,
    /// The nodes being listened to, and how (see [`NodeMonitor`]).
    node_monitor: NodeMonitor,
    audio: Option<Audio>,
    reroutes: Reroutes,
    /// The tracks that record audio when the record button is pressed.
    armed_audio: BTreeSet<NodeId>,
    /// The tracks that record MIDI, and play it live.
    armed_midi: BTreeSet<NodeId>,
    /// The take being recorded, if there is one.
    recording: Option<Recording>,
    /// A recording waiting out its count-in.
    counting_in: Option<CountIn>,
    /// Bars of metronome before a recording starts (0 to 2), and bars of the
    /// project played before a punch-in. App preferences, not saved with the
    /// project.
    count_in_bars: u8,
    pre_roll_bars: u8,
    /// How the MIDI of the next take is snapped. Not saved with the project.
    record_quantise: crate::midi_take::Quantise,
    /// The take folder and take being auditioned: it plays alone, in place of
    /// the comp, until this is cleared. Not saved, and not exported.
    audition: Option<(ClipId, usize)>,
    /// Whether the cycles of a loop recording of MIDI are takes or merge.
    record_midi: crate::midi_take::MidiRecord,
    /// Which Automation nodes are armed, and the Pass being recorded.
    automation: record_automation::Recorder,
    /// How armed nodes write. Saved as an app preference, not with the project.
    automation_mode: record_automation::AutomationMode,
    /// The lanes whose source is playing a move being recorded instead of
    /// the lane.
    live_lanes: std::collections::BTreeSet<noodle_core::LaneId>,
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
    /// The render to clip under way, if there is one.
    rendering: Option<render_clip::Run>,
    /// How the last export went, for the dialog.
    export_summary: Option<ExportSummary>,
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

/// A recording that starts when its count-in is over.
struct CountIn {
    due: Instant,
    /// Where the take begins and where it ends (the punch-out), as
    /// [`Recording::keep`].
    from: Tick,
    out: Option<Tick>,
}

/// A take being recorded.
struct Recording {
    /// The tracks that get an audio clip of it, and those that get a MIDI
    /// clip.
    audio_tracks: Vec<NodeId>,
    midi_tracks: Vec<NodeId>,
    /// Where the file starts on the timeline: the playhead when the input
    /// began to be recorded, which is before `keep.0` after a pre-roll.
    start: Tick,
    /// The part of the timeline the take keeps: from the first tick (the
    /// punch-in, or where the playhead was) to the second (the punch-out),
    /// if there is one. The file also holds the pre-roll before it.
    keep: (Tick, Option<Tick>),
    /// The loop being recorded in, when every cycle is a take.
    cycle: Option<(Tick, Tick)>,
    /// Whether the MIDI take has been restarted at `keep.0`, so the notes
    /// played in the pre-roll are not in it.
    midi_started: bool,
    /// The audio file, relative to the project file, when an input is being
    /// recorded.
    relative: Option<String>,
    /// The MIDI played during the take.
    midi: crate::midi_take::MidiTake,
    /// How the MIDI is snapped when the take ends.
    quantise: crate::midi_take::Quantise,
}

/// The clip for a take whose file has `frames` frames, showing the file from
/// frame `offset` (the frames before it were the sound already on its way
/// when the take began, or the pre-roll) for `length` frames, starting at
/// `at`. Keeps at least a frame.
fn recorded_clip(
    track: NodeId,
    at: Tick,
    relative: &str,
    frames: u64,
    offset: u64,
    length: u64,
) -> Clip {
    let skip = offset.min(frames.saturating_sub(1));
    let mut clip = Clip::audio(track, at, relative, length.min(frames - skip).max(1));
    if let noodle_core::ClipContent::Audio(audio) = &mut clip.content {
        audio.offset = skip;
    }
    clip
}

/// The takes a loop recording made. The file holds one pass through the loop
/// after another, `cycle` frames apart; the first pass shows its `window` from
/// frame `offset`. Each pass with at least a quarter of the window in the file
/// is a take (a sliver at the end, from stopping just after a wrap, is not).
/// Returns the takes and the one a new comp plays: the last complete one.
fn loop_takes(
    frames: u64,
    offset: u64,
    window: u64,
    cycle: u64,
) -> (Vec<noodle_core::Take>, usize) {
    let mut takes = Vec::new();
    let mut complete = None;
    if cycle == 0 {
        return (takes, 0);
    }
    for k in 0u64.. {
        let first = offset.saturating_add(k.saturating_mul(cycle));
        if first >= frames {
            break;
        }
        let content = (frames - first).min(window);
        if k > 0 && content * 4 < window {
            break;
        }
        takes.push(noodle_core::Take { delta: k * cycle });
        if content >= window {
            complete = Some(takes.len() - 1);
        }
    }
    let last = takes.len().saturating_sub(1);
    (takes, complete.unwrap_or(last))
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
    /// Output underruns since the stream opened, as of the last health check.
    underruns: u64,
}

/// Whether playback is coming from the cache, for the toolbar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CacheStatus {
    /// Turned off in the settings.
    Off,
    /// Nothing to cache yet: no engine running, or no group in the project.
    Idle,
    /// Computing the sound as it plays, with nothing from the cache yet.
    Live,
    /// Rendering entries for the cache, from 0 to 1.
    Rendering(f32),
    /// Playing groups from the cache, with no DSP in them.
    Cached,
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
            saved_position: 0,
            project: Project::new(),
            history: History::new(),
            registry,
            library: noodle_core::library::Library::in_memory(),
            plugins,
            plugin_scan,
            plugin_files,
            plugin_editors: Default::default(),
            plugin_live: Default::default(),
            plugin_editor_errors: Default::default(),
            param_learn: None,
            sandbox_plugins: false,
            host_restarts: Default::default(),
            hosts_checked: None,
            telemetry,
            clips,
            midi,
            samples,
            midi_connections: Vec::new(),
            learning: None,
            midi_watch: MidiWatch::default(),
            file_stamps: HashMap::new(),
            exporting: None,
            rendering: None,
            export_summary: None,
            stems: stems::Stems::new(),
            clip_problems: Vec::new(),
            freezing: Freezing::new(),
            auto_cache: true,
            pass_start: Tick(0),
            serving: None,
            seen_loop: None,
            parked: Tick(0),
            loop_open: false,
            path: None,
            next_id: Cell::new(0),
            next_frame_id: Cell::new(0),
            diagnostics: Vec::new(),
            audio_config: AudioConfig::default(),
            node_monitor: NodeMonitor::default(),
            audio: None,
            reroutes: Reroutes::default(),
            armed_audio: BTreeSet::new(),
            armed_midi: BTreeSet::new(),
            recording: None,
            counting_in: None,
            count_in_bars: 0,
            pre_roll_bars: 0,
            record_quantise: crate::midi_take::Quantise::default(),
            audition: None,
            record_midi: Default::default(),
            automation: record_automation::Recorder::default(),
            automation_mode: record_automation::AutomationMode::default(),
            live_lanes: Default::default(),
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

    /// The nodes whose output is playing on the monitor.
    pub fn monitored(&self) -> &[NodeId] {
        &self.node_monitor.nodes
    }

    /// How the monitored nodes meet the project's own outputs.
    pub fn monitor_mode(&self) -> MonitorMode {
        self.node_monitor.mode
    }

    /// The device the monitor plays on; empty is the main output.
    pub fn monitor_device(&self) -> &str {
        &self.node_monitor.device
    }

    /// Chooses the monitor's device. Playback restarts if it needs a stream
    /// the engine doesn't have.
    pub fn set_monitor_device(&mut self, device: &str) {
        if self.node_monitor.device != device {
            device.clone_into(&mut self.node_monitor.device);
            self.monitor_changed();
        }
    }

    /// Stops monitoring, if anything is.
    pub fn stop_monitor(&mut self) {
        if !self.node_monitor.nodes.is_empty() {
            self.node_monitor.nodes.clear();
            self.node_monitor.mode = MonitorMode::Sum;
            self.monitor_changed();
        }
    }

    /// A click on `node`'s monitor button.
    ///
    /// - Plain: monitor that node alone, summed with the project's outputs;
    ///   clicking it again stops.
    /// - Shift: add the node to those monitored, or take it off.
    /// - Ctrl or Cmd: monitor it alone and silence the other Output nodes on
    ///   the monitor's device. If it has the device to itself that changes
    ///   nothing.
    /// - Ctrl+Shift: monitor it alone and silence every Output node.
    ///
    /// Clicking the node again in the mode it is already in stops.
    pub fn click_monitor(&mut self, node: NodeId, command: bool, shift: bool) {
        let monitor = &mut self.node_monitor;
        let mode = match (command, shift) {
            (true, true) => MonitorMode::SilenceOthers,
            (true, false) => MonitorMode::Exclusive,
            _ => MonitorMode::Sum,
        };
        if shift && !command {
            match monitor.nodes.iter().position(|&n| n == node) {
                Some(at) => {
                    monitor.nodes.remove(at);
                }
                None => monitor.nodes.push(node),
            }
        } else if monitor.nodes == [node] && monitor.mode == mode {
            monitor.nodes.clear();
        } else {
            monitor.nodes = vec![node];
            monitor.mode = mode;
        }
        if monitor.nodes.is_empty() {
            monitor.mode = MonitorMode::Sum;
        }
        self.monitor_changed();
    }

    /// Monitors `node` in `mode`, alone or added to those already monitored,
    /// or stops monitoring it if `mode` is `None`.
    pub fn monitor_as(&mut self, node: NodeId, mode: Option<MonitorMode>, add: bool) {
        let monitor = &mut self.node_monitor;
        match (mode, add) {
            (None, _) => monitor.nodes.retain(|&n| n != node),
            (Some(_), true) if monitor.nodes.contains(&node) => {}
            (Some(_), true) => monitor.nodes.push(node),
            (Some(mode), false) => {
                monitor.nodes = vec![node];
                monitor.mode = mode;
            }
        }
        if monitor.nodes.is_empty() {
            monitor.mode = MonitorMode::Sum;
        }
        self.monitor_changed();
    }

    /// The monitor as the engine takes it.
    fn engine_monitor(&self) -> Option<noodle_engine::Monitor> {
        let monitor = &self.node_monitor;
        (!monitor.nodes.is_empty()).then(|| noodle_engine::Monitor {
            nodes: monitor.nodes.clone(),
            device: monitor.device.clone(),
            exclusive: monitor.mode == MonitorMode::Exclusive,
            silence_others: monitor.mode == MonitorMode::SilenceOthers,
        })
    }

    fn monitor_changed(&mut self) {
        self.recompile_with(false);
    }

    pub fn audio_config(&self) -> &AudioConfig {
        &self.audio_config
    }

    /// Chooses the device to play on. If playing, playback restarts there.
    pub fn set_audio_config(&mut self, config: AudioConfig) {
        let without_midi = |c: &AudioConfig| AudioConfig {
            midi_inputs: Vec::new(),
            ..c.clone()
        };
        let audio_changed = without_midi(&self.audio_config) != without_midi(&config);
        let midi_changed = self.audio_config.midi_inputs != config.midi_inputs;
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

    /// The names of the MIDI input ports that are open.
    #[cfg(test)]
    pub fn midi_inputs(&self) -> Vec<&str> {
        self.midi_connections
            .iter()
            .map(|(_, c)| c.name())
            .collect()
    }

    /// Opens the chosen MIDI input ports and closes the ones no longer
    /// chosen, leaving the rest as they are. A port that won't open is
    /// skipped and says why.
    fn connect_midi(&mut self) {
        let chosen = self.audio_config.midi_inputs.clone();
        let kept: Vec<_> = std::mem::take(&mut self.midi_connections)
            .into_iter()
            .filter_map(|(name, connection)| {
                if chosen.contains(&name) {
                    Some((name, connection))
                } else {
                    self.midi.all_notes_off_from(connection.name());
                    None
                }
            })
            .collect();
        self.midi_connections = kept;
        for name in chosen {
            if self.midi_connections.iter().any(|(n, _)| *n == name) {
                continue;
            }
            match noodle_io::connect_midi(&name, &self.midi) {
                Ok(connection) => self.midi_connections.push((name, connection)),
                Err(error) => self.message = Some(format!("MIDI input: {error}")),
            }
        }
    }

    /// Whether a MIDI port is chosen, so the window should look at the
    /// ports now and then even when nothing else makes it redraw.
    pub fn watching_midi(&self) -> bool {
        !self.audio_config.midi_inputs.is_empty()
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
                .is_some_and(|at| now.duration_since(at) < HOST_WATCH_INTERVAL)
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
                noodle_engine::wake::wake();
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

    /// Whether the node is waiting for a parameter to be moved.
    pub fn param_learning(&self, node: NodeId) -> bool {
        self.param_learn.as_ref().is_some_and(|l| l.node == node)
    }

    /// Opens the node's editor and remembers every parameter's value, so the
    /// next one that moves can be found. Works in the plugin's own window, so
    /// it needs no callback from the plugin and is the same for every format.
    fn start_param_learn(&mut self, node: NodeId) {
        self.param_learn = None;
        if !noodle_plugins::editor_is_open(node) {
            self.plugin_editor(node, true);
        }
        let Some(key) = self.plugin_editors.get(&node).copied() else {
            return;
        };
        match noodle_plugins::capture_key(key) {
            Ok(captured) => {
                self.param_learn = Some(ParamLearn {
                    node,
                    key,
                    baseline: captured.params,
                    checked: Instant::now(),
                });
                self.message = Some("Move a control in the plugin's editor".into());
            }
            Err(error) => self.message = Some(format!("Couldn't learn: {error}")),
        }
    }

    /// Looks, a few times a second, for the control the user moved. That
    /// parameter gets a port on the node. Building the node again closes the
    /// editor, so the state is kept in the same undo step, as the parameter
    /// picker does.
    fn poll_param_learn(&mut self) {
        let Some(learn) = &mut self.param_learn else {
            return;
        };
        let (node, key) = (learn.node, learn.key);
        if !noodle_plugins::editor_key_is_open(key) || !noodle_plugins::editor_instance_running(key)
        {
            self.param_learn = None;
            return;
        }
        if learn.checked.elapsed() < PARAM_LEARN_INTERVAL {
            return;
        }
        learn.checked = Instant::now();
        let baseline = learn.baseline.clone();
        let Ok(captured) = noodle_plugins::capture_params_key(key) else {
            return;
        };
        let Some(plugin) = self.plugin_of(node) else {
            self.param_learn = None;
            return;
        };
        let descriptor = plugin.descriptor().clone();
        let Some(config) = self.project.graph().node(node).map(|n| n.config.clone()) else {
            self.param_learn = None;
            return;
        };
        let mut chosen: Vec<u32> = noodle_plugins::exposed_params(&descriptor, &config)
            .iter()
            .map(|p| p.id)
            .collect();
        // A parameter already on the node can move by itself (a wire, an
        // automation lane), so only the others count.
        let others: Vec<(u32, f64)> = captured
            .params
            .iter()
            .filter(|(id, _)| !chosen.contains(id))
            .copied()
            .collect();
        let Some(id) = noodle_plugins::learned_param(&descriptor, &baseline, &others) else {
            return;
        };
        self.param_learn = None;
        let name = descriptor
            .params
            .iter()
            .find(|p| p.id == id)
            .map_or_else(|| format!("parameter {id}"), |p| p.name.clone());
        chosen.push(id);
        let config = config.with(
            noodle_plugins::PARAMS_KEY,
            noodle_core::Value::Text(noodle_plugins::params_text(&chosen)),
        );
        let command = Command::Batch(vec![
            Command::SetConfig {
                node,
                key: noodle_plugins::PARAMS_KEY.to_owned(),
                value: Some(noodle_core::Value::Text(noodle_plugins::params_text(
                    &chosen,
                ))),
            },
            captured.commands(node, &descriptor, &config),
        ]);
        self.plugin_editors.remove(&node);
        self.edit([Edit::Apply(command)]);
        self.message = Some(format!("Added {name} to the node"));
    }

    /// Why the node's plugin editor didn't open, if it hasn't.
    pub fn plugin_editor_error(&self, node: NodeId) -> Option<&str> {
        self.plugin_editor_errors.get(&node).map(String::as_str)
    }

    /// Stores the state of each plugin whose editor has closed. The node
    /// stays on the list until that worked, or its plugin instance is gone
    /// (the graph was built again, which loses what the editor changed).
    fn poll_plugin_editors(&mut self) {
        self.sync_open_plugin_editors();
        let closed: Vec<(NodeId, noodle_plugins::EditorKey)> = self
            .plugin_editors
            .iter()
            .map(|(n, k)| (*n, *k))
            .filter(|(_, key)| !noodle_plugins::editor_key_is_open(*key))
            .collect();
        for (node, key) in closed {
            self.end_live_gesture(node);
            self.plugin_live.remove(&node);
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

    /// While a plugin's editor is open, turns each change of a parameter
    /// value into a `SetParam` (no rebuild; the state waits for the window to
    /// close). Changes arriving one poll after another are one gesture and
    /// one undo step; a poll with no change ends it. Reads parameters only,
    /// at most every [`PLUGIN_LIVE_INTERVAL`].
    fn sync_open_plugin_editors(&mut self) {
        let now = Instant::now();
        let open: Vec<(NodeId, noodle_plugins::EditorKey)> = self
            .plugin_editors
            .iter()
            .map(|(n, k)| (*n, *k))
            .filter(|(_, key)| noodle_plugins::editor_key_is_open(*key))
            .collect();
        for (node, key) in open {
            let live = self.plugin_live.entry(node).or_default();
            if live
                .polled
                .is_some_and(|at| now.duration_since(at) < PLUGIN_LIVE_INTERVAL)
            {
                continue;
            }
            live.polled = Some(now);
            let Some(plugin) = self.plugin_of(node) else {
                continue;
            };
            let descriptor = plugin.descriptor().clone();
            let Ok(captured) = noodle_plugins::capture_params_key(key) else {
                continue;
            };
            let config = self
                .project
                .graph()
                .node(node)
                .map(|n| n.config.clone())
                .unwrap_or_default();
            let stored = self.project.graph().node(node).map(|n| n.params.clone());
            let live = self.plugin_live.entry(node).or_default();
            let commands =
                captured.changed_params(node, &descriptor, &config, &mut live.seen, |id| {
                    stored
                        .as_ref()
                        .and_then(|p| p.get(&noodle_plugins::param_key(id)).copied())
                });
            if commands.is_empty() {
                self.end_live_gesture(node);
            } else {
                self.plugin_live.entry(node).or_default().dragging = true;
                self.edit(commands.into_iter().map(Edit::Drag));
            }
        }
    }

    /// Closes the undo step of an editor gesture still going.
    fn end_live_gesture(&mut self, node: NodeId) {
        if let Some(live) = self.plugin_live.get_mut(&node)
            && std::mem::take(&mut live.dragging)
        {
            self.edit([Edit::EndDrag]);
        }
    }

    /// The hook that gives a render's registry the plugin node types.
    pub(crate) fn extend_registry(&self) -> Option<noodle_plugins::ExtendRegistry> {
        (!self.plugins.is_empty()).then(|| self.plugins.extender())
    }

    /// The chosen MIDI ports can each be unplugged and plugged back in, and
    /// the drivers don't say when, so look at the list of ports about once a
    /// second: let go of a port when it is gone, and open it again when it
    /// returns (or when it was missing from the start). Ports are matched by
    /// name without ALSA's numbers, which change when a device comes back.
    /// An unplug and replug between two looks that leaves the port's name
    /// as it was isn't seen.
    fn watch_midi(&mut self, now: Instant) {
        if self.audio_config.midi_inputs.is_empty() {
            return;
        }
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
        for name in self.audio_config.midi_inputs.clone() {
            let open = self.midi_connections.iter().position(|(n, _)| *n == name);
            if let Some(at) = open
                && !ports
                    .iter()
                    .any(|p| p == self.midi_connections[at].1.name())
            {
                let (_, connection) = self.midi_connections.remove(at);
                // The notes it held would never see their note-offs.
                self.midi.all_notes_off_from(connection.name());
                self.message = Some(format!("MIDI input {name:?} was unplugged"));
            } else if open.is_some() {
                continue;
            }
            if ports.iter().any(|p| noodle_io::same_port(p, &name))
                // Quietly when it fails: a port that is listed but won't open
                // would otherwise say so every second.
                && let Ok(connection) = (self.midi_watch.open)(&name, &self.midi)
            {
                self.midi_connections.push((name.clone(), connection));
                self.message = Some(format!("MIDI input {name:?} is connected again"));
            }
        }
    }

    /// Looks at whether an audio file a clip plays has
    /// been replaced on disk (re-exported from another program, say). If so,
    /// the clips are scheduled again so their streams read the new file, and
    /// anything frozen or offline that depends on it is keyed afresh, which
    /// renders it again.
    fn watch_files(&mut self) {
        let base = self.base().to_owned();
        let files = self
            .project
            .clips()
            .filter_map(|(_, clip)| match &clip.content {
                noodle_core::ClipContent::Audio(audio) => Some(base.join(&audio.source)),
                noodle_core::ClipContent::Midi(_) => None,
            });
        // Reads what the watcher thread last saw: no I/O here, and the
        // watcher wakes the window when a file differs.
        let watcher = noodle_io::AudioFiles::global();
        let files: Vec<PathBuf> = files.collect();
        self.file_stamps.retain(|path, _| files.contains(path));
        let mut changed = Vec::new();
        for path in files {
            let stamp = watcher.stamp(&path);
            // A file seen for the first time is only remembered.
            if let Some(before) = self.file_stamps.insert(path.clone(), stamp)
                && before != stamp
            {
                changed.push(path);
            }
        }
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
        self.history.position() != self.saved_position
    }

    /// Where in the edit history the project is, for telling whether it has
    /// changed since a copy was taken.
    pub fn history_position(&self) -> u64 {
        self.history.position()
    }

    /// The project file, if it has one.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The project as a file would hold it.
    pub fn project_ron(&self) -> String {
        self.project.to_ron()
    }

    /// Replaces the project with an autosaved copy of `origin` (`None` for an
    /// untitled one). It counts as unsaved, so closing asks first. Returns
    /// whether it loaded; if not, the reason is in [`Self::message`].
    pub fn recover(&mut self, text: &str, origin: Option<PathBuf>) -> bool {
        match Project::from_ron(text) {
            Ok(project) => {
                self.replace(project, origin);
                // No history position equals this one, so the project is
                // dirty until it is saved.
                self.saved_position = u64::MAX;
                self.message = Some("Recovered unsaved work. Save to keep it.".into());
                true
            }
            Err(error) => {
                self.message = Some(format!("Couldn't recover the autosave: {error}"));
                false
            }
        }
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
            if self.record_move(&edit) {
                continue;
            }
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
                    if self.automation.has_pass() {
                        let tick = self.playhead();
                        self.automation.released(self.automation_mode, tick);
                    }
                    self.history.end_group();
                    self.clips.set_preview(false);
                    continue;
                }
                Edit::PluginEditor { node, open } => {
                    self.plugin_editor(node, open);
                    continue;
                }
                Edit::ParamLearn { node, on } => {
                    if on {
                        self.start_param_learn(node);
                    } else {
                        self.param_learn = None;
                    }
                    continue;
                }
                Edit::Library(edit) => {
                    self.library_edit(edit);
                    continue;
                }
                Edit::MonitorAs { node, mode, add } => {
                    self.monitor_as(node, mode, add);
                    continue;
                }
                Edit::ReplaceNode { node, type_id } => {
                    self.history.end_group();
                    match replace::plan(&self.project, &self.registry, node, &type_id) {
                        Ok((command, report)) => {
                            let name = self
                                .registry
                                .get(&type_id)
                                .map_or(type_id.as_str(), |t| t.info().name);
                            self.message = Some(report.summary(name));
                            command
                        }
                        Err(why) => {
                            self.message = Some(format!("Can't replace: {why}"));
                            continue;
                        }
                    }
                }
                Edit::Monitor {
                    node,
                    command,
                    shift,
                } => {
                    self.click_monitor(node, command, shift);
                    continue;
                }
                Edit::SplitToStems(clip) => {
                    self.split_to_stems(clip);
                    continue;
                }
                Edit::RenderToClip { group, in_loop } => {
                    let region = self.project.transport_loop();
                    if !in_loop {
                        self.render_to_clip(group, None);
                    } else if region.on {
                        self.render_to_clip(group, Some((region.start, region.end)));
                    } else {
                        self.message = Some("Turn the loop on to render its span".into());
                    }
                    continue;
                }
                Edit::LearnMidi(target) => {
                    self.learn_midi(target);
                    continue;
                }
                Edit::CancelLearnMidi => {
                    self.learning = None;
                    continue;
                }
                Edit::ArmAutomation { node, on } => {
                    self.automation.arm(node, on);
                    continue;
                }
                Edit::Automate(target) => {
                    self.history.end_group();
                    let node = self.new_node_id();
                    let lane = self.project.next_lane_id();
                    match automate::command(&self.project, &self.registry, &target, node, lane) {
                        Ok(command) => command,
                        Err(error) => {
                            self.message = Some(format!("Couldn't automate: {error}"));
                            continue;
                        }
                    }
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
                self.saved_position = self.history.position();
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
    /// a Sampler or Sample node names into it, so a project saved anywhere takes its sounds
    /// along.
    fn save_samples(&mut self) {
        self.samples.set_dir(self.base().join("samples"));
        let hashes: std::collections::BTreeSet<String> = self
            .project
            .graph()
            .nodes()
            .filter(|(_, node)| {
                matches!(
                    node.type_id.as_str(),
                    noodle_nodes::SAMPLER_ID | noodle_nodes::SAMPLE_INPUT_ID
                )
            })
            .filter_map(|(_, node)| match node.config.get("sample") {
                Some(noodle_core::Value::Text(text)) => {
                    Some(noodle_nodes::SampleRef::parse(text).hash)
                }
                _ => None,
            })
            .filter(|hash| !hash.is_empty())
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
        self.project = project;
        self.history = History::new();
        self.saved_position = self.history.position();
        self.path = path;
        self.plugin_editors.clear();
        self.plugin_live.clear();
        self.param_learn = None;
        self.message = None;
        self.parked = Tick(0);
        self.pass_start = Tick(0);
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

    /// The sample rate the engine runs at, while it runs.
    pub fn engine_sample_rate(&self) -> Option<f32> {
        self.audio
            .as_ref()
            .map(|audio| audio.controller.settings().sample_rate)
    }

    /// Where the time from input to output goes, while the engine runs.
    pub fn latency_report(&self) -> Option<noodle_io::LatencyReport> {
        Some(self.audio.as_ref()?.playback.latency_report())
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
        let devices = self.wanted_devices();
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
                    underruns: 0,
                });
                self.recompile();
                if let Some(audio) = &mut self.audio {
                    audio.controller.set_tempo_map(self.project.tempo_map());
                    audio
                        .controller
                        .transport()
                        .set_loop(self.project.transport_loop().active());
                    // Carry on from where the playhead was left.
                    if self.parked != Tick(0) {
                        audio.controller.transport().seek(self.parked);
                    }
                    self.pass_start = self.parked;
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
            self.cancel_count_in();
            self.finish_automation_pass();
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

    /// The transport's loop: whether it is on and the span it repeats.
    pub fn loop_region(&self) -> LoopRegion {
        self.project.transport_loop()
    }

    /// Turns the loop on or off and sets its span, as an undoable edit of the
    /// project. Takes effect at once, even while playing.
    ///
    /// The bar fields change it on every frame of a drag, so the edits share
    /// one undo step until [`Session::settle_loop`] closes it.
    pub fn set_loop(&mut self, region: LoopRegion) {
        if region != self.project.transport_loop() {
            self.loop_open = true;
            self.edit([Edit::Drag(Command::SetLoop(region))]);
        }
    }

    /// Ends the undo step the loop's edits have been sharing, once the
    /// pointer is up.
    pub fn settle_loop(&mut self) {
        if std::mem::take(&mut self.loop_open) {
            self.edit([Edit::EndDrag]);
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
        self.cancel_count_in();
        let tick = Tick(tick.0.max(0));
        self.parked = tick;
        self.pass_start = tick;
        if let Some(audio) = &self.audio {
            audio.controller.transport().seek(tick);
        }
        self.begin_pass(tick);
    }

    /// A pass begins at `tick` (ADR 0010). If the plan that is playing serves
    /// another start from the cache it can't go on: it is recompiled at once,
    /// for the new start's entries or live. Otherwise the cache is looked at
    /// once the seeking pauses, so a drag of the playhead doesn't compile at
    /// every step.
    fn begin_pass(&mut self, tick: Tick) {
        if !self.auto_possible() || self.serving == Some(tick) {
            return;
        }
        if self.serving.is_some() {
            self.recompile_with(false);
        }
        self.freezing.touch();
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

    /// Whether an Automation node is armed.
    pub fn automation_armed(&self, node: NodeId) -> bool {
        self.automation.is_armed(node)
    }

    /// How armed Automation nodes write.
    pub fn automation_mode(&self) -> record_automation::AutomationMode {
        self.automation_mode
    }

    pub fn set_automation_mode(&mut self, mode: record_automation::AutomationMode) {
        self.automation_mode = mode;
    }

    /// What an Automation node's `value` shows: the value being written
    /// while a Pass is moving it, else the lane's value at the playhead,
    /// else the stored parameter.
    pub fn automation_value(&self, node: NodeId) -> Option<f32> {
        if let Some(held) = self.automation.held(node) {
            return Some(held);
        }
        let (_, lane) = self
            .project
            .lane_for(&noodle_core::Endpoint::new(node, record_automation::VALUE))?;
        lane.value_at(self.playhead().0 as f64)
    }

    /// A move of an armed node's `value` while the transport plays is
    /// recorded instead of applied. Says whether it was.
    fn record_move(&mut self, edit: &Edit) -> bool {
        let (Edit::Drag(command) | Edit::Apply(command)) = edit else {
            return false;
        };
        let Command::SetParam { node, key, value } = command else {
            return false;
        };
        let Some(value) = *value else {
            return false;
        };
        if key != record_automation::VALUE
            || !self.automation.is_armed(*node)
            || !self.transport_running()
        {
            return false;
        }
        let tick = self.playhead();
        let taken = self
            .automation
            .moved(self.automation_mode, *node, value, tick);
        // A single set, such as a typed value, is a touch that is over.
        if taken && matches!(edit, Edit::Apply(_)) {
            self.automation.released(self.automation_mode, tick);
        }
        taken
    }

    /// Feeds the Pass in progress, and ends it once the transport stops.
    fn maintain_automation_recording(&mut self) {
        let graph = self.project.graph();
        self.automation.retain(|node| graph.node(node).is_some());
        if !self.transport_running() {
            self.finish_automation_pass();
            self.push_live_automation();
            return;
        }
        if self.automation.armed().next().is_none() {
            return;
        }
        let tick = self.playhead();
        self.automation.advance(tick);
        let mode = self.automation_mode;
        let wired: Vec<NodeId> = self
            .automation
            .armed()
            .filter(|&node| {
                self.project
                    .graph()
                    .source(&noodle_core::Endpoint::new(node, record_automation::RECORD))
                    .is_some()
            })
            .collect();
        for node in wired {
            if let Some(reading) = self.telemetry.read_param(node, record_automation::RECORD) {
                self.automation.signal(mode, node, reading.value, tick);
            }
        }
        self.push_live_automation();
    }

    /// Plays the value being recorded in place of each armed node's lane,
    /// so a move is heard as it is made, and puts the lane back once it is
    /// let go.
    fn push_live_automation(&mut self) {
        let wanted: Vec<(noodle_core::LaneId, f32)> = self
            .automation
            .armed()
            .filter_map(|node| {
                let held = self.automation.held(node)?;
                let (lane, _) = self
                    .project
                    .lane_for(&noodle_core::Endpoint::new(node, record_automation::VALUE))?;
                Some((lane, held))
            })
            .collect();
        let Some(audio) = &mut self.audio else {
            self.live_lanes.clear();
            return;
        };
        for &(lane, held) in &wanted {
            let source = noodle_engine::lane_source_id(lane);
            audio
                .controller
                .set_param(source, noodle_engine::LIVE, held);
            audio
                .controller
                .set_param(source, noodle_engine::LIVE_ON, 1.0);
        }
        let now: std::collections::BTreeSet<_> = wanted.iter().map(|&(lane, _)| lane).collect();
        for &lane in self.live_lanes.difference(&now) {
            audio.controller.set_param(
                noodle_engine::lane_source_id(lane),
                noodle_engine::LIVE_ON,
                0.0,
            );
        }
        self.live_lanes = now;
    }

    /// Writes the Pass into the lanes as one undo step.
    fn finish_automation_pass(&mut self) {
        if !self.automation.has_pass() {
            return;
        }
        let tick = self.playhead();
        if let Some(command) = self.automation.finish(&self.project, tick) {
            self.edit([Edit::Apply(command)]);
        }
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

    /// When [`maintain`](Self::maintain) next has something to do that nothing
    /// will wake the window for, or `None` if it can sleep until something
    /// does. Finished jobs and scans wake it themselves
    /// (see [`noodle_engine::wake`]); this covers what has to be looked at:
    /// the stream's health, devices coming and going, and work waiting on a
    /// timer.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let mut soonest: Option<Instant> = None;
        let mut at = |when: Instant| {
            soonest = Some(soonest.map_or(when, |s| s.min(when)));
        };
        let frame = now + FRAME;
        if self.recording.is_some() {
            // The bus holds only so many messages, and a punch-out is
            // watched for.
            at(frame);
        }
        if let Some(count) = &self.counting_in {
            at(count.due);
        }
        if let Some(audio) = &self.audio {
            if audio.controller.has_pending() || audio.controller.transport().is_playing() {
                // A plan waiting for room, or the playhead moving.
                at(frame);
            }
            at(now + HEALTH_INTERVAL);
        }
        if self.learning.is_some() {
            at(now + FRAME);
        }
        if self.watching_midi() {
            at(self
                .midi_watch
                .checked
                .map_or(now, |c| c + MIDI_WATCH_INTERVAL));
        }
        if let Some(due) = self.freezing.due_at() {
            at(due);
        }
        if self.sandbox_plugins {
            at(now + HOST_WATCH_INTERVAL);
        }
        soonest.map(|s| s.max(now))
    }

    /// Housekeeping to do every frame: frees plans the audio thread is done
    /// with, and checks the device's health.
    pub fn maintain(&mut self) {
        // The bus holds only so many messages, so a take drains it often.
        if let Some(recording) = &mut self.recording {
            recording.midi.poll(self.midi.clock().anchor());
        }
        if self
            .counting_in
            .as_ref()
            .is_some_and(|count| Instant::now() >= count.due)
            && let Some(CountIn { from, out, .. }) = self.counting_in.take()
        {
            self.set_count_in_click(false);
            self.start_take(from, out);
        }
        self.watch_punch();
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
        self.watch_files();
        self.poll_export();
        self.poll_render();
        self.poll_split();
        self.poll_plugin_scan();
        self.poll_plugin_editors();
        self.poll_param_learn();
        self.poll_learn_midi();
        self.poll_plugin_hosts(Instant::now());
        self.maintain_automation_recording();
        let Some(audio) = &mut self.audio else {
            return;
        };
        audio.controller.maintain();
        if audio.controller.tempo_map() != self.project.tempo_map() {
            audio.controller.set_tempo_map(self.project.tempo_map());
        }
        // The loop is project state, so undo and loading change it too.
        let looping = self.project.transport_loop().active();
        audio.controller.transport().set_loop(looping);
        // A loop decides which passes the cache can serve.
        if looping != self.seen_loop {
            self.seen_loop = looping;
            if self.serving.is_some() {
                self.recompile_with(false);
            }
            self.freezing.touch();
            return;
        }
        let health = audio.playback.health();
        let now = Glitches {
            underruns: health.underruns(),
            input: health.input_glitches(),
        };
        audio.underruns = now.underruns;
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

    /// The take being auditioned, if any (see [`Session::set_take_audition`]).
    pub fn take_audition(&self) -> Option<(ClipId, usize)> {
        self.audition
    }

    /// Plays take `take` of the take folder (and of the folders linked to
    /// it) alone, in place of the comp, so it can be heard against the rest
    /// of the song; `None` goes back to the comp. The project is untouched.
    pub fn set_take_audition(&mut self, audition: Option<(ClipId, usize)>) {
        if self.audition != audition {
            self.audition = audition;
            self.feed_clips();
        }
    }

    /// Whether a loop recording of MIDI makes a take of every cycle or merges
    /// them into one clip.
    pub fn record_midi(&self) -> crate::midi_take::MidiRecord {
        self.record_midi
    }

    pub fn set_record_midi(&mut self, mode: crate::midi_take::MidiRecord) {
        self.record_midi = mode;
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
    ///
    /// With the punch range on, the take is the part of the timeline inside
    /// it: recording runs from the punch-in to the punch-out, and a pre-roll
    /// plays the project from some bars before the punch-in. With the loop
    /// on and the playhead in it, every cycle of the loop is a take in one
    /// take folder. A count-in of the metronome comes first when one is set.
    pub fn record(&mut self) {
        if self.recording.is_some() || self.counting_in.is_some() {
            return;
        }
        if self.armed_tracks().is_none() {
            return;
        }
        let pressed = self.playhead();
        let punch = self.project.punch().active();
        if let Some((_, end)) = punch
            && pressed >= end
        {
            self.message = Some("The playhead is past the punch range".into());
            return;
        }
        let from = punch.map_or(pressed, |(start, _)| start.max(pressed));
        let out = punch.map(|(_, end)| end);
        // The pre-roll starts the project playing some bars before the
        // punch-in, unless the playhead is already earlier.
        let begin = match punch {
            Some((start, _)) if pressed <= start && self.pre_roll_bars > 0 => {
                let back = i64::from(self.pre_roll_bars) * self.bar_ticks(start);
                pressed.min(Tick((start.0 - back).max(0)))
            }
            _ => pressed,
        };
        if begin != pressed {
            self.seek(begin);
        }
        let mut notice = None;
        if self.count_in_bars > 0 {
            if matches!(
                crate::metronome::binding(&self.project),
                crate::metronome::Binding::Missing
            ) {
                notice = Some("Add the metronome (transport bar) to hear the count-in");
            } else {
                self.open_stream(false);
                if self.audio.is_some() {
                    self.set_transport_running(false);
                    self.set_count_in_click(true);
                    let map = self.project.tempo_map();
                    let quarters = (i64::from(self.count_in_bars) * self.bar_ticks(begin)) as f64
                        / TICKS_PER_QUARTER as f64;
                    let seconds = quarters * 60.0 / map.bpm_at(begin);
                    self.counting_in = Some(CountIn {
                        due: Instant::now() + Duration::from_secs_f64(seconds),
                        from,
                        out,
                    });
                    self.message = Some("Counting in".into());
                    return;
                }
            }
        }
        self.start_take(from, out);
        if let Some(notice) = notice {
            self.message = Some(notice.into());
        }
    }

    /// The tracks armed for audio and for MIDI that are still in the project,
    /// and where takes are stored; `None` (with the reason in the message)
    /// when nothing can be recorded.
    fn armed_tracks(&mut self) -> Option<(Vec<NodeId>, Vec<NodeId>, PathBuf)> {
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
            return None;
        }
        let Some(project_path) = self.path.clone() else {
            self.message = Some("Save the project first: takes are stored next to it".into());
            return None;
        };
        Some((audio_tracks, midi_tracks, project_path))
    }

    /// How many ticks a bar lasts where `tick` is.
    fn bar_ticks(&self, tick: Tick) -> i64 {
        let map = self.project.tempo_map();
        let bar = map.position(tick).bar;
        map.signatures()
            .iter()
            .rev()
            .find(|change| change.bar <= bar)
            .map_or(4 * TICKS_PER_QUARTER, |change| {
                change.signature.ticks_per_bar()
            })
    }

    /// Turns the engine's count-in click on or off (see
    /// [`Transport::count_in`](noodle_engine::Transport::count_in)).
    fn set_count_in_click(&self, on: bool) {
        if let Some(audio) = &self.audio {
            audio.controller.transport().set_count_in(on);
        }
    }

    /// Whether a recording is waiting out its count-in.
    pub fn is_counting_in(&self) -> bool {
        self.counting_in.is_some()
    }

    /// Gives up on a recording that is counting in.
    pub(crate) fn cancel_count_in(&mut self) {
        if self.counting_in.take().is_some() {
            self.set_count_in_click(false);
            self.message = Some("Count-in stopped".into());
        }
    }

    /// Bars of metronome before a recording starts: 0, 1 or 2.
    pub fn count_in_bars(&self) -> u8 {
        self.count_in_bars
    }

    pub fn set_count_in_bars(&mut self, bars: u8) {
        self.count_in_bars = bars.min(2);
    }

    /// Bars of the project played before the punch-in.
    pub fn pre_roll_bars(&self) -> u8 {
        self.pre_roll_bars
    }

    pub fn set_pre_roll_bars(&mut self, bars: u8) {
        self.pre_roll_bars = bars.min(8);
    }

    /// Begins the take: `from` is where it starts keeping what is recorded,
    /// `out` where it stops (the punch-out).
    fn start_take(&mut self, from: Tick, out: Option<Tick>) {
        let Some((audio_tracks, midi_tracks, project_path)) = self.armed_tracks() else {
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
        // With the loop on and the take starting inside it, each cycle is a
        // take (unless a punch range holds the take to its own span).
        let cycle = self
            .project
            .transport_loop()
            .active()
            .filter(|&(a, b)| out.is_none() && from >= a && from < b);
        let keep_from = from.max(start);
        self.recording = Some(Recording {
            audio_tracks,
            midi_tracks,
            start,
            keep: (keep_from, out),
            cycle,
            midi_started: keep_from <= start,
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
            start: recording.keep.0,
            end: self.playhead().max(recording.keep.0),
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

    /// During a take: starts the MIDI take once the playhead reaches the
    /// punch-in, and ends the take at the punch-out.
    fn watch_punch(&mut self) {
        let Some(recording) = &self.recording else {
            return;
        };
        let head = self.playhead();
        let (restart, out) = (
            !recording.midi_started && head >= recording.keep.0,
            recording.keep.1.is_some_and(|out| head >= out),
        );
        if restart {
            let sample = match &self.audio {
                Some(audio) => audio.controller.transport().position(),
                None => self
                    .project
                    .tempo_map()
                    .sample_at(head, f64::from(FREEZE_SETTINGS.sample_rate)),
            };
            if let Some(recording) = &mut self.recording {
                recording.midi =
                    crate::midi_take::MidiTake::start(&self.midi, noodle_io::now_micros(), sample);
                recording.midi_started = true;
            }
        }
        if out {
            self.stop_recording();
        }
    }

    /// Ends the take and adds it to the project: an audio clip, if an input
    /// was recorded, and a MIDI clip, if any notes were played, on each track
    /// that was armed, all in one undo step. Does nothing if not recording.
    pub fn stop_recording(&mut self) {
        let Some(mut recording) = self.recording.take() else {
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
        let mut midi_passes = None;
        if !recording.midi_tracks.is_empty() {
            let finish = crate::midi_take::Finish {
                stop_micros,
                output_latency_micros: latency.as_micros() as u64,
                sample_rate: rate,
                tempo_map: self.project.tempo_map(),
                quantise: recording.quantise,
                anchor: self.midi.clock().anchor(),
            };
            recording.midi.poll(finish.anchor);
            // With the loop on and Takes chosen, each pass through it is a
            // take; Merge leaves them all in one clip.
            if self.record_midi == crate::midi_take::MidiRecord::Takes
                && let Some((a, b)) = recording.cycle
            {
                let table = TempoTable::new(self.project.tempo_map(), rate as f32);
                let at = |tick: Tick| table.sample_at_tick(tick) as f64;
                let cycles = crate::midi_take::Cycles {
                    first: at(b) - at(recording.keep.0),
                    cycle: at(b) - at(a),
                    start: a,
                    end: b,
                };
                midi_passes = recording
                    .midi
                    .passes(&finish, &cycles)
                    .filter(|(_, passes)| passes.len() > 1);
            }
            if midi_passes.is_none()
                && let Some(found) = recording.midi.clip_so_far(&finish)
            {
                notes.push(found);
            }
        }
        if take.is_none() && notes.is_empty() && midi_passes.is_none() {
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
        // it and the take lines up with the project the player heard. The
        // pre-roll before the punch-in is skipped too.
        let round_trip = ((latency + self.input_latency()).as_secs_f64() * rate) as u64;
        let table = TempoTable::new(self.project.tempo_map(), rate as f32);
        let at = |tick: Tick| table.sample_at_tick(tick);
        let (keep_from, keep_out) = recording.keep;
        let offset = round_trip + at(keep_from).saturating_sub(at(recording.start));
        if let Some(done) = take.as_ref().filter(|done| done.frames <= offset) {
            // Stopped before the take began, in the pre-roll.
            let _ = std::fs::remove_file(&done.path);
            take = None;
            if notes.is_empty() && midi_passes.is_none() {
                notices.push("Nothing was recorded".into());
                self.message = Some(notices.join("; "));
                return;
            }
        }
        // Where the file has the part of the timeline the take keeps.
        let length = keep_out.map_or(u64::MAX, |out| at(out).saturating_sub(at(keep_from)));
        let folder = take
            .as_ref()
            .zip(recording.cycle)
            .and_then(|(take, (a, b))| {
                let cycle = at(b).saturating_sub(at(a));
                let window = at(b).saturating_sub(at(keep_from));
                let (takes, active) = loop_takes(take.frames, offset, window, cycle);
                (takes.len() > 1).then_some((takes, active, window))
            });
        // Folders linked across tracks are compared by take number, so MIDI
        // has as many takes as the audio, whichever rule counted the slivers.
        if let (Some((_, passes)), Some((takes, _, _))) = (&mut midi_passes, &folder) {
            passes.truncate(takes.len());
            while passes.len() < takes.len() {
                let mut empty = passes
                    .last()
                    .cloned()
                    .unwrap_or_else(|| MidiClip::new(Tick(1)));
                empty.notes.clear();
                empty.controls.clear();
                passes.push(empty);
            }
        }
        // The take's clip tempo is the timeline's where it was recorded. The
        // sections on every armed track show the same clip, so they share one
        // warp. Its seconds count from the start of the file, which is
        // `offset` frames before the sections begin.
        let warp = take.as_ref().filter(|_| rate > 0.0).map(|take| {
            let skip = offset.min(take.frames.saturating_sub(1)) as f64 / rate;
            let seconds = take.frames as f64 / rate;
            let warp =
                noodle_core::ClipWarp::recorded(self.project.tempo_map(), keep_from, skip, seconds);
            (self.project.next_warp_id(), warp)
        });
        let mut taken: Vec<ClipId> = Vec::new();
        for &track in &audio_tracks {
            if let (Some(take), Some(relative)) = (&take, &recording.relative) {
                let mut clip = match &folder {
                    Some((_, _, window)) => {
                        recorded_clip(track, keep_from, relative, take.frames, offset, *window)
                    }
                    None => recorded_clip(track, keep_from, relative, take.frames, offset, length),
                };
                if let (Some((_, active, _)), noodle_core::ClipContent::Audio(audio)) =
                    (&folder, &mut clip.content)
                {
                    let takes = folder.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                    audio.takes = Some(noodle_core::TakeFolder::new(takes, *active));
                }
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
                let id = self.project.new_clip_id();
                taken.push(id);
                commands.push(Command::AddClip { id, clip });
            }
        }
        for &track in &midi_tracks {
            for (start, midi) in &notes {
                let mut clip = Clip::midi(track, *start, midi.length);
                if let noodle_core::ClipContent::Midi(content) = &mut clip.content {
                    content.notes = midi.notes.clone();
                    content.controls = midi.controls.clone();
                }
                let id = self.project.new_clip_id();
                taken.push(id);
                commands.push(Command::AddClip { id, clip });
            }
        }
        if let Some((start, passes)) = &midi_passes {
            for &track in &midi_tracks {
                let length = passes.first().map_or(Tick(1), |p| p.length);
                let mut clip = Clip::midi(track, *start, length);
                if let noodle_core::ClipContent::Midi(content) = &mut clip.content {
                    let latest = passes.len() - 1;
                    content.takes = Some(noodle_core::MidiTakes::new(
                        passes
                            .iter()
                            .map(|pass| noodle_core::TakeContent {
                                notes: pass.notes.clone(),
                                controls: pass.controls.clone(),
                            })
                            .collect(),
                        latest,
                    ));
                    content.play_comp();
                }
                let id = self.project.new_clip_id();
                taken.push(id);
                commands.push(Command::AddClip { id, clip });
            }
        }
        // The clips of one take on several tracks are one link, so they can
        // be slid, cut and warped as the one performance they are.
        if taken.len() >= 2 {
            commands.push(Command::SetClipGroup {
                id: self.project.new_clip_group_id(),
                group: Some(noodle_core::ClipGroup {
                    members: taken.into_iter().collect(),
                }),
            });
        }
        self.edit([Edit::Apply(Command::Batch(commands))]);
        if let (Some(relative), Some(take)) = (&recording.relative, &take) {
            let count = folder
                .as_ref()
                .map_or(String::new(), |f| format!(" as {} takes", f.0.len()));
            notices.push(match take.dropped {
                0 => format!("Recorded {relative}{count}"),
                lost => format!("Recorded {relative}{count}, but {lost} frames of input were lost"),
            });
        }
        for (_, midi) in &notes {
            notices.push(format!("Recorded {} notes", midi.notes.len()));
        }
        if let Some((_, passes)) = &midi_passes {
            notices.push(format!("Recorded MIDI as {} takes", passes.len()));
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
        let auditioning = self
            .audition
            .map(|(id, take)| self.project.auditioning(id, take));
        let project = auditioning.as_ref().unwrap_or(&self.project);
        self.clip_problems = self.clips.update(project, &table, rate as u32, base);
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

    /// Whether the playback cache could be used now: it is on, a stream is
    /// open and the project has a group to cache.
    fn auto_possible(&self) -> bool {
        self.auto_cache
            && self.audio.is_some()
            && self
                .project
                .graph()
                .nodes()
                .any(|(_, node)| node.type_id == noodle_core::group::GROUP && node.parent.is_none())
    }

    /// The pass the cache is looked up for, and the passes worth having
    /// entries for: the one playing, then the hot starts, 0 and the loop's
    /// start. `end` is the render length in frames.
    ///
    /// A pass that starts before the loop's end wraps there. Which clusters
    /// the cache serves in it is decided per cluster by its tier (see
    /// `Analysis::entries`): position-pure ones always, bounded ones from the
    /// loop's start on, with their tail carried across the wrap, and
    /// unbounded ones from the loop's start only, reset at the wrap.
    fn cache_passes(&self, rate: f32, end: usize) -> (Option<Pass>, Vec<Pass>) {
        if !self.auto_possible() {
            return (None, Vec::new());
        }
        let table = TempoTable::new(self.project.tempo_map(), rate);
        let looping = self.project.transport_loop().active();
        let pass = |start: Tick| {
            let from = table.sample_at_tick(start) as usize;
            match looping {
                Some((l, to)) if start < to => {
                    let frames = (table.sample_at_tick(to) as usize).saturating_sub(from);
                    Pass::new(start, &table, frames.max(1)).looping(l, to)
                }
                _ => Pass::new(start, &table, end.saturating_sub(from).max(rate as usize)),
            }
        };
        let current = pass(self.pass_start);
        let mut wanted = vec![current];
        for start in std::iter::once(Tick(0)).chain(looping.map(|(from, _)| from)) {
            if !wanted.iter().any(|p| p.tick == start) {
                wanted.push(pass(start));
            }
        }
        (Some(current), wanted)
    }

    /// Looks at the cache of renders for the project and starts the ones it
    /// lacks. Returns what plays in place of the cached nodes, if anything is
    /// frozen or offline, or the playback cache has entries for the pass.
    fn refresh_freezing(&mut self, start: bool) -> Option<noodle_nodes::FreezePlan> {
        // Frozen audio is keyed by the rate it was rendered at, so use the
        // stream's, or a common one before a stream is open.
        let settings = self
            .audio
            .as_ref()
            .map_or(FREEZE_SETTINGS, |audio| audio.controller.settings());
        let rate = settings.sample_rate;
        let base = self.base().to_owned();
        let clips = self.clips.clone();
        // Only measured when something needs it: it looks at every audio file.
        let needed = Freezing::in_use(&self.project, &self.registry) || self.auto_possible();
        let frames = if needed { self.freeze_frames(rate) } else { 0 };
        let (pass, passes) = self.cache_passes(rate, frames);
        let look = Look {
            project: &self.project,
            registry: &self.registry,
            settings,
            base: &base,
            frames,
            start,
            extend_registry: self.extend_registry(),
            live: &|node| clips.is_live(node),
            pass,
            passes,
        };
        let plan = match self.freezing.refresh(&look) {
            Ok(plan) => plan,
            Err(message) => {
                self.message = Some(message);
                None
            }
        };
        self.serving = plan
            .as_ref()
            .zip(pass)
            .filter(|(plan, _)| {
                plan.states.iter().any(|(kind, state)| {
                    kind.is_auto() && *state == noodle_nodes::TargetState::Ready
                })
            })
            .map(|(_, pass)| pass.tick);
        plan
    }

    /// The start of the pass whose cached entries are playing in place of
    /// the groups the playback cache picked, if any are.
    #[cfg(test)]
    pub fn cached_pass(&self) -> Option<Tick> {
        self.serving
    }

    /// Whether the playback cache picks groups to cache by itself.
    pub fn auto_cache(&self) -> bool {
        self.auto_cache
    }

    /// Where the sound is coming from, for the indicator in the toolbar.
    pub fn cache_status(&self) -> CacheStatus {
        if !self.auto_cache {
            CacheStatus::Off
        } else if !self.auto_possible() {
            CacheStatus::Idle
        } else if self.serving.is_some() {
            CacheStatus::Cached
        } else if let Some(fraction) = self.freezing.progress() {
            CacheStatus::Rendering(fraction)
        } else {
            CacheStatus::Live
        }
    }

    /// What the cache of renders holds. Reads the folder.
    pub fn cache_readout(&mut self) -> Result<crate::freezing::CacheReadout, String> {
        self.freezing.readout()
    }

    /// Times the output ran dry since playback started, if a stream is open.
    pub fn underruns(&self) -> Option<u64> {
        Some(self.audio.as_ref()?.underruns)
    }

    /// Turns the playback cache's own picks on or off. The explicit freeze
    /// is not affected.
    pub fn set_auto_cache(&mut self, on: bool) {
        if self.auto_cache != on {
            self.auto_cache = on;
            self.recompile();
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

    /// The devices besides the main one the stream plays on: those Output
    /// nodes are tied to, and the monitor's.
    fn wanted_devices(&self) -> Vec<String> {
        let mut devices = noodle_engine::output_devices(self.project.graph());
        let monitor = &self.node_monitor;
        if !monitor.nodes.is_empty()
            && !monitor.device.is_empty()
            && !devices.contains(&monitor.device)
        {
            devices.push(monitor.device.clone());
        }
        devices
    }

    /// Compiles the project. `start` is whether renders that are missing may
    /// be started now.
    fn recompile_with(&mut self, start: bool) {
        // A plugin the machine lacks stays in the project as a pass-through.
        self.registry.provide_placeholders(self.project.graph());
        // Samplers read their samples from beside the project file.
        // An unsaved project has no folder yet: its samples stay in memory
        // until the first save writes them.
        let dir = self.path.is_some().then(|| self.base().join("samples"));
        self.samples.set_dir_option(dir);
        // Devices are fixed for a stream's lifetime, so an Output node tied
        // to another one means playing afresh. `play` compiles.
        self.node_monitor
            .nodes
            .retain(|&node| self.project.graph().node(node).is_some());
        let wanted = self.wanted_devices();
        if self.audio.as_ref().is_some_and(|a| a.devices != wanted) {
            self.reopen_stream();
            if self.audio.is_some() {
                return;
            }
        }
        let monitor = self.engine_monitor();
        if let Some(audio) = &mut self.audio {
            audio.controller.set_monitor(monitor);
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
}

/// Whether a command changes how a clip is warped.
fn edits_warp(command: &Command) -> bool {
    match command {
        Command::SetWarp { .. } | Command::AddWarp { .. } => true,
        Command::Batch(commands) => commands.iter().any(edits_warp),
        _ => false,
    }
}

/// The transport's loop (saved with the project).
pub type LoopRegion = noodle_core::TransportLoop;

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
            | Command::SetClipGroup { .. }
            | Command::AddFrame { .. }
            | Command::RemoveFrame { .. }
            | Command::SetFrame { .. } => {}
            // The transport takes the loop when the edit is applied (see
            // `Session::edit`).
            Command::SetLoop(_) => {}
            // Solo is read when the project is compiled: it mutes the other
            // tracks' stages, which no parameter reaches.
            Command::SetParam { key, .. }
                if noodle_core::group::parse_control_key(key)
                    .is_some_and(|(control, _)| control == noodle_core::group::SOLO) =>
            {
                self.structural = true;
            }
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
        midi_inputs: Vec::new(),
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
    /// A sine through a chorus in a group, to the output. The chorus makes
    /// the group worth caching (see `DEFAULT_MIN_COST`).
    fn grouped_sine(session: &mut Session) -> (NodeId, NodeId) {
        let osc = session.new_node_id();
        let chorus = session.new_node_id();
        let out = session.new_node_id();
        session.edit([
            Edit::Apply(Command::AddNode {
                id: osc,
                node: Node::new("noodle.osc.sine"),
            }),
            Edit::Apply(Command::AddNode {
                id: chorus,
                node: Node::new(noodle_nodes::CHORUS_ID),
            }),
            Edit::Apply(Command::AddNode {
                id: out,
                node: Node::new(OUTPUT_ID),
            }),
            Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(osc, "out"),
                to: Endpoint::new(chorus, "in"),
            })),
            Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(chorus, "out"),
                to: Endpoint::new(out, "in"),
            })),
        ]);
        let mut next = session.new_node_id().0;
        let (group, command) =
            noodle_core::group::group_nodes(session.project(), &[osc, chorus], || {
                next += 1;
                NodeId(next)
            })
            .unwrap();
        session.edit([Edit::Apply(command)]);
        (group, osc)
    }

    #[test]
    fn the_loop_is_saved_with_the_project_and_comes_back_on_opening_and_on_undo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.ron");
        let mut session = Session::new(Nodes::all());
        let region = LoopRegion {
            on: true,
            start: Tick(960),
            end: Tick(7680),
        };
        // A drag of a bar field sets it many times: one undo step.
        for end in [Tick(2000), Tick(4000), Tick(7680)] {
            session.set_loop(LoopRegion { end, ..region });
        }
        session.settle_loop();
        assert_eq!(session.loop_region(), region);
        assert!(session.save_as(&path));
        let reopened = Session::open(Nodes::all(), &path).unwrap();
        assert_eq!(reopened.loop_region(), region);
        session.undo();
        assert_eq!(session.loop_region(), LoopRegion::default());
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

    /// The entries in the cache folder.
    fn cached_entries(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .map(|files| {
                files
                    .flatten()
                    .filter(|file| file.path().extension().is_none_or(|e| e != "partial"))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn stop_stop_play_plays_the_groups_from_the_cache_and_an_edit_renders_only_what_it_changed() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().to_owned());
        let (group, osc) = grouped_sine(&mut session);
        session.set_audio_config(virtual_config());
        session.start_engine();
        assert!(session.engine_running(), "{:?}", session.message());

        // The first pass is computed; the cache fills in behind it.
        assert!(matches!(
            session.cache_status(),
            CacheStatus::Live | CacheStatus::Rendering(_)
        ));
        until(&mut session, "the group is cached", |s| {
            s.cached_pass() == Some(Tick(0))
        });
        assert_eq!(session.cache_status(), CacheStatus::Cached);
        assert_eq!(cached_entries(dir.path()), 1);
        // The readout counts what is on disk, and the output has not run dry
        // on a virtual device.
        let readout = session.cache_readout().unwrap();
        assert_eq!(readout.entries, 1);
        assert!(readout.bytes > 0, "{readout:?}");
        assert_eq!(readout.evicted, 0);
        assert_eq!(session.underruns(), Some(0));
        // Group entries are not frozen nodes: nothing is badged.
        assert!(session.freeze_badges().is_empty());

        // Play, stop, stop (which rewinds), play: the same pass again, with
        // nothing to render and the cache still playing.
        session.play();
        std::thread::sleep(Duration::from_millis(100));
        session.stop();
        session.stop();
        assert_eq!(session.cached_pass(), Some(Tick(0)));
        let tap = session.virtual_capture().expect("the virtual output");
        tap.start();
        session.play();
        std::thread::sleep(Duration::from_millis(400));
        let heard = tap.take();
        let peak = heard.iter().fold(0.0f32, |a, &x| a.max(x.abs()));
        assert!(peak > 0.01, "peak {peak}");
        assert_eq!(session.cached_pass(), Some(Tick(0)));
        assert!(!session.freeze_pending());
        assert_eq!(cached_entries(dir.path()), 1);

        // A fader move is after the cut: the entry stands.
        let port = session.project().graph().group_ports(group).outputs[0].clone();
        session.edit([Edit::Apply(Command::SetParam {
            node: port.node,
            key: port.control_key(noodle_core::group::GAIN),
            value: Some(-12.0),
        })]);
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(10));
            session.maintain();
        }
        assert_eq!(session.cached_pass(), Some(Tick(0)));
        assert_eq!(cached_entries(dir.path()), 1);

        // A seek is a new pass: no entry for it yet, so it plays live, and
        // the cache catches up for the new start.
        session.seek(Tick(1000));
        assert_eq!(session.cached_pass(), None);
        until(&mut session, "the new start is cached", |s| {
            s.cached_pass() == Some(Tick(1000))
        });
        assert_eq!(cached_entries(dir.path()), 2);

        // Back at 0 the entry is there already.
        session.seek(Tick(0));
        until(&mut session, "the first start is served again", |s| {
            s.cached_pass() == Some(Tick(0))
        });
        assert_eq!(cached_entries(dir.path()), 2);

        // A node inside the group makes both entries stale. Only the pass
        // playing (a hot start) is rendered again; the other start is
        // rendered if it is played.
        session.edit([Edit::Apply(Command::SetParam {
            node: osc,
            key: "frequency".into(),
            value: Some(220.0),
        })]);
        until(&mut session, "the edit is cached", |s| {
            s.cached_pass() == Some(Tick(0)) && !s.freeze_pending()
        });
        assert_eq!(cached_entries(dir.path()), 3);

        // Turned off, everything plays live.
        session.set_auto_cache(false);
        assert_eq!(session.cached_pass(), None);
        assert_eq!(session.cache_status(), CacheStatus::Off);
        assert!(!session.auto_cache());
    }

    #[test]
    fn a_loop_is_cached_from_its_start_and_an_unbounded_group_stays_live_elsewhere_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().to_owned());
        grouped_sine(&mut session);
        session.set_loop(LoopRegion {
            on: true,
            start: Tick(0),
            end: Tick(3840),
        });
        session.set_audio_config(virtual_config());
        session.start_engine();
        assert!(session.engine_running(), "{:?}", session.message());

        // The lap from the loop's start is the entry, reset at the wrap.
        until(&mut session, "the loop is cached", |s| {
            s.cached_pass() == Some(Tick(0))
        });
        // The sine never forgets, so from the middle of the loop there is no
        // entry for it: it plays live, and nothing is rendered for the start.
        session.seek(Tick(1000));
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(10));
            session.maintain();
        }
        assert_eq!(session.cached_pass(), None);
        assert!(!session.freeze_pending());
        // Back at the loop's start, the lap is there already.
        session.seek(Tick(0));
        until(&mut session, "the lap is served again", |s| {
            s.cached_pass() == Some(Tick(0))
        });
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
        let whole = ExportChoice::mixdown(
            ExportFormat::Wav16,
            noodle_engine::ChannelLayout::Stereo,
            None,
        );
        assert!(
            session
                .start_export(dir.path().join("x.wav"), whole)
                .is_err()
        );
        let empty = ExportChoice::mixdown(
            ExportFormat::Wav16,
            noodle_engine::ChannelLayout::Stereo,
            Some((2.0, 2.0)),
        );
        assert!(
            session
                .start_export(dir.path().join("x.wav"), empty)
                .is_err()
        );

        let path = dir.path().join("half.wav");
        let choice = ExportChoice::mixdown(
            ExportFormat::Wav16,
            noodle_engine::ChannelLayout::Stereo,
            Some((0.0, 0.5)),
        );
        session.start_export(path.clone(), choice.clone()).unwrap();
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
    fn groups_are_exported_to_files_in_a_folder_with_the_mixdown() {
        use noodle_io::ExportFormat;

        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().join("cache"));
        sine_chain(&mut session, None);
        assert!(session.exportable_groups().is_empty());
        // Fold the oscillator into a group, which then has an output.
        let osc = session
            .project()
            .graph()
            .nodes()
            .find(|(_, n)| n.type_id == "noodle.osc.sine")
            .map(|(id, _)| id)
            .unwrap();
        let project = session.project().clone();
        let mut next = project.clone();
        let (group, command) =
            noodle_core::group::group_nodes(&project, &[osc], || next.new_node_id()).unwrap();
        session.edit([Edit::Apply(command)]);
        let groups = session.exportable_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, group);

        let folder = dir.path().join("out");
        let mut choice = ExportChoice::mixdown(
            ExportFormat::WavFloat,
            noodle_engine::ChannelLayout::Stereo,
            Some((0.0, 0.5)),
        );
        choice.groups = vec![group];
        session.start_export(folder.clone(), choice).unwrap();
        until(&mut session, "the export finishes", |s| {
            s.export_progress().is_none()
        });
        assert_eq!(
            session.message(),
            Some("Exported 2 files of 0.5 s to out"),
            "{:?}",
            session.message()
        );
        let name = &groups[0].1;
        for file in ["Mixdown".to_owned(), name.clone()] {
            let audio = noodle_io::decode_file(&folder.join(format!("{file}.wav"))).unwrap();
            assert_eq!(audio.samples.len(), 24_000 * 2, "{file}");
            assert!(audio.samples.iter().any(|&x| x.abs() > 0.05), "{file}");
        }
        // The dialog is told how loud each file is.
        let summary = session.last_export().unwrap();
        let names: Vec<&str> = summary.files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Mixdown", name.as_str()]);
        assert!(summary.files.iter().all(|(_, l)| l.lufs.is_finite()));

        // Nothing chosen is refused.
        let mut none = ExportChoice::mixdown(
            ExportFormat::WavFloat,
            noodle_engine::ChannelLayout::Stereo,
            None,
        );
        none.mixdown = false;
        assert!(session.start_export(folder, none).is_err());
    }

    #[test]
    fn export_file_names_are_clean_and_never_repeat() {
        let mut taken = vec!["Mixdown".to_owned()];
        for (name, want) in [
            ("Mixdown", "Mixdown 2"),
            ("mixdown", "mixdown 3"),
            ("Lead/Pad: 1", "Lead_Pad_ 1"),
            ("", "Track"),
            ("..", "Track 2"),
        ] {
            let got = export::unique_file_name(name, &taken);
            assert_eq!(got, want);
            taken.push(got);
        }
    }

    #[test]
    fn a_surround_export_writes_every_channel_of_the_layout() {
        use noodle_engine::ChannelLayout;
        use noodle_io::ExportFormat;

        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().join("cache"));
        // A sine panned to the centre speaker of the default 5.1.
        sine_chain(&mut session, Some(noodle_nodes::SURROUND_PAN_ID));
        let path = dir.path().join("surround.wav");
        let choice = ExportChoice::mixdown(
            ExportFormat::WavFloat,
            ChannelLayout::Surround51,
            Some((0.0, 0.25)),
        );
        session.start_export(path.clone(), choice.clone()).unwrap();
        until(&mut session, "the export finishes", |s| {
            s.export_progress().is_none()
        });
        let audio = noodle_io::decode_file(&path).unwrap();
        assert_eq!(audio.channels, 6);
        assert_eq!(audio.samples.len(), 12_000 * 6);
        let peak = |channel: usize| {
            audio
                .samples
                .iter()
                .skip(channel)
                .step_by(6)
                .fold(0.0f32, |peak, x| peak.max(x.abs()))
        };
        assert!(peak(2) > 0.05, "centre {}", peak(2));
        for channel in [0, 1, 3, 4, 5] {
            assert_eq!(peak(channel), 0.0, "channel {channel}");
        }

        // A stereo project exported as 5.1 fills the front pair only.
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().join("cache2"));
        sine_chain(&mut session, Some("noodle.util.pan"));
        let path = dir.path().join("stereo-in-51.wav");
        session.start_export(path.clone(), choice).unwrap();
        until(&mut session, "the export finishes", |s| {
            s.export_progress().is_none()
        });
        let audio = noodle_io::decode_file(&path).unwrap();
        assert_eq!(audio.channels, 6);
        let peak = |channel: usize| {
            audio
                .samples
                .iter()
                .skip(channel)
                .step_by(6)
                .fold(0.0f32, |peak, x| peak.max(x.abs()))
        };
        assert!(peak(0) > 0.05 && peak(1) > 0.05);
        assert!((2..6).all(|channel| peak(channel) == 0.0));
    }

    #[test]
    fn exporting_to_a_narrower_layout_folds_the_mix_down() {
        use noodle_engine::ChannelLayout;
        use noodle_io::ExportFormat;

        let dir = tempfile::tempdir().unwrap();
        let peak = |path: &std::path::Path, channels: usize| {
            let audio = noodle_io::decode_file(path).unwrap();
            assert_eq!(audio.channels, channels);
            audio.samples.iter().fold(0.0f32, |p, x| p.max(x.abs()))
        };
        let export = |session: &mut Session, name: &str, layout| {
            let path = dir.path().join(name);
            let choice = ExportChoice::mixdown(ExportFormat::WavFloat, layout, Some((0.0, 0.25)));
            session.start_export(path.clone(), choice).unwrap();
            until(session, "the export finishes", |s| {
                s.export_progress().is_none()
            });
            path
        };

        // A sine panned hard right, exported as mono: the right channel is
        // not lost.
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().join("cache"));
        sine_chain(&mut session, Some("noodle.util.pan"));
        let pan = NodeId(3);
        session.edit(vec![Edit::Apply(Command::SetParam {
            node: pan,
            key: "pan".into(),
            value: Some(1.0),
        })]);
        let path = export(&mut session, "mono.wav", ChannelLayout::Mono);
        assert!(peak(&path, 1) > 0.05);

        // The default 5.1 pan puts everything in the centre; as stereo it
        // folds into both ears instead of vanishing.
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().join("cache2"));
        sine_chain(&mut session, Some(noodle_nodes::SURROUND_PAN_ID));
        let path = export(&mut session, "stereo.wav", ChannelLayout::Stereo);
        assert!(peak(&path, 2) > 0.05);
    }

    #[test]
    fn an_export_includes_offline_nodes_and_can_be_cancelled() {
        use noodle_io::ExportFormat;

        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Nodes::all());
        session.use_cache_dir(dir.path().join("cache"));
        sine_chain(&mut session, Some("noodle.offline.reverse"));
        // Inside the 30 s the offline node is rendered over.
        let choice = ExportChoice::mixdown(
            ExportFormat::Flac16,
            noodle_engine::ChannelLayout::Stereo,
            Some((1.0, 2.0)),
        );
        let path = dir.path().join("reversed.flac");
        session.start_export(path.clone(), choice.clone()).unwrap();
        until(&mut session, "the export finishes", |s| {
            s.export_progress().is_none()
        });
        assert_eq!(session.message(), Some("Exported 1.0 s to reversed.flac"));
        let audio = noodle_io::decode_file(&path).unwrap();
        assert!(audio.samples.iter().any(|&x| x.abs() > 0.05), "silent");

        // Cancelled, it leaves no file.
        let path = dir.path().join("cancelled.flac");
        session.start_export(path.clone(), choice.clone()).unwrap();
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
            midi_inputs: vec!["No Such Port".into()],
            ..AudioConfig::default()
        });
        assert!(session.midi_inputs().is_empty());
        assert!(
            session
                .message()
                .is_some_and(|m| m.starts_with("MIDI input:"))
        );
        // Turning it off again needs no port and says nothing new.
        session.set_audio_config(AudioConfig::default());
        assert!(session.midi_inputs().is_empty());
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
        session.audio_config.midi_inputs = vec!["Keys".into()];
        let mut now = Instant::now();
        let mut step = |session: &mut Session| {
            now += Duration::from_secs(2);
            session.watch_midi(now);
        };
        // Chosen but not open (it was missing when chosen): opens once listed.
        step(&mut session);
        assert_eq!(session.midi_inputs(), ["Keys"]);
        // Unplugged.
        PORTS.with(|p| p.borrow_mut().clear());
        step(&mut session);
        assert!(session.midi_inputs().is_empty());
        assert!(session.message().is_some_and(|m| m.contains("unplugged")));
        // Still gone: nothing changes.
        step(&mut session);
        assert!(session.midi_inputs().is_empty());
        // Plugged back in.
        PORTS.with(|p| *p.borrow_mut() = vec!["Keys".into()]);
        step(&mut session);
        assert_eq!(session.midi_inputs(), ["Keys"]);
        // Looks at most once a second.
        PORTS.with(|p| p.borrow_mut().clear());
        session.watch_midi(now + Duration::from_millis(100));
        assert_eq!(session.midi_inputs(), ["Keys"]);
    }

    #[test]
    fn a_port_that_comes_back_with_other_numbers_is_reopened_and_held_notes_released() {
        let mut session = Session::new(Nodes::all());
        session.midi_watch.list = Box::new(fake_ports);
        session.midi_watch.open = fake_open;
        PORTS.with(|p| *p.borrow_mut() = vec!["Keys MIDI 1 24:0".into()]);
        session.audio_config.midi_inputs = vec!["Keys MIDI 1 24:0".into()];
        let mut receiver = session.midi.subscribe();
        let mut now = Instant::now();
        let mut step = |session: &mut Session| {
            now += Duration::from_secs(2);
            session.watch_midi(now);
        };
        step(&mut session);
        assert_eq!(session.midi_inputs(), ["Keys MIDI 1 24:0"]);
        // Unplugged and plugged in again between two looks, as number 28.
        PORTS.with(|p| *p.borrow_mut() = vec!["Keys MIDI 1 28:0".into()]);
        step(&mut session);
        assert_eq!(session.midi_inputs(), ["Keys MIDI 1 28:0"]);
        // The notes it held were let go.
        assert_eq!(receiver.pop(), Some([0xB0, 123, 0]));
        // And it stays open on later looks.
        step(&mut session);
        assert_eq!(session.midi_inputs(), ["Keys MIDI 1 28:0"]);
    }

    #[test]
    fn several_midi_ports_are_watched_and_replugged_one_at_a_time() {
        let mut session = Session::new(Nodes::all());
        session.midi_watch.list = Box::new(fake_ports);
        session.midi_watch.open = fake_open;
        PORTS.with(|p| *p.borrow_mut() = vec!["Keys".into(), "Pads".into()]);
        session.audio_config.midi_inputs = vec!["Keys".into(), "Pads".into(), "Absent".into()];
        let mut keys = session.midi.subscribe_port("Keys");
        let mut pads = session.midi.subscribe_port("Pads");
        let mut now = Instant::now();
        let mut step = |session: &mut Session| {
            now += Duration::from_secs(2);
            session.watch_midi(now);
        };
        step(&mut session);
        assert_eq!(session.midi_inputs(), ["Keys", "Pads"]);
        // One goes: only its notes are released, and the other stays open.
        PORTS.with(|p| *p.borrow_mut() = vec!["Pads".into()]);
        step(&mut session);
        assert_eq!(session.midi_inputs(), ["Pads"]);
        assert_eq!(keys.pop(), Some([0xB0, 123, 0]));
        assert_eq!(pads.pop(), None);
        // The missing one turns up, and the one that left returns.
        PORTS.with(|p| *p.borrow_mut() = vec!["Keys".into(), "Pads".into(), "Absent".into()]);
        step(&mut session);
        assert_eq!(session.midi_inputs(), ["Pads", "Keys", "Absent"]);
    }

    #[test]
    fn falling_back_keeps_the_input_and_drops_the_output_choices() {
        let chosen = AudioConfig {
            host: Some("jack".into()),
            output: Some("jack:system".into()),
            input: noodle_io::InputChoice::Default,
            sample_rate: Some(96_000),
            buffer_size: Some(64),
            channels: Some(6),
            midi_inputs: vec!["Keys".into()],
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
        // Solo mutes other tracks' stages, so it recompiles.
        let solo = Command::SetParam {
            node,
            key: "solo.1".into(),
            value: Some(1.0),
        };
        assert!(Effect::of(&solo).structural);
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
    fn a_take_keeps_the_pedal_and_the_mod_wheel_and_plays_them_back() {
        use noodle_core::ControlKind;
        let dir = tempfile::tempdir().unwrap();
        let example =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/midi-cc.ron");
        let mut session = Session::open(Nodes::all(), &example).expect("the example opens");
        // The example's own clip goes; the take replaces it.
        let existing: Vec<_> = session.project().clips().map(|(id, _)| id).collect();
        for id in existing {
            session.edit([Edit::Apply(Command::RemoveClip { id })]);
        }
        let track = NodeId(1);
        assert!(session.save_as(&dir.path().join("song.ron")));
        session.fake_input = Some(Box::new(NoInput));
        session.arm_kind(track, ArmKind::Midi, true);
        session.record();
        assert!(session.is_recording(), "{:?}", session.message());
        let wait = || std::thread::sleep(Duration::from_millis(80));
        // Pedal down, a short note with the wheel moving, then the pedal up.
        let bus = session.midi_bus().clone();
        bus.send([0xB0, 64, 127]);
        bus.send([0x90, 57, 100]);
        bus.send([0xB0, 1, 20]);
        wait();
        bus.send([0xB0, 1, 100]);
        bus.send([0x80, 57, 0]);
        wait();
        wait();
        bus.send([0xB0, 64, 0]);
        wait();
        session.stop_recording();

        let (_, clip) = session
            .project()
            .clips()
            .find(|(_, clip)| clip.as_midi().is_some())
            .expect("the take became a clip");
        let midi = clip.as_midi().unwrap().clone();
        assert_eq!(midi.notes.len(), 1);
        let lane = |kind| -> Vec<(Tick, f32)> {
            let mut points: Vec<_> = midi
                .controls
                .iter()
                .filter(|p| p.kind == kind)
                .map(|p| (p.tick, p.value))
                .collect();
            points.sort_by_key(|p| p.0);
            points
        };
        let pedal = lane(ControlKind::Controller(64));
        let wheel = lane(ControlKind::Controller(1));
        assert_eq!(
            pedal.iter().map(|p| p.1).collect::<Vec<_>>(),
            [1.0, 0.0],
            "{pedal:?}"
        );
        assert_eq!(wheel.len(), 2, "{wheel:?}");
        assert!((wheel[0].1 - 20.0 / 127.0).abs() < 1e-6);
        assert!((wheel[1].1 - 100.0 / 127.0).abs() < 1e-6);

        // Played back, the pedal holds the note after the key is let go; the
        // same take without its controllers does not.
        let tick_to_frames = |tick: Tick| (tick.0 as f64 / 960.0 * 0.5 * 48_000.0) as usize;
        let note_end = tick_to_frames(Tick(midi.notes[0].start.0 + midi.notes[0].length.0));
        let pedal_up = tick_to_frames(pedal[1].0);
        let (from, to) = (note_end + 4_000, pedal_up.saturating_sub(500));
        assert!(from + 1_000 < to, "the window {from}..{to}");
        let render = |project: &Project| {
            let mut registry = noodle_engine::Registry::with_builtins();
            let render = noodle_nodes::render_project_with_clips(
                project,
                &mut registry,
                dir.path(),
                noodle_engine::Settings {
                    sample_rate: 48_000.0,
                    max_frames: 128,
                    channels: 1,
                },
                pedal_up + 8_000,
            )
            .unwrap();
            assert!(render.problems.is_empty(), "{:?}", render.problems);
            render.render.samples
        };
        let level = |samples: &[f32]| {
            (samples[from..to].iter().map(|x| x * x).sum::<f32>() / (to - from) as f32).sqrt()
        };
        let heard = render(session.project());
        assert!(level(&heard) > 0.01, "held by the pedal: {}", level(&heard));
        let mut bare = session.project().clone();
        let id = bare.clips().next().map(|(id, _)| id).unwrap();
        let mut stripped = bare.clip(id).unwrap().clone();
        if let noodle_core::ClipContent::Midi(m) = &mut stripped.content {
            m.controls.clear();
        }
        noodle_core::History::default()
            .apply(&mut bare, Command::SetClip { id, clip: stripped })
            .unwrap();
        assert!(level(&render(&bare)) < 0.001, "without the pedal");
        // After the pedal is up, and the clip's end, it is quiet again.
        assert!(heard[pedal_up + 6_000..].iter().all(|x| x.abs() < 0.001));
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

        // The take's clips are one link, undone with the take.
        let ids: Vec<_> = session.project().clips().map(|(id, _)| id).collect();
        assert_eq!(session.project().linked_with(ids[0]), ids);

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

    /// Feeds `frames` of mono input to the recorder in chunks the writer can
    /// keep up with.
    fn feed(tap: &mut noodle_io::RecordTap, frames: usize, value: impl Fn(usize) -> f32) {
        for chunk in (0..frames).step_by(4_800) {
            let samples: Vec<f32> = (chunk..(chunk + 4_800).min(frames)).map(&value).collect();
            tap.push(&samples);
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
    }

    #[test]
    fn loop_recording_makes_a_take_of_every_cycle_in_one_folder() {
        const CYCLE: usize = 24_000;
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 2);
        session.arm(tracks[0], true);
        session.arm(tracks[1], true);
        // One beat (half a second at 120 bpm) is the loop.
        session.set_loop(LoopRegion {
            on: true,
            start: Tick(0),
            end: Tick(960),
        });
        session.record();
        assert!(session.is_recording(), "{:?}", session.message());
        // Three whole cycles and half of a fourth, each at its own level.
        let level = |i: usize| (i / CYCLE + 1) as f32 * 0.1 + (i % CYCLE) as f32 * 1e-7;
        feed(&mut tap, 3 * CYCLE + CYCLE / 2, level);
        session.stop_recording();

        let clips: Vec<_> = session.project().clips().collect();
        assert_eq!(clips.len(), 2, "{:?}", session.message());
        for (_, clip) in &clips {
            let audio = clip.as_audio().unwrap();
            assert_eq!((audio.offset, audio.length), (0, CYCLE as u64));
            let folder = audio.takes.as_ref().expect("a take folder");
            let deltas: Vec<u64> = folder.takes.iter().map(|t| t.delta).collect();
            assert_eq!(deltas, [0, 24_000, 48_000, 72_000]);
            // A new comp plays the last complete take.
            assert_eq!(folder.comp.len(), 1);
            assert_eq!(folder.comp[0].take, 2);
        }
        // Each take is its cycle's samples, in the one file.
        let file = noodle_io::read_wav(&dir.path().join("song recordings/take-001.wav")).unwrap();
        assert_eq!(file.samples.len(), 3 * CYCLE + CYCLE / 2);
        let folder = clips[0].1.as_audio().unwrap().takes.as_ref().unwrap();
        for (k, take) in folder.takes.iter().enumerate() {
            let at = take.delta as usize + 100;
            assert!(
                (file.samples[at] - ((k + 1) as f32 * 0.1 + 100.0 * 1e-7)).abs() < 1e-6,
                "take {k}"
            );
        }
        // Both tracks' folders are one link, and one undo takes them away.
        let ids: Vec<_> = clips.iter().map(|(id, _)| *id).collect();
        assert_eq!(session.project().linked_with(ids[0]), ids);
        assert!(session.message().unwrap().contains("4 takes"));
        session.undo();
        assert_eq!(session.project().clips().count(), 0);
    }

    #[test]
    fn a_loop_recording_comped_by_swipes_plays_the_comps_samples() {
        use crate::timeline::comp;
        const CYCLE: usize = 24_000;
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 2);
        session.arm(tracks[0], true);
        session.arm(tracks[1], true);
        session.set_loop(LoopRegion {
            on: true,
            start: Tick(0),
            end: Tick(960),
        });
        session.record();
        let level = |i: usize| (i / CYCLE + 1) as f32 * 0.1 + (i % CYCLE) as f32 * 1e-7;
        feed(&mut tap, 3 * CYCLE + CYCLE / 2, level);
        session.stop_recording();
        let ids: Vec<ClipId> = session.project().clips().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 2);

        // Swipe the first quarter second from take 0 and the next eighth from
        // take 1; the rest stays the last take. Swiping one folder comps both.
        let map = session.project().tempo_map().clone();
        for (range, take) in [((0, 480), 0), ((480, 720), 1)] {
            let command = comp::swipe(
                session.project(),
                &map,
                ids[0],
                (Tick(range.0), Tick(range.1)),
                take,
                &mut |_| 48_000,
            )
            .unwrap();
            session.edit([Edit::Apply(command)]);
        }
        for id in &ids {
            let folder = session
                .project()
                .clip(*id)
                .unwrap()
                .as_audio()
                .unwrap()
                .takes
                .clone()
                .unwrap();
            let comp: Vec<(u64, usize)> = folder.comp.iter().map(|s| (s.at, s.take)).collect();
            assert_eq!(comp, [(0, 0), (12_000, 1), (18_000, 2)], "{id:?}");
        }

        // It plays the comp's samples (a join's crossfade aside), and the
        // offline render, which export uses, is the same one.
        // Both tracks into a mixer and the output, as they would be in a song.
        let mut bare = Project::new();
        let mut commands = vec![
            Command::AddNode {
                id: NodeId(90),
                node: Node::new(noodle_engine::OUTPUT_ID),
            },
            Command::AddNode {
                id: NodeId(91),
                node: Node::new("noodle.util.mix").with_config(
                    noodle_core::Config::new().with("inputs", noodle_core::Value::Int(2)),
                ),
            },
            Command::Connect(noodle_core::Connection {
                from: noodle_core::Endpoint::new(NodeId(91), "out"),
                to: noodle_core::Endpoint::new(NodeId(90), "in"),
            }),
        ];
        for (n, id) in ids.iter().enumerate() {
            let track = NodeId(n as u64 + 1);
            commands.push(Command::AddNode {
                id: track,
                node: Node::new(noodle_nodes::TRACK_INPUT_ID),
            });
            commands.push(Command::Connect(noodle_core::Connection {
                from: noodle_core::Endpoint::new(track, "audio"),
                to: noodle_core::Endpoint::new(NodeId(91), format!("in{}", n + 1)),
            }));
            let mut clip = session.project().clip(*id).unwrap().clone();
            clip.node = track;
            if let noodle_core::ClipContent::Audio(audio) = &mut clip.content {
                audio.warp = None;
            }
            commands.push(Command::AddClip { id: *id, clip });
        }
        History::new()
            .apply(&mut bare, Command::Batch(commands))
            .unwrap();
        let mut registry = noodle_engine::Registry::with_builtins();
        let rendered = noodle_nodes::render_project_with_clips(
            &bare,
            &mut registry,
            dir.path(),
            noodle_engine::Settings {
                sample_rate: 48_000.0,
                max_frames: 512,
                channels: 2,
            },
            CYCLE,
        )
        .unwrap();
        assert!(rendered.problems.is_empty(), "{:?}", rendered.problems);
        let left: Vec<f32> = rendered.render.samples.iter().step_by(2).copied().collect();
        for (frame, take) in [(6_000usize, 0usize), (15_000, 1), (21_000, 2)] {
            // Two tracks of the same comp, summed.
            let expect = 2.0 * ((take + 1) as f32 * 0.1 + frame as f32 * 1e-7);
            assert!(
                (left[frame] - expect).abs() < 1e-4,
                "frame {frame}: {} vs {expect}",
                left[frame]
            );
        }
        session.undo();
        session.undo();
        let folder = session
            .project()
            .clip(ids[1])
            .unwrap()
            .as_audio()
            .unwrap()
            .takes
            .clone()
            .unwrap();
        assert_eq!(folder.comp.len(), 1, "one undo per swipe");
    }

    #[test]
    fn stopping_a_loop_recording_in_the_first_cycle_makes_a_plain_clip() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        session.set_loop(LoopRegion {
            on: true,
            start: Tick(0),
            end: Tick(960),
        });
        session.record();
        feed(&mut tap, 10_000, |_| 0.2);
        session.stop_recording();
        let clip = session
            .project()
            .clips()
            .next()
            .unwrap()
            .1
            .as_audio()
            .unwrap()
            .clone();
        assert!(clip.takes.is_none());
        assert_eq!(clip.length, 10_000);
    }

    #[test]
    fn a_punch_keeps_only_its_range_and_a_pre_roll_starts_the_project_earlier() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        // Punch in at bar 2 for one beat, with a bar of pre-roll.
        session.edit([Edit::Apply(Command::SetPunch(noodle_core::PunchRange {
            on: true,
            start: Tick(3840),
            end: Tick(3840 + 960),
        }))]);
        session.set_pre_roll_bars(1);
        session.advance_playhead(Tick(3840));
        session.record();
        assert!(session.is_recording(), "{:?}", session.message());
        assert_eq!(session.playhead(), Tick(0), "the pre-roll goes back a bar");
        // The bar of pre-roll, the beat in the range, and a beat past it.
        feed(&mut tap, 96_000 + 24_000 + 24_000, |i| i as f32 * 1e-6);
        // Still before the punch-out: the take goes on.
        session.advance_playhead(Tick(3840 + 480));
        session.maintain();
        assert!(session.is_recording());
        session.advance_playhead(Tick(3840 + 960));
        session.maintain();
        assert!(!session.is_recording(), "the punch-out ends the take");

        let (_, clip) = session.project().clips().next().unwrap();
        assert_eq!(clip.start, Tick(3840));
        let audio = clip.as_audio().unwrap();
        assert_eq!((audio.offset, audio.length), (96_000, 24_000));
        let file = noodle_io::read_wav(&dir.path().join("song recordings/take-001.wav")).unwrap();
        assert!((file.samples[audio.offset as usize] - 0.096).abs() < 1e-6);
    }

    #[test]
    fn recording_past_the_punch_range_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        session.edit([Edit::Apply(Command::SetPunch(noodle_core::PunchRange {
            on: true,
            start: Tick(960),
            end: Tick(1920),
        }))]);
        session.advance_playhead(Tick(2000));
        session.record();
        assert!(!session.is_recording());
        assert!(session.message().unwrap().contains("past the punch range"));
    }

    #[test]
    fn stopping_in_the_pre_roll_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, mut tap, tracks) = recording_session(dir.path(), 1);
        session.arm(tracks[0], true);
        session.edit([Edit::Apply(Command::SetPunch(noodle_core::PunchRange {
            on: true,
            start: Tick(3840),
            end: Tick(4800),
        }))]);
        session.set_pre_roll_bars(1);
        session.advance_playhead(Tick(3840));
        session.record();
        feed(&mut tap, 4_800, |_| 0.3);
        session.stop_recording();
        assert_eq!(session.project().clips().count(), 0);
        assert!(!dir.path().join("song recordings/take-001.wav").exists());
    }

    #[test]
    fn a_count_in_waits_before_the_take_starts() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _tap, tracks) = recording_session(dir.path(), 1);
        session.set_audio_config(crate::testkit::virtual_audio());
        session.arm(tracks[0], true);
        session.set_count_in_bars(1);
        // Without a metronome in the project there is nothing to count with:
        // the take starts at once, and says why.
        session.record();
        assert!(session.is_recording());
        assert!(session.message().unwrap().contains("metronome"));
        session.stop_recording();

        let ids = std::array::from_fn(|_| session.new_node_id());
        let press = crate::metronome::press(session.project(), || ids).unwrap();
        session.edit([Edit::Apply(press)]);
        session.record();
        assert!(session.is_counting_in());
        assert!(!session.is_recording());
        // Pressing record's button again gives up on it.
        session.cancel_count_in();
        assert!(!session.is_counting_in());
        session.record();
        assert!(session.is_counting_in());
        // When the bar is over, the take starts.
        session.counting_in.as_mut().unwrap().due = Instant::now();
        session.maintain();
        assert!(!session.is_counting_in());
        assert!(session.is_recording());
        session.stop_recording();
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
    fn an_idle_session_asks_for_no_wake_up_and_a_busy_one_for_the_next_look() {
        let mut session = Session::new(Nodes::all());
        let now = Instant::now();
        assert_eq!(session.next_deadline(now), None, "nothing to look at");

        session.set_audio_config(virtual_config());
        session.start_engine();
        assert!(session.engine_running(), "{:?}", session.message());
        let idle = session.next_deadline(now).expect("health is checked");
        assert!(
            idle - now >= Duration::from_millis(500),
            "idle health checks are not frame-rate: {:?}",
            idle - now
        );

        session.set_transport_running(true);
        let playing = session.next_deadline(now).unwrap();
        assert!(playing - now <= Duration::from_millis(20), "the playhead");
    }

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
    fn monitor_clicks_follow_the_modifiers() {
        let mut session = Session::new(Nodes::all());
        let (a, b) = (NodeId(1), NodeId(2));
        for id in [a, b] {
            session.edit([Edit::Apply(Command::AddNode {
                id,
                node: Node::new("noodle.osc.sine"),
            })]);
        }
        let state = |s: &Session| (s.monitored().to_vec(), s.monitor_mode());
        // Plain: one node, summed; again stops.
        session.click_monitor(a, false, false);
        assert_eq!(state(&session), (vec![a], MonitorMode::Sum));
        session.click_monitor(b, false, false);
        assert_eq!(state(&session), (vec![b], MonitorMode::Sum));
        // Shift adds, and takes off again.
        session.click_monitor(a, false, true);
        assert_eq!(state(&session), (vec![b, a], MonitorMode::Sum));
        session.click_monitor(b, false, true);
        assert_eq!(state(&session), (vec![a], MonitorMode::Sum));
        session.click_monitor(a, false, true);
        assert!(session.monitored().is_empty());
        // Ctrl: alone and exclusive. On a node already summed it changes the
        // mode rather than stopping; the same again stops.
        session.click_monitor(a, false, false);
        session.click_monitor(a, true, false);
        assert_eq!(state(&session), (vec![a], MonitorMode::Exclusive));
        session.click_monitor(a, true, false);
        assert_eq!(state(&session), (vec![], MonitorMode::Sum));
        // Ctrl+Shift silences everything else.
        session.click_monitor(b, true, true);
        assert_eq!(state(&session), (vec![b], MonitorMode::SilenceOthers));
        session.stop_monitor();
        assert_eq!(state(&session), (vec![], MonitorMode::Sum));
    }

    /// How much of each of `frequencies` the first channel of interleaved
    /// stereo `heard` holds, after its first 20 ms (the fade-in).
    fn energy_at(heard: &[f32], frequencies: [f32; 2]) -> [f32; 2] {
        frequencies.map(|f| {
            let (mut sin, mut cos) = (0.0f32, 0.0f32);
            for (n, frame) in heard.chunks(2).enumerate().skip(960) {
                let phase = std::f32::consts::TAU * f * n as f32 / 48_000.0;
                sin += frame[0] * phase.sin();
                cos += frame[0] * phase.cos();
            }
            (sin * sin + cos * cos).sqrt() / (heard.len() / 2) as f32
        })
    }

    #[test]
    fn a_monitored_node_is_heard_summed_exclusively_or_alone() {
        const A: f32 = 440.0;
        const B: f32 = 1000.0;
        let mut session = Session::new(Nodes::all());
        let sines = [(NodeId(1), A), (NodeId(2), B)];
        for (id, frequency) in sines {
            session.edit([
                Edit::Apply(Command::AddNode {
                    id,
                    node: Node::new("noodle.osc.sine"),
                }),
                Edit::Apply(Command::SetParam {
                    node: id,
                    key: "frequency".into(),
                    value: Some(frequency),
                }),
            ]);
        }
        // Only the first sine is wired to an Output; the second is silent
        // until it is monitored.
        session.edit([
            Edit::Apply(Command::AddNode {
                id: NodeId(3),
                node: Node::new(noodle_engine::OUTPUT_ID),
            }),
            Edit::Apply(Command::Connect(Connection {
                from: Endpoint::new(NodeId(1), "out"),
                to: Endpoint::new(NodeId(3), "in"),
            })),
        ]);
        session.set_audio_config(virtual_config());
        session.start_engine();
        session.play();
        let tap = session.virtual_capture().expect("the virtual output");
        let hear = |session: &Session| {
            let _ = session;
            tap.start();
            std::thread::sleep(Duration::from_millis(300));
            energy_at(&tap.take(), [A, B])
        };

        let [a, b] = hear(&session);
        assert!(a > 0.1 && b < 0.01, "plain: {a} {b}");

        session.click_monitor(NodeId(2), false, false);
        std::thread::sleep(Duration::from_millis(100));
        let [a, b] = hear(&session);
        assert!(a > 0.1 && b > 0.1, "summed: {a} {b}");

        session.click_monitor(NodeId(2), true, false);
        std::thread::sleep(Duration::from_millis(100));
        let [a, b] = hear(&session);
        assert!(a < 0.01 && b > 0.1, "exclusive: {a} {b}");

        session.click_monitor(NodeId(2), true, true);
        std::thread::sleep(Duration::from_millis(100));
        let [a, b] = hear(&session);
        assert!(a < 0.01 && b > 0.1, "silencing the others: {a} {b}");

        session.stop_monitor();
        std::thread::sleep(Duration::from_millis(100));
        let [a, b] = hear(&session);
        assert!(a > 0.1 && b < 0.01, "stopped: {a} {b}");
    }

    #[test]
    fn the_monitor_menu_sets_a_mode_directly() {
        let mut session = Session::new(Nodes::all());
        let (a, b) = (NodeId(1), NodeId(2));
        for id in [a, b] {
            session.edit([Edit::Apply(Command::AddNode {
                id,
                node: Node::new("noodle.osc.sine"),
            })]);
        }
        let state = |s: &Session| (s.monitored().to_vec(), s.monitor_mode());
        session.edit([Edit::MonitorAs {
            node: a,
            mode: Some(MonitorMode::SilenceOthers),
            add: false,
        }]);
        // Choosing a mode again keeps it, where a click would stop.
        session.edit([Edit::MonitorAs {
            node: a,
            mode: Some(MonitorMode::SilenceOthers),
            add: false,
        }]);
        assert_eq!(state(&session), (vec![a], MonitorMode::SilenceOthers));
        session.edit([Edit::MonitorAs {
            node: b,
            mode: Some(MonitorMode::Sum),
            add: true,
        }]);
        assert_eq!(state(&session), (vec![a, b], MonitorMode::SilenceOthers));
        session.edit([Edit::MonitorAs {
            node: a,
            mode: None,
            add: false,
        }]);
        assert_eq!(state(&session), (vec![b], MonitorMode::SilenceOthers));
        session.edit([Edit::MonitorAs {
            node: b,
            mode: None,
            add: false,
        }]);
        assert_eq!(state(&session), (vec![], MonitorMode::Sum));
    }

    #[test]
    fn deleting_a_monitored_node_stops_monitoring_it() {
        let mut session = Session::new(Nodes::all());
        session.edit([Edit::Apply(Command::AddNode {
            id: NodeId(1),
            node: Node::new("noodle.osc.sine"),
        })]);
        session.click_monitor(NodeId(1), false, false);
        session.edit([Edit::Apply(Command::RemoveNode { id: NodeId(1) })]);
        assert!(session.monitored().is_empty());
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
        let clip = recorded_clip(NodeId(1), Tick(960), "t.wav", 48_000, 1_200, u64::MAX);
        assert_eq!(clip.start, Tick(960));
        assert_eq!(audio(&clip), (1_200, 46_800));
    }

    #[test]
    fn a_punch_out_cuts_the_take_short() {
        let clip = recorded_clip(NodeId(1), Tick(0), "t.wav", 48_000, 1_000, 20_000);
        assert_eq!(audio(&clip), (1_000, 20_000));
    }

    #[test]
    fn no_latency_leaves_the_take_whole() {
        let clip = recorded_clip(NodeId(1), Tick(0), "t.wav", 48_000, 0, u64::MAX);
        assert_eq!(audio(&clip), (0, 48_000));
    }

    #[test]
    fn a_take_shorter_than_the_latency_keeps_a_frame() {
        let clip = recorded_clip(NodeId(1), Tick(0), "t.wav", 100, 5_000, u64::MAX);
        assert_eq!(audio(&clip), (99, 1));
    }
}
