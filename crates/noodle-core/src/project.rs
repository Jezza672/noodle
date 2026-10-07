//! The whole project, and saving and loading it.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Graph;

/// Everything that gets saved. For now that's just the graph. Change it
/// through a [`History`](crate::History), so every change can be undone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Project {
    graph: Graph,
}

/// How a project is laid out on disk.
#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: u32,
    graph: Graph,
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

    /// The project as RON, the text format project files use: it's readable
    /// and diffs well.
    pub fn to_ron(&self) -> String {
        let file = ProjectFile {
            format: Self::FORMAT,
            graph: self.graph.clone(),
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
        Ok(Self { graph: file.graph })
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
    use crate::{Command, Config, Connection, Endpoint, History, Node, NodeId, Value};

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
        let id = project.graph_mut().new_id();
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

        let text = project.to_ron();
        assert_eq!(Project::from_ron(&text).unwrap(), project, "{text}");
    }

    #[test]
    fn new_ids_follow_the_highest_loaded_id() {
        let mut project = Project::from_ron(HAND_WRITTEN).unwrap();
        assert_eq!(project.graph_mut().new_id(), NodeId(3));
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
