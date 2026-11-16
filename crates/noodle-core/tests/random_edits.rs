//! Random edit sequences on a graph with groups: every state can be reached
//! again by undo and redo, saving and loading changes nothing, and a graph
//! that loads keeps the rules the editor enforces (wires join nodes in one
//! group, groups nest without loops).
//!
//! Edits that the graph refuses are fine and skipped; the test is about what
//! it accepts.

use noodle_core::group::{GROUP, group_nodes};
use noodle_core::{Command, Connection, Endpoint, History, Node, NodeId, Project};

/// A small, seeded random number generator, so failures reproduce.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> Option<T> {
        (!items.is_empty()).then(|| items[self.below(items.len())])
    }
}

/// One random edit, or `None` when the state gives it nothing to act on.
fn random_edit(rng: &mut XorShift, project: &mut Project) -> Option<Command> {
    let ids: Vec<NodeId> = project.graph().nodes().map(|(id, _)| id).collect();
    let groups: Vec<NodeId> = project
        .graph()
        .nodes()
        .filter(|(_, n)| n.type_id == GROUP)
        .map(|(id, _)| id)
        .collect();
    Some(match rng.below(9) {
        0 | 1 => {
            let id = project.new_node_id();
            let mut node = Node::new(if rng.below(4) == 0 { GROUP } else { "n" });
            node.parent = rng.pick(&groups).filter(|_| rng.below(2) == 0);
            Command::AddNode { id, node }
        }
        2..=4 => {
            let (from, to) = (rng.pick(&ids)?, rng.pick(&ids)?);
            Command::Connect(Connection {
                from: Endpoint::new(from, ["out", "a"][rng.below(2)]),
                to: Endpoint::new(to, ["in", "b"][rng.below(2)]),
            })
        }
        5 => {
            let to = rng.pick(&ids)?;
            Command::Disconnect {
                input: Endpoint::new(to, ["in", "b"][rng.below(2)]),
            }
        }
        6 => Command::SetParent {
            node: rng.pick(&ids)?,
            parent: rng.pick(&groups).filter(|_| rng.below(3) != 0),
        },
        7 => Command::RemoveNode {
            id: rng.pick(&ids)?,
        },
        _ => {
            // Fold up a few siblings, the way the editor's Group does.
            let first = rng.pick(&ids)?;
            let parent = project.graph().node(first)?.parent;
            let picked: Vec<NodeId> = ids
                .iter()
                .copied()
                .filter(|&id| project.graph().node(id).is_some_and(|n| n.parent == parent))
                .filter(|_| rng.below(3) == 0)
                .chain([first])
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            group_nodes(&project.clone(), &picked, || project.new_node_id())
                .ok()?
                .1
        }
    })
}

#[test]
fn random_edits_undo_redo_and_round_trip_exactly() {
    let (mut accepted, mut nested) = (0, 0);
    for seed in 1..=40u64 {
        let mut rng = XorShift(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut project = Project::new();
        let mut history = History::new();
        let mut states = vec![project.clone()];
        for step in 0..60 {
            let Some(command) = random_edit(&mut rng, &mut project) else {
                continue;
            };
            let before = project.clone();
            if history.apply(&mut project, command).is_err() {
                // A refused edit leaves the project as it was.
                assert_eq!(
                    project, before,
                    "seed {seed} step {step}: refused edit changed it"
                );
                continue;
            }
            states.push(project.clone());
            accepted += 1;
            nested += usize::from(project.graph().nodes().any(|(_, n)| n.parent.is_some()));
            let reloaded = Project::from_ron(&project.to_ron()).unwrap_or_else(|e| {
                panic!("seed {seed} step {step}: saved project won't load: {e}")
            });
            assert_eq!(
                reloaded.graph(),
                project.graph(),
                "seed {seed} step {step}: round trip"
            );
        }
        let last = project.clone();
        for (n, state) in states.iter().enumerate().rev().skip(1) {
            assert!(history.undo(&mut project).unwrap(), "seed {seed}: undo {n}");
            assert_eq!(&project, state, "seed {seed}: undo to state {n}");
        }
        for (n, state) in states.iter().enumerate().skip(1) {
            assert!(history.redo(&mut project).unwrap(), "seed {seed}: redo {n}");
            assert_eq!(&project, state, "seed {seed}: redo to state {n}");
        }
        assert_eq!(project, last, "seed {seed}");
    }
    // The generator has to get somewhere, or the test proves nothing.
    assert!(
        accepted > 600 && nested > 200,
        "{accepted} edits, {nested} with groups"
    );
}
