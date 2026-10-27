//! The partition: which clusters the playback cache keeps (ADR 0010).
//!
//! A cut protects what is upstream of it from edits downstream, so it is
//! worth making where the upstream is expensive to render, and not where it
//! is cheap: storage is per audio-second whatever is cut, so a long chain of
//! gain stages costs the bytes of one reverb and isn't worth them. The
//! candidates are:
//!
//! - *groups*: the top-level groups, cut before their outputs' stages. A
//!   group is a prior (the user drew a boundary there), not a mandate: one
//!   that turns out cheap is dropped, and the clusters inside it are looked
//!   at on their own;
//! - *joins*: each branch that feeds a node with more than one audio source.
//!   An edit to one branch then renders that branch and the join, and reads
//!   the others;
//! - *fan-outs*: the output of a node that more than one node reads, so a
//!   shared send or a drum rack is rendered once and read by both.
//!
//! They are taken upstream first, and each one's cost is what it would
//! render that no cut upstream of it already holds. A cut therefore never
//! pays for what a cut before it has stored.
//!
//! The partition is a function of the graph's wiring and the nodes' types,
//! configs and ids, so it comes out the same after any edit that doesn't
//! change those: moving or renaming a node, a fader or a solo, leaves every
//! cut and every key where it was.
//!
//! Expensive-node and chain-midpoint cuts, and the cost being measured
//! instead of declared, are not here yet.

use std::collections::{BTreeMap, BTreeSet};

use noodle_core::{NodeId, Project};

use super::{Analysis, InputOrigin, Target, TargetKind};

/// The clusters to cache: the `groups` worth it, and the cuts between and
/// inside them. `taken` are the nodes that already have a target (offline and
/// frozen ones). The result is in no particular order.
pub(super) fn picks(
    project: &Project,
    analysis: &Analysis,
    groups: Vec<Target>,
    taken: &BTreeSet<NodeId>,
    min_cost: u32,
) -> Vec<Target> {
    let position: BTreeMap<NodeId, usize> = analysis
        .order
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();

    // Who reads each output, and which nodes feed a join.
    let mut readers: BTreeMap<NodeId, BTreeMap<usize, BTreeSet<NodeId>>> = BTreeMap::new();
    let mut branches: BTreeSet<(NodeId, usize)> = BTreeSet::new();
    for (&reader, info) in &analysis.nodes {
        let mut audio = BTreeSet::new();
        for origin in &info.inputs {
            match *origin {
                InputOrigin::Wire(source, output) => {
                    audio.insert((source, output));
                    readers
                        .entry(source)
                        .or_default()
                        .entry(output)
                        .or_default()
                        .insert(reader);
                }
                InputOrigin::Modulated(source, output, _) => {
                    readers
                        .entry(source)
                        .or_default()
                        .entry(output)
                        .or_default()
                        .insert(reader);
                }
                InputOrigin::Value(_) => {}
            }
        }
        // A group's stage takes the signal and its controls (gain, mute,
        // solo): those are not branches of a mix.
        if info.type_id != noodle_core::group::GROUP_STAGE
            && audio
                .iter()
                .map(|&(source, _)| source)
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        {
            branches.extend(audio);
        }
    }

    let in_group_outputs: BTreeSet<NodeId> = groups
        .iter()
        .flat_map(|t| t.outputs.iter().map(|&(n, _)| n))
        .collect();
    let inside_frozen = |id: NodeId| {
        let mut at = Some(id);
        while let Some(node) = at {
            if project.frozen().contains(&node) {
                return true;
            }
            at = project.graph().node(node).and_then(|n| n.parent);
        }
        false
    };

    let mut units: Vec<Target> = groups;
    for (&node, outputs) in &readers {
        let Some(info) = analysis.nodes.get(&node) else {
            continue;
        };
        // The outputs worth a cut: those a join takes as a branch, and those
        // more than one node reads.
        let wanted: Vec<usize> = outputs
            .iter()
            .filter(|&(&output, readers)| readers.len() > 1 || branches.contains(&(node, output)))
            .map(|(&output, _)| output)
            .collect();
        if wanted.is_empty()
            || info.offline
            || info.key.is_err()
            || taken.contains(&node)
            || in_group_outputs.contains(&node)
            || inside_frozen(node)
        {
            continue;
        }
        units.push(Target {
            kind: TargetKind::Cut(node),
            outputs: wanted.into_iter().map(|output| (node, output)).collect(),
        });
    }

    // Upstream first, so a cut's cost leaves out what is already stored.
    let latest = |t: &Target| t.outputs.iter().filter_map(|(n, _)| position.get(n)).max();
    units.sort_by_key(|t| (latest(t).copied(), t.kind));
    let mut stored: BTreeSet<NodeId> = BTreeSet::new();
    let mut picked = Vec::new();
    for unit in units {
        if cost_of(analysis, &unit, &stored) >= min_cost {
            stored.extend(unit.outputs.iter().map(|&(n, _)| n));
            picked.push(unit);
        }
    }
    picked
}

/// What rendering a second of `target` costs, leaving out everything behind
/// a node that is `stored` already.
fn cost_of(analysis: &Analysis, target: &Target, stored: &BTreeSet<NodeId>) -> u32 {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<NodeId> = target.outputs.iter().map(|&(n, _)| n).collect();
    let mut cost = 0u32;
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        let Some(info) = analysis.nodes.get(&node) else {
            continue;
        };
        cost = cost.saturating_add(info.cost);
        for origin in &info.inputs {
            if let InputOrigin::Wire(source, _) | InputOrigin::Modulated(source, _, _) = origin
                && !stored.contains(source)
            {
                stack.push(*source);
            }
        }
    }
    cost
}
