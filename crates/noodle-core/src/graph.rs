//! The node graph as the project stores it: every node's settings and what's
//! connected to what.
//!
//! The graph doesn't know which node types exist or what ports they have. The
//! engine checks that when it compiles the graph, so a project that uses a
//! missing node type still loads, and the problem is shown on the node.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Config, EditError};

/// Identifies a node within a project. IDs aren't reused within a session, so
/// undo can always put a node back under its old ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// The node type's ID, e.g. `"noodle.osc.sine"`.
    #[serde(rename = "type")]
    pub type_id: String,
    /// Parameter values by port key. Parameters that aren't listed are at
    /// their defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "Config::is_empty")]
    pub config: Config,
    /// Where the node sits in the editor.
    #[serde(default)]
    pub position: Position,
}

impl Node {
    pub fn new(type_id: impl Into<String>) -> Self {
        Self {
            type_id: type_id.into(),
            params: BTreeMap::new(),
            config: Config::new(),
            position: Position::default(),
        }
    }

    pub fn at(self, x: f32, y: f32) -> Self {
        Self {
            position: Position { x, y },
            ..self
        }
    }

    pub fn with_param(mut self, key: impl Into<String>, value: f32) -> Self {
        self.params.insert(key.into(), value);
        self
    }

    pub fn with_config(self, config: Config) -> Self {
        Self { config, ..self }
    }
}

/// One port on one node, identified by the port's key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    pub node: NodeId,
    pub port: String,
}

impl Endpoint {
    pub fn new(node: NodeId, port: impl Into<String>) -> Self {
        Self {
            node,
            port: port.into(),
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.node, self.port)
    }
}

/// A wire from an output to an input.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Connection {
    pub from: Endpoint,
    pub to: Endpoint,
}

/// Nodes and the connections between them. Change it through
/// [`Command`](crate::Command)s, so every change can be undone.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "GraphFile", into = "GraphFile")]
pub struct Graph {
    nodes: BTreeMap<NodeId, Node>,
    /// Maps each connected input to its source. Keyed by input because an
    /// input takes at most one connection.
    sources: BTreeMap<Endpoint, Endpoint>,
    next_id: u64,
}

impl Default for Graph {
    fn default() -> Self {
        Self {
            nodes: BTreeMap::new(),
            sources: BTreeMap::new(),
            next_id: 1,
        }
    }
}

/// Two graphs are equal if they have the same nodes and connections; which
/// IDs they'd hand out next doesn't matter.
impl PartialEq for Graph {
    fn eq(&self, other: &Self) -> bool {
        self.nodes == other.nodes && self.sources == other.sources
    }
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes.iter().map(|(&id, node)| (id, node))
    }

    /// Ordered by input.
    pub fn connections(&self) -> impl Iterator<Item = Connection> {
        self.sources.iter().map(|(to, from)| Connection {
            from: from.clone(),
            to: to.clone(),
        })
    }

    /// What's connected to an input, if anything.
    pub fn source(&self, input: &Endpoint) -> Option<&Endpoint> {
        self.sources.get(input)
    }

    /// Reserves an ID for a node that's about to be added.
    /// The ID [`new_id`](Self::new_id) would hand out next.
    pub(crate) fn next_id(&self) -> NodeId {
        NodeId(self.next_id)
    }

    pub(crate) fn new_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    pub(crate) fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, EditError> {
        self.nodes.get_mut(&id).ok_or(EditError::NoSuchNode(id))
    }

    pub(crate) fn insert_node(&mut self, id: NodeId, node: Node) -> Result<(), EditError> {
        if self.nodes.contains_key(&id) {
            return Err(EditError::NodeExists(id));
        }
        self.nodes.insert(id, node);
        self.next_id = self.next_id.max(id.0 + 1);
        Ok(())
    }

    /// Removes a node along with its connections, and returns both.
    pub(crate) fn remove_node(&mut self, id: NodeId) -> Result<(Node, Vec<Connection>), EditError> {
        let node = self.nodes.remove(&id).ok_or(EditError::NoSuchNode(id))?;
        let connections: Vec<Connection> = self
            .connections()
            .filter(|c| c.from.node == id || c.to.node == id)
            .collect();
        for connection in &connections {
            self.sources.remove(&connection.to);
        }
        Ok((node, connections))
    }

    /// Returns what the input was connected to before, if anything.
    pub(crate) fn connect(
        &mut self,
        connection: Connection,
    ) -> Result<Option<Endpoint>, EditError> {
        for node in [connection.from.node, connection.to.node] {
            if !self.nodes.contains_key(&node) {
                return Err(EditError::NoSuchNode(node));
            }
        }
        Ok(self.sources.insert(connection.to, connection.from))
    }

    /// Returns what the input was connected to.
    pub(crate) fn disconnect(&mut self, input: &Endpoint) -> Result<Endpoint, EditError> {
        self.sources
            .remove(input)
            .ok_or_else(|| EditError::NotConnected(input.clone()))
    }
}

/// How a graph is laid out in a project file.
#[derive(Serialize, Deserialize)]
struct GraphFile {
    #[serde(default)]
    nodes: BTreeMap<NodeId, Node>,
    #[serde(default)]
    connections: Vec<Connection>,
}

impl From<Graph> for GraphFile {
    fn from(graph: Graph) -> Self {
        Self {
            connections: graph.connections().collect(),
            nodes: graph.nodes,
        }
    }
}

impl TryFrom<GraphFile> for Graph {
    type Error = EditError;

    fn try_from(file: GraphFile) -> Result<Self, EditError> {
        let mut graph = Graph::new();
        for (id, node) in file.nodes {
            graph.insert_node(id, node)?;
        }
        for connection in file.connections {
            let input = connection.to.clone();
            if graph.connect(connection)?.is_some() {
                return Err(EditError::ConnectedTwice(input));
            }
        }
        Ok(graph)
    }
}
