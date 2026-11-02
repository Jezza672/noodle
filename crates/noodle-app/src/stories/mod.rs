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
mod handle;
mod registry;
mod report;
mod routes;

use std::path::Path;

use egui::Vec2;
use egui_kittest::Harness;

use crate::app::App;
use crate::testkit;
use handle::{AppHarness, Handle, Probe};
use report::{LateScore, RouteScore};

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
/// late changes from where it ended.
pub fn run(route: &Route, size: Option<usize>) -> Outcome {
    let size = size.or(route.size);
    let n = size.unwrap_or(1);
    let dir = tempfile::tempdir().expect("a scratch directory");
    crate::prefs::init(dir.path().join("prefs.ron"));
    let mut h = window((route.fixture)(dir.path()));
    let rec = testkit::record();

    (route.steps)(&mut Handle::new(&mut h), n);
    let log = rec.log();
    let mut failure = (route.done)(&mut Probe::new(&mut h), n).err();
    let base = effort::score(&log);

    let mut late = Vec::new();
    let mut seen = log.len();
    for change in route.late {
        (change.steps)(&mut Handle::new(&mut h), n);
        let all = rec.log();
        let mine = effort::score(&all[seen..]);
        seen = all.len();
        let result = (change.done)(&mut Probe::new(&mut h), n);
        if let Err(why) = &result {
            failure.get_or_insert(format!("late change {:?}: {why}", change.name));
        }
        late.push(LateScore {
            name: change.name.into(),
            passed: result.is_ok(),
            cost: (base.actions > 0).then(|| mine.actions as f64 / base.actions as f64),
            effort: mine,
        });
    }
    Outcome {
        score: RouteScore {
            story: route.story.into(),
            tag: route.tag.name().into(),
            name: route.name.into(),
            size,
            passed: failure.is_none(),
            effort: base,
            late,
        },
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
            report::write(Path::new(&dir), &scores).expect("the scorecard is written");
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
