//! Rewriting a flattened graph so part of it plays from somewhere else, or is
//! watched: cached audio standing in for a node's output (freeze and offline
//! nodes), and taps that deliver a node's output to a callback (the renders
//! that fill the cache).
//!
//! This runs on the compiled graph, after groups are flattened and lanes
//! are added, so it sees the same nodes the schedule will.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use noodle_core::{Config, Connection, Endpoint, Graph, Node, NodeId};

use crate::NodeType;

/// The port a replacement source and a tap sink use.
const OUT: &str = "out";
const IN: &str = "in";

/// A source that plays in place of one output of a node.
#[derive(Clone)]
pub struct Replacement {
    /// A type with no inputs and one output, `out`. Its output shape must be
    /// the shape of the output it replaces.
    pub node_type: Arc<dyn NodeType>,
    /// Its config. A compiled plan keeps a node's state across updates only
    /// while the type and config stay the same, so this should name what the
    /// source plays (the cache key).
    pub config: Config,
}

/// Outputs to replace, by node and output port key in the *flattened* graph
/// (see [`Analysis`](crate::Analysis) for how to find them).
pub type Replacements = BTreeMap<(NodeId, String), Replacement>;

/// A sink that watches one output.
#[derive(Clone)]
pub struct TapSpec {
    pub endpoint: Endpoint,
    /// A type with one input, `in`, and no outputs.
    pub node_type: Arc<dyn NodeType>,
}

/// What [`rewrite`] adds besides plain nodes: the types of the nodes it made
/// up, by ID, since the registry has no entry for them.
pub(crate) type Overrides = BTreeMap<NodeId, Arc<dyn NodeType>>;

/// The ID of the source standing in for output `index` of `node`. Far above
/// any project node and below the automation lanes' sources, which count
/// down from the top.
fn replacement_id(node: NodeId, index: usize) -> NodeId {
    NodeId((1 << 62) | (node.0 << 6) | (index as u64 & 63))
}

fn tap_id(index: usize) -> NodeId {
    NodeId((1 << 62) | (1 << 61) | index as u64)
}

pub(crate) struct Rewritten<'a> {
    pub graph: Cow<'a, Graph>,
    pub overrides: Overrides,
}

/// Applies replacements and taps to `graph`.
///
/// - Every wire from a replaced output is rewired to its source, and nodes
///   that only fed the replaced outputs are dropped, so a frozen subgraph
///   costs nothing.
/// - With taps, the graph is cut down to the taps and what feeds them, so a
///   render does only the work it needs and nothing with side effects.
pub(crate) fn rewrite<'a>(
    graph: &'a Graph,
    replacements: &Replacements,
    taps: &[TapSpec],
) -> Rewritten<'a> {
    if replacements.is_empty() && taps.is_empty() {
        return Rewritten {
            graph: Cow::Borrowed(graph),
            overrides: Overrides::new(),
        };
    }
    let mut overrides = Overrides::new();
    let mut nodes: BTreeMap<NodeId, Node> = graph.nodes().map(|(id, n)| (id, n.clone())).collect();
    let mut connections: Vec<Connection> = graph.connections().collect();

    // Replacements whose node isn't in the graph have nothing to replace.
    let mut index_of_node: BTreeMap<NodeId, usize> = BTreeMap::new();
    let mut redirect: BTreeMap<Endpoint, Endpoint> = BTreeMap::new();
    for ((node, port), replacement) in replacements {
        if !nodes.contains_key(node) {
            continue;
        }
        let index = index_of_node.entry(*node).or_default();
        let id = replacement_id(*node, *index);
        *index += 1;
        let mut source = Node::new(replacement.node_type.info().id);
        source.config = replacement.config.clone();
        nodes.insert(id, source);
        overrides.insert(id, Arc::clone(&replacement.node_type));
        redirect.insert(Endpoint::new(*node, port.clone()), Endpoint::new(id, OUT));
    }
    let seeds: BTreeSet<NodeId> = redirect.keys().map(|e| e.node).collect();
    for connection in &mut connections {
        if let Some(source) = redirect.get(&connection.from) {
            connection.from = source.clone();
        }
    }
    let mut roots = BTreeSet::new();
    for (i, tap) in taps.iter().enumerate() {
        let id = tap_id(i);
        let mut sink = Node::new(tap.node_type.info().id);
        sink.config = Config::new();
        nodes.insert(id, sink);
        overrides.insert(id, Arc::clone(&tap.node_type));
        // A tap on an output that was just replaced watches the source.
        let from = redirect
            .get(&tap.endpoint)
            .cloned()
            .unwrap_or_else(|| tap.endpoint.clone());
        connections.push(Connection {
            from,
            to: Endpoint::new(id, IN),
        });
        roots.insert(id);
    }
    if !seeds.is_empty() {
        for id in unneeded(&connections, &seeds) {
            nodes.remove(&id);
        }
        connections.retain(|c| nodes.contains_key(&c.from.node) && nodes.contains_key(&c.to.node));
    }

    if !roots.is_empty() {
        let keep = ancestors(&connections, &roots);
        nodes.retain(|id, _| keep.contains(id));
        connections.retain(|c| keep.contains(&c.from.node) && keep.contains(&c.to.node));
    }

    let graph = Graph::from_parts(nodes, connections)
        .expect("rewriting keeps wires between nodes that exist");
    Rewritten {
        graph: Cow::Owned(graph),
        overrides,
    }
}

/// `roots` and everything upstream of them.
fn ancestors(connections: &[Connection], roots: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
    let mut upstream: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for c in connections {
        upstream.entry(c.to.node).or_default().push(c.from.node);
    }
    let mut seen = roots.clone();
    let mut stack: Vec<NodeId> = roots.iter().copied().collect();
    while let Some(node) = stack.pop() {
        for &from in upstream.get(&node).into_iter().flatten() {
            if seen.insert(from) {
                stack.push(from);
            }
        }
    }
    seen
}

/// The nodes that only existed to feed `seeds`: the seeds themselves, if
/// nothing reads what they still output, and everything upstream of them that
/// nothing else reads. `connections` are the wires after the replaced
/// outputs were redirected.
fn unneeded(connections: &[Connection], seeds: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
    let mut removed = ancestors(connections, seeds);
    // A node stays if anything outside the removed set reads it. Removing a
    // node from the set can keep others, so repeat until nothing changes.
    loop {
        let keep: Vec<NodeId> = removed
            .iter()
            .copied()
            .filter(|node| {
                connections
                    .iter()
                    .any(|c| c.from.node == *node && !removed.contains(&c.to.node))
            })
            .collect();
        if keep.is_empty() {
            return removed;
        }
        for node in keep {
            removed.remove(&node);
        }
    }
}
