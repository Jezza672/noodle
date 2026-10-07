//! Drives the widgets through a headless egui context with synthetic input,
//! feeding each edit back in as the app would.

use egui::{Context, Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Ui, pos2, vec2};
use noodle_core::Value;
use noodle_engine::{ConfigInfo, ParamInfo, Unit};

use super::config::ConfigEdit;
use super::param::{Gesture, ParamEdit};
use super::*;

const WIDTH: f32 = 200.0;

struct Harness {
    ctx: Context,
    time: f64,
    modifiers: Modifiers,
}

impl Harness {
    fn new() -> Self {
        Self {
            ctx: Context::default(),
            time: 0.0,
            modifiers: Modifiers::NONE,
        }
    }

    fn frame(&mut self, events: Vec<Event>, mut ui_fn: impl FnMut(&mut Ui)) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            time: Some(self.time),
            events: [Event::ModifiersChanged(self.modifiers)]
                .into_iter()
                .chain(events)
                .collect(),
            ..Default::default()
        };
        self.time += 1.0 / 60.0;
        self.ctx
            .run_ui(input, |ui| ui_fn(ui))
            .drop_without_applying_deltas();
    }
}

fn pointer(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn key(key: Key) -> Event {
    key_with(Modifiers::NONE, key)
}

fn key_with(modifiers: Modifiers, key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// A parameter field whose edits are applied to `value`, recording what it
/// reported each frame.
struct Param {
    info: ParamInfo,
    value: f32,
    rect: Rect,
    id: egui::Id,
    log: Vec<(Option<ParamEdit>, Gesture)>,
}

impl Param {
    fn new(info: ParamInfo) -> Self {
        let value = info.default;
        Self {
            info,
            value,
            rect: Rect::NOTHING,
            id: egui::Id::NULL,
            log: Vec::new(),
        }
    }

    fn frame(&mut self, h: &mut Harness, events: Vec<Event>) {
        h.frame(events, |ui| {
            let out = ParamField::new("Value", &self.info, self.value)
                .width(WIDTH)
                .show(ui);
            self.rect = out.response.rect;
            self.id = out.response.id;
            match out.edit {
                Some(ParamEdit::Set(v)) => self.value = v,
                Some(ParamEdit::Reset) => self.value = self.info.default,
                None => {}
            }
            self.log.push((out.edit, out.gesture));
        });
    }

    /// Presses at the field's centre, moves `dx` over a few frames, and
    /// releases.
    fn drag(&mut self, h: &mut Harness, dx: f32) {
        self.frame(h, vec![]);
        let start = self.rect.center();
        self.frame(h, vec![Event::PointerMoved(start)]);
        self.frame(h, vec![pointer(start, true)]);
        for i in 1..=4 {
            let pos = start + vec2(dx * i as f32 / 4.0, 0.0);
            self.frame(h, vec![Event::PointerMoved(pos)]);
        }
        let end = start + vec2(dx, 0.0);
        self.frame(h, vec![pointer(end, false)]);
        self.frame(h, vec![]);
    }

    fn click(&mut self, h: &mut Harness) {
        self.frame(h, vec![]);
        let at = self.rect.center();
        self.frame(h, vec![Event::PointerMoved(at)]);
        self.frame(h, vec![pointer(at, true)]);
        self.frame(h, vec![pointer(at, false)]);
        // The text box takes focus on the frame after the click.
        self.frame(h, vec![]);
        self.frame(h, vec![]);
    }

    fn gestures(&self) -> Vec<Gesture> {
        let mut gestures: Vec<_> = self.log.iter().map(|&(_, g)| g).collect();
        gestures.dedup();
        gestures
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-3 * a.abs().max(b.abs()).max(1.0)
}

#[test]
fn dragging_moves_along_the_taper_and_reports_the_gesture() {
    let mut h = Harness::new();
    let mut p = Param::new(ParamInfo::new(-10.0, 10.0, 0.0));
    p.drag(&mut h, WIDTH / 4.0);
    assert!(close(p.value, 5.0), "{}", p.value);
    assert_eq!(
        p.gestures(),
        [
            Gesture::Idle,
            Gesture::Dragging,
            Gesture::Released,
            Gesture::Idle
        ]
    );
    // Every edit came during the drag.
    assert!(
        p.log
            .iter()
            .all(|(edit, g)| edit.is_none() || *g != Gesture::Idle)
    );
}

#[test]
fn dragging_a_log_parameter_moves_by_ratios() {
    let mut h = Harness::new();
    let mut p = Param::new(ParamInfo::new(20.0, 20_000.0, 20.0).log().unit(Unit::Hertz));
    p.drag(&mut h, WIDTH / 3.0);
    assert!(close(p.value, 200.0), "{}", p.value);
}

#[test]
fn shift_drags_finely() {
    let mut h = Harness::new();
    h.modifiers = Modifiers::SHIFT;
    let mut p = Param::new(ParamInfo::new(-10.0, 10.0, 0.0));
    p.drag(&mut h, WIDTH / 4.0);
    assert!(close(p.value, 0.5), "{}", p.value);
}

#[test]
fn dragging_clamps_at_the_ends() {
    let mut h = Harness::new();
    let mut p = Param::new(ParamInfo::new(-10.0, 10.0, 0.0));
    p.drag(&mut h, WIDTH * 2.0);
    assert_eq!(p.value, 10.0);
    // Coming back starts from the end, not from where the pointer overshot.
    p.drag(&mut h, -WIDTH / 4.0);
    assert!(close(p.value, 5.0), "{}", p.value);
}

#[test]
fn slow_drags_add_up_to_a_step() {
    let mut h = Harness::new();
    let mut info = ParamInfo::choice(Vec::<&str>::new());
    info.max = 4.0;
    let mut p = Param::new(info);
    // Each frame moves a fifth of a step, which rounds away on its own.
    p.drag(&mut h, WIDTH / 4.0 * 0.8);
    assert_eq!(p.value, 1.0);
}

#[test]
fn clicking_lets_you_type_a_value() {
    let mut h = Harness::new();
    let mut p = Param::new(
        ParamInfo::new(20.0, 20_000.0, 440.0)
            .log()
            .unit(Unit::Hertz),
    );
    p.click(&mut h);
    let select_all = key_with(Modifiers::COMMAND, Key::A);
    p.frame(&mut h, vec![select_all, Event::Text("2k".into())]);
    p.frame(&mut h, vec![key(Key::Enter)]);
    assert_eq!(p.value, 2_000.0);
    assert_eq!(p.log.iter().filter(|(e, _)| e.is_some()).count(), 1);
    assert!(p.log.iter().all(|&(_, g)| g == Gesture::Idle));
}

#[test]
fn committing_unchanged_text_leaves_the_value_alone() {
    let mut h = Harness::new();
    // Shown as "1.23 kHz", which would round the value if parsed back.
    let mut p = Param::new(ParamInfo::new(20.0, 20_000.0, 1_234.5).unit(Unit::Hertz));
    p.click(&mut h);
    p.frame(&mut h, vec![key(Key::Enter)]);
    assert_eq!(p.value, 1_234.5);
    assert!(p.log.iter().all(|(e, _)| e.is_none()));
}

#[test]
fn escape_cancels_typing() {
    let mut h = Harness::new();
    let mut p = Param::new(ParamInfo::new(0.0, 1.0, 0.5));
    p.click(&mut h);
    p.frame(&mut h, vec![Event::Text("9".into())]);
    p.frame(&mut h, vec![key(Key::Escape)]);
    assert_eq!(p.value, 0.5);
    // And the field is back to a slider that can be dragged.
    p.drag(&mut h, WIDTH / 4.0);
    assert!(close(p.value, 0.75), "{}", p.value);
}

#[test]
fn backspace_while_hovering_resets() {
    let mut h = Harness::new();
    let mut p = Param::new(ParamInfo::new(-10.0, 10.0, 0.0));
    p.value = 7.0;
    p.frame(&mut h, vec![]);
    p.frame(&mut h, vec![Event::PointerMoved(p.rect.center())]);
    p.frame(&mut h, vec![key(Key::Backspace)]);
    assert_eq!(p.log.last().unwrap().0, Some(ParamEdit::Reset));
    assert_eq!(p.value, 0.0);

    // Not while the pointer is elsewhere.
    p.value = 7.0;
    p.frame(&mut h, vec![Event::PointerMoved(pos2(700.0, 500.0))]);
    p.frame(&mut h, vec![key(Key::Backspace)]);
    assert_eq!(p.value, 7.0);
}

#[test]
fn compact_fields_are_shorter() {
    let mut h = Harness::new();
    let info = ParamInfo::new(0.0, 1.0, 0.5);
    let (mut full, mut compact) = (Rect::NOTHING, Rect::NOTHING);
    h.frame(vec![], |ui| {
        full = ParamField::new("A", &info, 0.5).show(ui).response.rect;
        compact = ParamField::new("B", &info, 0.5)
            .compact(true)
            .show(ui)
            .response
            .rect;
    });
    assert!(compact.height() < full.height());
}

#[test]
fn config_commits_only_when_the_drag_ends() {
    const INPUTS: ConfigInfo = ConfigInfo::int("inputs", "Inputs", 2);
    struct Config {
        value: Option<Value>,
        rect: Rect,
        edits: Vec<ConfigEdit>,
    }
    impl Config {
        fn frame(&mut self, h: &mut Harness, events: Vec<Event>) {
            h.frame(events, |ui| {
                let out = ConfigField::new(&INPUTS, self.value.as_ref()).show(ui);
                self.rect = out.response.rect;
                if let Some(edit) = out.edit {
                    self.value = match &edit {
                        ConfigEdit::Set(v) => Some(v.clone()),
                        ConfigEdit::Reset => None,
                    };
                    self.edits.push(edit);
                }
            });
        }
    }

    let mut h = Harness::new();
    let mut c = Config {
        value: None,
        rect: Rect::NOTHING,
        edits: Vec::new(),
    };
    c.frame(&mut h, vec![]);
    let start = pos2(c.rect.right() - 5.0, c.rect.center().y);
    c.frame(&mut h, vec![Event::PointerMoved(start)]);
    c.frame(&mut h, vec![pointer(start, true)]);
    for i in 1..=4 {
        let pos = start + vec2(20.0 * i as f32, 0.0);
        c.frame(&mut h, vec![Event::PointerMoved(pos)]);
    }
    assert!(c.edits.is_empty(), "committed mid-drag: {:?}", c.edits);
    c.frame(&mut h, vec![pointer(start + vec2(80.0, 0.0), false)]);
    c.frame(&mut h, vec![]);
    assert_eq!(c.edits, [ConfigEdit::Set(Value::Int(6))]);
}

#[test]
fn show_at_fills_the_rect_and_zoom_scales_the_height() {
    let mut h = Harness::new();
    let info = ParamInfo::new(0.0, 1.0, 0.5);
    let target = Rect::from_min_size(pos2(100.0, 50.0), vec2(120.0, 30.0));
    let (mut placed, mut normal, mut zoomed) = (Rect::NOTHING, Rect::NOTHING, Rect::NOTHING);
    h.frame(vec![], |ui| {
        placed = ParamField::new("A", &info, 0.5)
            .compact(true)
            .show_at(ui, target)
            .response
            .rect;
        normal = ParamField::new("B", &info, 0.5).show(ui).response.rect;
        zoomed = ParamField::new("C", &info, 0.5)
            .zoom(2.0)
            .show(ui)
            .response
            .rect;
    });
    assert_eq!(placed, target);
    assert_eq!(zoomed.height(), normal.height() * 2.0);
}

#[test]
fn the_keyboard_can_change_and_type_values() {
    let mut h = Harness::new();
    let mut p = Param::new(ParamInfo::new(0.0, 100.0, 50.0));
    p.frame(&mut h, vec![]);
    // Tab focuses the field; arrows move a hundredth of the travel.
    p.frame(&mut h, vec![key(Key::Tab)]);
    p.frame(&mut h, vec![key(Key::ArrowRight), key(Key::ArrowRight)]);
    assert!(close(p.value, 52.0), "{}", p.value);
    p.frame(&mut h, vec![key(Key::ArrowLeft)]);
    assert!(close(p.value, 51.0), "{}", p.value);

    // Enter starts typing.
    p.frame(&mut h, vec![key(Key::Enter)]);
    p.frame(&mut h, vec![]);
    p.frame(&mut h, vec![]);
    let select_all = key_with(Modifiers::COMMAND, Key::A);
    p.frame(&mut h, vec![select_all, Event::Text("20".into())]);
    p.frame(&mut h, vec![key(Key::Enter)]);
    assert_eq!(p.value, 20.0);
}

#[test]
fn assistive_tech_can_step_values() {
    use egui::accesskit::{Action, ActionRequest, TreeId};
    let mut h = Harness::new();
    h.ctx.enable_accesskit();
    let mut info = ParamInfo::choice(Vec::<&str>::new());
    info.max = 4.0;
    let mut p = Param::new(info);
    p.frame(&mut h, vec![]);
    let request = |action| {
        Event::AccessKitActionRequest(ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: p.id.accesskit_id(),
            data: None,
        })
    };
    let (up, down) = (request(Action::Increment), request(Action::Decrement));
    p.frame(&mut h, vec![up.clone(), up]);
    assert_eq!(p.value, 2.0);
    p.frame(&mut h, vec![down]);
    assert_eq!(p.value, 1.0);
}

#[test]
fn backspace_resets_a_drop_down_too() {
    let mut h = Harness::new();
    let mut p = Param::new(ParamInfo::choice(["Sine", "Saw", "Square"]));
    p.value = 2.0;
    p.frame(&mut h, vec![]);
    p.frame(&mut h, vec![Event::PointerMoved(p.rect.center())]);
    p.frame(&mut h, vec![key(Key::Backspace)]);
    assert_eq!(p.log.last().unwrap().0, Some(ParamEdit::Reset));
    assert_eq!(p.value, 0.0);
}

#[test]
fn a_focused_checkbox_commits_when_toggled() {
    const FLAG: ConfigInfo = ConfigInfo {
        key: "flag",
        name: "Flag",
        default: Value::Bool(false),
    };
    let mut h = Harness::new();
    let mut edits = Vec::new();
    // Tab to the checkbox, then toggle it with Space while it keeps focus.
    for events in [vec![], vec![key(Key::Tab)], vec![key(Key::Space)], vec![]] {
        h.frame(events, |ui| {
            edits.extend(ConfigField::new(&FLAG, None).show(ui).edit);
        });
    }
    assert_eq!(edits, [ConfigEdit::Set(Value::Bool(true))]);
}

#[test]
fn a_field_abandoned_mid_typing_comes_back_as_a_slider() {
    let mut h = Harness::new();
    let mut p = Param::new(ParamInfo::new(0.0, 100.0, 50.0));
    p.click(&mut h);
    p.frame(&mut h, vec![Event::Text("7".into())]);
    // The field stops being drawn while it has focus, as when its node is
    // zoomed out of view or removed by undo.
    h.frame(vec![], |_| {});
    h.frame(vec![], |_| {});

    // Back again, it's a slider: dragging changes the value, and the
    // abandoned text was neither committed nor kept.
    p.drag(&mut h, WIDTH / 4.0);
    assert!(close(p.value, 75.0), "{}", p.value);
}
