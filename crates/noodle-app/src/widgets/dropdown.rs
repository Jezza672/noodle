//! A drop-down list that replaces `egui::ComboBox`.
//!
//! egui's combo box opens through its popup machinery, which did not open on
//! macOS. This one is a button and an `Area` it draws itself, and closes on
//! its own terms: a click inside the list, a press outside it, or Escape.
//! The API follows `ComboBox`, and the button reports itself as a combo box
//! to accessibility, so tests and `labelled_by` work the same.

use egui::{Id, InnerResponse, Key, Ui, WidgetInfo, WidgetType};

pub struct Dropdown {
    id: Id,
    text: String,
    width: Option<f32>,
}

impl Dropdown {
    pub fn from_id_salt(salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        Self {
            id: Id::new(salt),
            text: String::new(),
            width: None,
        }
    }

    pub fn selected_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Draw the button, and while it is open the list, filled by `add`.
    /// The inner value is `None` while the list is closed.
    pub fn show_ui<R>(
        self,
        ui: &mut Ui,
        add: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<Option<R>> {
        let id = ui.make_persistent_id(self.id);
        let open_id = id.with("open");
        let width_id = id.with("width");
        let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
        let mut button = egui::Button::new(format!("{} ▾", self.text));
        if let Some(width) = self.width {
            button = button.min_size(egui::vec2(width, 0.0));
        }
        let response = ui.add(button);
        response.widget_info(|| WidgetInfo {
            current_text_value: Some(self.text.clone()),
            ..WidgetInfo::new(WidgetType::ComboBox)
        });
        // The button's own text is the value; a name comes from `labelled_by`.
        ui.ctx()
            .accesskit_node_builder(response.id, |node| node.clear_label());
        let was_open = open;
        if response.clicked() {
            open = !open;
        }
        let mut inner = None;
        if open {
            let rect = response.rect;
            let list = egui::Area::new(id.with("list"))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.left_bottom())
                .constrain(true)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        // Rows never wrap, and the list never gets narrower
                        // than it has been while open, so a hovered row
                        // can't change its width and jump.
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        let widest = ui.data(|d| d.get_temp::<f32>(width_id)).unwrap_or(0.0);
                        ui.set_min_width(rect.width().max(widest));
                        egui::ScrollArea::vertical()
                            .max_height(260.0)
                            .show(ui, |ui| {
                                // Justified, so a row is the whole width.
                                let inner = ui
                                    .with_layout(
                                        egui::Layout::top_down_justified(egui::Align::LEFT),
                                        add,
                                    )
                                    .inner;
                                let width = ui.min_rect().width();
                                ui.data_mut(|d| d.insert_temp(width_id, width.max(widest)));
                                inner
                            })
                            .inner
                    })
                });
            inner = Some(list.inner.inner);
            let (pressed_outside, released_inside) = ui.input(|i| {
                let pos = i.pointer.interact_pos();
                let inside = pos.is_some_and(|p| list.response.rect.contains(p));
                (
                    i.pointer.any_pressed() && pos.is_some_and(|p| !inside && !rect.contains(p)),
                    i.pointer.any_released() && inside,
                )
            });
            // The frame that opens it has no settled list to test against.
            let closes =
                pressed_outside || released_inside || ui.input(|i| i.key_pressed(Key::Escape));
            if was_open && closes {
                open = false;
            }
        }
        ui.data_mut(|d| {
            d.insert_temp(open_id, open);
            if !open {
                d.remove::<f32>(width_id);
            }
        });
        InnerResponse::new(inner, response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    const ITEMS: [&str; 3] = [
        "Short",
        "A rather long item that would wrap in a narrow list",
        "Medium length item",
    ];

    #[derive(Default)]
    struct State {
        rows: Vec<egui::Rect>,
    }

    fn rig() -> egui_kittest::Harness<'static, State> {
        crate::testkit::harness(State::default(), egui::vec2(220.0, 400.0), |ui, state| {
            Dropdown::from_id_salt("t")
                .selected_text("Short")
                .width(60.0)
                .show_ui(ui, |ui| {
                    state.rows = ITEMS
                        .iter()
                        .map(|item| ui.selectable_label(false, *item).rect)
                        .collect();
                });
        })
    }

    #[test]
    fn rows_keep_their_size_and_stay_on_one_line_when_hovered() {
        let mut h = rig();
        h.get_by_role(egui::accesskit::Role::ComboBox).click();
        h.run();
        let before = h.state().rows.clone();
        assert_eq!(before.len(), 3);
        // One line each: no taller than a button.
        let line = before[0].height();
        assert!(before.iter().all(|r| (r.height() - line).abs() < 0.5));
        for item in ITEMS {
            h.get_by_label(item).hover();
            h.run();
            assert_eq!(h.state().rows, before, "hovering {item}");
        }
    }
}
