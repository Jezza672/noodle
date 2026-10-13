//! Group templates: a group and everything inside it, saved for reuse.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::group::{GAIN, GROUP, GROUP_INPUT, GROUP_OUTPUT, parse_control_key};
use crate::{Command, Connection, Endpoint, Graph, Node, NodeId, Position};

/// A saved group. Its nodes use their own IDs (the group itself is the one
/// node with no parent), positions are relative to the group's, and it holds
/// no wires to anything outside, no automation lanes and no mute or solo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupTemplate {
    pub name: String,
    /// The entry's stable name in its store; empty means "from the name".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub slug: String,
    /// Not listed in the library: the app instantiates it itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    nodes: BTreeMap<NodeId, Node>,
    connections: Vec<Connection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateError {
    /// The node isn't a group.
    NotAGroup(NodeId),
    /// A template needs exactly one node with no parent, and it must be a
    /// group.
    BadRoot,
    /// A node's parent isn't a group in the template, or a wire joins nodes
    /// in different groups.
    Malformed(String),
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAGroup(id) => write!(f, "{id} isn't a group"),
            Self::BadRoot => write!(f, "a template needs exactly one group at its top"),
            Self::Malformed(why) => write!(f, "malformed template: {why}"),
        }
    }
}

impl std::error::Error for TemplateError {}

impl GroupTemplate {
    /// Saves `group` and its contents. Wires to the outside, mute, solo and
    /// the boundary gains are left out.
    pub fn from_group(
        graph: &Graph,
        group: NodeId,
        name: impl Into<String>,
    ) -> Result<Self, TemplateError> {
        let root = graph.node(group).ok_or(TemplateError::NotAGroup(group))?;
        if root.type_id != GROUP {
            return Err(TemplateError::NotAGroup(group));
        }
        let mut inside = graph.descendants(group);
        inside.sort();
        let local: BTreeMap<NodeId, NodeId> = std::iter::once(group)
            .chain(inside.iter().copied())
            .enumerate()
            .map(|(i, id)| (id, NodeId(i as u64 + 1)))
            .collect();
        let mut nodes = BTreeMap::new();
        for (&old, &new) in &local {
            let mut node = graph.node(old).expect("listed above").clone();
            if old == group {
                node.position = Position::default();
                node.parent = None;
                node.template = None;
                node.preset = None;
            } else {
                node.position = Position {
                    x: node.position.x - root.position.x,
                    y: node.position.y - root.position.y,
                };
                node.parent = node.parent.map(|p| local[&p]);
            }
            // A fresh copy's nodes say nothing about presets applied to the
            // originals.
            node.preset = None;
            if matches!(node.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT) {
                // The group's own gain is set where it is used. A nested
                // group's gain is its level in the mix inside, so it stays,
                // as in a group preset; mute and solo never do.
                let own = graph.node(old).and_then(|n| n.parent) == Some(group);
                node.params.retain(|key, _| match parse_control_key(key) {
                    Some((GAIN, _)) => !own,
                    Some(_) => false,
                    None => true,
                });
            }
            nodes.insert(new, node);
        }
        let connections = graph
            .connections()
            .filter(|c| local.contains_key(&c.from.node) && local.contains_key(&c.to.node))
            .map(|c| Connection {
                from: Endpoint::new(local[&c.from.node], c.from.port),
                to: Endpoint::new(local[&c.to.node], c.to.port),
            })
            .collect();
        let template = Self {
            name: name.into(),
            slug: String::new(),
            hidden: false,
            nodes,
            connections,
        };
        template.validate()?;
        Ok(template)
    }

    /// Checks the template is a well-formed group, as loading a project
    /// checks its graph.
    pub fn validate(&self) -> Result<(), TemplateError> {
        let mut roots = self.nodes.iter().filter(|(_, n)| n.parent.is_none());
        match (roots.next(), roots.next()) {
            (Some((_, root)), None) if root.type_id == GROUP => {}
            _ => return Err(TemplateError::BadRoot),
        }
        Graph::from_parts(
            self.nodes.iter().map(|(&id, n)| (id, n.clone())),
            self.connections.iter().cloned(),
        )
        .map(|_| ())
        .map_err(|e| TemplateError::Malformed(e.to_string()))
    }

    fn root(&self) -> NodeId {
        *self
            .nodes
            .iter()
            .find(|(_, n)| n.parent.is_none())
            .expect("validated")
            .0
    }

    /// How many nodes the template adds, the group included.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The template's nodes and wires as a graph, for looking at.
    pub fn graph(&self) -> Graph {
        Graph::from_parts(
            self.nodes.iter().map(|(&id, n)| (id, n.clone())),
            self.connections.iter().cloned(),
        )
        .expect("validated")
    }

    /// Adds an unlinked copy of the group inside `parent` (`None` for the top
    /// level) with its group node at `position`, as one command. `source` is
    /// the template's entry ID, which the group remembers. IDs come from
    /// `new_id` in order: the group, then what's directly inside it by the
    /// template's order, then what's inside those.
    ///
    /// Returns the new group's ID with the command.
    pub fn instantiate(
        &self,
        source: &str,
        parent: Option<NodeId>,
        position: Position,
        mut new_id: impl FnMut() -> NodeId,
    ) -> (NodeId, Command) {
        let root = self.root();
        let mut queue = VecDeque::from([root]);
        let mut order = Vec::new();
        while let Some(at) = queue.pop_front() {
            order.push(at);
            // BTreeMap order: lowest local ID first.
            queue.extend(
                self.nodes
                    .iter()
                    .filter(|(_, n)| n.parent == Some(at))
                    .map(|(&id, _)| id),
            );
        }
        let ids: BTreeMap<NodeId, NodeId> = order.iter().map(|&old| (old, new_id())).collect();
        let mut commands = Vec::new();
        for old in &order {
            let mut node = self.nodes[old].clone();
            node.position = Position {
                x: node.position.x + position.x,
                y: node.position.y + position.y,
            };
            if *old == root {
                node.parent = parent;
                node.template = Some(source.to_string());
            } else {
                node.parent = node.parent.map(|p| ids[&p]);
            }
            commands.push(Command::AddNode { id: ids[old], node });
        }
        for c in &self.connections {
            commands.push(Command::Connect(Connection {
                from: Endpoint::new(ids[&c.from.node], c.from.port.clone()),
                to: Endpoint::new(ids[&c.to.node], c.to.port.clone()),
            }));
        }
        (ids[&root], Command::Batch(commands))
    }
}

/// The ID of the hidden template tracks are made from.
pub const TRACK: &str = "system:template:track";

/// The template of a track: a group with a group input `in`, a track input,
/// and a group output `out` with its gain and mute set, wired from the track
/// input's audio. Hidden; [`create_track`](crate::group::create_track) is the
/// only thing that uses it.
pub fn track_template() -> &'static GroupTemplate {
    static TRACK_TEMPLATE: OnceLock<GroupTemplate> = OnceLock::new();
    TRACK_TEMPLATE.get_or_init(|| {
        let items = super::store::parse_pack(include_str!("../../library/system/track.ron"))
            .expect("the embedded track template is valid");
        match items.into_iter().next() {
            Some(super::Item::Template(template)) => template,
            _ => panic!("the embedded track file holds a template"),
        }
    })
}
