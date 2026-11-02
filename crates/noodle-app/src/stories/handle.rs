//! What a route is allowed to touch.
//!
//! A route gets a [`Handle`] to act and a [`Probe`] to check. The harness is
//! a private field of each, in this module, so a route (which lives in a
//! sibling module) cannot reach the `App`, the `Session` or a `Command`: it
//! can only send the pointer and keyboard events in `testkit`, and read.
//! The project it reads is a `&Project`, whose mutators are crate-private to
//! `noodle-core`.

use std::collections::BTreeMap;
use std::time::Duration;

use egui::{Key, Modifiers, MouseWheelUnit, Pos2, Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use noodle_core::{ClipId, NodeId, Project};

use super::rework::Change;
use crate::app::App;
use crate::editor::socket_on_screen;
use crate::testkit;

pub type AppHarness = Harness<'static, App>;

/// The user's hands: pointer and keyboard, and nothing else.
pub struct Handle<'h> {
    h: &'h mut AppHarness,
    /// The kind of each node, as of the last action.
    kinds: BTreeMap<NodeId, String>,
    /// Which kinds of node the actions added and removed, in order.
    changes: Vec<Change>,
}

impl<'h> Handle<'h> {
    pub(super) fn new(h: &'h mut AppHarness) -> Self {
        let kinds = node_kinds(h);
        Self {
            h,
            kinds,
            changes: Vec::new(),
        }
    }

    /// What the actions added and removed, for spotting rework.
    pub(super) fn finish(self) -> Vec<Change> {
        self.changes
    }

    /// Notes which nodes the last action added or removed. An undo or redo
    /// is not counted: it is not the user throwing work away.
    fn watch(&mut self, quiet: bool) {
        let now = node_kinds(self.h);
        if !quiet {
            for (id, kind) in &self.kinds {
                if !now.contains_key(id) {
                    self.changes.push(Change::Removed(kind.clone()));
                }
            }
            for (id, kind) in &now {
                if !self.kinds.contains_key(id) {
                    self.changes.push(Change::Added(kind.clone()));
                }
            }
        }
        self.kinds = now;
    }

    /// The read-only view, for looking at the screen between actions.
    pub fn probe(&mut self) -> Probe<'_> {
        Probe { h: self.h }
    }

    pub fn click(&mut self, at: Pos2, modifiers: Modifiers) {
        testkit::click(self.h, at, modifiers);
        self.watch(false);
    }

    pub fn right_click(&mut self, at: Pos2) {
        testkit::click_with(self.h, egui::PointerButton::Secondary, Modifiers::NONE, at);
        self.watch(false);
    }

    pub fn double_click(&mut self, at: Pos2) {
        testkit::double_click(self.h, at);
        self.watch(false);
    }

    pub fn drag(&mut self, modifiers: Modifiers, path: &[Pos2]) {
        testkit::drag(self.h, modifiers, path);
        self.watch(false);
    }

    /// A drag while the transport runs (the UI cannot be waited on).
    pub fn drag_playing(&mut self, path: &[Pos2]) {
        testkit::drag_playing(self.h, path);
        self.watch(false);
    }

    pub fn drag_between(&mut self, modifiers: Modifiers, from: Pos2, to: Pos2) {
        testkit::drag_between(self.h, modifiers, from, to);
        self.watch(false);
    }

    /// A key chord with the pointer over `at`.
    pub fn press(&mut self, at: Pos2, modifiers: Modifiers, key: Key) {
        testkit::press(self.h, at, modifiers, key);
        self.watch(is_undo(modifiers, key));
    }

    /// [`Handle::press`] for when the transport runs: the app repaints
    /// continuously then, so the UI cannot be waited on to settle.
    pub fn press_playing(&mut self, at: Pos2, modifiers: Modifiers, key: Key) {
        testkit::press_playing(self.h, at, modifiers, key);
        self.watch(is_undo(modifiers, key));
    }

    pub fn wheel(&mut self, at: Pos2, unit: MouseWheelUnit, delta: Vec2) {
        testkit::wheel(self.h, at, unit, delta);
        self.watch(false);
    }

    pub fn drop_files(&mut self, at: Pos2, files: &[std::path::PathBuf]) {
        testkit::drop_files(self.h, at, files);
        self.watch(false);
    }

    pub fn type_text(&mut self, at: Pos2, text: &str) {
        testkit::type_text(self.h, at, text);
        self.watch(false);
    }

    /// Marks the start of a new top-level step of the route. The effort
    /// scorer puts a mental operator before each.
    pub fn step(&mut self, name: &str) {
        testkit::step_marker(self.h, name);
    }

    /// Says that the actions from here to the next step only recreate
    /// something that already existed, because the app made that necessary.
    pub fn recreating(&mut self, name: &str) {
        testkit::recreate_marker(self.h, name);
    }

    /// Records time the user spent waiting on the app (analysis, rendering).
    pub fn waited(&mut self, secs: f64) {
        testkit::waited(self.h, secs);
    }

    /// Where the socket `key` of `node` is on screen.
    pub fn socket(&mut self, node: NodeId, output: bool, key: &str) -> Pos2 {
        self.probe().socket(node, output, key)
    }

    /// The centre of the widget labelled `label`.
    pub fn find(&mut self, label: &str) -> Pos2 {
        self.probe().find(label)
    }

    pub fn arm_button(&mut self, node: NodeId) -> Pos2 {
        self.probe().arm_button(node)
    }

    /// Where the arrangement drew the clip.
    pub fn clip_rect(&mut self, id: ClipId) -> Rect {
        self.probe().clip_rect(id)
    }

    /// Where the arrangement drew take `k` of a take folder.
    pub fn take_lane(&mut self, id: ClipId, k: usize) -> Rect {
        self.probe().take_lane(id, k)
    }
}

/// Read-only access, for the done check and for finding things to act on.
pub struct Probe<'h> {
    h: &'h mut AppHarness,
}

impl<'h> Probe<'h> {
    pub(super) fn new(h: &'h mut AppHarness) -> Self {
        Self { h }
    }
}

impl Probe<'_> {
    pub fn project(&self) -> &Project {
        self.h.state().session().project()
    }

    /// The nodes in the graph, in ID order.
    pub fn nodes(&self) -> Vec<NodeId> {
        self.project().graph().nodes().map(|(id, _)| id).collect()
    }

    pub fn socket(&self, node: NodeId, output: bool, key: &str) -> Pos2 {
        let app = self.h.state();
        socket_on_screen(app.editor(), app.session(), node, output, key)
            .unwrap_or_else(|| panic!("the port {key:?} is not drawn"))
    }

    pub fn find(&self, label: &str) -> Pos2 {
        self.h.get_by_label(label).rect().center()
    }

    pub fn clip_rect(&self, id: ClipId) -> Rect {
        self.h.state().timeline().clip_rect(id).unwrap_or_else(|| {
            panic!(
                "clip {id:?} is not drawn; tracks {:?} scroll {}",
                self.h.state().session().project().clips().count(),
                self.h.state().timeline().scroll_y()
            )
        })
    }

    pub fn take_lane(&self, id: ClipId, k: usize) -> Rect {
        self.try_take_lane(id, k)
            .unwrap_or_else(|| panic!("take {k} of clip {id:?} is not drawn"))
    }

    /// The lane, if it is on screen.
    pub fn try_take_lane(&self, id: ClipId, k: usize) -> Option<Rect> {
        self.h.state().timeline().take_lane(id, k)
    }

    /// Where an Automation node's arm button is.
    pub fn arm_button(&self, node: NodeId) -> Pos2 {
        let app = self.h.state();
        crate::editor::arm_button_on_screen(app.editor(), app.session(), node)
            .unwrap_or_else(|| panic!("{node:?} has no arm button on screen"))
    }

    /// Lets scrolling and animations finish; no input.
    pub fn settle(&mut self) {
        self.h.run_steps(30);
    }

    /// Whether the transport is running.
    pub fn transport_running(&self) -> bool {
        self.h.state().session().transport_running()
    }

    /// What reaches the virtual output over the next `time`, as frames go
    /// by. Empty if the engine is not running.
    pub fn hear(&mut self, time: Duration) -> Vec<f32> {
        let Some(tap) = self.h.state().session().virtual_capture() else {
            return Vec::new();
        };
        tap.start();
        let frames = 10;
        for _ in 0..frames {
            std::thread::sleep(time / frames);
            self.h.run_steps(3);
        }
        tap.take()
    }
}

fn node_kinds(h: &AppHarness) -> BTreeMap<NodeId, String> {
    h.state()
        .session()
        .project()
        .graph()
        .nodes()
        .map(|(id, n)| (id, n.type_id.clone()))
        .collect()
}

/// Whether the chord is undo or redo.
fn is_undo(modifiers: Modifiers, key: Key) -> bool {
    key == Key::Z && modifiers.command
}

/// The largest absolute sample.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |a, &x| a.max(x.abs()))
}
