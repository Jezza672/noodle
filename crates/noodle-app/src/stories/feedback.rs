//! Group F of the scorecard: do I know it worked?
//!
//! Three questions, each answered from a run of the route:
//!
//! - **Time to hear.** The route listens to the virtual output where a user
//!   would (`Probe::hear` puts a marker in the log). The first listen that
//!   caught sound is the first moment the user could hear the work, and the
//!   actions and KLM seconds before it are the cost of getting there. A
//!   route that never listens is reported as such: it is the route's
//!   author's job to listen as soon as there is something to hear.
//! - **Live editing.** The route is run again with the transport playing.
//!   Each step that stops the transport without being asked, and a step or
//!   done check that fails, is listed.
//! - **State visibility.** Steps the author tagged with `Handle::unseen`:
//!   their result cannot be seen without opening another view.

use super::effort;
use crate::testkit::{AUDIBLE, Action, ActionKind};

/// When the route first had something to hear.
#[derive(Clone, Debug, PartialEq)]
pub enum Hearing {
    /// The route never listens.
    NotListened,
    /// The route listened, and every time it was silent.
    Silent,
    /// The first listen that caught sound came after this much work.
    At {
        actions: usize,
        klm_seconds: f64,
        peak: f32,
    },
}

pub fn time_to_hear(log: &[Action]) -> Hearing {
    let mut listened = false;
    for (i, a) in log.iter().enumerate() {
        if let ActionKind::Heard(peak) = a.kind {
            listened = true;
            if peak > AUDIBLE {
                let before = effort::score(&log[..i]);
                return Hearing::At {
                    actions: before.actions,
                    klm_seconds: before.klm_seconds,
                    peak,
                };
            }
        }
    }
    if listened {
        Hearing::Silent
    } else {
        Hearing::NotListened
    }
}

/// The steps whose result cannot be seen without opening another view, by
/// the author's own name for the tag.
pub fn unseen(log: &[Action]) -> Vec<String> {
    log.iter()
        .filter_map(|a| match &a.kind {
            ActionKind::Unseen(name) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// A route run again with the transport playing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveEdit {
    /// The step each unasked stop of the transport happened in.
    pub stops: Vec<String>,
    /// Why the run failed, if it did, and in which step.
    pub failed: Option<String>,
}

impl LiveEdit {
    pub fn ok(&self) -> bool {
        self.stops.is_empty() && self.failed.is_none()
    }

    pub fn summary(&self) -> String {
        if self.ok() {
            return "works".into();
        }
        let mut parts = Vec::new();
        if !self.stops.is_empty() {
            let mut stops = self.stops.clone();
            stops.dedup();
            parts.push(format!("stops in {}", stops.join(", ")));
        }
        if let Some(why) = &self.failed {
            parts.push(format!("FAILS {why}"));
        }
        parts.join("; ")
    }
}

/// What a route's run says about feedback.
#[derive(Clone, Debug, PartialEq)]
pub struct Feedback {
    pub hearing: Hearing,
    pub unseen: Vec<String>,
    /// How many top-level steps the route has, for the share unseen.
    pub steps: usize,
    pub live: LiveEdit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, PointerButton, Pos2};

    fn act(kind: ActionKind) -> Action {
        Action {
            kind,
            modifiers: Modifiers::NONE,
            start: Pos2::ZERO,
            end: Pos2::ZERO,
            start_hit: None,
            end_hit: None,
            start_view: "node editor".into(),
            end_view: "node editor".into(),
            time: 0.0,
        }
    }

    fn click() -> Action {
        act(ActionKind::Click(PointerButton::Primary))
    }

    #[test]
    fn the_cost_is_the_work_before_the_first_listen_that_caught_sound() {
        let log = [
            click(),
            click(),
            act(ActionKind::Heard(0.0)),
            click(),
            act(ActionKind::Heard(0.4)),
            click(),
        ];
        let Hearing::At {
            actions,
            klm_seconds,
            peak,
        } = time_to_hear(&log)
        else {
            panic!("it was heard");
        };
        assert_eq!(actions, 3);
        // Three clicks: 3 x (P + 2B), nothing else.
        assert!(
            (klm_seconds - 3.0 * (1.1 + 0.2)).abs() < 1e-9,
            "{klm_seconds}"
        );
        assert!((peak - 0.4).abs() < 1e-6);
    }

    #[test]
    fn silence_and_never_listening_are_told_apart() {
        assert_eq!(time_to_hear(&[click()]), Hearing::NotListened);
        assert_eq!(
            time_to_hear(&[click(), act(ActionKind::Heard(0.0))]),
            Hearing::Silent
        );
    }

    #[test]
    fn tagged_steps_are_listed_in_order() {
        let log = [
            act(ActionKind::Unseen("the lane".into())),
            click(),
            act(ActionKind::Unseen("the take".into())),
        ];
        assert_eq!(unseen(&log), ["the lane", "the take"]);
    }

    #[test]
    fn a_live_edit_summary_names_the_steps() {
        assert_eq!(LiveEdit::default().summary(), "works");
        let live = LiveEdit {
            stops: vec!["a".into(), "a".into(), "b".into()],
            failed: Some("in c: x".into()),
        };
        assert!(!live.ok());
        assert_eq!(live.summary(), "stops in a, b; FAILS in c: x");
    }
}
