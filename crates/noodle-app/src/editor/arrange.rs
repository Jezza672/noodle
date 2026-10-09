//! Auto-arrange: a layered layout of the nodes.
//!
//! Signal flows left to right, so a node goes in a column one past its
//! furthest source. Within a column nodes are ordered to reduce wire
//! crossings (barycentre sweeps, starting from their current vertical order so
//! a tidy graph stays put), then each column is nudged towards the middle of
//! its neighbours. Unconnected pieces of the graph are laid out separately and
//! stacked, so one doesn't tug at another.
//!
//! The layout is a pure function from sizes and wires to positions, so it is
//! tested without a UI. The editor turns the result into one batch of
//! `MoveNode` commands, which is one undo step.
//!
//! Frames: a frame and the nodes inside it are left where they are, as
//! moving them needs the frame to be resized. The rest are laid out beside
//! them, clear of every frame.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Pos2, Rect, Vec2};
use noodle_core::NodeId;

/// Space between columns, where the wires run.
const COLUMN_GAP: f32 = 70.0;
/// Space between nodes in a column.
const ROW_GAP: f32 = 24.0;
/// Space between separate pieces of the graph.
const COMPONENT_GAP: f32 = 50.0;
/// Barycentre sweeps, each a pass down and a pass up the columns.
const SWEEPS: usize = 8;
/// Passes pulling nodes towards their neighbours.
const RELAX_PASSES: usize = 6;

/// A node to place: where it is now, and how big it is.
#[derive(Clone, Copy)]
pub struct Item {
    pub rect: Rect,
}

/// Where each of `items` should go (its top-left corner), given the wires
/// between them as `(from, to)`. Wires to nodes that aren't in `items` are
/// ignored. `avoid` are rectangles the layout must stay clear of.
pub fn arrange(
    items: &BTreeMap<NodeId, Item>,
    wires: &[(NodeId, NodeId)],
    avoid: &[Rect],
) -> BTreeMap<NodeId, Pos2> {
    if items.is_empty() {
        return BTreeMap::new();
    }
    let origin = items
        .values()
        .map(|i| i.rect)
        .reduce(Rect::union)
        .map_or(Pos2::ZERO, |r| r.min);

    let edges = acyclic_edges(items, wires);
    let components = components(items, &edges);

    // Each component is laid out from the origin, then stacked.
    let mut positions = BTreeMap::new();
    let mut cursor = 0.0_f32;
    let mut placed: Vec<(BTreeMap<NodeId, Pos2>, Vec2)> = Vec::new();
    for nodes in components {
        placed.push(layout_component(items, &edges, &nodes));
    }
    for (local, size) in &placed {
        for (id, p) in local {
            positions.insert(*id, Pos2::new(p.x, p.y + cursor));
        }
        cursor += size.y + COMPONENT_GAP;
    }
    let height = (cursor - COMPONENT_GAP).max(0.0);
    let width = placed.iter().map(|(_, s)| s.x).fold(0.0, f32::max);

    // Move the whole layout clear of anything it must avoid, to the right.
    let mut shift = Vec2::new(origin.x, origin.y);
    let block = |shift: Vec2| {
        Rect::from_min_size(Pos2::ZERO + shift, Vec2::new(width, height)).expand(ROW_GAP / 2.0)
    };
    for _ in 0..avoid.len() + 1 {
        let Some(hit) = avoid.iter().find(|r| r.intersects(block(shift))) else {
            break;
        };
        shift.x = hit.right() + COLUMN_GAP + ROW_GAP / 2.0;
    }
    positions
        .into_iter()
        .map(|(id, p)| (id, p + shift))
        .collect()
}

/// The wires with feedback removed: a depth-first walk, in on-screen order,
/// drops any wire that closes a loop. Self-loops and duplicates go too.
fn acyclic_edges(
    items: &BTreeMap<NodeId, Item>,
    wires: &[(NodeId, NodeId)],
) -> BTreeSet<(NodeId, NodeId)> {
    let mut out: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for &(from, to) in wires {
        if from != to && items.contains_key(&from) && items.contains_key(&to) {
            out.entry(from).or_default().push(to);
        }
    }
    for targets in out.values_mut() {
        targets.sort();
        targets.dedup();
    }
    // Visit left-most first so the wires that run backwards are the ones cut.
    let mut order: Vec<NodeId> = items.keys().copied().collect();
    order.sort_by(|a, b| items[a].rect.min.x.total_cmp(&items[b].rect.min.x));
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }
    let mut marks: BTreeMap<NodeId, Mark> = BTreeMap::new();
    let mut kept = BTreeSet::new();
    for root in order {
        if marks.contains_key(&root) {
            continue;
        }
        // Iterative DFS: (node, next child index).
        marks.insert(root, Mark::Open);
        let mut stack = vec![(root, 0_usize)];
        while let Some(top) = stack.last_mut() {
            let node = top.0;
            let children = out.get(&node).map_or(&[][..], Vec::as_slice);
            if let Some(&child) = children.get(top.1) {
                top.1 += 1;
                match marks.get(&child) {
                    Some(Mark::Open) => {} // a back edge: feedback, dropped
                    Some(Mark::Done) => {
                        kept.insert((node, child));
                    }
                    None => {
                        kept.insert((node, child));
                        marks.insert(child, Mark::Open);
                        stack.push((child, 0));
                    }
                }
            } else {
                marks.insert(node, Mark::Done);
                stack.pop();
            }
        }
    }
    kept
}

/// Groups of nodes joined by wires, each in on-screen order (top to bottom).
fn components(
    items: &BTreeMap<NodeId, Item>,
    edges: &BTreeSet<(NodeId, NodeId)>,
) -> Vec<Vec<NodeId>> {
    let mut neighbours: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for &(a, b) in edges {
        neighbours.entry(a).or_default().push(b);
        neighbours.entry(b).or_default().push(a);
    }
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for &start in items.keys() {
        if !seen.insert(start) {
            continue;
        }
        let mut group = vec![start];
        let mut i = 0;
        while i < group.len() {
            for &n in neighbours.get(&group[i]).into_iter().flatten() {
                if seen.insert(n) {
                    group.push(n);
                }
            }
            i += 1;
        }
        group.sort_by(|a, b| by_position(items, *a, *b));
        result.push(group);
    }
    // Top to bottom as they are now.
    result.sort_by(|a, b| by_position(items, a[0], b[0]));
    result
}

fn by_position(items: &BTreeMap<NodeId, Item>, a: NodeId, b: NodeId) -> std::cmp::Ordering {
    let (a_item, b_item) = (&items[&a].rect, &items[&b].rect);
    a_item
        .min
        .y
        .total_cmp(&b_item.min.y)
        .then(a_item.min.x.total_cmp(&b_item.min.x))
        .then(a.cmp(&b))
}

/// Lays out one connected piece, with its top-left at the origin. Returns the
/// positions and the size it takes.
fn layout_component(
    items: &BTreeMap<NodeId, Item>,
    edges: &BTreeSet<(NodeId, NodeId)>,
    nodes: &[NodeId],
) -> (BTreeMap<NodeId, Pos2>, Vec2) {
    let in_piece: BTreeSet<NodeId> = nodes.iter().copied().collect();
    let mut sources: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    let mut targets: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for &(a, b) in edges {
        if in_piece.contains(&a) && in_piece.contains(&b) {
            targets.entry(a).or_default().push(b);
            sources.entry(b).or_default().push(a);
        }
    }
    let none: Vec<NodeId> = Vec::new();
    let srcs = |n: &NodeId| sources.get(n).unwrap_or(&none);
    let tgts = |n: &NodeId| targets.get(n).unwrap_or(&none);

    // Longest path from the left. `nodes` isn't in dependency order, so
    // iterate to a fixed point; the edges are acyclic so this ends.
    let mut layer: BTreeMap<NodeId, usize> = nodes.iter().map(|&n| (n, 0)).collect();
    loop {
        let mut changed = false;
        for n in nodes {
            let want = srcs(n).iter().map(|s| layer[s] + 1).max().unwrap_or(0);
            if want > layer[n] {
                layer.insert(*n, want);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Pull nodes with no sources up against what they feed, so a lone
    // generator isn't left far from its use.
    for n in nodes {
        if srcs(n).is_empty()
            && let Some(first) = tgts(n).iter().map(|t| layer[t]).min()
        {
            layer.insert(*n, first.saturating_sub(1));
        }
    }

    let depth = layer.values().copied().max().unwrap_or(0) + 1;
    let mut columns: Vec<Vec<NodeId>> = vec![Vec::new(); depth];
    for n in nodes {
        columns[layer[n]].push(*n);
    }
    // `nodes` is in on-screen order, so each column starts in it too.

    // Crossing reduction: order by the mean index of the neighbours in the
    // previous column (down) or the next (up). Nodes with none keep their slot.
    let index = |columns: &[Vec<NodeId>]| -> BTreeMap<NodeId, f32> {
        columns
            .iter()
            .flat_map(|c| c.iter().enumerate().map(|(i, n)| (*n, i as f32)))
            .collect()
    };
    for _ in 0..SWEEPS {
        for forward in [true, false] {
            let order: Vec<usize> = if forward {
                (1..depth).collect()
            } else {
                (0..depth.saturating_sub(1)).rev().collect()
            };
            for c in order {
                let pos = index(&columns);
                let mut keyed: Vec<(f32, usize, NodeId)> = columns[c]
                    .iter()
                    .enumerate()
                    .map(|(slot, n)| {
                        let neighbours = if forward { srcs(n) } else { tgts(n) };
                        let key = if neighbours.is_empty() {
                            slot as f32
                        } else {
                            neighbours.iter().map(|m| pos[m]).sum::<f32>() / neighbours.len() as f32
                        };
                        (key, slot, *n)
                    })
                    .collect();
                keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                columns[c] = keyed.into_iter().map(|(_, _, n)| n).collect();
            }
        }
    }

    // Columns left to right.
    let widths: Vec<f32> = columns
        .iter()
        .map(|c| c.iter().map(|n| items[n].rect.width()).fold(0.0, f32::max))
        .collect();
    let mut x = Vec::with_capacity(depth);
    let mut cursor = 0.0;
    for w in &widths {
        x.push(cursor);
        cursor += w + COLUMN_GAP;
    }
    let width = (cursor - COLUMN_GAP).max(0.0);

    // Rows: stack each column, then relax towards the neighbours' centres,
    // pushing apart anything that would overlap.
    let height = |n: &NodeId| items[n].rect.height();
    let mut y: BTreeMap<NodeId, f32> = BTreeMap::new();
    for column in &columns {
        let mut top = 0.0;
        for n in column {
            y.insert(*n, top);
            top += height(n) + ROW_GAP;
        }
    }
    for pass in 0..RELAX_PASSES {
        let columns_order: Vec<usize> = if pass % 2 == 0 {
            (0..depth).collect()
        } else {
            (0..depth).rev().collect()
        };
        for c in columns_order {
            let wanted: Vec<f32> = columns[c]
                .iter()
                .map(|n| {
                    let linked: Vec<f32> = srcs(n)
                        .iter()
                        .chain(tgts(n))
                        .map(|m| y[m] + height(m) / 2.0)
                        .collect();
                    if linked.is_empty() {
                        y[n]
                    } else {
                        linked.iter().sum::<f32>() / linked.len() as f32 - height(n) / 2.0
                    }
                })
                .collect();
            place_column(&columns[c], &wanted, &height, &mut y);
        }
    }

    // Put the top of the piece at zero.
    let top = columns
        .iter()
        .flatten()
        .map(|n| y[n])
        .fold(f32::INFINITY, f32::min);
    let bottom = columns
        .iter()
        .flatten()
        .map(|n| y[n] + height(n))
        .fold(f32::NEG_INFINITY, f32::max);
    let mut out = BTreeMap::new();
    for (c, column) in columns.iter().enumerate() {
        for n in column {
            out.insert(*n, Pos2::new(x[c], y[n] - top));
        }
    }
    (out, Vec2::new(width, bottom - top))
}

/// Puts the nodes of one column, in order, as near their wanted tops as they
/// can go without overlapping: each one is placed where it is wanted unless
/// the one above is in the way, then the whole run is shifted back to keep the
/// total distance from what was wanted small.
fn place_column(
    column: &[NodeId],
    wanted: &[f32],
    height: &dyn Fn(&NodeId) -> f32,
    y: &mut BTreeMap<NodeId, f32>,
) {
    // Blocks of touching nodes, merged while they overlap (the usual
    // pool-adjacent-violators approach to ordered placement).
    struct Block {
        first: usize,
        count: usize,
        /// Sum of `wanted - offset-within-block` over the members.
        sum: f32,
        top: f32,
        extent: f32,
    }
    let mut blocks: Vec<Block> = Vec::new();
    for (i, n) in column.iter().enumerate() {
        let mut block = Block {
            first: i,
            count: 1,
            sum: wanted[i],
            top: wanted[i],
            extent: height(n),
        };
        while let Some(prev) = blocks.last() {
            if prev.top + prev.extent + ROW_GAP <= block.top {
                break;
            }
            let prev = blocks.pop().unwrap();
            let offset = prev.extent + ROW_GAP;
            let count = prev.count + block.count;
            let sum = prev.sum + block.sum - offset * block.count as f32;
            block = Block {
                first: prev.first,
                count,
                sum,
                top: sum / count as f32,
                extent: offset + block.extent,
            };
        }
        blocks.push(block);
    }
    for block in blocks {
        let mut top = block.top;
        for n in &column[block.first..block.first + block.count] {
            y.insert(*n, top);
            top += height(n) + ROW_GAP;
        }
    }
}

#[cfg(test)]
mod tests;
