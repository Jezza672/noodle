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

use noodle_core::{Command, EditError, History, NodeId, Project};
use noodle_engine::{Controller, Diagnostic, Registry, compile};
use noodle_io::{OutputError, Playback};

/// Frames per block while playing: about 11 ms at 48 kHz.
const MAX_FRAMES: usize = 512;

/// A change a view wants made.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the editor and properties panel are placeholders")
)]
pub enum Edit {
    /// One undo step.
    Apply(Command),
    /// Part of a continuous gesture, such as dragging a node or a knob. Every
    /// `Drag` until the next [`Edit::EndDrag`] is one undo step.
    Drag(Command),
    EndDrag,
}

pub struct Session {
    project: Project,
    history: History,
    registry: Registry,
    /// Where the project was loaded from or last saved to.
    path: Option<PathBuf>,
    /// The project as it was last saved or loaded, to tell whether it has
    /// unsaved changes.
    saved: Project,
    dirty: bool,
    /// The next ID [`Session::new_node_id`] can hand out. Views only get
    /// `&Session`, so it's a `Cell`.
    next_id: Cell<u64>,
    diagnostics: Vec<Diagnostic>,
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
    pub fn new(registry: Registry) -> Self {
        Self::with_project(registry, Project::new(), None)
    }

    pub fn open(registry: Registry, path: &Path) -> Result<Self, FileError> {
        let project = load(path)?;
        Ok(Self::with_project(registry, project, Some(path.to_owned())))
    }

    fn with_project(registry: Registry, project: Project, path: Option<PathBuf>) -> Self {
        let mut session = Self {
            saved: Project::new(),
            project: Project::new(),
            history: History::new(),
            registry,
            path: None,
            dirty: false,
            next_id: Cell::new(0),
            diagnostics: Vec::new(),
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
    #[cfg_attr(not(test), expect(dead_code, reason = "the editor is a placeholder"))]
    pub fn new_node_id(&self) -> NodeId {
        let id = self.next_id.get().max(self.project.next_node_id().0);
        self.next_id.set(id + 1);
        NodeId(id)
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
        let mut structural = false;
        let mut params = Vec::new();
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
            let effect = match kind(&command) {
                Kind::Layout => None,
                Kind::Param(node, key, value) => Some(Some((node, key.to_owned(), value))),
                Kind::Structural => Some(None),
            };
            match self.history.apply(&mut self.project, command) {
                Ok(()) => match effect {
                    Some(Some(param)) => params.push(param),
                    Some(None) => structural = true,
                    None => {}
                },
                Err(error) => self.message = Some(format!("Couldn't edit: {error}")),
            }
        }
        if structural {
            self.recompile();
        } else if let Some(audio) = &mut self.audio {
            for (node, key, value) in params {
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

    /// Saves to the project's own file. Returns false if it has none yet, so
    /// the caller should ask where to save it.
    pub fn save(&mut self) -> bool {
        match self.path.clone() {
            Some(path) => {
                self.save_as(&path);
                true
            }
            None => false,
        }
    }

    pub fn save_as(&mut self, path: &Path) {
        match std::fs::write(path, self.project.to_ron()) {
            Ok(()) => {
                self.path = Some(path.to_owned());
                self.saved = self.project.clone();
                self.dirty = false;
                self.message = Some(format!("Saved {}", path.display()));
            }
            Err(error) => {
                self.message = Some(format!("Couldn't save {}: {error}", path.display()));
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
        match noodle_io::play(MAX_FRAMES) {
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

enum Kind<'a> {
    /// Changes nothing the engine sees, such as a node's position.
    Layout,
    /// Sets a parameter, which the engine can take without recompiling.
    Param(NodeId, &'a str, f32),
    Structural,
}

fn kind(command: &Command) -> Kind<'_> {
    match command {
        Command::MoveNode { .. } => Kind::Layout,
        // Resetting to the default needs the default from the node type, so
        // it recompiles, which reads it.
        Command::SetParam {
            node,
            key,
            value: Some(value),
        } => Kind::Param(*node, key, *value),
        Command::Batch(commands) => {
            if commands
                .iter()
                .all(|command| matches!(kind(command), Kind::Layout))
            {
                Kind::Layout
            } else {
                Kind::Structural
            }
        }
        _ => Kind::Structural,
    }
}

fn play_error(error: &OutputError) -> String {
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

    fn registry() -> Registry {
        let mut registry = Registry::with_builtins();
        noodle_nodes::register_all(&mut registry);
        registry
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
        let mut session = Session::new(registry());
        let sine = add(&mut session, Node::new("noodle.osc.sine"));
        assert!(session.can_undo());
        session.undo();
        assert!(session.project().graph().node(sine).is_none());
        session.redo();
        assert!(session.project().graph().node(sine).is_some());
    }

    #[test]
    fn a_drag_is_one_undo_step() {
        let mut session = Session::new(registry());
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
        let mut session = Session::new(registry());
        assert!(session.diagnostics().is_empty());
        add(&mut session, Node::new("no.such.type"));
        assert_eq!(session.diagnostics().len(), 1);
        session.undo();
        assert!(session.diagnostics().is_empty());
    }

    #[test]
    fn a_failed_edit_is_reported_and_skipped() {
        let mut session = Session::new(registry());
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
        let mut session = Session::new(registry());
        assert!(!session.is_dirty());
        assert!(!session.save(), "nowhere to save yet");

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
        let mut session = Session::new(registry());
        let sine = add(&mut session, Node::new("noodle.osc.sine"));
        let output = add(&mut session, Node::new(OUTPUT_ID));
        session.edit([Edit::Apply(Command::Connect(Connection {
            from: Endpoint::new(sine, "out"),
            to: Endpoint::new(output, "in"),
        }))]);
        session.save_as(&path);

        let opened = Session::open(registry(), &path).unwrap();
        assert_eq!(opened.project(), session.project());
        assert!(!opened.is_dirty() && !opened.can_undo());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_bad_file_keeps_the_current_project() {
        let path = temp("bad.ron");
        std::fs::write(&path, "not a project").unwrap();
        let mut session = Session::new(registry());
        let sine = add(&mut session, Node::new("noodle.osc.sine"));
        assert!(!session.load(&path));
        assert!(session.project().graph().node(sine).is_some());
        assert!(session.message().is_some_and(|m| m.contains("Can't load")));
        assert!(Session::open(registry(), &path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn node_ids_are_unique_even_before_use() {
        let mut session = Session::new(registry());
        let existing = add(&mut session, Node::new("noodle.osc.sine"));
        let (a, b) = (session.new_node_id(), session.new_node_id());
        assert!(a != b && a != existing && b != existing);

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
        let mut session = Session::new(registry());
        add(&mut session, Node::new("no.such.type"));
        session.new_project();
        assert_eq!(session.project(), &Project::new());
        assert!(!session.is_dirty() && !session.can_undo());
        assert!(session.diagnostics().is_empty());
    }

    #[test]
    fn classifies_commands() {
        let node = NodeId(1);
        let moved = Command::MoveNode {
            node,
            position: Position::default(),
        };
        assert!(matches!(kind(&moved), Kind::Layout));
        assert!(matches!(
            kind(&Command::Batch(vec![moved.clone(), moved.clone()])),
            Kind::Layout
        ));
        let set = Command::SetParam {
            node,
            key: "gain".into(),
            value: Some(-6.0),
        };
        assert!(matches!(kind(&set), Kind::Param(_, "gain", -6.0)));
        let reset = Command::SetParam {
            node,
            key: "gain".into(),
            value: None,
        };
        assert!(matches!(kind(&reset), Kind::Structural));
        assert!(matches!(
            kind(&Command::Batch(vec![moved, set])),
            Kind::Structural
        ));
        assert!(matches!(
            kind(&Command::RemoveNode { id: node }),
            Kind::Structural
        ));
    }
}
