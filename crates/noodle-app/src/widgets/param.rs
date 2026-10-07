//! The parameter field: one widget for every [`ParamInfo`], in the style of
//! Blender's number fields.
//!
//! - **Continuous and unlabelled stepped parameters** are a slider field with
//!   the name on the left, the value on the right and a fill showing where the
//!   value sits along its taper. Drag sideways to change it (hold Shift for
//!   fine control), click to type a value, and press Backspace while hovering
//!   to reset it to the default. The right-click menu also resets.
//! - **Stepped parameters with labels** are a drop-down.
//!
//! The field doesn't own the value: it's given the current value each frame
//! and reports what the user asked for as a [`ParamEdit`], which the caller
//! turns into a command. [`Gesture`] says whether the edit is part of a drag,
//! so a whole drag can become one undo step.

use egui::{
    Align2, ComboBox, CursorIcon, FontId, Id, Key, Modifiers, Rect, Response, Sense, StrokeKind,
    TextEdit, TextStyle, Ui, WidgetInfo, pos2, vec2,
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
        }
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
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "for the node editor, which lands separately")
    )]
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

    fn show_choice(self, ui: &mut Ui, labels: &[std::borrow::Cow<'static, str>]) -> ParamOutput {
        let id = ui.make_persistent_id(self.id_salt);
        let size = self.size(ui);
        let current = taper::clamp(self.info, self.value) as usize;
        let mut selected = current;

        let inner = ui.allocate_ui_with_layout(
            size,
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if !self.compact && !self.label.is_empty() {
                    ui.label(egui::RichText::new(self.label).font(self.font(ui)));
                }
                let selected_text =
                    egui::RichText::new(labels.get(current).map_or("", |l| l.as_ref()))
                        .font(self.font(ui));
                ComboBox::from_id_salt(id)
                    .width(ui.available_width())
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for (index, label) in labels.iter().enumerate() {
                            ui.selectable_value(&mut selected, index, label.as_ref());
                        }
                    })
                    .response
            },
        );

        let mut response = inner.inner;
        let mut edit = None;
        if selected != current {
            edit = Some(ParamEdit::Set(selected as f32));
            response.mark_changed();
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
        ParamOutput {
            response: response.on_hover_text(self.tooltip()),
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
