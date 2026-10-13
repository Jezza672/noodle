//! What makes two groups "the same" for presets.
//!
//! Two groups have the same topology if they have the same node types, the
//! same wires between them, the same boundary ports under the same IDs and names, and
//! the same port layout on every node. Node IDs, labels, editor positions and
//! config that does not shape ports (a sample, an expression) don't count.
//!
//! Nodes are put in a canonical order that doesn't depend on IDs, found by
//! refining colours like the Weisfeiler-Leman test: each node starts with a
//! colour from its type, layout and boundary name, and takes on the colours
//! of its neighbours until nothing changes. Nodes that are still
//! indistinguishable (the five saws of a unison) are ordered by ID, so a
//! group copied from a template keeps their order.

use std::collections::BTreeMap;

use super::{LayoutOf, fnv};
use crate::group::{GROUP, GROUP_INPUT, GROUP_OUTPUT};
use crate::{Graph, Node, NodeId};

/// A group's nodes in canonical order, and the key two groups share when they
/// have the same topology.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topology {
    /// Everything inside the group, at any depth.
    pub order: Vec<NodeId>,
    /// 16 hex digits.
    pub key: String,
}

/// A boundary node's ports as `ID:name`, in order. The IDs are part of the
/// topology because wires name their port by ID.
fn ports(node: &Node) -> String {
    let ports: Vec<String> = node
        .boundary_ports()
        .iter()
        .map(|(id, name)| format!("{id}:{name}"))
        .collect();
    ports.join(",")
}

fn label(node: &Node, layout: LayoutOf) -> String {
    match node.type_id.as_str() {
        GROUP => "group".into(),
        GROUP_INPUT => format!("in:{}", ports(node)),
        GROUP_OUTPUT => format!("out:{}", ports(node)),
        kind => {
            format!("{kind}|{}", layout(node).unwrap_or_default())
        }
    }
}

pub fn topology(graph: &Graph, group: NodeId, layout: LayoutOf) -> Topology {
    let ids = graph.descendants(group);
    let index: BTreeMap<NodeId, usize> = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let labels: Vec<String> = ids
        .iter()
        .map(|&id| label(graph.node(id).expect("a descendant"), layout))
        .collect();
    let parents: Vec<Option<usize>> = ids
        .iter()
        .map(|id| {
            let parent = graph.node(*id).and_then(|n| n.parent)?;
            index.get(&parent).copied()
        })
        .collect();
    // (from, from port, to, to port), inside the group only.
    let edges: Vec<(usize, String, usize, String)> = graph
        .connections()
        .filter_map(|c| {
            Some((
                *index.get(&c.from.node)?,
                c.from.port,
                *index.get(&c.to.node)?,
                c.to.port,
            ))
        })
        .collect();

    let mut colour: Vec<u64> = labels.iter().map(|l| fnv(&[l.as_bytes()])).collect();
    let classes = |colour: &[u64]| {
        let mut sorted = colour.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        sorted.len()
    };
    let mut count = classes(&colour);
    for _ in 0..ids.len() {
        let mut incoming: Vec<Vec<u64>> = vec![Vec::new(); ids.len()];
        let mut outgoing: Vec<Vec<u64>> = vec![Vec::new(); ids.len()];
        for (from, from_port, to, to_port) in &edges {
            incoming[*to].push(fnv(&[
                &colour[*from].to_le_bytes(),
                from_port.as_bytes(),
                to_port.as_bytes(),
            ]));
            outgoing[*from].push(fnv(&[
                &colour[*to].to_le_bytes(),
                from_port.as_bytes(),
                to_port.as_bytes(),
            ]));
        }
        let next: Vec<u64> = (0..ids.len())
            .map(|i| {
                incoming[i].sort_unstable();
                outgoing[i].sort_unstable();
                let parent = parents[i].map_or(0, |p| colour[p]);
                let mut bytes = Vec::new();
                bytes.extend(colour[i].to_le_bytes());
                bytes.extend(parent.to_le_bytes());
                bytes.extend(incoming[i].iter().flat_map(|c| c.to_le_bytes()));
                bytes.push(0xfe);
                bytes.extend(outgoing[i].iter().flat_map(|c| c.to_le_bytes()));
                fnv(&[&bytes])
            })
            .collect();
        let refined = classes(&next);
        colour = next;
        if refined == count {
            break;
        }
        count = refined;
    }

    // Canonical order: by colour, then ID.
    let mut ranked: Vec<usize> = (0..ids.len()).collect();
    ranked.sort_by_key(|&i| (colour[i], ids[i]));
    let mut position = vec![0usize; ids.len()];
    for (rank, &i) in ranked.iter().enumerate() {
        position[i] = rank;
    }

    let mut lines: Vec<String> = ranked
        .iter()
        .map(|&i| {
            let parent = parents[i].map_or("group".to_string(), |p| position[p].to_string());
            format!("n{}:{}:{parent}", position[i], labels[i])
        })
        .collect();
    let mut wire_lines: Vec<String> = edges
        .iter()
        .map(|(from, from_port, to, to_port)| {
            format!(
                "w{}.{from_port}>{}.{to_port}",
                position[*from], position[*to]
            )
        })
        .collect();
    wire_lines.sort();
    lines.extend(wire_lines);
    let key = format!("{:016x}", fnv(&[lines.join("\n").as_bytes()]));

    Topology {
        order: ranked.iter().map(|&i| ids[i]).collect(),
        key,
    }
}
