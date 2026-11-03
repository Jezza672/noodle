//! Group A of the scorecard: how much does the user have to do?
//!
//! Turns an action log into counts and a Keystroke-Level Model time. The
//! operator costs are the standard ones. The rules that place the operators
//! are below, and are the only judgement in the number:
//!
//! - A click is a point (P) and two button events (B, down and up); a double
//!   click is P and four B; a drag is P, B, P, B (press, move, release); a
//!   wheel scroll is P and one K; a file drop is P and two B.
//! - A key chord is one K, however many modifiers it holds. Typed text is one
//!   K per character.
//! - Code (a Script node's program) is typed like text, one K per
//!   character, but it also takes an M for every statement, since each one
//!   has to be worked out, not just typed. Writing code also needs the user
//!   to know the script language, which no time captures, so the statements
//!   are counted on their own (`code_statements`) and the scorecard shows
//!   them in a Code column.
//! - A move of a control on a MIDI control surface is a point (P): reaching
//!   for the knob or fader. It has no button events.
//! - Homing (H) is added whenever the hand changes between the pointer
//!   (click, drag, wheel, drop), the keyboard (chord, text) and the control
//!   surface.
//! - A mental operator (M) goes before each text entry, each view switch (an
//!   action that starts in a different view than the last one ended in), and
//!   each top-level step the route marks.
//!
//! Waiting on the app is reported on its own and is not part of the KLM time,
//! which is the expert's own time.

use crate::testkit::{Action, ActionKind};

/// Seconds per keystroke.
pub const K: f64 = 0.2;
/// Seconds to point at a target.
pub const P: f64 = 1.1;
/// Seconds per mouse button event.
pub const B: f64 = 0.1;
/// Seconds to move a hand between mouse and keyboard.
pub const H: f64 = 0.4;
/// Seconds of mental preparation.
pub const M: f64 = 1.35;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts {
    pub clicks: usize,
    pub double_clicks: usize,
    pub drags: usize,
    pub keys: usize,
    pub wheels: usize,
    pub drops: usize,
    pub texts: usize,
    pub controls: usize,
    /// Programs typed into Script nodes.
    pub code: usize,
}

/// How many of each KLM operator the route needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Operators {
    pub k: usize,
    pub p: usize,
    pub b: usize,
    pub h: usize,
    pub m: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Effort {
    pub counts: Counts,
    /// Every action, of every kind.
    pub actions: usize,
    pub steps: usize,
    pub view_switches: usize,
    /// Times an action started in a dialog, menu or popup that the last one
    /// had not ended in.
    pub dialogs_opened: usize,
    pub operators: Operators,
    pub klm_seconds: f64,
    pub wait_seconds: f64,
    /// Statements in the code the user had to write.
    pub code_statements: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Hand {
    Pointer,
    Keyboard,
    Surface,
}

/// How many statements a program has: the pieces between line ends, `;`
/// and braces that hold anything. A declaration counts as one.
pub fn statements(code: &str) -> usize {
    code.split(['\n', ';', '{', '}'])
        .map(|s| s.split("//").next().unwrap_or("").trim())
        .filter(|s| !s.is_empty())
        .count()
}

pub fn score(log: &[Action]) -> Effort {
    let mut e = Effort::default();
    let mut hand: Option<Hand> = None;
    // Where the last real action ended.
    let mut view: Option<&str> = None;
    for a in log {
        let (this_hand, ops) = match &a.kind {
            ActionKind::Recreate(_) | ActionKind::Heard(_) | ActionKind::Unseen(_) => continue,
            ActionKind::Step(_) => {
                e.steps += 1;
                e.operators.m += 1;
                continue;
            }
            ActionKind::Wait(secs) => {
                e.wait_seconds += secs;
                continue;
            }
            ActionKind::Click(_) => {
                e.counts.clicks += 1;
                (Hand::Pointer, (0, 1, 2))
            }
            ActionKind::DoubleClick => {
                e.counts.double_clicks += 1;
                (Hand::Pointer, (0, 1, 4))
            }
            ActionKind::Drag(_) => {
                e.counts.drags += 1;
                (Hand::Pointer, (0, 2, 2))
            }
            ActionKind::Wheel => {
                e.counts.wheels += 1;
                (Hand::Pointer, (1, 1, 0))
            }
            ActionKind::Drop { .. } => {
                e.counts.drops += 1;
                (Hand::Pointer, (0, 1, 2))
            }
            ActionKind::Key(_) => {
                e.counts.keys += 1;
                (Hand::Keyboard, (1, 0, 0))
            }
            ActionKind::Text(text) => {
                e.counts.texts += 1;
                e.operators.m += 1;
                (Hand::Keyboard, (text.chars().count(), 0, 0))
            }
            ActionKind::Code(code) => {
                let statements = statements(code);
                e.counts.code += 1;
                e.code_statements += statements;
                e.operators.m += statements;
                (Hand::Keyboard, (code.chars().count(), 0, 0))
            }
            ActionKind::Control => {
                e.counts.controls += 1;
                (Hand::Surface, (0, 1, 0))
            }
        };
        e.actions += 1;
        e.operators.k += ops.0;
        e.operators.p += ops.1;
        e.operators.b += ops.2;
        if hand.is_some_and(|prev| prev != this_hand) {
            e.operators.h += 1;
        }
        hand = Some(this_hand);
        if let Some(prev) = view
            && !prev.is_empty()
            && !a.start_view.is_empty()
            && prev != a.start_view
        {
            e.view_switches += 1;
            e.operators.m += 1;
            if a.start_view == "overlay" {
                e.dialogs_opened += 1;
            }
        }
        view = Some(&a.end_view);
    }
    let o = &e.operators;
    e.klm_seconds =
        o.k as f64 * K + o.p as f64 * P + o.b as f64 * B + o.h as f64 * H + o.m as f64 * M;
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers, PointerButton, Pos2};

    fn act(kind: ActionKind, view: &str) -> Action {
        Action {
            kind,
            modifiers: Modifiers::NONE,
            start: Pos2::ZERO,
            end: Pos2::ZERO,
            start_hit: None,
            end_hit: None,
            start_view: view.into(),
            end_view: view.into(),
            time: 0.0,
        }
    }

    fn click(view: &str) -> Action {
        act(ActionKind::Click(PointerButton::Primary), view)
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn an_empty_log_costs_nothing() {
        assert_eq!(score(&[]), Effort::default());
    }

    #[test]
    fn three_clicks_in_one_view() {
        let e = score(&[click("a"), click("a"), click("a")]);
        assert_eq!(e.actions, 3);
        assert_eq!(e.counts.clicks, 3);
        assert_eq!(e.view_switches, 0);
        // 3 x (P + 2B) = 3 x 1.3
        assert_eq!(
            e.operators,
            Operators {
                k: 0,
                p: 3,
                b: 6,
                h: 0,
                m: 0
            }
        );
        assert!(near(e.klm_seconds, 3.9), "{}", e.klm_seconds);
    }

    #[test]
    fn a_drag_is_two_points_and_two_buttons() {
        let e = score(&[act(ActionKind::Drag(PointerButton::Primary), "a")]);
        assert_eq!(
            e.operators,
            Operators {
                k: 0,
                p: 2,
                b: 2,
                h: 0,
                m: 0
            }
        );
        assert!(near(e.klm_seconds, 2.4));
    }

    #[test]
    fn a_chord_is_one_key_and_changing_hands_costs_homing() {
        // click, chord, click: two hand changes.
        let e = score(&[click("a"), act(ActionKind::Key(Key::A), "a"), click("a")]);
        assert_eq!(
            e.operators,
            Operators {
                k: 1,
                p: 2,
                b: 4,
                h: 2,
                m: 0
            }
        );
        // 0.2 + 2.2 + 0.4 + 0.8
        assert!(near(e.klm_seconds, 3.6), "{}", e.klm_seconds);
    }

    #[test]
    fn text_costs_a_key_per_character_and_a_mental_operator() {
        let e = score(&[act(ActionKind::Text("sine".into()), "a")]);
        assert_eq!(e.counts.texts, 1);
        assert_eq!(
            e.operators,
            Operators {
                k: 4,
                p: 0,
                b: 0,
                h: 0,
                m: 1
            }
        );
        assert!(near(e.klm_seconds, 0.8 + 1.35));
    }

    #[test]
    fn a_view_switch_and_a_step_each_get_a_mental_operator() {
        let log = [
            act(ActionKind::Step("one".into()), ""),
            click("node editor"),
            click("node editor"),
            // Starts in the timeline after ending in the node editor.
            click("timeline"),
            // A menu opening.
            click("overlay"),
            act(ActionKind::Step("two".into()), ""),
            click("timeline"),
        ];
        let e = score(&log);
        assert_eq!(e.actions, 5);
        assert_eq!(e.steps, 2);
        // node editor -> timeline, timeline -> overlay, overlay -> timeline
        assert_eq!(e.view_switches, 3);
        assert_eq!(e.dialogs_opened, 1);
        assert_eq!(e.operators.m, 2 + 3);
    }

    #[test]
    fn waiting_is_reported_apart_from_the_klm_time() {
        let e = score(&[
            click("a"),
            act(ActionKind::Wait(2.5), ""),
            act(ActionKind::Wait(1.0), ""),
        ]);
        assert!(near(e.wait_seconds, 3.5));
        assert_eq!(e.actions, 1);
        assert!(near(e.klm_seconds, 1.3));
    }

    #[test]
    fn wheels_and_drops() {
        let e = score(&[
            act(ActionKind::Wheel, "a"),
            act(ActionKind::Drop { files: 3 }, "a"),
            act(ActionKind::DoubleClick, "a"),
        ]);
        assert_eq!(
            e.operators,
            Operators {
                k: 1,
                p: 3,
                b: 6,
                h: 0,
                m: 0
            }
        );
        assert_eq!(e.counts.wheels + e.counts.drops + e.counts.double_clicks, 3);
    }

    #[test]
    fn code_costs_a_thought_per_statement_on_top_of_its_typing() {
        let code = "events in n; events out o\nif n_on { note_on(o, n_key, 1) } // pass";
        assert_eq!(statements(code), 4);
        let typed = score(&[act(ActionKind::Text(code.into()), "a")]);
        let written = score(&[act(ActionKind::Code(code.into()), "a")]);
        assert_eq!(written.operators.k, typed.operators.k);
        assert_eq!(written.operators.m, 4);
        assert_eq!(typed.operators.m, 1);
        assert_eq!(written.code_statements, 4);
        assert_eq!(written.counts.code, 1);
        assert!(written.klm_seconds > typed.klm_seconds + 2.0 * M);
    }

    #[test]
    fn a_control_surface_move_is_a_reach_with_its_own_hand() {
        let e = score(&[
            act(ActionKind::Click(PointerButton::Primary), "a"),
            act(ActionKind::Control, ""),
        ]);
        assert_eq!(e.counts.controls, 1);
        assert_eq!((e.operators.p, e.operators.h), (2, 1));
    }
}
