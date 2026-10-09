use super::*;

fn id(n: u64) -> NodeId {
    NodeId(n)
}

fn item(x: f32, y: f32, w: f32, h: f32) -> Item {
    Item {
        rect: Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)),
    }
}

fn rect_at(items: &BTreeMap<NodeId, Item>, p: &BTreeMap<NodeId, Pos2>, n: NodeId) -> Rect {
    Rect::from_min_size(p[&n], items[&n].rect.size())
}

fn no_overlaps(items: &BTreeMap<NodeId, Item>, p: &BTreeMap<NodeId, Pos2>) {
    let ids: Vec<_> = items.keys().copied().collect();
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
            assert!(
                !rect_at(items, p, *a).intersects(rect_at(items, p, *b)),
                "{a:?} overlaps {b:?}"
            );
        }
    }
}

#[test]
fn a_chain_runs_left_to_right_on_one_row() {
    // Scrambled start positions, wired 1 -> 2 -> 3.
    let items = BTreeMap::from([
        (id(1), item(300.0, 40.0, 100.0, 50.0)),
        (id(2), item(0.0, 200.0, 100.0, 50.0)),
        (id(3), item(150.0, 0.0, 100.0, 50.0)),
    ]);
    let p = arrange(&items, &[(id(1), id(2)), (id(2), id(3))], &[]);
    assert!(p[&id(1)].x < p[&id(2)].x && p[&id(2)].x < p[&id(3)].x);
    assert_eq!(p[&id(1)].y, p[&id(2)].y);
    assert_eq!(p[&id(2)].y, p[&id(3)].y);
    // A column gap apart.
    assert_eq!(p[&id(2)].x - p[&id(1)].x, 100.0 + COLUMN_GAP);
    no_overlaps(&items, &p);
}

#[test]
fn a_fan_in_stacks_sources_beside_their_sink() {
    let items = BTreeMap::from([
        (id(1), item(0.0, 0.0, 80.0, 40.0)),
        (id(2), item(0.0, 0.0, 80.0, 40.0)),
        (id(3), item(0.0, 0.0, 80.0, 40.0)),
    ]);
    let p = arrange(&items, &[(id(1), id(3)), (id(2), id(3))], &[]);
    assert_eq!(p[&id(1)].x, p[&id(2)].x);
    assert!(p[&id(3)].x > p[&id(1)].x);
    // The sink sits between its two sources.
    let mid = (p[&id(1)].y + p[&id(2)].y) / 2.0;
    assert!((p[&id(3)].y - mid).abs() < 0.01);
    no_overlaps(&items, &p);
}

#[test]
fn a_lone_source_sits_next_to_what_it_feeds() {
    // 1 -> 2 -> 3, and 4 -> 3. Node 4 has no sources, so it should be one
    // column from 3, not stranded in column 0.
    let items: BTreeMap<_, _> = (1..=4)
        .map(|n| (id(n), item(0.0, n as f32 * 100.0, 80.0, 40.0)))
        .collect();
    let p = arrange(
        &items,
        &[(id(1), id(2)), (id(2), id(3)), (id(4), id(3))],
        &[],
    );
    assert_eq!(p[&id(4)].x, p[&id(2)].x);
}

#[test]
fn crossings_are_removed() {
    // Two chains a1->b1 and a2->b2 that start crossed (a1 above a2 but b1
    // below b2).
    let items = BTreeMap::from([
        (id(1), item(0.0, 0.0, 80.0, 40.0)),
        (id(2), item(0.0, 100.0, 80.0, 40.0)),
        (id(3), item(300.0, 100.0, 80.0, 40.0)),
        (id(4), item(300.0, 0.0, 80.0, 40.0)),
    ]);
    // 1->3 and 2->4: with these positions 3 is below 4 but 1 is above 2.
    let p = arrange(&items, &[(id(1), id(3)), (id(2), id(4))], &[]);
    let order_in = |a, b| p[&id(a)].y < p[&id(b)].y;
    assert_eq!(order_in(1, 2), order_in(3, 4));
}

#[test]
fn feedback_does_not_hang_and_is_laid_out() {
    let items = BTreeMap::from([
        (id(1), item(0.0, 0.0, 80.0, 40.0)),
        (id(2), item(100.0, 0.0, 80.0, 40.0)),
        (id(3), item(200.0, 0.0, 80.0, 40.0)),
    ]);
    // 1 -> 2 -> 3 -> 1, and a self loop.
    let p = arrange(
        &items,
        &[
            (id(1), id(2)),
            (id(2), id(3)),
            (id(3), id(1)),
            (id(2), id(2)),
        ],
        &[],
    );
    assert_eq!(p.len(), 3);
    assert!(p[&id(1)].x < p[&id(2)].x && p[&id(2)].x < p[&id(3)].x);
}

#[test]
fn separate_pieces_are_stacked_without_overlap() {
    let items: BTreeMap<_, _> = (1..=4)
        .map(|n| (id(n), item(0.0, 0.0, 80.0, 40.0)))
        .collect();
    let p = arrange(&items, &[(id(1), id(2)), (id(3), id(4))], &[]);
    no_overlaps(&items, &p);
    assert!(p[&id(3)].y >= p[&id(1)].y + 40.0 + COMPONENT_GAP);
}

#[test]
fn tall_nodes_do_not_overlap() {
    let items = BTreeMap::from([
        (id(1), item(0.0, 0.0, 80.0, 300.0)),
        (id(2), item(0.0, 0.0, 80.0, 20.0)),
        (id(3), item(0.0, 0.0, 80.0, 20.0)),
        (id(4), item(0.0, 0.0, 80.0, 20.0)),
    ]);
    let wires = [(id(1), id(4)), (id(2), id(4)), (id(3), id(4))];
    let p = arrange(&items, &wires, &[]);
    no_overlaps(&items, &p);
}

#[test]
fn layout_stays_anchored_at_the_top_left() {
    let items = BTreeMap::from([
        (id(1), item(500.0, 300.0, 80.0, 40.0)),
        (id(2), item(900.0, 700.0, 80.0, 40.0)),
    ]);
    let p = arrange(&items, &[(id(1), id(2))], &[]);
    assert_eq!(p[&id(1)], Pos2::new(500.0, 300.0));
}

#[test]
fn it_clears_obstacles() {
    let items = BTreeMap::from([
        (id(1), item(0.0, 0.0, 80.0, 40.0)),
        (id(2), item(0.0, 0.0, 80.0, 40.0)),
    ]);
    let frame = Rect::from_min_size(Pos2::new(-10.0, -10.0), Vec2::new(300.0, 200.0));
    let p = arrange(&items, &[(id(1), id(2))], &[frame]);
    for n in [id(1), id(2)] {
        assert!(!rect_at(&items, &p, n).intersects(frame));
    }
}

#[test]
fn it_is_deterministic_and_idempotent() {
    let items: BTreeMap<_, _> = (1..=6)
        .map(|n| {
            (
                id(n),
                item(
                    (n * 37 % 5) as f32 * 90.0,
                    (n * 53 % 7) as f32 * 60.0,
                    90.0,
                    40.0,
                ),
            )
        })
        .collect();
    let wires = [
        (id(1), id(3)),
        (id(2), id(3)),
        (id(3), id(5)),
        (id(4), id(5)),
        (id(5), id(6)),
        (id(1), id(6)),
    ];
    let first = arrange(&items, &wires, &[]);
    assert_eq!(first, arrange(&items, &wires, &[]));
    // Arranging an arranged graph leaves it alone.
    let moved: BTreeMap<_, _> = items
        .iter()
        .map(|(n, i)| {
            (
                *n,
                Item {
                    rect: Rect::from_min_size(first[n], i.rect.size()),
                },
            )
        })
        .collect();
    assert_eq!(first, arrange(&moved, &wires, &[]));
}
