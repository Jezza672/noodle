//! The strip along the top of the piano roll, clip editor and sample view: a
//! readable title on the left and a Close button that is always on the right,
//! however long the hint between them is.

use egui::{Rect, RichText, Ui, vec2};

use crate::theme::timeline as colors;

/// How tall the strip is.
pub const HEIGHT: f32 = 26.0;

/// Draws the strip into `rect`: `title`, then whatever `middle` adds, then
/// `hint` in the room that is left (cut short rather than pushing Close out of
/// view). Says whether Close was pressed.
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    title: &str,
    hint: &str,
    middle: impl FnOnce(&mut Ui),
) -> bool {
    let mut closed = false;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(8.0, 2.0)))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            closed = ui.button("Close").clicked();
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // The panel's own text colour is too dim on its fill for a title.
                ui.label(RichText::new(title).strong().color(colors::TEXT));
                ui.separator();
                middle(ui);
                if !hint.is_empty() {
                    ui.add(egui::Label::new(RichText::new(hint).color(colors::TEXT_WEAK)).truncate());
                }
            });
        },
    );
    closed
}
