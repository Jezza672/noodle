//! The user actions every UI test performs, in one place: pointer drags and
//! clicks, double clicks, key presses, the wheel and dropped files.
//!
//! They send the events a real pointer and keyboard would, a frame at a
//! time, so a test exercises the same code paths a person does. Each is
//! generic over the harness's state, so any view's rig can use them.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, TouchPhase, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use noodle_io::{AudioConfig, VIRTUAL_OUTPUT};

// ---------------------------------------------------------------------
// The action log
//
// The usability scorecard measures a route by what a user would have to do.
// While a `Recording` is alive, every helper below appends an `Action`. It
// is off by default, so existing tests are unchanged. The log is per thread
// (libtest runs each test on its own), so tests running side by side do not
// mix their actions.
// ---------------------------------------------------------------------

/// What one user action was.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionKind {
    /// A press and release without movement.
    Click(PointerButton),
    DoubleClick,
    /// A press, a move and a release.
    Drag(PointerButton),
    /// A key press (with its modifiers: a chord is one action).
    Key(Key),
    /// A wheel or trackpad scroll.
    Wheel,
    /// Files dropped from the desktop.
    Drop {
        files: usize,
    },
    /// Text typed into the focused field.
    Text(String),
    /// A knob, fader or button moved on a MIDI control surface.
    Control,
    /// A program written into a Script node: typed like text, but it needs
    /// the user to know the script language and to think through each
    /// statement.
    Code(String),
    /// Not an action: the route says a new top-level step starts here.
    Step(String),
    /// Not an action: the user waited on the app for this many seconds.
    Wait(f64),
    /// Not an action: the actions up to the next [`ActionKind::Step`] only
    /// recreate something that already existed (the route's author says so).
    Recreate(String),
}

/// The widget under a point, as egui's accessibility tree shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub label: String,
    pub role: String,
    pub rect: Rect,
}

/// One entry in the action log.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub kind: ActionKind,
    pub modifiers: Modifiers,
    /// Where the pointer started and ended. Equal for a click or a key.
    pub start: Pos2,
    pub end: Pos2,
    /// The widget at each end, when there was one.
    pub start_hit: Option<Hit>,
    pub end_hit: Option<Hit>,
    /// The view each end is in: `node editor`, `timeline`, `piano roll`,
    /// `clip editor`, `mixer`, `outputs`, `scope`, `properties`, `menu`,
    /// `status`, `overlay` (a dialog, menu or popup), or empty.
    pub start_view: String,
    pub end_view: String,
    /// The frame time in seconds.
    pub time: f64,
}

thread_local! {
    static LOG: RefCell<Option<Vec<Action>>> = const { RefCell::new(None) };
    static VIEWS: RefCell<Vec<(&'static str, Rect)>> = const { RefCell::new(Vec::new()) };
}

/// Recording is on while this is alive.
pub struct Recording(());

/// Starts recording this thread's actions.
pub fn record() -> Recording {
    LOG.with(|l| *l.borrow_mut() = Some(Vec::new()));
    Recording(())
}

impl Recording {
    /// The log so far.
    pub fn log(&self) -> Vec<Action> {
        LOG.with(|l| l.borrow().clone().unwrap_or_default())
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        LOG.with(|l| *l.borrow_mut() = None);
    }
}

fn recording() -> bool {
    LOG.with(|l| l.borrow().is_some())
}

/// The app calls this at the start of each frame.
pub fn clear_views() {
    VIEWS.with(|v| v.borrow_mut().clear());
}

/// The app calls this inside each panel: `ui` is drawing the view `name`.
pub fn mark_view(ui: &egui::Ui, name: &'static str) {
    let rect = ui.max_rect();
    VIEWS.with(|v| v.borrow_mut().push((name, rect)));
}

/// Says that a new top-level step of the route starts here.
pub fn step_marker<S>(h: &Harness<'_, S>, name: &str) {
    push(
        h,
        ActionKind::Step(name.into()),
        Modifiers::NONE,
        None,
        None,
    );
}

/// Says the actions that follow, up to the next step marker, only recreate
/// something that already existed, because the app made it necessary.
pub fn recreate_marker<S>(h: &Harness<'_, S>, name: &str) {
    push(
        h,
        ActionKind::Recreate(name.into()),
        Modifiers::NONE,
        None,
        None,
    );
}

/// Says the user moved a control on a MIDI control surface. The caller
/// sends the MIDI itself.
pub fn control_marker<S>(h: &Harness<'_, S>) {
    push(h, ActionKind::Control, Modifiers::NONE, None, None);
}

/// Says the user waited `secs` seconds on the app.
pub fn waited<S>(h: &Harness<'_, S>, secs: f64) {
    push(h, ActionKind::Wait(secs), Modifiers::NONE, None, None);
}

fn view_at<S>(h: &Harness<'_, S>, pos: Pos2) -> String {
    // Dialogs, menus and popups sit above the panels.
    if let Some(layer) = h.ctx.layer_id_at(pos)
        && layer.order != egui::Order::Background
    {
        return "overlay".into();
    }
    VIEWS.with(|v| {
        v.borrow()
            .iter()
            .filter(|(_, r)| r.contains(pos))
            .min_by(|a, b| a.1.area().total_cmp(&b.1.area()))
            .map(|(name, _)| (*name).to_string())
            .unwrap_or_default()
    })
}

/// The smallest widget whose rect holds `pos`.
fn hit_at<S>(h: &Harness<'_, S>, pos: Pos2) -> Option<Hit> {
    let root = h.root();
    let ppp = h.ctx.pixels_per_point();
    root.children_recursive()
        .filter_map(|node| {
            let b = node.accesskit_node().bounding_box()?;
            let rect = Rect::from_min_max(
                Pos2::new(b.x0 as f32 / ppp, b.y0 as f32 / ppp),
                Pos2::new(b.x1 as f32 / ppp, b.y1 as f32 / ppp),
            );
            if !rect.contains(pos) {
                return None;
            }
            let ak = node.accesskit_node();
            Some(Hit {
                label: ak.label().unwrap_or_default(),
                role: format!("{:?}", ak.role()),
                rect,
            })
        })
        // Ties go to the later, deeper node.
        .min_by(|a, b| a.rect.area().total_cmp(&b.rect.area()))
}

fn push<S>(
    h: &Harness<'_, S>,
    kind: ActionKind,
    modifiers: Modifiers,
    start: Option<Pos2>,
    end: Option<Pos2>,
) {
    if !recording() {
        return;
    }
    let start = start.unwrap_or(Pos2::ZERO);
    let end = end.unwrap_or(start);
    let pointing = !matches!(
        kind,
        ActionKind::Step(_) | ActionKind::Wait(_) | ActionKind::Control
    );
    let action = Action {
        kind,
        modifiers,
        start,
        end,
        start_hit: pointing.then(|| hit_at(h, start)).flatten(),
        end_hit: pointing.then(|| hit_at(h, end)).flatten(),
        start_view: if pointing {
            view_at(h, start)
        } else {
            String::new()
        },
        end_view: if pointing {
            view_at(h, end)
        } else {
            String::new()
        },
        time: h.ctx.input(|i| i.time),
    };
    LOG.with(|l| {
        if let Some(log) = l.borrow_mut().as_mut() {
            log.push(action);
        }
    });
}

/// Presses `button` at the first point, moves through the rest in small
/// steps, and releases at the last, holding `modifiers` throughout. It stops
/// after the release frame: callers settle the UI themselves.
fn gesture<S>(h: &mut Harness<'_, S>, button: PointerButton, modifiers: Modifiers, path: &[Pos2]) {
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerMoved(path[0]));
    h.step();
    h.event(Event::PointerButton {
        pos: path[0],
        button,
        pressed: true,
        modifiers,
    });
    h.step();
    for &p in &path[1..] {
        h.event(Event::PointerMoved(p));
        h.step();
    }
    h.event(Event::PointerButton {
        pos: *path.last().unwrap(),
        button,
        pressed: false,
        modifiers,
    });
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::NONE));
}

/// [`gesture`], then runs until the UI settles.
pub fn drag_with<S>(
    h: &mut Harness<'_, S>,
    button: PointerButton,
    modifiers: Modifiers,
    path: &[Pos2],
) {
    let kind = if path.len() == 1 {
        ActionKind::Click(button)
    } else {
        ActionKind::Drag(button)
    };
    push(h, kind, modifiers, Some(path[0]), path.last().copied());
    gesture(h, button, modifiers, path);
    h.run();
}

/// A primary-button drag that settles in a fixed three frames, for apps that
/// keep repainting while audio plays, where `run` would never finish.
pub fn drag_playing<S>(h: &mut Harness<'_, S>, path: &[Pos2]) {
    let kind = if path.len() == 1 {
        ActionKind::Click(PointerButton::Primary)
    } else {
        ActionKind::Drag(PointerButton::Primary)
    };
    push(
        h,
        kind,
        Modifiers::NONE,
        Some(path[0]),
        path.last().copied(),
    );
    gesture(h, PointerButton::Primary, Modifiers::NONE, path);
    h.run_steps(3);
}

/// A right click that settles in a fixed three frames, for an app that keeps
/// repainting.
pub fn right_click_playing<S>(h: &mut Harness<'_, S>, at: Pos2) {
    let kind = ActionKind::Click(PointerButton::Secondary);
    push(h, kind, Modifiers::NONE, Some(at), Some(at));
    gesture(h, PointerButton::Secondary, Modifiers::NONE, &[at]);
    h.run_steps(3);
}

/// A primary-button drag along `path` with `modifiers` held.
pub fn drag<S>(h: &mut Harness<'_, S>, modifiers: Modifiers, path: &[Pos2]) {
    drag_with(h, PointerButton::Primary, modifiers, path);
}

/// A drag from `from` to `to` in six equal steps.
pub fn drag_between<S>(h: &mut Harness<'_, S>, modifiers: Modifiers, from: Pos2, to: Pos2) {
    let path: Vec<Pos2> = (0..=6)
        .map(|i| from + (to - from) * (i as f32 / 6.0))
        .collect();
    drag(h, modifiers, &path);
}

/// Presses and releases `button` at `pos`, holding `modifiers`.
pub fn click_with<S>(
    h: &mut Harness<'_, S>,
    button: PointerButton,
    modifiers: Modifiers,
    pos: Pos2,
) {
    drag_with(h, button, modifiers, &[pos]);
}

/// A primary-button click at `pos`.
pub fn click<S>(h: &mut Harness<'_, S>, pos: Pos2, modifiers: Modifiers) {
    click_with(h, PointerButton::Primary, modifiers, pos);
}

/// Two primary clicks at `at`, each in its own frame, as a person's are.
pub fn double_click<S>(h: &mut Harness<'_, S>, at: Pos2) {
    push(h, ActionKind::DoubleClick, Modifiers::NONE, Some(at), None);
    h.event(Event::PointerMoved(at));
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        h.step();
    }
    h.run();
}

/// Presses `key` with the pointer over `at`, holding `modifiers`.
pub fn press<S>(h: &mut Harness<'_, S>, at: Pos2, modifiers: Modifiers, key: Key) {
    push(h, ActionKind::Key(key), modifiers, Some(at), None);
    h.hover_at(at);
    h.step();
    h.key_press_modifiers(modifiers, key);
    h.run();
}

/// [`press`] for when audio plays and the app keeps repainting: it steps a
/// fixed three frames rather than waiting for the UI to settle.
pub fn press_playing<S>(h: &mut Harness<'_, S>, at: Pos2, modifiers: Modifiers, key: Key) {
    push(h, ActionKind::Key(key), modifiers, Some(at), None);
    h.hover_at(at);
    h.step();
    h.key_press_modifiers(modifiers, key);
    h.run_steps(3);
}

/// One wheel or trackpad event over `at`. It steps once rather than running,
/// because egui keeps repainting while it smooths a scroll.
pub fn wheel<S>(h: &mut Harness<'_, S>, at: Pos2, unit: MouseWheelUnit, delta: Vec2) {
    wheel_with(h, at, unit, delta, Modifiers::NONE);
}

/// [`wheel`] holding `modifiers`.
pub fn wheel_with<S>(
    h: &mut Harness<'_, S>,
    at: Pos2,
    unit: MouseWheelUnit,
    delta: Vec2,
    modifiers: Modifiers,
) {
    push(h, ActionKind::Wheel, modifiers, Some(at), None);
    h.hover_at(at);
    h.event(Event::MouseWheel {
        unit,
        delta,
        phase: TouchPhase::Move,
        modifiers,
    });
    h.step();
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not needed".into())
    }
}

/// Drops `files` from the desktop with the pointer at `at`.
pub fn drop_files<S>(h: &mut Harness<'_, S>, at: Pos2, files: &[PathBuf]) {
    let kind = ActionKind::Drop { files: files.len() };
    push(h, kind, Modifiers::NONE, Some(at), None);
    h.event(Event::PointerMoved(at));
    h.step();
    for file in files {
        h.input_mut()
            .dropped_files
            .push(Arc::new(Dropped(file.clone())));
    }
    h.step();
    h.run();
}

/// Types `text` into the focused field, as one text entry.
pub fn type_text<S>(h: &mut Harness<'_, S>, at: Pos2, text: &str) {
    push(
        h,
        ActionKind::Text(text.into()),
        Modifiers::NONE,
        Some(at),
        None,
    );
    h.event(Event::Text(text.into()));
    h.run();
}

/// Types the program `code` into the focused field (a Script node's text),
/// logged as code rather than text so the scorecard can tell them apart.
pub fn type_code<S>(h: &mut Harness<'_, S>, at: Pos2, code: &str) {
    push(
        h,
        ActionKind::Code(code.into()),
        Modifiers::NONE,
        Some(at),
        None,
    );
    h.event(Event::Text(code.into()));
    h.run_steps(3);
}

/// [`type_text`] for when the app keeps repainting: it steps a fixed three
/// frames rather than waiting for the UI to settle.
pub fn type_text_playing<S>(h: &mut Harness<'_, S>, at: Pos2, text: &str) {
    push(
        h,
        ActionKind::Text(text.into()),
        Modifiers::NONE,
        Some(at),
        None,
    );
    h.event(Event::Text(text.into()));
    h.run_steps(3);
}

/// Audio settings that play on the virtual output: the engine runs in real
/// time on every platform and nothing is heard.
pub fn virtual_audio() -> AudioConfig {
    AudioConfig {
        output: Some(VIRTUAL_OUTPUT.into()),
        ..AudioConfig::default()
    }
}

/// A harness that draws one view with `ui` over `state`, at `size`, and has
/// settled its first frames.
pub fn harness<S>(
    state: S,
    size: Vec2,
    ui: impl FnMut(&mut egui::Ui, &mut S) + 'static,
) -> Harness<'static, S> {
    let mut h = Harness::builder().with_size(size).build_ui_state(ui, state);
    h.run();
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Ui {
        clicks: u32,
        text: String,
    }

    /// Two "views" side by side: a button and a drag handle on the left, a
    /// text field on the right.
    fn rig() -> Harness<'static, Ui> {
        harness(Ui::default(), Vec2::new(400.0, 200.0), |ui, st| {
            ui.horizontal(|ui| {
                ui.scope(|ui| {
                    mark_view(ui, "left");
                    ui.set_width(150.0);
                    if ui.button("Press me").clicked() {
                        st.clicks += 1;
                    }
                    let _ = ui.button("Other");
                });
                ui.scope(|ui| {
                    mark_view(ui, "right");
                    ui.set_width(200.0);
                    ui.text_edit_singleline(&mut st.text);
                });
            });
        })
    }

    fn centre<S>(h: &Harness<'_, S>, label: &str) -> Pos2 {
        use egui_kittest::kittest::Queryable;
        h.get_by_label(label).rect().center()
    }

    /// Strips what varies from run to run.
    fn shape(log: &[Action]) -> Vec<(ActionKind, String, String, String, String)> {
        log.iter()
            .map(|a| {
                (
                    a.kind.clone(),
                    a.start_hit
                        .as_ref()
                        .map(|h| h.label.clone())
                        .unwrap_or_default(),
                    a.end_hit
                        .as_ref()
                        .map(|h| h.label.clone())
                        .unwrap_or_default(),
                    a.start_view.clone(),
                    a.end_view.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn nothing_is_logged_unless_recording() {
        let mut h = rig();
        let at = centre(&h, "Press me");
        click(&mut h, at, Modifiers::NONE);
        assert_eq!(h.state().clicks, 1);
        let rec = record();
        assert!(rec.log().is_empty());
    }

    #[test]
    fn the_log_holds_each_action_with_its_widgets_and_views() {
        let mut h = rig();
        let rec = record();
        let button = centre(&h, "Press me");
        let slider = centre(&h, "Other");
        let field = h.ctx.input(|_| Pos2::new(300.0, button.y));

        step_marker(&h, "start");
        click(&mut h, button, Modifiers::SHIFT);
        // A drag from the button in one view to the field in the other.
        drag_between(&mut h, Modifiers::NONE, button, field);
        click(&mut h, field, Modifiers::NONE);
        type_text(&mut h, field, "abc");
        press(&mut h, slider, Modifiers::COMMAND, Key::S);
        waited(&h, 2.5);

        assert_eq!(h.state().clicks, 1);
        assert_eq!(h.state().text, "abc");
        let log = rec.log();
        assert_eq!(
            shape(&log),
            vec![
                (
                    ActionKind::Step("start".into()),
                    "".into(),
                    "".into(),
                    "".into(),
                    "".into()
                ),
                (
                    ActionKind::Click(PointerButton::Primary),
                    "Press me".into(),
                    "Press me".into(),
                    "left".into(),
                    "left".into()
                ),
                (
                    ActionKind::Drag(PointerButton::Primary),
                    "Press me".into(),
                    log[2]
                        .end_hit
                        .as_ref()
                        .map(|h| h.label.clone())
                        .unwrap_or_default(),
                    "left".into(),
                    "right".into()
                ),
                (
                    ActionKind::Click(PointerButton::Primary),
                    log[3]
                        .start_hit
                        .as_ref()
                        .map(|h| h.label.clone())
                        .unwrap_or_default(),
                    log[3]
                        .end_hit
                        .as_ref()
                        .map(|h| h.label.clone())
                        .unwrap_or_default(),
                    "right".into(),
                    "right".into()
                ),
                (
                    ActionKind::Text("abc".into()),
                    log[4]
                        .start_hit
                        .as_ref()
                        .map(|h| h.label.clone())
                        .unwrap_or_default(),
                    log[4]
                        .end_hit
                        .as_ref()
                        .map(|h| h.label.clone())
                        .unwrap_or_default(),
                    "right".into(),
                    "right".into()
                ),
                (
                    ActionKind::Key(Key::S),
                    "Other".into(),
                    "Other".into(),
                    "left".into(),
                    "left".into()
                ),
                (
                    ActionKind::Wait(2.5),
                    "".into(),
                    "".into(),
                    "".into(),
                    "".into()
                ),
            ]
        );
        assert_eq!(log[1].modifiers, Modifiers::SHIFT);
        assert_eq!(log[5].modifiers, Modifiers::COMMAND);
        assert_eq!((log[2].start, log[2].end), (button, field));
        assert!(log[2].start_hit.as_ref().unwrap().rect.contains(button));
        assert!(log[2].start_view != log[2].end_view, "a view switch");
        assert!(log.windows(2).all(|w| w[0].time <= w[1].time));
    }
}
