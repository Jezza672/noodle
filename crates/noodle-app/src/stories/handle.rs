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
    /// The step the route is in, by the name it gave.
    step: String,
    /// Set when the route runs with the transport playing.
    live: Option<Live>,
}

/// The transport while a route runs on top of it.
struct Live {
    /// The step each unasked stop happened in.
    stops: Vec<String>,
}

impl<'h> Handle<'h> {
    pub(super) fn new(h: &'h mut AppHarness) -> Self {
        let kinds = node_kinds(h);
        Self {
            h,
            kinds,
            changes: Vec::new(),
            step: String::new(),
            live: None,
        }
    }

    /// Runs the route with the transport playing, which the caller has
    /// started. The route's own Space presses are skipped, since the user
    /// would not press play twice. A step that stops the transport is
    /// recorded, and the transport is started again so the later steps are
    /// also tried live.
    pub(super) fn live(mut self) -> Self {
        self.live = Some(Live { stops: Vec::new() });
        self
    }

    /// Whether the press is the route's own play or stop, which a live run
    /// leaves out.
    fn skipped(&self, modifiers: Modifiers, key: Key) -> bool {
        self.live.is_some() && key == Key::Space && !modifiers.any()
    }

    /// The step each unasked stop happened in.
    pub(super) fn stops(&self) -> Vec<String> {
        self.live.as_ref().map_or(Vec::new(), |l| l.stops.clone())
    }

    /// The name of the step the route is in.
    pub(super) fn step_name(&self) -> &str {
        &self.step
    }

    fn check_transport(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if !self.h.state().session().transport_running() {
            live.stops.push(self.step.clone());
            self.h.state_mut().session_mut().play();
        }
    }

    /// What the actions added and removed, for spotting rework.
    pub(super) fn finish(self) -> Vec<Change> {
        self.changes
    }

    /// Notes which nodes the last action added or removed. An undo or redo
    /// is not counted: it is not the user throwing work away.
    fn watch(&mut self, quiet: bool) {
        self.watch_nodes(quiet);
        self.check_transport();
    }

    fn watch_nodes(&mut self, quiet: bool) {
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

    /// [`Handle::right_click`] for when the app keeps repainting.
    pub fn right_click_playing(&mut self, at: Pos2) {
        testkit::right_click_playing(self.h, at);
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
        if self.skipped(modifiers, key) {
            return;
        }
        testkit::press(self.h, at, modifiers, key);
        self.watch(is_undo(modifiers, key));
    }

    /// [`Handle::press`] for when the transport runs: the app repaints
    /// continuously then, so the UI cannot be waited on to settle.
    pub fn press_playing(&mut self, at: Pos2, modifiers: Modifiers, key: Key) {
        if self.skipped(modifiers, key) {
            return;
        }
        testkit::press_playing(self.h, at, modifiers, key);
        self.watch(is_undo(modifiers, key));
    }

    pub fn wheel(&mut self, at: Pos2, unit: MouseWheelUnit, delta: Vec2) {
        testkit::wheel(self.h, at, unit, delta);
        self.watch(false);
    }

    /// [`Handle::wheel`] holding `modifiers` (Ctrl zooms).
    pub fn wheel_with(
        &mut self,
        at: Pos2,
        unit: MouseWheelUnit,
        delta: Vec2,
        modifiers: Modifiers,
    ) {
        testkit::wheel_with(self.h, at, unit, delta, modifiers);
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

    /// Types a program into the focused Script text box. Scored as code:
    /// see `effort.rs`.
    pub fn type_code(&mut self, at: Pos2, code: &str) {
        testkit::type_code(self.h, at, code);
        self.watch(false);
    }

    /// [`Handle::type_text`] for when the app keeps repainting (audio
    /// playing, or live values on the canvas).
    pub fn type_text_playing(&mut self, at: Pos2, text: &str) {
        testkit::type_text_playing(self.h, at, text);
        self.watch(false);
    }

    /// Moves controller `cc` on channel 1 of a MIDI control surface to
    /// `value`. The surface is the session's MIDI bus, which every open
    /// input feeds, so the app hears it as it would a real one.
    pub fn control(&mut self, cc: u8, value: u8) {
        testkit::control_marker(self.h);
        self.h
            .state()
            .session()
            .midi_bus()
            .send([0xB0, cc, value & 0x7F]);
        self.h.run_steps(3);
        self.watch(false);
    }

    /// Marks the start of a new top-level step of the route. The effort
    /// scorer puts a mental operator before each.
    pub fn step(&mut self, name: &str) {
        self.step = name.into();
        testkit::step_marker(self.h, name);
    }

    /// Says that the result of the actions from here to the next step cannot
    /// be seen without opening another view (the warp markers, the recorded
    /// automation points, the sidechain amount).
    pub fn unseen(&mut self, name: &str) {
        testkit::unseen_marker(self.h, name);
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

    pub fn find_like(&mut self, word: &str) -> Pos2 {
        self.probe().find_like(word)
    }

    pub fn find_first(&mut self, label: &str) -> Pos2 {
        self.probe().find_first(label)
    }

    pub fn arm_button(&mut self, node: NodeId) -> Pos2 {
        self.probe().arm_button(node)
    }

    /// The centre of the row of `node`'s port `key`, where a parameter's
    /// slider is drawn. Tells apart two nodes with the same labels.
    pub fn port_row(&mut self, node: NodeId, output: bool, key: &str) -> Pos2 {
        self.probe().port_row(node, output, key)
    }

    /// Where the piano roll drew `key` at `tick`, scrolling to it first as a
    /// user must when it is off screen.
    pub fn roll_point(&mut self, tick: i64, key: u8) -> Pos2 {
        for _ in 0..40 {
            self.probe().settle();
            if let Some(at) = self.probe().roll_point(tick, key) {
                return at;
            }
            let grid = self.probe().roll_grid().expect("the piano roll is open");
            // Rows run high keys at the top; the wheel moves the view.
            let y = self
                .probe()
                .roll_point_unclamped(key)
                .unwrap_or(grid.center().y);
            let x = self.probe().roll_x(tick).unwrap_or(grid.center().x);
            let dy = if y < grid.top() {
                120.0
            } else if y > grid.bottom() {
                -120.0
            } else {
                0.0
            };
            let dx = if x < grid.left() {
                120.0
            } else if x > grid.right() {
                -120.0
            } else {
                0.0
            };
            self.wheel(grid.center(), MouseWheelUnit::Point, Vec2::new(dx, dy));
        }
        let g = self.probe().roll_grid();
        panic!(
            "key {key} never scrolled into view; grid {g:?}, y {:?}, x {:?}",
            self.probe().roll_point_unclamped(key),
            self.probe().roll_x(tick)
        );
    }

    /// Where the ruler is at `seconds` into the song.
    pub fn ruler_at(&mut self, seconds: f64) -> Pos2 {
        self.probe().settle();
        self.probe().ruler_at(seconds)
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

    /// The centre of the one multi-line text box on screen (a Script node's
    /// text in the properties).
    pub fn text_box(&self) -> Pos2 {
        self.h
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .rect()
            .center()
    }

    /// The first widget showing `value` (a number field with no label).
    pub fn find_value(&self, value: &str) -> Pos2 {
        self.h
            .get_all_by_value(value)
            .next()
            .unwrap_or_else(|| panic!("nothing shows {value:?}"))
            .rect()
            .center()
    }

    /// The folder the project is saved in.
    pub fn project_dir(&self) -> std::path::PathBuf {
        self.h
            .state()
            .session()
            .directory()
            .expect("the project is saved")
            .to_path_buf()
    }

    /// Where the node editor's canvas is on screen.
    pub fn canvas(&self) -> Rect {
        self.h.state().editor().canvas
    }

    pub fn port_row(&self, node: NodeId, output: bool, key: &str) -> Pos2 {
        self.port_rect(node, output, key).center()
    }

    /// The whole row of `node`'s port `key`.
    pub fn port_rect(&self, node: NodeId, output: bool, key: &str) -> Rect {
        let app = self.h.state();
        crate::editor::port_row_on_screen(app.editor(), app.session(), node, output, key)
            .unwrap_or_else(|| panic!("the row of {key:?} is not drawn"))
    }

    /// The first of several widgets with the same label (the snap menu's
    /// button and the piano roll's label for the same step).
    pub fn find_first(&self, label: &str) -> Pos2 {
        self.h
            .get_all_by_label(label)
            .next()
            .unwrap_or_else(|| panic!("nothing labelled {label:?}"))
            .rect()
            .center()
    }

    /// Where the playhead is.
    pub fn playhead(&self) -> noodle_core::Tick {
        self.h.state().session().playhead()
    }

    /// Where the arrangement's ruler is at `seconds` into the song.
    pub fn ruler_at(&self, seconds: f64) -> Pos2 {
        self.h
            .state()
            .timeline()
            .ruler_at(seconds)
            .unwrap_or_else(|| panic!("{seconds} s is not on the ruler"))
    }

    /// The centre of the first widget whose label contains `word`: for a
    /// submenu, whose label carries an arrow.
    pub fn find_like(&self, word: &str) -> Pos2 {
        self.h
            .get_all_by_label_contains(word)
            .next()
            .unwrap_or_else(|| panic!("nothing labelled like {word:?}"))
            .rect()
            .center()
    }

    /// Whether a freeze is still rendering in the background.
    pub fn freezing(&self) -> bool {
        self.h.state().session().freeze_pending()
    }

    /// Whether anything on screen has a label containing `word`.
    pub fn mentions(&self, word: &str) -> bool {
        self.h.query_all_by_label_contains(word).next().is_some()
    }

    /// Whether anything on screen is labelled exactly `label`.
    pub fn has(&self, label: &str) -> bool {
        self.h.query_all_by_label(label).next().is_some()
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

    /// Where the piano roll drew `key` at `tick`, if it is open and the
    /// point is on screen.
    pub fn roll_point(&self, tick: i64, key: u8) -> Option<Pos2> {
        let g = self.h.state().piano_roll().geometry()?;
        let at = Pos2::new(g.x(tick), g.y(key));
        g.grid.contains(at).then_some(at)
    }

    fn roll_x(&self, tick: i64) -> Option<f32> {
        Some(self.h.state().piano_roll().geometry()?.x(tick))
    }

    fn roll_point_unclamped(&self, key: u8) -> Option<f32> {
        Some(self.h.state().piano_roll().geometry()?.y(key))
    }

    /// The piano roll's grid, if it is open.
    pub fn roll_grid(&self) -> Option<Rect> {
        Some(self.h.state().piano_roll().geometry()?.grid)
    }

    /// The notes of the clip open in the piano roll: start, length, key.
    pub fn roll_notes(&self, clip: ClipId) -> Vec<(i64, i64, u8)> {
        self.project()
            .clip(clip)
            .and_then(|c| c.as_midi())
            .map(|m| {
                m.notes
                    .iter()
                    .map(|n| (n.start.0, n.length.0, n.key))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where a click in the arrangement puts a point on `lane` at `tick`
    /// (before snapping) with `value`, if the lane's row is on screen.
    pub fn lane_point(&self, lane: noodle_core::LaneId, tick: i64, value: f32) -> Option<Pos2> {
        self.h
            .state()
            .timeline()
            .automation()
            .aim(lane, tick, value)
    }

    /// Where the arrangement drew `lane`'s row, if it is on screen.
    pub fn lane_row(&self, lane: noodle_core::LaneId) -> Option<Rect> {
        self.h.state().timeline().automation().row(lane)
    }

    /// Where the arrangement drew point `index` of `lane`.
    pub fn lane_point_drawn(&self, lane: noodle_core::LaneId, index: usize) -> Option<Pos2> {
        self.h.state().timeline().automation().point_at(lane, index)
    }

    /// Where an Automation node's arm button is.
    pub fn arm_button(&self, node: NodeId) -> Pos2 {
        let app = self.h.state();
        crate::editor::arm_button_on_screen(app.editor(), app.session(), node)
            .unwrap_or_else(|| panic!("{node:?} has no arm button on screen"))
    }

    pub fn automation_mode(&self) -> crate::session::record_automation::AutomationMode {
        self.h.state().session().automation_mode()
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
        let heard = tap.take();
        testkit::heard(self.h, peak(&heard));
        heard
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
