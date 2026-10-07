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

use noodle_core::{Command, EditError, FrameId, History, NodeId, Project};
use noodle_engine::{Controller, Diagnostic, Registry, Telemetry, compile};
use noodle_io::{AudioConfig, AudioError, Playback};

/// Frames per block while playing: about 11 ms at 48 kHz.
const MAX_FRAMES: usize = 512;

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
}

impl Nodes {
    /// Every node type the app offers.
    pub fn all() -> Self {
        let mut registry = Registry::with_builtins();
        let telemetry = noodle_nodes::register_all(&mut registry);
        Self {
            registry,
            telemetry,
        }
    }
}

pub struct Session {
    project: Project,
    history: History,
    registry: Registry,
    telemetry: Telemetry,
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
    /// Something the user should know, such as a failed save, shown until the
    /// next one replaces it.
    message: Option<String>,
}

struct Audio {
    playback: Playback,
    controller: Controller,
    underruns: u64,
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
        } = nodes;
        let mut session = Self {
            saved: Project::new(),
            project: Project::new(),
            history: History::new(),
            registry,
            telemetry,
            path: None,
            dirty: false,
            next_id: Cell::new(0),
            next_frame_id: Cell::new(0),
            diagnostics: Vec::new(),
            audio_config: AudioConfig::default(),
            audio: None,
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

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the device picker isn't built yet")
    )]
    pub fn audio_config(&self) -> &AudioConfig {
        &self.audio_config
    }

    /// Chooses the device to play on. If playing, playback restarts there.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the device picker isn't built yet")
    )]
    pub fn set_audio_config(&mut self, config: AudioConfig) {
        self.audio_config = config;
        if self.audio.take().is_some() {
            self.play();
        }
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
        } else if let Some(audio) = &mut self.audio {
            for (node, key, value) in effect.params {
                audio.controller.set_param(node, &key, value);
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
    }

    pub fn is_playing(&self) -> bool {
        self.audio.is_some()
    }

    pub fn play(&mut self) {
        if self.audio.is_some() {
            return;
        }
        match noodle_io::play(&self.audio_config, MAX_FRAMES) {
            Ok((playback, controller)) => {
                self.audio = Some(Audio {
                    playback,
                    controller,
                    underruns: 0,
                });
                self.recompile();
                self.message = None;
            }
            Err(error) => self.message = Some(play_error(&error)),
        }
    }

    pub fn stop(&mut self) {
        self.audio = None;
    }

    /// Housekeeping to do every frame: frees plans the audio thread is done
    /// with, and checks the device's health.
    pub fn maintain(&mut self) {
        let Some(audio) = &mut self.audio else {
            return;
        };
        audio.controller.maintain();
        let health = audio.playback.health();
        let mut stopped = None;
        for error in health.errors() {
            if noodle_io::is_fatal(&error) {
                stopped = Some(format!("Playback stopped: {error}"));
            } else {
                self.message = Some(error.to_string());
            }
        }
        let underruns = health.underruns();
        if underruns > audio.underruns {
            audio.underruns = underruns;
            self.message = Some(format!("{underruns} underruns since playback started"));
        }
        if let Some(message) = stopped {
            self.audio = None;
            self.message = Some(message);
        }
    }

    fn recompile(&mut self) {
        self.diagnostics = match &mut self.audio {
            Some(audio) => audio
                .controller
                .update(self.project.graph(), &self.registry),
            None => compile(self.project.graph(), &self.registry).1,
        };
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
            Command::Batch(commands) => commands.iter().for_each(|c| self.add(c)),
            // Includes resetting a parameter to its default, which needs the
            // default from the node type; recompiling reads it.
            _ => self.structural = true,
        }
    }

    fn merge(&mut self, other: Self) {
        self.structural |= other.structural;
        self.params.extend(other.params);
    }
}

fn play_error(error: &AudioError) -> String {
    format!("Can't play: {error}")
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
fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".saving");
    let temp = PathBuf::from(temp);
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        std::io::Write::write_all(&mut file, contents.as_bytes())?;
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
            }
        );
        assert!(Effect::of(&set(None)).structural);
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
}
