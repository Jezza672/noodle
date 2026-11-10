//! The strip along the top of the piano roll, clip editor and sample view.
//!
//! It follows the transport pill's rules: every control sits in a slot of a
//! fixed width (so a changing value never moves its neighbours), related
//! controls share a rounded pill, icons are drawn rather than taken from the
//! font, and a number that needs room opens in a popover instead of sitting
//! in the bar. When the panel is too narrow, the controls the user needs
//! least fold into a "…" menu, so none of them is ever out of reach. Close is
//! always on the right.

use egui::{
    Align, Color32, CornerRadius, Layout, PopupCloseBehavior, Pos2, Rect, Response, RichText,
    Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, WidgetInfo, WidgetType, vec2,
};

use crate::theme::{self, timeline as colors};

/// How tall the strip is.
pub const HEIGHT: f32 = 34.0;

/// How tall a pill is.
const PILL_HEIGHT: f32 = 26.0;
/// The width of an icon-only button.
pub const SQUARE: f32 = 26.0;
/// The room the "…" button needs.
const MORE_WIDTH: f32 = SQUARE;
/// The gap between slots.
const GAP: f32 = 8.0;

/// A drawn icon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Pen,
    Line,
    Cursor,
    Loop,
    Dice,
    Lanes,
    Tag,
    Wave,
    Close,
    Help,
    More,
}

/// Which side of the bar a slot is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

/// A control in the bar.
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    /// The width of its slot, fixed.
    pub width: f32,
    /// How long it survives a narrow panel: the highest value folds into the
    /// "..." menu first, the lowest stays longest.
    pub keep: u8,
    pub side: Side,
    /// Starts a new group after a divider.
    pub divider: bool,
}

impl Slot {
    pub const fn left(width: f32, keep: u8) -> Self {
        Self {
            width,
            keep,
            side: Side::Left,
            divider: false,
        }
    }

    pub const fn right(width: f32, keep: u8) -> Self {
        Self {
            width,
            keep,
            side: Side::Right,
            divider: false,
        }
    }

    /// This slot, after a divider.
    pub const fn after_divider(self) -> Self {
        Self {
            divider: true,
            ..self
        }
    }
}

/// Where a slot is being drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// In the bar, in its pill.
    Bar,
    /// In the "…" menu, because the bar was too narrow.
    Menu,
}

/// The indices of `slots` that fit in `room`, with the "…" button's room kept
/// back when any do not.
pub fn fit(slots: &[Slot], room: f32) -> Vec<bool> {
    let mut order: Vec<usize> = (0..slots.len()).collect();
    order.sort_by_key(|&i| slots[i].keep);
    let cost = |s: &Slot| s.width + GAP + if s.divider { GAP } else { 0.0 };
    let all: f32 = slots.iter().map(cost).sum();
    if all <= room {
        return vec![true; slots.len()];
    }
    let room = room - MORE_WIDTH - GAP;
    let mut shown = vec![false; slots.len()];
    let mut used = 0.0;
    for i in order {
        used += cost(&slots[i]);
        if used > room {
            break;
        }
        shown[i] = true;
    }
    shown
}

/// Draws the strip into `rect`: `title` in `title_width`, the `slots` (each
/// drawn by `draw` with its index), then Close. Slots that do not fit move
/// into a "…" menu. Says whether Close was pressed.
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    title: &str,
    title_width: f32,
    slots: &[Slot],
    draw: &mut dyn FnMut(&mut Ui, usize, Mode),
) -> bool {
    let inner = rect.shrink2(vec2(8.0, (HEIGHT - PILL_HEIGHT) / 2.0));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::PANEL);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::TRANSPORT_OUTLINE),
    );
    let room = inner.width() - title_width - SQUARE - 2.0 * GAP;
    let shown = fit(slots, room);
    let hidden: Vec<usize> = (0..slots.len()).filter(|&i| !shown[i]).collect();
    let mut closed = false;

    // Right to left: Close, then "…", then the right-hand slots.
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            closed = icon_button(ui, Icon::Close, "Close", "Close the editor").clicked();
            if !hidden.is_empty() {
                let more = icon_button(ui, Icon::More, "More controls", "More controls");
                egui::Popup::menu(&more)
                    .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        ui.set_min_width(230.0);
                        for &i in &hidden {
                            draw(ui, i, Mode::Menu);
                        }
                    });
            }
            for (i, slot) in slots.iter().enumerate().rev() {
                if shown[i] && slot.side == Side::Right {
                    draw_slot(ui, slot, i, draw);
                }
            }
            // The left-hand side, in the room that is left.
            let left = Rect::from_min_max(
                inner.min,
                Pos2::new(ui.available_rect_before_wrap().right() - GAP, inner.max.y),
            );
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(left)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    let (_, rect) = ui.allocate_space(vec2(title_width, PILL_HEIGHT));
                    let mut child = ui.new_child(
                        UiBuilder::new()
                            .id_salt("toolbar title")
                            .max_rect(rect)
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    child.add(
                        egui::Label::new(RichText::new(title).strong().color(colors::TEXT))
                            .truncate(),
                    );
                    for (i, slot) in slots.iter().enumerate() {
                        if shown[i] && slot.side == Side::Left {
                            draw_slot(ui, slot, i, draw);
                        }
                    }
                },
            );
        },
    );
    closed
}

fn draw_slot(ui: &mut Ui, slot: &Slot, i: usize, draw: &mut dyn FnMut(&mut Ui, usize, Mode)) {
    if slot.divider {
        let (rect, _) = ui.allocate_exact_size(vec2(1.0, 14.0), Sense::hover());
        ui.painter().vline(
            rect.center().x,
            rect.y_range(),
            Stroke::new(1.0, theme::TRANSPORT_OUTLINE),
        );
    }
    let (_, rect) = ui.allocate_space(vec2(slot.width, PILL_HEIGHT));
    // A child that is not allocated: `scope_builder` would move the cursor back
    // to the end of what was drawn, so the next slot would follow the content
    // rather than the slot's fixed width.
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(("toolbar slot", i))
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 0.0;
    draw(&mut child, i, Mode::Bar);
}

/// Paints the rounded capsule behind a slot's buttons.
pub fn pill(ui: &Ui, rect: Rect) {
    ui.painter().rect(
        rect,
        CornerRadius::same(PILL_HEIGHT as u8 / 2),
        theme::TRANSPORT_FILL,
        Stroke::new(1.0, theme::TRANSPORT_OUTLINE),
        StrokeKind::Inside,
    );
}

/// What a pill does.
pub struct Pill<'a> {
    pub icon: Icon,
    /// The text beside the icon, and the button's accessible name.
    pub label: &'a str,
    /// Lit with the accent colour.
    pub active: bool,
    pub enabled: bool,
    pub tip: &'a str,
}

/// How a pill's popover is reached.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Opens {
    /// The pill itself acts; a chevron beside it opens the popover.
    Chevron,
    /// Pressing the pill opens the popover.
    Itself,
}

/// What came of a pill.
#[derive(Default)]
pub struct PillOut {
    /// The pill itself was pressed (never set for [`Opens::Itself`]).
    pub pressed: bool,
}

/// A pill in a bar slot, or a section of the "…" menu, with a popover of
/// `contents`. For [`Opens::Chevron`] the popover has its own button, named
/// `more`.
pub fn pill_with_popover(
    ui: &mut Ui,
    mode: Mode,
    spec: &Pill,
    opens: Opens,
    more: &str,
    contents: impl FnOnce(&mut Ui),
) -> PillOut {
    let mut out = PillOut::default();
    match mode {
        Mode::Bar => {
            pill(ui, ui.max_rect());
            let width = ui.max_rect().width();
            let chevron = if opens == Opens::Chevron { SQUARE } else { 0.0 };
            let main = Rect::from_min_size(
                ui.max_rect().min + vec2(1.0, 1.0),
                vec2(width - chevron - 2.0, PILL_HEIGHT - 2.0),
            );
            let response = pill_button(ui, main, spec, opens == Opens::Itself);
            let popover = if opens == Opens::Chevron {
                out.pressed = response.clicked();
                let at = Rect::from_min_size(
                    Pos2::new(main.right(), main.top()),
                    vec2(chevron - 1.0, main.height()),
                );
                chevron_button(ui, at, more, spec.enabled || opens == Opens::Chevron)
            } else {
                response
            };
            egui::Popup::menu(&popover)
                .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    ui.set_min_width(220.0);
                    contents(ui);
                });
        }
        Mode::Menu => {
            ui.separator();
            if opens == Opens::Itself {
                ui.label(RichText::new(spec.label).small().color(colors::TEXT_WEAK));
            }
            if opens == Opens::Chevron {
                out.pressed = ui
                    .add_enabled(
                        spec.enabled,
                        egui::Button::new(spec.label).selected(spec.active),
                    )
                    .on_hover_text(spec.tip)
                    .clicked();
            }
            contents(ui);
        }
    }
    out
}

/// A pill holding one button, filling the slot. Says whether it was pressed.
pub fn pill_only(ui: &mut Ui, spec: &Pill) -> bool {
    let rect = ui.max_rect();
    pill(ui, rect);
    pill_button(ui, rect.shrink(1.0), spec, false).clicked()
}

/// A button with an icon and a label, painted into `rect`.
fn pill_button(ui: &mut Ui, rect: Rect, spec: &Pill, with_chevron: bool) -> Response {
    let sense = if spec.enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.allocate_rect(rect, sense);
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::Button, spec.enabled, spec.active, spec.label)
    });
    let hovered = response.hovered() && spec.enabled;
    let (fill, ink) = if spec.active {
        (theme::ACCENT, Color32::from_rgb(18, 22, 10))
    } else if !spec.enabled {
        (Color32::TRANSPARENT, colors::TEXT_WEAK.gamma_multiply(0.6))
    } else if hovered {
        (Color32::from_rgb(48, 54, 66), colors::TEXT)
    } else {
        (Color32::TRANSPARENT, Color32::from_rgb(201, 206, 218))
    };
    ui.painter().rect_filled(
        rect.shrink(1.0),
        CornerRadius::same((PILL_HEIGHT as u8 - 4) / 2),
        fill,
    );
    let icon_at = Pos2::new(rect.left() + 12.0, rect.center().y);
    paint_icon(ui.painter(), icon_at, spec.icon, ink);
    if !spec.label.is_empty() {
        let mut label = spec.label.to_owned();
        if with_chevron {
            label.push_str("  ");
        }
        ui.painter().text(
            Pos2::new(rect.left() + 24.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
            ink,
        );
    }
    if with_chevron {
        paint_chevron(
            ui.painter(),
            Pos2::new(rect.right() - 10.0, rect.center().y),
            ink,
        );
    }
    if !spec.tip.is_empty() {
        return response.on_hover_text(spec.tip);
    }
    response
}

fn chevron_button(ui: &mut Ui, rect: Rect, name: &str, enabled: bool) -> Response {
    let response = ui.allocate_rect(rect, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, name));
    let ink = if response.hovered() {
        colors::TEXT
    } else {
        colors::TEXT_WEAK
    };
    if response.hovered() {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            CornerRadius::same(PILL_HEIGHT as u8 / 2 - 2),
            Color32::from_rgb(48, 54, 66),
        );
    }
    paint_chevron(ui.painter(), rect.center(), ink);
    response.on_hover_text(name)
}

fn paint_chevron(painter: &egui::Painter, c: Pos2, ink: Color32) {
    painter.add(Shape::line(
        vec![
            c + vec2(-3.5, -1.5),
            c + vec2(0.0, 2.0),
            c + vec2(3.5, -1.5),
        ],
        Stroke::new(1.4, ink),
    ));
}

/// An icon-only button in its own round outline.
pub fn icon_button(ui: &mut Ui, icon: Icon, name: &str, tip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(SQUARE, PILL_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    let hovered = response.hovered();
    ui.painter().rect(
        rect,
        CornerRadius::same(PILL_HEIGHT as u8 / 2),
        if hovered {
            Color32::from_rgb(48, 54, 66)
        } else {
            theme::TRANSPORT_FILL
        },
        Stroke::new(1.0, theme::TRANSPORT_OUTLINE),
        StrokeKind::Inside,
    );
    paint_icon(
        ui.painter(),
        rect.center(),
        icon,
        if hovered {
            colors::TEXT
        } else {
            Color32::from_rgb(201, 206, 218)
        },
    );
    response.on_hover_text(tip)
}

/// A segment of icon buttons (a tool choice) in one pill: says which was
/// pressed.
pub fn tool_segment<T: Copy + PartialEq>(
    ui: &mut Ui,
    tools: &[(T, Icon, &str, &str)],
    current: T,
) -> Option<T> {
    pill(ui, ui.max_rect());
    let mut picked = None;
    let mut x = ui.max_rect().left() + 1.0;
    for &(tool, icon, name, tip) in tools {
        let rect = Rect::from_min_size(
            Pos2::new(x, ui.max_rect().top() + 1.0),
            vec2(SQUARE, PILL_HEIGHT - 2.0),
        );
        x += SQUARE;
        let spec = Pill {
            icon,
            label: "",
            active: tool == current,
            enabled: true,
            tip,
        };
        let response = ui.allocate_rect(rect, Sense::click());
        response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, spec.active, name));
        let (fill, ink) = if spec.active {
            (theme::ACCENT, Color32::from_rgb(18, 22, 10))
        } else if response.hovered() {
            (Color32::from_rgb(48, 54, 66), colors::TEXT)
        } else {
            (Color32::TRANSPARENT, Color32::from_rgb(201, 206, 218))
        };
        ui.painter().rect_filled(
            rect.shrink(1.0),
            CornerRadius::same((PILL_HEIGHT as u8 - 4) / 2),
            fill,
        );
        paint_icon(ui.painter(), rect.center(), icon, ink);
        if response.on_hover_text(tip).clicked() {
            picked = Some(tool);
        }
    }
    picked
}

/// Width of a [`tool_segment`] of `n` tools.
pub const fn segment_width(n: usize) -> f32 {
    n as f32 * SQUARE + 2.0
}

/// Draws `icon` centred on `c`, about 15 points across.
pub fn paint_icon(painter: &egui::Painter, c: Pos2, icon: Icon, ink: Color32) {
    let stroke = Stroke::new(1.5, ink);
    // Icons are drawn in a 24-unit box.
    let k = 15.0 / 24.0;
    let p = |x: f32, y: f32| c + vec2((x - 12.0) * k, (y - 12.0) * k);
    let line = |points: &[(f32, f32)]| {
        painter.add(Shape::line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    let closed = |points: &[(f32, f32)]| {
        painter.add(Shape::closed_line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    match icon {
        Icon::Pen => closed(&[
            (4.0, 20.0),
            (5.0, 16.0),
            (16.0, 5.0),
            (19.0, 8.0),
            (8.0, 19.0),
        ]),
        Icon::Line => {
            line(&[(5.0, 19.0), (19.0, 5.0)]);
            painter.circle_stroke(p(5.0, 19.0), 2.2, stroke);
            painter.circle_stroke(p(19.0, 5.0), 2.2, stroke);
        }
        Icon::Cursor => closed(&[(6.0, 4.0), (18.0, 11.0), (12.5, 12.5), (10.0, 18.0)]),
        Icon::Loop => {
            line(&[(7.0, 10.0), (7.0, 8.0), (9.0, 6.0), (20.0, 6.0)]);
            line(&[(17.0, 3.0), (20.0, 6.0), (17.0, 9.0)]);
            line(&[(17.0, 14.0), (17.0, 16.0), (15.0, 18.0), (4.0, 18.0)]);
            line(&[(7.0, 15.0), (4.0, 18.0), (7.0, 21.0)]);
        }
        Icon::Dice => {
            painter.rect_stroke(
                Rect::from_center_size(c, vec2(12.0, 12.0)),
                3.0,
                stroke,
                StrokeKind::Middle,
            );
            for (x, y) in [(9.0, 9.0), (15.0, 15.0), (15.0, 9.0), (9.0, 15.0)] {
                painter.circle_filled(p(x, y), 0.9, ink);
            }
        }
        Icon::Lanes => {
            for (y, x) in [(6.0, 9.0), (12.0, 15.0), (18.0, 7.0)] {
                line(&[(4.0, y), (20.0, y)]);
                painter.circle_filled(p(x, y), 2.0, ink);
            }
        }
        Icon::Tag => {
            closed(&[
                (3.0, 12.0),
                (3.0, 4.0),
                (11.0, 4.0),
                (20.0, 13.0),
                (12.0, 21.0),
            ]);
            painter.circle_filled(p(7.5, 8.5), 1.2, ink);
        }
        Icon::Wave => {
            for (x, half) in [
                (4.0, 1.0),
                (8.0, 5.0),
                (12.0, 8.0),
                (16.0, 4.0),
                (20.0, 2.0),
            ] {
                line(&[(x, 12.0 - half), (x, 12.0 + half)]);
            }
        }
        Icon::Close => {
            line(&[(7.0, 7.0), (17.0, 17.0)]);
            line(&[(17.0, 7.0), (7.0, 17.0)]);
        }
        Icon::Help => {
            painter.circle_stroke(c, 7.0, stroke);
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                "?",
                egui::FontId::proportional(10.0),
                ink,
            );
        }
        Icon::More => {
            for x in [4.0, 12.0, 20.0] {
                painter.circle_filled(p(x, 12.0), 1.4, ink);
            }
        }
    }
}

/// The hint popover's content, for a bar's `?` slot: `hint` as wrapped text.
pub fn hint_popover(ui: &mut Ui, mode: Mode, hint: &str) {
    let spec = Pill {
        icon: Icon::Help,
        label: "",
        active: false,
        enabled: true,
        tip: "How to use this editor",
    };
    match mode {
        Mode::Bar => {
            let rect = Rect::from_min_size(ui.max_rect().min, vec2(SQUARE, PILL_HEIGHT));
            let response = icon_button_at(ui, rect, &spec, "Hints");
            egui::Popup::menu(&response).show(|ui| {
                ui.set_max_width(280.0);
                ui.label(RichText::new(hint).color(colors::TEXT_WEAK));
            });
        }
        Mode::Menu => {
            ui.separator();
            ui.label(RichText::new("Hints").small().color(colors::TEXT_WEAK));
            ui.add(egui::Label::new(RichText::new(hint).color(colors::TEXT_WEAK)).wrap());
        }
    }
}

fn icon_button_at(ui: &mut Ui, rect: Rect, spec: &Pill, name: &str) -> Response {
    let response = ui.allocate_rect(rect, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    ui.painter().rect(
        rect,
        CornerRadius::same(PILL_HEIGHT as u8 / 2),
        if response.hovered() {
            Color32::from_rgb(48, 54, 66)
        } else {
            theme::TRANSPORT_FILL
        },
        Stroke::new(1.0, theme::TRANSPORT_OUTLINE),
        StrokeKind::Inside,
    );
    paint_icon(ui.painter(), rect.center(), spec.icon, colors::TEXT);
    response.on_hover_text(spec.tip)
}

/// A read-only chip in a pill, such as the snap step: `text` in a slot.
pub fn chip(ui: &mut Ui, mode: Mode, name: &str, text: &str, tip: &str) {
    match mode {
        Mode::Bar => {
            let rect = ui.max_rect();
            pill(ui, rect);
            let response = ui.allocate_rect(rect, Sense::click());
            let full = format!("{name}: {text}");
            response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &full));
            ui.painter()
                .with_clip_rect(rect.shrink2(vec2(8.0, 0.0)))
                .text(
                    Pos2::new(rect.left() + 10.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &full,
                    egui::FontId::proportional(12.0),
                    colors::TEXT,
                );
            response.on_hover_text(tip);
        }
        Mode::Menu => {
            ui.separator();
            ui.label(format!("{name}: {text}")).on_hover_text(tip);
        }
    }
}

/// A label with no pill, such as the count of selected notes.
pub fn status(ui: &mut Ui, mode: Mode, text: &str, strong: bool) {
    let color = if strong {
        colors::TEXT
    } else {
        colors::TEXT_WEAK
    };
    match mode {
        Mode::Bar => {
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).truncate());
        }
        Mode::Menu => {
            ui.separator();
            ui.label(RichText::new(text).color(color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOTS: [Slot; 4] = [
        Slot::left(100.0, 1),
        Slot::left(100.0, 3),
        Slot::left(100.0, 2),
        Slot::right(26.0, 4),
    ];

    #[test]
    fn everything_shows_when_there_is_room() {
        assert_eq!(fit(&SLOTS, 1000.0), vec![true; 4]);
    }

    #[test]
    fn the_highest_keep_values_fold_away_first() {
        // Room for two 100-wide slots and the "..." button, not three.
        let room = 2.0 * (100.0 + GAP) + MORE_WIDTH + GAP + 1.0;
        assert_eq!(fit(&SLOTS, room), vec![true, false, true, false]);
    }

    #[test]
    fn nothing_shown_ever_overflows_the_room() {
        for room in (0..600).step_by(7) {
            let room = room as f32;
            let shown = fit(&SLOTS, room);
            let used: f32 = SLOTS
                .iter()
                .zip(&shown)
                .filter(|(_, shown)| **shown)
                .map(|(s, _)| s.width + GAP)
                .sum();
            let hidden = shown.iter().any(|s| !s);
            let more = if hidden { MORE_WIDTH + GAP } else { 0.0 };
            assert!(
                used + more <= room.max(MORE_WIDTH + GAP),
                "{room}: {shown:?}"
            );
        }
    }
}
