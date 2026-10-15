//! The parameter field: one widget for every [`ParamInfo`], in the style of
//! Blender's number fields.
//!
//! - **Continuous and unlabelled stepped parameters** are a slider field with
//!   the name on the left, the value on the right and a fill showing where the
//!   value sits along its taper. Drag sideways to change it (hold Shift for
//!   fine control), click to type a value, and press Backspace while hovering
//!   to reset it to the default. The right-click menu also resets.
//! - **Stepped parameters with labels** are a toggle button when they are Off
//!   and On, radio-style segments when a few short options fit the row, and
//!   otherwise a drop-down.
//!
//! The field doesn't own the value: it's given the current value each frame
//! and reports what the user asked for as a [`ParamEdit`], which the caller
//! turns into a command. [`Gesture`] says whether the edit is part of a drag,
//! so a whole drag can become one undo step.

use egui::{
    Align2, CursorIcon, FontId, Id, Key, Modifiers, Rect, Response, Sense, StrokeKind, TextEdit,
    TextStyle, Ui, WidgetInfo, pos2, vec2,
};
use noodle_core::{Command, NodeId};
use noodle_engine::{ParamInfo, ParamKind};

use super::format::{format_value, parse_value};
use super::taper;
use crate::session::Edit;

/// What the user asked to do to the parameter this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamEdit {
    /// Set the parameter to this value, already clamped to its range.
    Set(f32),
    /// Put the parameter back to its default (`SetParam` with `None`).
    Reset,
}

/// Where the field is in a drag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Gesture {
    #[default]
    Idle,
    /// A drag is in progress, and any edit this frame is part of it.
    Dragging,
    /// The drag ended this frame. Any edit this frame is its last step, and
    /// the drag's edits should now be closed into one undo step.
    Released,
}

#[derive(Debug)]
pub struct ParamOutput {
    pub response: Response,
    pub edit: Option<ParamEdit>,
    pub gesture: Gesture,
}

impl ParamOutput {
    /// Whether `edit` belongs to a drag, to be grouped with the rest of it.
    pub fn in_drag(&self) -> bool {
        self.gesture != Gesture::Idle
    }

    /// The session edits for this frame, for parameter `key` of `node`: a
    /// drag's steps become one undo step, closed when it's released.
    pub fn edits(&self, node: NodeId, key: &str) -> Vec<Edit> {
        let mut edits = Vec::new();
        if let Some(edit) = self.edit {
            let command = Command::SetParam {
                node,
                key: key.to_owned(),
                value: match edit {
                    ParamEdit::Set(value) => Some(value),
                    ParamEdit::Reset => None,
                },
            };
            edits.push(if self.in_drag() {
                Edit::Drag(command)
            } else {
                Edit::Apply(command)
            });
        }
        if self.gesture == Gesture::Released {
            edits.push(Edit::EndDrag);
        }
        edits
    }
}

/// What a wired parameter is doing, for the field to show instead of (or on
/// top of) its own value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Live {
    /// The effective value now.
    pub value: f32,
    /// The lowest and highest effective values over the last few seconds, so
    /// the depth of the modulation shows against the parameter's range.
    pub min: f32,
    pub max: f32,
}

/// A field for one parameter. Build it each frame, then [`show`](Self::show) it.
#[must_use = "call `show` to draw the field"]
pub struct ParamField<'a> {
    id_salt: Id,
    label: &'a str,
    info: &'a ParamInfo,
    value: f32,
    compact: bool,
    width: Option<f32>,
    height: Option<f32>,
    zoom: f32,
    live: Option<Live>,
    /// Whether the wire replaces the value (the fill follows the live value)
    /// rather than offsetting it (the fill stays at the base).
    replaced: bool,
}

impl<'a> ParamField<'a> {
    /// `label` is shown on the field, and also identifies it, so give fields
    /// in the same `Ui` different labels or use [`id_salt`](Self::id_salt).
    pub fn new(label: &'a str, info: &'a ParamInfo, value: f32) -> Self {
        Self {
            id_salt: Id::new(label),
            label,
            info,
            value,
            compact: false,
            width: None,
            height: None,
            zoom: 1.0,
            live: None,
            replaced: false,
        }
    }

    /// Marks the parameter as wired. The field draws the signal's range as
    /// two ticks (the lowest and highest values lately) and a marker at the
    /// value now. If the wire `replaced` the value, the fill and the number
    /// follow the live value, which `value` is then expected to be; otherwise
    /// they show the base value that the wire moves.
    pub fn live(mut self, live: Option<Live>, replaced: bool) -> Self {
        self.live = live;
        self.replaced = replaced;
        self
    }

    /// Distinguishes fields that share a label, e.g. the parameter's key.
    pub fn id_salt(mut self, salt: impl egui::AsId) -> Self {
        self.id_salt = Id::new(salt);
        self
    }

    /// The smaller version drawn on a node's body: smaller text, and
    /// drop-downs without their label.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Defaults to the available width.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn show(self, ui: &mut Ui) -> ParamOutput {
        match &self.info.kind {
            ParamKind::Stepped { labels } if !labels.is_empty() => self.show_choice(ui, labels),
            _ => self.show_slider(ui),
        }
    }

    /// Scales the text and height, for drawing inside a zoomed canvas.
    pub fn zoom(mut self, zoom: f32) -> Self {
        self.zoom = zoom;
        self
    }

    /// Shows the field filling `rect`, in screen space, e.g. a row of a node
    /// body that the editor has already laid out and scaled. Combine with
    /// [`zoom`](Self::zoom) so the text scales too.
    pub fn show_at(mut self, ui: &mut Ui, rect: Rect) -> ParamOutput {
        self.width = Some(rect.width());
        self.height = Some(rect.height());
        let builder = egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center));
        ui.scope_builder(builder, |ui| self.show(ui)).inner
    }

    fn size(&self, ui: &Ui) -> egui::Vec2 {
        let height = ui.spacing().interact_size.y;
        let height = if self.compact { height - 2.0 } else { height };
        let height = self.height.unwrap_or(height * self.zoom);
        vec2(self.width.unwrap_or_else(|| ui.available_width()), height)
    }

    fn font(&self, ui: &Ui) -> FontId {
        let style = if self.compact {
            TextStyle::Small
        } else {
            TextStyle::Body
        };
        let mut font = style.resolve(ui.style());
        font.size *= self.zoom;
        font
    }

    fn show_slider(self, ui: &mut Ui) -> ParamOutput {
        let id = ui.make_persistent_id(self.id_salt);
        let size = self.size(ui);
        let typing_id = id.with("typing");

        if let Some(typing) = ui.data(|d| d.get_temp::<Typing>(typing_id)) {
            return self.show_typing(ui, typing_id, typing, size);
        }

        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let mut response = ui.interact(rect, id, Sense::click_and_drag());
        let mut edit = None;
        let mut gesture = Gesture::Idle;

        let drag_id = id.with("drag");
        if response.drag_started() {
            let start = taper::to_normalized(self.info, self.value);
            ui.data_mut(|d| d.insert_temp(drag_id, start));
        }
        if response.dragged() {
            gesture = Gesture::Dragging;
            let fine = if ui.input(|i| i.modifiers.shift) {
                0.1
            } else {
                1.0
            };
            let travel = response.drag_delta().x / rect.width().max(1.0) * fine;
            // Track the unrounded position, so slow drags on a stepped or
            // long log range still add up to a step.
            let position = ui.data_mut(|d| {
                let position = d.get_temp_mut_or(drag_id, 0.0f32);
                *position = (*position + travel).clamp(0.0, 1.0);
                *position
            });
            let value = taper::from_normalized(self.info, position);
            if value != self.value {
                edit = Some(ParamEdit::Set(value));
                response.mark_changed();
            }
        }
        if response.drag_stopped() {
            gesture = Gesture::Released;
            ui.data_mut(|d| d.remove::<f32>(drag_id));
        }

        // Keyboard and assistive tech: arrows (or AccessKit's increment and
        // decrement) nudge the value while the field has focus, so a mouse is
        // never required.
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        ..Default::default()
                    },
                )
            });
        }
        let nudges = ui.input(|i| {
            use egui::accesskit::Action;
            let mut nudges = i.num_accesskit_action_requests(id, Action::Increment) as i32
                - i.num_accesskit_action_requests(id, Action::Decrement) as i32;
            if focused {
                nudges += i.num_presses(Key::ArrowRight) as i32;
                nudges -= i.num_presses(Key::ArrowLeft) as i32;
            }
            nudges
        });
        if nudges != 0 {
            let value = self.nudged(nudges, ui.input(|i| i.modifiers.shift));
            if value != self.value {
                edit = Some(ParamEdit::Set(value));
                response.mark_changed();
            }
        }
        // A focused field also counts as clicked when Enter or Space is
        // pressed, so this starts typing from the keyboard too.
        if response.clicked() {
            let text = format_value(self.info, self.value);
            ui.data_mut(|d| {
                d.insert_temp(
                    typing_id,
                    Typing {
                        original: text.clone(),
                        text,
                        focused: false,
                    },
                )
            });
            ui.ctx().request_repaint();
        }

        if wants_reset(ui, &response) {
            edit = Some(ParamEdit::Reset);
        }
        response.context_menu(|ui| {
            if ui.button("Reset to default").clicked() {
                edit = Some(ParamEdit::Reset);
                ui.close();
            }
        });

        let enabled = ui.is_enabled();
        response.widget_info(|| {
            let mut info = WidgetInfo::slider(enabled, f64::from(self.value), self.label);
            info.current_text_value = Some(format_value(self.info, self.value));
            info
        });
        ui.ctx().accesskit_node_builder(id, |node| {
            use egui::accesskit::Action;
            node.set_min_numeric_value(f64::from(self.info.min));
            node.set_max_numeric_value(f64::from(self.info.max));
            node.add_action(Action::Increment);
            node.add_action(Action::Decrement);
        });
        self.paint_slider(ui, rect, &response);
        let response = response
            .on_hover_cursor(CursorIcon::ResizeHorizontal)
            .on_hover_text(self.tooltip());
        ParamOutput {
            response,
            edit,
            gesture,
        }
    }

    fn show_typing(
        self,
        ui: &mut Ui,
        typing_id: Id,
        mut typing: Typing,
        size: egui::Vec2,
    ) -> ParamOutput {
        let response = ui.add_sized(
            size,
            TextEdit::singleline(&mut typing.text)
                .id(typing_id.with("text"))
                .font(self.font(ui)),
        );
        // egui drops focus silently if the text box stops being drawn while
        // focused, e.g. its node is zoomed out of view or removed by undo.
        // `lost_focus` never fires then, so the edit would stay open as a
        // dead text box. It counts as cancelled instead, since the field may
        // no longer even be the same one.
        let abandoned = typing.focused && !response.has_focus() && !response.lost_focus();
        if !typing.focused {
            response.request_focus();
            typing.focused = true;
        }

        let mut edit = None;
        if abandoned {
            ui.data_mut(|d| d.remove::<Typing>(typing_id));
            ui.ctx().request_repaint();
        } else if response.lost_focus() {
            let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
            if !cancelled && typing.text != typing.original {
                edit = parse_value(self.info, &typing.text).map(ParamEdit::Set);
            }
            ui.data_mut(|d| d.remove::<Typing>(typing_id));
        } else {
            ui.data_mut(|d| d.insert_temp(typing_id, typing));
        }
        ParamOutput {
            response,
            edit,
            gesture: Gesture::Idle,
        }
    }

    fn paint_slider(&self, ui: &Ui, rect: Rect, response: &Response) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let visuals = if ui.is_enabled() {
            *ui.style().interact(response)
        } else {
            ui.visuals().widgets.noninteractive
        };
        let painter = ui.painter_at(rect);
        let radius = visuals.corner_radius;
        painter.rect_filled(rect, radius, visuals.bg_fill);

        let t = taper::to_normalized(self.info, self.value);
        if t > 0.0 {
            let fill =
                Rect::from_min_max(rect.min, pos2(rect.lerp_inside(vec2(t, 0.0)).x, rect.max.y));
            let mut color = ui.visuals().selection.bg_fill;
            if !ui.is_enabled() {
                color = color.gamma_multiply(0.4);
            }
            painter.rect_filled(fill, radius, color);
        }
        if let Some(live) = self.live {
            self.paint_live(ui, &painter, rect, live);
        }
        let stroke = if response.has_focus() {
            ui.visuals().selection.stroke
        } else {
            visuals.bg_stroke
        };
        painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);

        let font = self.font(ui);
        let color = visuals.text_color();
        let pad = ui.spacing().button_padding.x;
        let value = format_value(self.info, self.value);
        if self.label.is_empty() {
            painter.text(rect.center(), Align2::CENTER_CENTER, value, font, color);
        } else {
            let left = rect.left_center() + vec2(pad, 0.0);
            let right = rect.right_center() - vec2(pad, 0.0);
            painter.text(left, Align2::LEFT_CENTER, self.label, font.clone(), color);
            painter.text(right, Align2::RIGHT_CENTER, value, font, color);
        }
    }

    /// The marks of a wired parameter: a tick at the lowest and highest
    /// values lately, and a marker at the value now.
    fn paint_live(&self, ui: &Ui, painter: &egui::Painter, rect: Rect, live: Live) {
        let x = |value: f32| rect.left() + taper::to_normalized(self.info, value) * rect.width();
        let color = ui.visuals().strong_text_color();
        let tick = egui::Stroke::new(1.5 * self.zoom.max(0.5), color);
        for value in [live.min, live.max] {
            let x = x(value);
            painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], tick);
        }
        // A bar between the ticks shows the swing at a glance.
        let (low, high) = (x(live.min), x(live.max));
        if high - low > 1.0 {
            let y = rect.bottom() - 2.0 * self.zoom.max(0.5);
            painter.line_segment([pos2(low, y), pos2(high, y)], tick);
        }
        if !self.replaced {
            // The fill is the base here, so the live value gets its own mark.
            painter.circle_filled(
                pos2(x(live.value), rect.center().y),
                2.5 * self.zoom.max(0.5),
                color,
            );
        }
    }

    fn show_choice(self, ui: &mut Ui, labels: &[std::borrow::Cow<'static, str>]) -> ParamOutput {
        let id = ui.make_persistent_id(self.id_salt);
        let size = self.size(ui);
        let current = taper::clamp(self.info, self.value) as usize;
        let font = self.font(ui);
        let mut picked = None;
        let mut resets = false;

        // Whatever is left of the row once the name has been drawn.
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        let mut inner = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let ui = &mut inner;
        let mut name_width = 0.0;
        // A toggle is named by its own button.
        if !self.compact && !self.label.is_empty() && !ChoiceStyle::is_toggle(labels) {
            let label = ui.label(egui::RichText::new(self.label).font(font.clone()));
            name_width = label.rect.width() + ui.spacing().item_spacing.x;
        }
        let control = Rect::from_min_size(
            pos2(rect.left() + name_width, rect.top()),
            vec2((rect.width() - name_width).max(1.0), rect.height()),
        );

        let style = ChoiceStyle::of(ui, labels, &font, control.width());
        let on = |label: &str| label.eq_ignore_ascii_case("on");
        let mut responses = Vec::new();
        match style {
            ChoiceStyle::Toggle => {
                // On or off: one button, lit while on, named for the setting.
                let on_index = if on(&labels[1]) { 1 } else { 0 };
                let text = if self.label.is_empty() {
                    labels[on_index].as_ref()
                } else {
                    self.label
                };
                let button = ui.put(
                    control,
                    egui::Button::selectable(
                        current == on_index,
                        egui::RichText::new(text).font(font.clone()),
                    )
                    .min_size(vec2(0.0, 0.0)),
                );
                if button.clicked() {
                    picked = Some(1 - current.min(1));
                }
                responses.push(button);
            }
            ChoiceStyle::Segments => {
                // A few options: all of them at once, the chosen one lit.
                let width = control.width() / labels.len() as f32;
                for (index, label) in labels.iter().enumerate() {
                    let segment = Rect::from_min_size(
                        pos2(control.left() + index as f32 * width, control.top()),
                        vec2(width, control.height()),
                    );
                    let button = ui.put(
                        segment,
                        egui::Button::selectable(
                            index == current,
                            egui::RichText::new(label.as_ref()).font(font.clone()),
                        )
                        .min_size(vec2(0.0, 0.0)),
                    );
                    if button.clicked() {
                        picked = Some(index);
                    }
                    responses.push(button);
                }
            }
            ChoiceStyle::List => {
                let text = labels.get(current).map_or("", |l| l.as_ref());
                let (button, chosen) = dropdown(ui, id, control, text, labels, current);
                picked = chosen;
                responses.push(button);
            }
        }

        let mut edit = None;
        let mut response = response;
        if let Some(index) = picked.filter(|&index| index != current) {
            edit = Some(ParamEdit::Set(index as f32));
            response.mark_changed();
        }
        // The buttons cover the row, so ask where the pointer is, not who
        // is hovered.
        let targeted = responses.iter().any(Response::has_focus)
            || (ui.rect_contains_pointer(rect) && ui.memory(|m| m.focused()).is_none());
        resets |= targeted && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace));
        for button in &responses {
            button.context_menu(|ui| {
                if ui.button("Reset to default").clicked() {
                    resets = true;
                    ui.close();
                }
            });
        }
        if resets {
            edit = Some(ParamEdit::Reset);
        }
        let tooltip = self.tooltip();
        for button in &responses {
            button.clone().on_hover_text(tooltip.clone());
        }
        ParamOutput {
            response: response.on_hover_text(tooltip),
            edit,
            gesture: Gesture::Idle,
        }
    }

    /// The value `nudges` keyboard steps away: one step for stepped
    /// parameters, else a hundredth of the travel (a thousandth with Shift).
    fn nudged(&self, nudges: i32, fine: bool) -> f32 {
        if taper::is_stepped(self.info) {
            return taper::clamp(self.info, self.value + nudges as f32);
        }
        let step = if fine { 0.001 } else { 0.01 };
        let position = taper::to_normalized(self.info, self.value) + nudges as f32 * step;
        taper::from_normalized(self.info, position)
    }

    fn tooltip(&self) -> String {
        let info = self.info;
        let mut text = String::new();
        if !self.label.is_empty() {
            text.push_str(self.label);
            text.push('\n');
        }
        text.push_str(&format!("Default: {}", format_value(info, info.default)));
        if !matches!(&info.kind, ParamKind::Stepped { labels } if !labels.is_empty()) {
            text.push_str(&format!(
                "\nRange: {} to {}\nDrag or use the arrow keys to change, Shift for fine control, click \
                 or Enter to type, Backspace to reset",
                format_value(info, info.min),
                format_value(info, info.max)
            ));
        }
        text
    }
}

/// How a choice between named options is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChoiceStyle {
    /// Off and On: one button that is lit while on.
    Toggle,
    /// A few short options, side by side.
    Segments,
    /// Too many (or too long) to show at once: a button that opens a list.
    List,
}

impl ChoiceStyle {
    /// The most options shown side by side.
    const MAX_SEGMENTS: usize = 4;

    /// Exactly Off and On, in either order.
    fn is_toggle(labels: &[std::borrow::Cow<'static, str>]) -> bool {
        labels.len() == 2
            && (labels[0].eq_ignore_ascii_case("off") && labels[1].eq_ignore_ascii_case("on")
                || labels[0].eq_ignore_ascii_case("on") && labels[1].eq_ignore_ascii_case("off"))
    }

    fn of(ui: &Ui, labels: &[std::borrow::Cow<'static, str>], font: &FontId, width: f32) -> Self {
        if Self::is_toggle(labels) {
            return Self::Toggle;
        }
        if labels.len() > Self::MAX_SEGMENTS {
            return Self::List;
        }
        // Each option needs its text and the button's padding.
        let pad = 2.0 * ui.spacing().button_padding.x;
        let needed: f32 = labels
            .iter()
            .map(|label| {
                ui.painter()
                    .layout_no_wrap(label.to_string(), font.clone(), egui::Color32::WHITE)
                    .size()
                    .x
                    + pad
            })
            .sum();
        if needed <= width {
            Self::Segments
        } else {
            Self::List
        }
    }
}

/// A button showing the chosen option that opens a list under it. This is
/// not egui's `ComboBox`: the list is a plain area that closes itself, so it
/// does not depend on how the platform delivers a click (the combo box did
/// not open for some Mac users). Returns the button and the option chosen
/// this frame.
fn dropdown(
    ui: &mut Ui,
    id: Id,
    rect: Rect,
    text: &str,
    labels: &[std::borrow::Cow<'static, str>],
    current: usize,
) -> (Response, Option<usize>) {
    let mut picked = None;
    let inner = ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        crate::widgets::Dropdown::from_id_salt(id)
            .selected_text(text)
            .width(rect.width())
            .show_ui(ui, |ui| {
                for (index, label) in labels.iter().enumerate() {
                    if ui
                        .selectable_label(index == current, label.as_ref())
                        .clicked()
                    {
                        picked = Some(index);
                    }
                }
            })
            .response
    });
    (inner.inner, picked)
}

/// A field that's being typed into.
#[derive(Clone, Debug)]
struct Typing {
    text: String,
    /// What the field showed when typing started. Committing it unchanged
    /// leaves the value alone, rather than rounding it to what was shown.
    original: String,
    focused: bool,
}

/// Backspace resets a field while the pointer is over it (and nothing else is
/// being typed into) or while it has keyboard focus.
fn wants_reset(ui: &Ui, response: &Response) -> bool {
    let focus = ui.memory(|m| m.focused());
    let targeted = response.has_focus() || (response.hovered() && focus.is_none());
    targeted && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace))
}
