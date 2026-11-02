//! Group C of the scorecard: do I ever have to throw work away?
//!
//! - **Forced rework.** The route's author marks the steps that only recreate
//!   something (`Handle::recreating`); the scorer counts the actions in them.
//!   It also flags a node kind that is added after a node of that kind was
//!   removed, which is what delete-and-recreate looks like in the graph.
//!   Undoing is not counted as a removal.
//! - **Undo needed.** The undo chords the route presses.
//! - **Undo round trip.** After the route, undo to the start and redo to the
//!   end. Both ends must match the project as it was. The number of undo
//!   steps per user action is reported too (one is ideal).

use crate::testkit::{Action, ActionKind};

/// A node of this kind appeared or disappeared.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Added(String),
    Removed(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rework {
    /// Recreate steps the author marked.
    pub marked: usize,
    /// Actions inside the marked steps.
    pub marked_actions: usize,
    /// Node kinds added after one of that kind was removed, one entry per
    /// recreation.
    pub detected: Vec<String>,
    /// Undo chords pressed.
    pub undos: usize,
}

impl Rework {
    /// Whether the route recreated anything.
    pub fn any(&self) -> bool {
        self.marked > 0 || !self.detected.is_empty()
    }
}

pub fn score(log: &[Action], changes: &[Change]) -> Rework {
    let mut r = Rework::default();
    let mut recreating = false;
    for a in log {
        match &a.kind {
            ActionKind::Recreate(_) => {
                r.marked += 1;
                recreating = true;
            }
            ActionKind::Step(_) => recreating = false,
            ActionKind::Wait(_) => {}
            kind => {
                if matches!(kind, ActionKind::Key(egui::Key::Z))
                    && a.modifiers.command
                    && !a.modifiers.shift
                {
                    r.undos += 1;
                }
                r.marked_actions += usize::from(recreating);
            }
        }
    }
    let mut removed: Vec<&str> = Vec::new();
    for change in changes {
        match change {
            Change::Removed(kind) => removed.push(kind),
            Change::Added(kind) => {
                if let Some(at) = removed.iter().position(|k| k == kind) {
                    removed.swap_remove(at);
                    r.detected.push(kind.clone());
                }
            }
        }
    }
    r
}

/// What undoing everything and redoing it all found.
#[derive(Clone, Debug, PartialEq)]
pub struct RoundTrip {
    pub undo_steps: usize,
    pub redo_steps: usize,
    /// After undoing everything, the state is the one the route began in.
    pub restores_start: bool,
    /// After redoing everything, the state is the one the route ended in.
    pub restores_end: bool,
}

impl RoundTrip {
    pub fn ok(&self) -> bool {
        self.restores_start && self.restores_end
    }
}

/// A runaway guard: no route has this many undo steps.
const LIMIT: usize = 10_000;

/// Undoes to the start, then redoes to the end, comparing the state (as
/// `snapshot` shows it) with `start` and `end`. `undo` and `redo` return
/// whether they did anything.
pub fn round_trip(
    start: &str,
    end: &str,
    mut undo: impl FnMut() -> bool,
    mut redo: impl FnMut() -> bool,
    snapshot: impl Fn() -> String,
) -> RoundTrip {
    let mut undo_steps = 0;
    while undo_steps < LIMIT && undo() {
        undo_steps += 1;
    }
    let restores_start = snapshot() == start;
    let mut redo_steps = 0;
    while redo_steps < LIMIT && redo() {
        redo_steps += 1;
    }
    RoundTrip {
        undo_steps,
        redo_steps,
        restores_start,
        restores_end: snapshot() == end,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use egui::{Key, Modifiers, Pos2};

    use super::*;

    fn act(kind: ActionKind, modifiers: Modifiers) -> Action {
        Action {
            kind,
            modifiers,
            start: Pos2::ZERO,
            end: Pos2::ZERO,
            start_hit: None,
            end_hit: None,
            start_view: String::new(),
            end_view: String::new(),
            time: 0.0,
        }
    }

    fn click() -> Action {
        act(
            ActionKind::Click(egui::PointerButton::Primary),
            Modifiers::NONE,
        )
    }

    fn step() -> Action {
        act(ActionKind::Step("s".into()), Modifiers::NONE)
    }

    #[test]
    fn marked_steps_count_their_actions_up_to_the_next_step() {
        let log = [
            step(),
            click(),
            act(
                ActionKind::Recreate("redo the filter".into()),
                Modifiers::NONE,
            ),
            click(),
            click(),
            step(),
            click(),
        ];
        let r = score(&log, &[]);
        assert_eq!(r.marked, 1);
        assert_eq!(r.marked_actions, 2);
        assert!(r.any());
    }

    #[test]
    fn undo_chords_are_counted_but_redo_is_not() {
        let log = [
            act(ActionKind::Key(Key::Z), Modifiers::COMMAND),
            act(ActionKind::Key(Key::Z), Modifiers::COMMAND),
            act(
                ActionKind::Key(Key::Z),
                Modifiers::COMMAND | Modifiers::SHIFT,
            ),
            act(ActionKind::Key(Key::Z), Modifiers::NONE),
        ];
        assert_eq!(score(&log, &[]).undos, 2);
    }

    #[test]
    fn adding_a_kind_after_removing_it_is_flagged() {
        let removed_then_added = [
            Change::Added("sine".into()),
            Change::Removed("sine".into()),
            Change::Added("sine".into()),
        ];
        let r = score(&[], &removed_then_added);
        assert_eq!(r.detected, vec!["sine".to_string()]);
        assert!(r.any());
    }

    #[test]
    fn removing_one_kind_and_adding_another_is_not_flagged() {
        let r = score(
            &[],
            &[Change::Removed("sine".into()), Change::Added("saw".into())],
        );
        assert!(!r.any());
        // Adding before removing is not a recreation either.
        let r = score(
            &[],
            &[Change::Added("sine".into()), Change::Removed("sine".into())],
        );
        assert!(!r.any());
    }

    #[test]
    fn one_removal_pays_for_one_recreation() {
        let r = score(
            &[],
            &[
                Change::Removed("gain".into()),
                Change::Added("gain".into()),
                Change::Added("gain".into()),
            ],
        );
        assert_eq!(r.detected.len(), 1);
    }

    /// A toy document with an undo stack, whose undo may be seeded to leave
    /// something behind.
    struct Toy {
        text: RefCell<String>,
        undo: RefCell<Vec<usize>>,
        redo: RefCell<Vec<char>>,
        leak: bool,
    }

    impl Toy {
        fn new(leak: bool) -> Self {
            Toy {
                text: RefCell::new(String::new()),
                undo: RefCell::new(Vec::new()),
                redo: RefCell::new(Vec::new()),
                leak,
            }
        }
        fn type_char(&self, c: char) {
            self.text.borrow_mut().push(c);
            self.undo.borrow_mut().push(1);
        }
        fn undo(&self) -> bool {
            if self.undo.borrow_mut().pop().is_none() {
                return false;
            }
            // The seeded bug: the first character is never taken back.
            if self.leak && self.undo.borrow().is_empty() {
                return true;
            }
            let c = self.text.borrow_mut().pop().unwrap();
            self.redo.borrow_mut().push(c);
            true
        }
        fn redo(&self) -> bool {
            let Some(c) = self.redo.borrow_mut().pop() else {
                return false;
            };
            self.text.borrow_mut().push(c);
            self.undo.borrow_mut().push(1);
            true
        }
        fn run(&self) -> RoundTrip {
            let start = self.text.borrow().clone();
            for c in ['a', 'b', 'c'] {
                self.type_char(c);
            }
            let end = self.text.borrow().clone();
            round_trip(
                &start,
                &end,
                || self.undo(),
                || self.redo(),
                || self.text.borrow().clone(),
            )
        }
    }

    #[test]
    fn a_complete_undo_round_trips() {
        let r = Toy::new(false).run();
        assert!(r.ok(), "{r:?}");
        assert_eq!((r.undo_steps, r.redo_steps), (3, 3));
    }

    #[test]
    fn an_undo_that_leaves_something_behind_is_caught() {
        let r = Toy::new(true).run();
        assert!(!r.restores_start, "{r:?}");
        assert!(!r.ok());
    }
}
