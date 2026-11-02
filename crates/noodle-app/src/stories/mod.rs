//! The usability scorecard's routes: scripted user journeys that reach a
//! story's goal through the real UI, so their cost can be measured.
//! Design: `docs/architecture/usability.md`. Stories: `docs/usability/stories.md`.
//!
//! A [`Route`] has a story ID, a tag, a fixture to start from, optionally a
//! size, the steps and a done check. The steps get a [`Handle`], which can
//! only send input events; the done check gets a [`Probe`], which can only
//! read. A [`LateChange`] starts from the end state of a route.
//!
//! Every route is run as an ordinary test, so a UI change that breaks a
//! route fails CI. With `NOODLE_SCORECARD_DIR` set, the run also writes the
//! scorecard there (CI uploads it).
//!
//! **A PR that changes how a story is done updates its route in the same
//! PR.**

mod effort;
mod feedback;
mod handle;
mod registry;
mod repetition;
mod report;
mod rework;
mod robustness;
mod routes;
mod surface;
mod vocabulary;

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use egui::Vec2;
use egui_kittest::Harness;
use noodle_core::Project;

use crate::app::App;
use crate::session::{Nodes, Saved, Session};
use crate::testkit;
use feedback::{Feedback, LiveEdit};
use handle::{AppHarness, Handle, Probe};
use report::{LateScore, RouteScore};
use robustness::{Reopen, Robustness};

/// How a newcomer or an expert does the thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    /// What a newcomer would find.
    Default,
    /// The fastest known way.
    Expert,
    Keyboard,
    Scripted,
}

impl Tag {
    fn name(self) -> &'static str {
        match self {
            Tag::Default => "default",
            Tag::Expert => "expert",
            Tag::Keyboard => "keyboard",
            Tag::Scripted => "scripted",
        }
    }
}

/// Builds the app a route starts from. It gets a scratch directory that
/// lives as long as the run.
pub type Fixture = fn(&Path) -> App;
pub type Steps = fn(&mut Handle, usize);
pub type Check = fn(&mut Probe, usize) -> Result<(), String>;

pub struct Route {
    /// A story ID from the registry. `S00` is the framework's own check.
    pub story: &'static str,
    pub tag: Tag,
    pub name: &'static str,
    pub fixture: Fixture,
    /// The default size, for a route that scales (samples, tracks, takes).
    /// Steps and check get 1 when there is none.
    pub size: Option<usize>,
    pub steps: Steps,
    pub done: Check,
    pub late: &'static [LateChange],
}

/// A change of mind after the route is finished. It starts from the route's
/// end state, with its own steps and its own done check.
pub struct LateChange {
    pub name: &'static str,
    pub steps: Steps,
    pub done: Check,
    /// The parts of the project the change must leave alone. The runner
    /// shows each before and after; one that differs is reported as lost.
    pub keep: &'static [Keep],
}

/// A named part of the project, as text, for telling whether a late change
/// touched it: one track's automation, the comp choices, a plugin's state.
pub struct Keep {
    pub name: &'static str,
    pub of: fn(&Project) -> String,
}

pub struct Outcome {
    pub score: RouteScore,
    /// Why the route's done check failed, if it did.
    pub failure: Option<String>,
}

fn window(app: App) -> AppHarness {
    // While audio plays the app keeps repainting, so settle a few frames
    // rather than `run` until it stops.
    let mut h = Harness::builder()
        .with_size(Vec2::new(1280.0, 800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

/// Runs `route` at `size` (its own default if `None`), then each of its
/// late changes from where it ended. A route with a size is run again at
/// twice that size, to see what each extra item costs.
pub fn run(route: &Route, size: Option<usize>) -> Outcome {
    let size = size.or(route.size);
    let mut played = play(route, size.unwrap_or(1), true);
    let mut per_item = None;
    if let Some(n) = size {
        let more = play(route, n * 2, false);
        if let Some(why) = more.failure {
            played
                .failure
                .get_or_insert(format!("at size {}: {why}", n * 2));
        }
        per_item = repetition::per_item((n, &played.effort), (n * 2, &more.effort));
    }
    Outcome {
        score: RouteScore {
            story: route.story.into(),
            tag: route.tag.name().into(),
            name: route.name.into(),
            size,
            passed: played.failure.is_none(),
            effort: played.effort,
            per_item,
            rework: played.rework,
            round_trip: played.round_trip,
            late: played.late,
            surfaces: played.surfaces,
            feedback: played.feedback,
            robustness: played.robustness,
        },
        failure: played.failure,
    }
}

struct Played {
    effort: effort::Effort,
    rework: rework::Rework,
    round_trip: rework::RoundTrip,
    late: Vec<LateScore>,
    surfaces: surface::Surfaces,
    feedback: Feedback,
    robustness: Robustness,
    failure: Option<String>,
}

/// The message of a caught panic.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".into())
}

/// Runs `route` again with the transport playing, as a user editing while
/// listening would. The route's own Space presses are left out, and the
/// transport is stopped before the done check, which looks at the result.
fn live_edit(route: &Route, n: usize) -> LiveEdit {
    let dir = tempfile::tempdir().expect("a scratch directory");
    crate::prefs::init(dir.path().join("prefs.ron"));
    let mut h = window((route.fixture)(dir.path()));
    h.state_mut().session_mut().play();
    h.run_steps(3);
    // While the transport runs the UI never stops repainting.
    let _playing = testkit::playing_mode();
    let mut handle = Handle::new(&mut h).live();
    let ran = catch_unwind(AssertUnwindSafe(|| (route.steps)(&mut handle, n)));
    let stops = handle.stops();
    let step = handle.step_name().to_string();
    drop(handle);
    let mut live = LiveEdit {
        stops,
        failed: None,
    };
    if let Err(payload) = ran {
        let step = if step.is_empty() {
            "the first step"
        } else {
            &step
        };
        live.failed = Some(format!("in {step}: {}", panic_message(payload)));
        return live;
    }
    h.state_mut().session_mut().set_transport_running(false);
    h.run_steps(3);
    let checked = catch_unwind(AssertUnwindSafe(|| {
        (route.done)(&mut Probe::new(&mut h), n)
    }));
    match checked {
        Ok(Ok(())) => {}
        Ok(Err(why)) => live.failed = Some(format!("at the end: {why}")),
        Err(payload) => live.failed = Some(format!("at the end: {}", panic_message(payload))),
    }
    live
}

/// Saves the project, opens the file again, and checks that it reads back
/// the same and that the route's done check still passes on it.
fn reopen(h: &mut AppHarness, route: &Route, n: usize) -> Reopen {
    let session = h.state_mut().session_mut();
    let saved = session.project_ron();
    let playing = session.transport_running();
    if !matches!(session.save(), Saved::Yes) {
        return Reopen::CannotSave;
    }
    let Some(path) = session.path().map(Path::to_owned) else {
        return Reopen::CannotSave;
    };
    let mut again = match Session::open(Nodes::all(), &path) {
        Ok(session) => session,
        Err(error) => return Reopen::Differs(format!("(it does not open: {error})")),
    };
    again.set_audio_config(testkit::virtual_audio());
    if let Some(difference) = robustness::first_difference(&saved, &again.project_ron()) {
        return Reopen::Differs(difference);
    }
    // A done check that listens needs the transport as it was left.
    if playing {
        again.play();
    }
    let mut h = window(App::new(again));
    let checked = catch_unwind(AssertUnwindSafe(|| {
        (route.done)(&mut Probe::new(&mut h), n)
    }));
    match checked {
        Ok(Ok(())) => Reopen::Same,
        Ok(Err(why)) => Reopen::DoneFails(why),
        Err(payload) => Reopen::DoneFails(panic_message(payload)),
    }
}

fn play(route: &Route, n: usize, with_late: bool) -> Played {
    let dir = tempfile::tempdir().expect("a scratch directory");
    crate::prefs::init(dir.path().join("prefs.ron"));
    let mut h = window((route.fixture)(dir.path()));
    let rec = testkit::record();
    let start = h.state().session().project_ron();

    let mut handle = Handle::new(&mut h);
    (route.steps)(&mut handle, n);
    let changes = handle.finish();
    let log = rec.log();
    let mut failure = (route.done)(&mut Probe::new(&mut h), n).err();
    let base = effort::score(&log);
    let rework = rework::score(&log, &changes);
    let surfaces = surface::score(&log);
    // The done check may listen too, so read the log again.
    let after = rec.log();
    let mut feedback = Feedback {
        hearing: feedback::time_to_hear(&after),
        unseen: feedback::unseen(&after),
        steps: base.steps,
        live: LiveEdit::default(),
    };

    // Undo everything and redo it, before the late changes start from the
    // end state.
    let end = h.state().session().project_ron();
    let round_trip = {
        let h = RefCell::new(&mut h);
        rework::round_trip(
            &start,
            &end,
            || {
                let mut h = h.borrow_mut();
                let session = h.state_mut().session_mut();
                let did = session.can_undo();
                session.undo();
                did
            },
            || {
                let mut h = h.borrow_mut();
                let session = h.state_mut().session_mut();
                let did = session.can_redo();
                session.redo();
                did
            },
            || h.borrow().state().session().project_ron(),
        )
    };
    h.run_steps(3);
    // Late changes start from the end state, so they cannot be scored from
    // a wrong one.
    let with_late = with_late && round_trip.ok();
    if !round_trip.ok() {
        failure.get_or_insert(format!("the undo round trip fails: {round_trip:?}"));
    }

    // Save and reopen the end state, before the late changes move it.
    let reopened = reopen(&mut h, route, n);
    if !reopened.ok() {
        failure.get_or_insert(format!("save and reopen: {}", reopened.summary()));
    }

    let mut late = Vec::new();
    let mut seen = after.len();
    for change in route.late.iter().filter(|_| with_late) {
        let before: Vec<String> = change
            .keep
            .iter()
            .map(|k| (k.of)(h.state().session().project()))
            .collect();
        (change.steps)(&mut Handle::new(&mut h), n);
        let all = rec.log();
        let mine = effort::score(&all[seen..]);
        seen = all.len();
        let lost: Vec<String> = change
            .keep
            .iter()
            .zip(&before)
            .filter(|(k, was)| (k.of)(h.state().session().project()) != **was)
            .map(|(k, _)| k.name.to_string())
            .collect();
        let result = (change.done)(&mut Probe::new(&mut h), n);
        if let Err(why) = &result {
            failure.get_or_insert(format!("late change {:?}: {why}", change.name));
        }
        late.push(LateScore {
            name: change.name.into(),
            passed: result.is_ok(),
            cost: (base.actions > 0).then(|| mine.actions as f64 / base.actions as f64),
            klm_ratio: (base.klm_seconds > 0.0).then(|| mine.klm_seconds / base.klm_seconds),
            lost,
            effort: mine,
        });
    }
    let robustness = Robustness {
        underruns: h.state().session().underruns(),
        reopen: reopened,
    };
    // The live run is a pass of its own, so only the main one pays for it.
    if with_late {
        // Reported, not asserted: a step that cannot be done while the
        // transport plays is a finding about the app, not a broken route.
        feedback.live = live_edit(route, n);
    }
    Played {
        effort: base,
        rework,
        round_trip,
        late,
        surfaces,
        feedback,
        robustness,
        failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every route runs, reaches its goal, and leaves a scorecard.
    #[test]
    fn every_route_reaches_its_goal() {
        let mut scores = Vec::new();
        let mut failures = Vec::new();
        for route in routes::all() {
            let out = run(&route, None);
            if let Some(why) = out.failure {
                failures.push(format!(
                    "{} {} {}: {why}",
                    route.story,
                    route.tag.name(),
                    route.name
                ));
            }
            scores.push(out.score);
        }
        if let Some(dir) = std::env::var_os("NOODLE_SCORECARD_DIR") {
            let words = vocabulary::check(
                &Nodes::all().registry,
                &noodle_core::library::Library::in_memory(),
                vocabulary::TERMS,
            );
            report::write(Path::new(&dir), &scores, &words).expect("the scorecard is written");
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    // --- the scorers, on small routes of their own ---

    use egui::{Key, Modifiers, Pos2};

    use super::routes::{add_node, empty_project};

    const SINE: &str = "noodle.osc.sine";

    fn sines(p: &Project) -> String {
        let ids: Vec<String> = p
            .graph()
            .nodes()
            .filter(|(_, n)| n.type_id == SINE)
            .map(|(id, _)| format!("{id:?}"))
            .collect();
        ids.join(",")
    }

    fn count_sines(p: &mut Probe, at_least: usize) -> Result<(), String> {
        let n = p
            .project()
            .graph()
            .nodes()
            .filter(|(_, n)| n.type_id == SINE)
            .count();
        (n >= at_least)
            .then_some(())
            .ok_or(format!("{n} sines, wanted {at_least}"))
    }

    fn nothing(_: &mut Handle, _: usize) {}
    fn fine(_: &mut Probe, _: usize) -> Result<(), String> {
        Ok(())
    }

    fn route(steps: Steps, done: Check, size: Option<usize>, late: &'static [LateChange]) -> Route {
        Route {
            story: "S00",
            tag: Tag::Default,
            name: "test",
            fixture: empty_project,
            size,
            steps,
            done,
            late,
        }
    }

    fn by_hand(h: &mut Handle, n: usize) {
        for i in 0..n {
            add_node(h, Pos2::new(150.0 + 120.0 * i as f32, 300.0), "sine");
        }
    }

    fn all_by_hand(p: &mut Probe, n: usize) -> Result<(), String> {
        count_sines(p, n)
    }

    /// One node, then select all and duplicate: the same however many are
    /// wanted.
    fn in_one_gesture(h: &mut Handle, _n: usize) {
        let at = Pos2::new(300.0, 300.0);
        add_node(h, at, "sine");
        h.press(at, Modifiers::NONE, Key::A);
        h.press(at, Modifiers::SHIFT, Key::D);
    }

    #[test]
    fn a_chore_done_by_hand_costs_its_actions_per_item() {
        let out = run(&route(by_hand, all_by_hand, Some(3), &[]), None);
        assert_eq!(out.failure, None);
        let p = out
            .score
            .per_item
            .expect("a sized route has a cost per item");
        assert_eq!((p.small, p.large), (3, 6));
        // Shift+A, the query, Enter.
        assert!((p.actions - 3.0).abs() < 1e-9, "{p:?}");
        assert!(p.klm_seconds > 1.0);
    }

    #[test]
    fn one_gesture_for_any_number_costs_about_nothing_per_item() {
        let out = run(&route(in_one_gesture, fine, Some(3), &[]), None);
        assert_eq!(out.failure, None);
        let p = out.score.per_item.unwrap();
        assert!(p.actions.abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn a_route_without_a_size_has_no_cost_per_item() {
        let out = run(&route(in_one_gesture, fine, None, &[]), None);
        assert!(out.score.per_item.is_none());
    }

    fn delete_and_recreate(h: &mut Handle, _n: usize) {
        let at = Pos2::new(300.0, 300.0);
        h.step("add");
        add_node(h, at, "sine");
        h.step("throw it away");
        h.press(at, Modifiers::NONE, Key::A);
        h.press(at, Modifiers::NONE, Key::Delete);
        h.step("add it again");
        h.recreating("the sine");
        add_node(h, at, "sine");
    }

    fn one_sine(p: &mut Probe, _n: usize) -> Result<(), String> {
        count_sines(p, 1)
    }

    #[test]
    fn a_delete_and_recreate_is_flagged() {
        let out = run(&route(delete_and_recreate, one_sine, None, &[]), None);
        assert_eq!(out.failure, None);
        let r = out.score.rework;
        assert_eq!(r.detected, vec![SINE.to_string()]);
        assert_eq!(r.marked, 1);
        assert_eq!(r.marked_actions, 3);
        assert_eq!(r.undos, 0);
    }

    #[test]
    fn a_route_that_only_adds_has_no_rework() {
        let out = run(&route(by_hand, all_by_hand, Some(2), &[]), None);
        assert!(!out.score.rework.any());
    }

    fn mistake_and_undo(h: &mut Handle, _n: usize) {
        let at = Pos2::new(300.0, 300.0);
        add_node(h, at, "sine");
        h.press(at, Modifiers::NONE, Key::A);
        h.press(at, Modifiers::NONE, Key::Delete);
        h.press(at, Modifiers::COMMAND, Key::Z);
    }

    #[test]
    fn undos_are_counted_and_do_not_look_like_a_recreation() {
        let out = run(&route(mistake_and_undo, one_sine, None, &[]), None);
        assert_eq!(out.failure, None);
        assert_eq!(out.score.rework.undos, 1);
        assert!(out.score.rework.detected.is_empty());
    }

    #[test]
    fn the_real_undo_stack_round_trips() {
        let out = run(&route(delete_and_recreate, one_sine, None, &[]), None);
        let t = out.score.round_trip;
        assert!(t.ok(), "{t:?}");
        assert!(t.undo_steps >= 3 && t.undo_steps == t.redo_steps, "{t:?}");
    }

    const KEEP_SINES: &[Keep] = &[Keep {
        name: "sines",
        of: sines,
    }];

    fn two_nodes(h: &mut Handle, _n: usize) {
        add_node(h, Pos2::new(200.0, 300.0), "sine");
        add_node(h, Pos2::new(600.0, 300.0), "output");
    }

    /// Selects everything, sines included, and deletes it.
    fn wipe(h: &mut Handle, _n: usize) {
        let at = Pos2::new(640.0, 600.0);
        h.press(at, Modifiers::NONE, Key::A);
        h.press(at, Modifiers::NONE, Key::Delete);
    }

    /// Adds an output and leaves the sines alone.
    fn add_another(h: &mut Handle, _n: usize) {
        add_node(h, Pos2::new(900.0, 500.0), "output");
    }

    const LATE: &[LateChange] = &[LateChange {
        name: "wipe the lot",
        steps: wipe,
        done: fine,
        keep: KEEP_SINES,
    }];
    const LATE_CAREFUL: &[LateChange] = &[LateChange {
        name: "add an output",
        steps: add_another,
        done: fine,
        keep: KEEP_SINES,
    }];

    #[test]
    fn a_late_change_that_wipes_what_it_must_keep_is_lossy() {
        let out = run(&route(two_nodes, one_sine, None, LATE), None);
        assert_eq!(out.failure, None);
        let late = &out.score.late[0];
        assert_eq!(late.lost, vec!["sines".to_string()]);
        assert!(late.cost.unwrap() > 0.0 && late.klm_ratio.unwrap() > 0.0);
    }

    #[test]
    fn a_late_change_that_leaves_it_alone_is_not() {
        let out = run(&route(two_nodes, one_sine, None, LATE_CAREFUL), None);
        assert_eq!(out.failure, None);
        let late = &out.score.late[0];
        assert!(late.lost.is_empty(), "{:?}", late.lost);
        assert!(late.passed);
        // Two actions-worth of work against the route's.
        assert!(late.cost.unwrap() < 1.5);
    }

    // --- feedback, surfaces and robustness ---

    #[test]
    fn a_route_that_listens_reports_what_it_took_to_hear() {
        let smoke = routes::all()
            .into_iter()
            .find(|r| r.story == "S00")
            .expect("the framework route");
        let out = run(&smoke, None);
        assert_eq!(out.failure, None);
        let feedback::Hearing::At { actions, .. } = out.score.feedback.hearing else {
            panic!("{:?}", out.score.feedback.hearing);
        };
        // Everything it did before it listened.
        assert_eq!(actions, out.score.effort.actions);
    }

    #[test]
    fn a_route_that_never_listens_says_so() {
        let out = run(&route(by_hand, all_by_hand, Some(1), &[]), None);
        assert_eq!(out.score.feedback.hearing, feedback::Hearing::NotListened);
    }

    fn tags_a_step(h: &mut Handle, _n: usize) {
        h.step("make it");
        add_node(h, Pos2::new(300.0, 300.0), "sine");
        h.step("look at it");
        h.unseen("the result is in another view");
    }

    #[test]
    fn steps_tagged_unseen_are_listed_and_the_search_is_shift_a_then_typing() {
        let out = run(&route(tags_a_step, one_sine, None, &[]), None);
        assert_eq!(out.failure, None);
        assert_eq!(out.score.feedback.unseen, ["the result is in another view"]);
        assert_eq!(out.score.feedback.steps, 2);
        let s = &out.score.surfaces;
        // Shift+A is hidden; the typing and Enter are the search.
        assert_eq!((s.total, s.hidden_count()), (3, 1));
        assert_eq!(s.count(surface::Surface::Search), 2);
    }

    /// Clicks the stop button, when there is one to click (the transport
    /// plays only in a live run).
    fn stops_the_transport(h: &mut Handle, _n: usize) {
        h.step("stop it");
        if h.probe().transport_running() {
            let stop = h.find("⏹");
            h.click(stop, Modifiers::NONE);
        }
        h.step("carry on");
        add_node(h, Pos2::new(300.0, 300.0), "sine");
    }

    #[test]
    fn a_step_that_stops_the_transport_is_named_in_the_live_run() {
        let out = run(&route(stops_the_transport, one_sine, None, &[]), None);
        assert_eq!(out.failure, None);
        let live = &out.score.feedback.live;
        assert_eq!(live.stops, ["stop it"]);
        assert_eq!(live.failed, None);
    }

    #[test]
    fn a_step_that_cannot_be_done_while_playing_fails_the_live_run_only() {
        fn cannot(h: &mut Handle, _n: usize) {
            h.step("find the button");
            if h.probe().transport_running() {
                h.find("a label nothing has");
            }
            add_node(h, Pos2::new(300.0, 300.0), "sine");
        }
        let out = run(&route(cannot, one_sine, None, &[]), None);
        // The route itself is fine.
        assert_eq!(out.failure, None);
        let failed = out.score.feedback.live.failed.expect("fails live");
        assert!(failed.starts_with("in find the button:"), "{failed}");
    }

    #[test]
    fn a_route_that_edits_while_playing_without_trouble_works_live() {
        let out = run(&route(by_hand, all_by_hand, None, &[]), None);
        assert!(
            out.score.feedback.live.ok(),
            "{:?}",
            out.score.feedback.live
        );
    }

    #[test]
    fn a_project_that_reads_back_the_same_passes_the_reopen_check() {
        let out = run(&route(by_hand, all_by_hand, Some(2), &[]), None);
        assert_eq!(out.score.robustness.reopen, Reopen::Same);
    }

    #[allow(dead_code)]
    fn unused(_: Steps) {
        let _ = nothing;
    }
}
