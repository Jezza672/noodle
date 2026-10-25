//! The body of a modal dialog.
//!
//! `egui::Modal` is not a scrolling container. Its area is clipped to the
//! window (`egui-0.36.2/src/containers/area.rs:630`), so a dialog with more
//! in it than the window has loses the bottom of itself: the buttons at the
//! end are drawn where nothing can reach them, and no wheel brings them
//! back.
//!
//! [`modal`] gives the dialog's area the window's size to measure itself
//! with, so egui's first sizing pass has room, and puts the body in a scroll
//! area, so the dialog is never taller than the window and everything in it
//! can be reached.

use egui::{Id, Ui};

/// How much room is left between the dialog and the window's edge.
const MARGIN: f32 = 24.0;

/// A modal dialog whose body scrolls when it does not fit the window.
///
/// The scroll bar is hidden, like the sequencer's and the properties
/// panel's: a solid one would take width from the body, and a floating one
/// would lie over it. The wheel over the dialog scrolls it.
pub fn modal(ctx: &egui::Context, id: &str, add: impl FnOnce(&mut Ui)) -> egui::ModalResponse<()> {
    let id = Id::new(id);
    let area = egui::Modal::default_area(id).default_size(ctx.content_rect().size());
    egui::Modal::new(id).area(area).show(ctx, |ui| {
        let height = (ctx.content_rect().height() - MARGIN).max(0.0);
        egui::ScrollArea::vertical()
            .id_salt("body")
            .max_height(height)
            .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, add);
    })
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// The window the dialog has to fit in.
    const WINDOW: egui::Vec2 = egui::vec2(400.0, 150.0);

    /// How many rows the dialog's body has, so that it is taller than the
    /// window.
    const ROWS: usize = 40;

    /// A dialog whose body is taller than the window, with a button at the
    /// end of it that says when it was pressed.
    #[derive(Default)]
    struct State {
        pressed: usize,
    }

    fn rig() -> Harness<'static, State> {
        crate::testkit::harness(State::default(), WINDOW, |ui, state: &mut State| {
            crate::dialog::modal(ui.ctx(), "tall", |ui| {
                for row in 0..ROWS {
                    ui.label(format!("row {row}"));
                }
                if ui.button("Bottom").clicked() {
                    state.pressed += 1;
                }
            });
        })
    }

    /// The button at the end of the body.
    fn bottom(h: &Harness<'_, State>) -> egui::Rect {
        h.get_by_label("Bottom").rect()
    }

    #[test]
    fn a_dialog_too_tall_for_the_window_scrolls_to_its_end() {
        use crate::testkit::wheel;
        let mut h = rig();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, WINDOW);
        assert!(
            !screen.contains_rect(bottom(&h)),
            "the button starts below the fold"
        );
        for _ in 0..20 {
            wheel(
                &mut h,
                egui::pos2(WINDOW.x * 0.5, WINDOW.y * 0.5),
                egui::MouseWheelUnit::Point,
                egui::vec2(0.0, -80.0),
            );
        }
        h.run_steps(20);
        assert!(
            screen.contains_rect(bottom(&h)),
            "the button scrolls into view: {:?}",
            bottom(&h)
        );
        // And it is really there to be pressed.
        h.get_by_label("Bottom").click();
        h.run();
        assert_eq!(h.state().pressed, 1, "the button was pressed");
    }

    #[test]
    fn a_dialog_that_fits_keeps_its_own_height() {
        let mut h = crate::testkit::harness(State::default(), WINDOW, |ui, state: &mut State| {
            crate::dialog::modal(ui.ctx(), "short", |ui| {
                ui.label("one row");
                if ui.button("Bottom").clicked() {
                    state.pressed += 1;
                }
            });
        });
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, WINDOW);
        assert!(
            screen.contains_rect(bottom(&h)),
            "the button is in view without scrolling"
        );
        h.get_by_label("Bottom").click();
        h.run();
        assert_eq!(h.state().pressed, 1, "the button was pressed");
    }
}
