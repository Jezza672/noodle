//! Presets: stored values for a node, or for everything inside a group.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::LayoutOf;
use super::topology::topology;
use crate::group::{GROUP, GROUP_INPUT, GROUP_OUTPUT};
use crate::{Command, Config, Graph, Node, NodeId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetError {
    NoSuchNode(NodeId),
    /// The node isn't of the preset's type, or its ports differ.
    WrongNode,
    /// The group's topology isn't the preset's.
    WrongTopology,
}

impl fmt::Display for PresetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchNode(id) => write!(f, "{id} is gone"),
            Self::WrongNode => write!(f, "the preset is for a different kind of node"),
            Self::WrongTopology => write!(f, "the preset is for a differently built group"),
        }
    }
}

impl std::error::Error for PresetError {}

/// The values one node holds: parameters that are set, and config.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PresetValues {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "Config::is_empty")]
    pub config: Config,
}

impl PresetValues {
    pub fn of(node: &Node) -> Self {
        Self {
            params: node.params.clone(),
            config: node.config.clone(),
        }
    }

    /// Whether `node` holds exactly these values.
    pub fn matches(&self, node: &Node) -> bool {
        self.params.len() == node.params.len()
            && self
                .params
                .iter()
                .zip(&node.params)
                .all(|((a, x), (b, y))| a == b && x.to_bits() == y.to_bits())
            && self.config == node.config
    }

    /// The commands that make `node` hold these values and nothing else:
    /// parameters and config that aren't stored go back to their defaults.
    fn set_on(&self, id: NodeId, node: &Node) -> Vec<Command> {
        let mut commands = Vec::new();
        for (key, &value) in &self.params {
            if node.params.get(key).map(|v| v.to_bits()) != Some(value.to_bits()) {
                commands.push(Command::SetParam {
                    node: id,
                    key: key.clone(),
                    value: Some(value),
                });
            }
        }
        for key in node.params.keys().filter(|k| !self.params.contains_key(*k)) {
            commands.push(Command::SetParam {
                node: id,
                key: key.clone(),
                value: None,
            });
        }
        for (key, value) in self.config.iter() {
            if node.config.get(key) != Some(value) {
                commands.push(Command::SetConfig {
                    node: id,
                    key: key.to_string(),
                    value: Some(value.clone()),
                });
            }
        }
        for (key, _) in node.config.iter().filter(|(k, _)| self.config.get(k).is_none()) {
            commands.push(Command::SetConfig {
                node: id,
                key: key.to_string(),
                value: None,
            });
        }
        commands
    }
}

/// A preset for one node: its parameters and config, for any node of the same
/// type and port layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodePreset {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub slug: String,
    pub node_type: String,
    /// The port layout it was made for.
    pub layout: String,
    pub values: PresetValues,
}

impl NodePreset {
    pub fn capture(node: &Node, layout: &str, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            slug: String::new(),
            node_type: node.type_id.clone(),
            layout: layout.to_string(),
            values: PresetValues::of(node),
        }
    }

    /// Whether `node` is of this preset's type with the same ports.
    pub fn applies_to(&self, node: &Node, layout: LayoutOf) -> bool {
        node.type_id == self.node_type && layout(node).as_deref() == Some(self.layout.as_str())
    }

    /// Whether `node` still holds exactly the preset's values.
    pub fn matches(&self, node: &Node) -> bool {
        self.values.matches(node)
    }

    /// Sets the node's values to the preset's, and records `entry` (the
    /// preset's ID) as the last one applied: one command, so one undo step.
    /// Wires, lanes, mute and solo are not touched.
    pub fn apply(
        &self,
        graph: &Graph,
        id: NodeId,
        entry: &str,
        layout: LayoutOf,
    ) -> Result<Command, PresetError> {
        let node = graph.node(id).ok_or(PresetError::NoSuchNode(id))?;
        if !self.applies_to(node, layout) {
            return Err(PresetError::WrongNode);
        }
        let mut commands = self.values.set_on(id, node);
        commands.push(Command::SetPreset {
            node: id,
            preset: Some(entry.to_string()),
        });
        Ok(Command::Batch(commands))
    }
}

/// A preset for a group: the values of every node inside, in the group's
/// canonical order. The group's own boundary controls (gain, mute, solo) are
/// not in it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupPreset {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub slug: String,
    /// The [topology key](super::topology::Topology::key) it was made for.
    pub topology: String,
    /// Nodes that hold nothing (boundary nodes, nested groups) are left out
    /// of the list's meaning but keep their place, so positions line up.
    pub nodes: Vec<PresetValues>,
}

/// Group and boundary nodes carry names and controls, not settings.
fn has_settings(node: &Node) -> bool {
    !matches!(
        node.type_id.as_str(),
        GROUP | GROUP_INPUT | GROUP_OUTPUT
    )
}

impl GroupPreset {
    pub fn capture(
        graph: &Graph,
        group: NodeId,
        layout: LayoutOf,
        name: impl Into<String>,
    ) -> Self {
        let order = topology(graph, group, layout);
        let nodes = order
            .order
            .iter()
            .map(|&id| {
                let node = graph.node(id).expect("a descendant");
                if has_settings(node) {
                    PresetValues::of(node)
                } else {
                    PresetValues::default()
                }
            })
            .collect();
        Self {
            name: name.into(),
            slug: String::new(),
            topology: order.key,
            nodes,
        }
    }

    /// Whether the group has the topology this preset was made for.
    pub fn applies_to(&self, graph: &Graph, group: NodeId, layout: LayoutOf) -> bool {
        topology(graph, group, layout).key == self.topology
    }

    /// Whether every node in the group still holds the preset's values.
    pub fn matches(&self, graph: &Graph, group: NodeId, layout: LayoutOf) -> bool {
        let order = topology(graph, group, layout);
        order.key == self.topology
            && order.order.iter().zip(&self.nodes).all(|(&id, values)| {
                let node = graph.node(id).expect("a descendant");
                !has_settings(node) || values.matches(node)
            })
    }

    /// Sets every node inside to the preset's values and records `entry`. One
    /// command. Boundary gain, mute and solo, wires and lanes are untouched.
    pub fn apply(
        &self,
        graph: &Graph,
        group: NodeId,
        entry: &str,
        layout: LayoutOf,
    ) -> Result<Command, PresetError> {
        if graph.node(group).is_none() {
            return Err(PresetError::NoSuchNode(group));
        }
        let order = topology(graph, group, layout);
        if order.key != self.topology || order.order.len() != self.nodes.len() {
            return Err(PresetError::WrongTopology);
        }
        let mut commands = Vec::new();
        for (&id, values) in order.order.iter().zip(&self.nodes) {
            let node = graph.node(id).expect("a descendant");
            if !has_settings(node) {
                continue;
            }
            commands.extend(values.set_on(id, node));
            // Whatever preset the node itself last had no longer describes it.
            if node.preset.is_some() {
                commands.push(Command::SetPreset {
                    node: id,
                    preset: None,
                });
            }
        }
        commands.push(Command::SetPreset {
            node: group,
            preset: Some(entry.to_string()),
        });
        Ok(Command::Batch(commands))
    }
}

