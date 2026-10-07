//! The whole project, and saving and loading it.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{EditError, Frame, FrameId, Graph, NodeId};

/// Everything that gets saved: the graph, and the frames drawn around parts of
/// it. Change it through a [`History`](crate::History), so every change can
/// be undone.
#[derive(Clone, Debug)]
pub struct Project {
    graph: Graph,
    frames: BTreeMap<FrameId, Frame>,
    next_frame_id: u64,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            graph: Graph::default(),
            frames: BTreeMap::new(),
            next_frame_id: 1,
        }
    }
}

/// Like [`Graph`], two projects are equal if their contents are, whichever IDs
/// they'd hand out next.
impl PartialEq for Project {
    fn eq(&self, other: &Self) -> bool {
        self.graph == other.graph && self.frames == other.frames
    }
}

/// How a project is laid out on disk.
#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: u32,
    graph: Graph,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    frames: BTreeMap<FrameId, Frame>,
}

impl Project {
    /// The current file format version. Bump it when the format changes in a
    /// way older versions can't read, and migrate older files when loading.
    pub const FORMAT: u32 = 1;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub(crate) fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    /// Reserves an ID for a node that's about to be added.
    pub fn new_node_id(&mut self) -> NodeId {
        self.graph.new_id()
    }

    /// The ID [`new_node_id`](Self::new_node_id) would hand out next, without
    /// reserving it. It's never the ID of a node in the project, even one
    /// that was removed, so it can't clash with an undo.
    pub fn next_node_id(&self) -> NodeId {
        self.graph.next_id()
    }

    pub fn frame(&self, id: FrameId) -> Option<&Frame> {
        self.frames.get(&id)
    }

    pub fn frames(&self) -> impl Iterator<Item = (FrameId, &Frame)> {
        self.frames.iter().map(|(&id, frame)| (id, frame))
    }

    /// Reserves an ID for a frame that's about to be added.
    pub fn new_frame_id(&mut self) -> FrameId {
        let id = self.next_frame_id();
        self.next_frame_id += 1;
        id
    }

    /// The ID [`new_frame_id`](Self::new_frame_id) would return next.
    pub fn next_frame_id(&self) -> FrameId {
        FrameId(self.next_frame_id)
    }

    pub(crate) fn insert_frame(&mut self, id: FrameId, frame: Frame) -> Result<(), EditError> {
        if self.frames.contains_key(&id) {
            return Err(EditError::FrameExists(id));
        }
        self.frames.insert(id, frame);
        self.next_frame_id = self.next_frame_id.max(id.0 + 1);
        Ok(())
    }

    pub(crate) fn remove_frame(&mut self, id: FrameId) -> Result<Frame, EditError> {
        self.frames.remove(&id).ok_or(EditError::NoSuchFrame(id))
    }

    pub(crate) fn frame_mut(&mut self, id: FrameId) -> Result<&mut Frame, EditError> {
        self.frames.get_mut(&id).ok_or(EditError::NoSuchFrame(id))
    }

    /// The project as RON, the text format project files use: it's readable
    /// and diffs well.
    pub fn to_ron(&self) -> String {
        let file = ProjectFile {
            format: Self::FORMAT,
            graph: self.graph.clone(),
            frames: self.frames.clone(),
        };
        // Depth 3 puts each node and each connection on its own line.
        let pretty = ron::ser::PrettyConfig::default().depth_limit(3);
        ron::ser::to_string_pretty(&file, pretty).expect("projects always serialize")
    }

    pub fn from_ron(text: &str) -> Result<Self, LoadError> {
        let file: ProjectFile = ron::from_str(text).map_err(LoadError::Invalid)?;
        if file.format > Self::FORMAT {
            return Err(LoadError::NewerFormat(file.format));
        }
        let next_frame_id = file.frames.keys().last().map_or(1, |id| id.0 + 1);
        Ok(Self {
            graph: file.graph,
            frames: file.frames,
            next_frame_id,
        })
    }
}

#[derive(Debug)]
pub enum LoadError {
    /// Bad syntax, or contents that don't make sense, such as a connection to
    /// a node that doesn't exist.
    Invalid(ron::error::SpannedError),
    /// Saved by a newer version of Noodle.
    NewerFormat(u32),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(f),
            Self::NewerFormat(format) => write!(
                f,
                "this project was saved in format {format} by a newer version; \
                 this version reads up to format {}",
                Project::FORMAT
            ),
        }
    }
}

impl std::error::Error for LoadError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Command, Config, Connection, Endpoint, Frame, FrameId, History, Node, NodeId, Position,
        Value,
    };

    const HAND_WRITTEN: &str = r#"
        (
            format: 1,
            graph: (
                nodes: {
                    1: (type: "noodle.osc.sine", params: {"frequency": 220.0}),
                    2: (type: "noodle.util.mix", config: {"inputs": 3}, position: (x: 200.0, y: 0.0)),
                },
                connections: [
                    (from: (node: 1, port: "out"), to: (node: 2, port: "in1")),
                ],
            ),
        )
    "#;

    #[test]
    fn a_project_with_nans_equals_its_copy() {
        let mut project = Project::from_ron(HAND_WRITTEN).unwrap();
        let mut history = History::new();
        let frame = project.new_frame_id();
        let node = Node::new("noodle.util.gain")
            .with_param("gain", f32::NAN)
            .with_config(Config::new().with("x", Value::Float(f64::NAN)))
            .at(f32::NAN, 0.0);
        let id = project.new_node_id();
        history
            .apply(&mut project, Command::AddNode { id, node })
            .unwrap();
        let frame_value = Frame {
            label: "f".into(),
            position: Position {
                x: 0.0,
                y: f32::NAN,
            },
            width: f32::NAN,
            height: 1.0,
        };
        history
            .apply(
                &mut project,
                Command::AddFrame {
                    id: frame,
                    frame: frame_value,
                },
            )
            .unwrap();
        assert_eq!(project, project.clone());
    }

    #[test]
    fn reads_a_hand_written_file() {
        let project = Project::from_ron(HAND_WRITTEN).unwrap();
        let graph = project.graph();
        let sine = graph.node(NodeId(1)).unwrap();
        assert_eq!(sine.params["frequency"], 220.0);
        let mix = graph.node(NodeId(2)).unwrap();
        assert_eq!(mix.config.get("inputs"), Some(&Value::Int(3)));
        assert_eq!(mix.position.x, 200.0);
        assert_eq!(
            graph.source(&Endpoint::new(NodeId(2), "in1")),
            Some(&Endpoint::new(NodeId(1), "out"))
        );
    }

    #[test]
    fn round_trips_through_ron() {
        let mut project = Project::from_ron(HAND_WRITTEN).unwrap();
        let mut history = History::new();
        let id = project.new_node_id();
        let node = Node::new("noodle.util.gain")
            .with_param("gain", -6.0)
            .with_config(Config::new().with("flag", Value::Bool(true)))
            .at(1.5, -2.0);
        history
            .apply(&mut project, Command::AddNode { id, node })
            .unwrap();
        history
            .apply(
                &mut project,
                Command::Connect(Connection {
                    from: Endpoint::new(NodeId(2), "out"),
                    to: Endpoint::new(id, "in"),
                }),
            )
            .unwrap();

        let frame = Frame {
            label: "Drums".into(),
            position: Position { x: -20.0, y: -40.0 },
            width: 400.0,
            height: 250.5,
        };
        let frame_id = project.new_frame_id();
        history
            .apply(
                &mut project,
                Command::AddFrame {
                    id: frame_id,
                    frame,
                },
            )
            .unwrap();

        let text = project.to_ron();
        let loaded = Project::from_ron(&text).unwrap();
        assert_eq!(loaded, project, "{text}");
        assert!(loaded.frame(frame_id).is_some(), "{text}");
    }

    #[test]
    fn new_frame_ids_follow_the_highest_loaded_id() {
        let text = HAND_WRITTEN.trim_end().trim_end_matches(')').to_string()
            + r#"frames: { 4: (label: "A", position: (x: 0.0, y: 0.0), width: 10.0, height: 10.0) }, )"#;
        let mut project = Project::from_ron(&text).unwrap();
        assert_eq!(project.frame(FrameId(4)).unwrap().label, "A");
        assert_eq!(project.new_frame_id(), FrameId(5));
    }

    #[test]
    fn new_ids_follow_the_highest_loaded_id() {
        let mut project = Project::from_ron(HAND_WRITTEN).unwrap();
        assert_eq!(project.new_node_id(), NodeId(3));
    }

    #[test]
    fn rejects_a_connection_to_a_missing_node() {
        let text = HAND_WRITTEN.replace("node: 2, port", "node: 7, port");
        let error = Project::from_ron(&text).unwrap_err().to_string();
        assert!(error.contains("there's no node #7"), "{error}");
    }

    #[test]
    fn rejects_an_input_with_two_connections() {
        let text = HAND_WRITTEN.replace(
            "connections: [",
            r#"connections: [ (from: (node: 2, port: "out"), to: (node: 2, port: "in1")),"#,
        );
        let error = Project::from_ron(&text).unwrap_err().to_string();
        assert!(error.contains("more than one connection"), "{error}");
    }

    #[test]
    fn rejects_a_newer_format() {
        let text = HAND_WRITTEN.replace("format: 1", "format: 99");
        assert!(matches!(
            Project::from_ron(&text),
            Err(LoadError::NewerFormat(99))
        ));
    }
}
