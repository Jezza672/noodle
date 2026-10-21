//! Node monitoring: listening to a node's output through a chosen device.
//!
//! A monitor is not part of the project. [`Controller::set_monitor`](crate::Controller::set_monitor)
//! hands the compiler a [`Monitor`], and after flattening it adds a hidden
//! Output node per monitored node, wired to that node's first signal output.
//! The tap is therefore what the node really sends on: for a group, the
//! output after its gain, mute and solo stage, so a muted track is silent in
//! the monitor too. (A node inside a group is tapped before the group's own
//! stage.) Hidden Outputs are marked with [`MONITOR_KEY`] so they sum onto a
//! device other Output nodes already play on, instead of being refused as a
//! second owner.

use std::collections::BTreeSet;

use noodle_core::group::GROUP;
use noodle_core::{Config, Connection, Endpoint, Graph, Node, NodeId, Value};

use crate::builtin::{OUTPUT_DEVICE, OUTPUT_DEVICE_KEY, OUTPUT_ID};
use crate::registry::Registry;

/// The config key that marks an Output node as a monitor's.
pub const MONITOR_KEY: &str = "monitor";

/// What to monitor, and how it meets the rest of the outputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Monitor {
    /// The nodes to listen to, summed.
    pub nodes: Vec<NodeId>,
    /// The device they play on; empty is the main output.
    pub device: String,
    /// Silence the other Output nodes that play on the monitor's device.
    pub exclusive: bool,
    /// Silence every Output node, whatever its device.
    pub silence_others: bool,
}

/// The ID of the hidden Output for the `n`th monitored node: one no project
/// node or stage can have.
fn monitor_id(n: usize) -> NodeId {
    NodeId((1 << 62) | (1 << 61) | n as u64)
}

/// Whether the node is one of the monitor's hidden Outputs.
pub(crate) fn is_monitor_config(config: &Config) -> bool {
    config.get(MONITOR_KEY).is_some()
}

/// The flat graph with `monitor` applied. `original` is the graph before
/// flattening, which says where a group's output comes from. `main` is the
/// main device's ID, if it has one, which an empty device name also means.
pub(crate) fn apply(
    original: &Graph,
    flat: &Graph,
    keep: &BTreeSet<NodeId>,
    registry: &Registry,
    monitor: &Monitor,
    main: Option<&str>,
) -> Graph {
    let canonical = |device: &str| {
        if Some(device) == main {
            String::new()
        } else {
            device.to_owned()
        }
    };
    let device = canonical(&monitor.device);
    let silenced = |node: &Node| {
        node.type_id == OUTPUT_ID
            && (monitor.silence_others
                || (monitor.exclusive
                    && canonical(&OUTPUT_DEVICE.get_text(&node.config)) == device))
    };
    let removed: BTreeSet<NodeId> = flat
        .nodes()
        .filter(|(_, node)| silenced(node))
        .map(|(id, _)| id)
        .collect();
    let mut nodes: Vec<(NodeId, Node)> = flat
        .nodes()
        .filter(|(id, _)| !removed.contains(id))
        .map(|(id, node)| (id, node.clone()))
        .collect();
    let mut connections: Vec<Connection> = flat
        .connections()
        .filter(|c| !removed.contains(&c.to.node))
        .collect();
    let mut config = Config::new().with(MONITOR_KEY, Value::Text("1".into()));
    if !device.is_empty() {
        config.set(OUTPUT_DEVICE_KEY, Value::Text(device));
    }
    let mut n = 0;
    for &id in &monitor.nodes {
        let Some(from) = source(original, flat, keep, registry, id) else {
            continue;
        };
        let out = monitor_id(n);
        n += 1;
        nodes.push((out, Node::new(OUTPUT_ID).with_config(config.clone())));
        connections.push(Connection {
            from,
            to: Endpoint::new(out, "in"),
        });
    }
    Graph::from_parts(nodes, connections).expect("a flat graph stays valid with outputs added")
}

/// The flat endpoint of a node's main output: its first signal output, or for
/// a group the first of its outputs. `None` for a node with no signal output.
fn source(
    original: &Graph,
    flat: &Graph,
    keep: &BTreeSet<NodeId>,
    registry: &Registry,
    id: NodeId,
) -> Option<Endpoint> {
    let node = original.node(id)?;
    if node.type_id == GROUP {
        return crate::flatten::group_first_output(original, keep, id);
    }
    let node = flat.node(id)?;
    let layout = registry.get(&node.type_id)?.layout(&node.config).ok()?;
    let port = layout.outputs.first()?;
    Some(Endpoint::new(id, port.key.to_string()))
}
