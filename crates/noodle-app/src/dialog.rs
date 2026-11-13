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
///
/// A scroll area sizes itself from its content as it was last frame, so a
/// body that grows (a radio choice that adds rows) would draw one frame at
/// the old height, with its end outside the frame, and nothing repaints it
/// until the next input. When the frame is the wrong height the pass is
/// discarded and run again as a sizing pass, so the dialog is right the
/// frame it changes.
///
/// Returns the rectangle of the body's frame, for tests.
pub fn modal(
    ctx: &egui::Context,
    id: &str,
    add: impl FnOnce(&mut Ui),
) -> egui::ModalResponse<egui::Rect> {
    let id = Id::new(id);
    // The area is as big as its content was last frame, so a body that has
    // grown is squeezed into the old size. A sizing pass lets it measure
    // itself against the whole window again.
    let resize_key = id.with("resize");
    let resize = ctx
        .data_mut(|d| d.remove_temp::<bool>(resize_key))
        .unwrap_or(false);
    let area = egui::Modal::default_area(id)
        .default_size(ctx.content_rect().size())
        .sizing_pass(resize);
    egui::Modal::new(id).area(area).show(ctx, |ui| {
        let height = (ctx.content_rect().height() - MARGIN).max(0.0);
        let out = egui::ScrollArea::vertical()
            .id_salt("body")
            .max_height(height)
            .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, add);
        let key = id.with("content height");
        let before = ctx.data_mut(|d| d.get_temp::<f32>(key));
        ctx.data_mut(|d| d.insert_temp(key, out.content_size.y));
        if resize {
            ctx.request_discard("dialog resized");
        } else if before.is_some_and(|h| (h - out.content_size.y).abs() > 0.5) {
            ctx.data_mut(|d| d.insert_temp(resize_key, true));
            ctx.request_discard("dialog body changed height");
        }
        out.inner_rect
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

    /// A dialog whose body grows when a button in it is pressed has a frame
    /// that holds the new rows in the very frame they appear.
    #[test]
    fn a_dialog_that_grows_has_a_frame_that_holds_it_at_once() {
        #[derive(Default)]
        struct Grow {
            more: bool,
            frame: Option<egui::Rect>,
        }
        let mut h = crate::testkit::harness(
            Grow::default(),
            egui::vec2(400.0, 400.0),
            |ui, s: &mut Grow| {
                let out = crate::dialog::modal(ui.ctx(), "grow", |ui| {
                    if ui.button("More").clicked() {
                        s.more = true;
                    }
                    if s.more {
                        for row in 0..3 {
                            ui.label(format!("extra {row}"));
                        }
                    }
                    ui.label("end");
                });
                s.frame = Some(out.inner);
            },
        );
        h.get_by_label("More").click();
        h.step();
        let end = h.get_by_label("end").rect();
        let frame = h.state().frame.unwrap();
        assert!(
            frame.bottom() >= end.bottom(),
            "the frame {frame:?} ends before the last row {end:?}"
        );
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
