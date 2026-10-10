//! The curve editor: draws a [`Curve`] and edits it with the mouse.
//!
//! Drag an anchor or a handle to move it; click on the curve to add an
//! anchor there; double-click or right-click an anchor to delete it. The two
//! ends are fixed. Like the other widgets it reports edits and leaves the
//! project to the caller: a drag is a run of [`Edit::Drag`]s ended by
//! [`Edit::EndDrag`], so it undoes as one step.

use egui::{Color32, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2};
use noodle_core::{Command, NodeId, Value};
use noodle_nodes::{Curve, Handle};

use crate::session::Edit;
use crate::theme;

/// How close, in points, the pointer must be to the curve to add an anchor.
const CLICK_DISTANCE: f32 = 8.0;
const GRAB: f32 = 14.0;

#[must_use = "call `show` to draw the editor"]
pub struct CurveEditor<'a> {
    node: NodeId,
    key: &'a str,
    curve: &'a Curve,
}

impl<'a> CurveEditor<'a> {
    pub fn new(node: NodeId, key: &'a str, curve: &'a Curve) -> Self {
        Self { node, key, curve }
    }

    pub fn show(self, ui: &mut Ui) -> Vec<Edit> {
        let side = ui.available_width().clamp(120.0, 240.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
        let id = Id::new((self.node, self.key, "curve"));
        let to_screen = |x: f32, y: f32| pos2(rect.left() + x * side, rect.bottom() - y * side);
        let from_screen = |p: Pos2| {
            (
                ((p.x - rect.left()) / side).clamp(0.0, 1.0),
                ((rect.bottom() - p.y) / side).clamp(0.0, 1.0),
            )
        };

        let mut curve = self.curve.clone();
        let mut edits = Vec::new();
        let set = |curve: &Curve| Command::SetConfig {
            node: self.node,
            key: self.key.to_owned(),
            value: Some(Value::Text(curve.to_text())),
        };

        // Background first, so the anchors and handles drawn after it win.
        let background = ui.interact(rect, id.with("bg"), Sense::click());
        let points = curve.points().to_vec();
        let last = points.len() - 1;
        let mut moved = false;
        let mut finished = false;
        let mut deleted = None;

        // Handles before anchors, so an anchor on top of a handle wins.
        for (i, p) in points.iter().enumerate() {
            for which in [Handle::In, Handle::Out] {
                if (which == Handle::In && i == 0) || (which == Handle::Out && i == last) {
                    continue;
                }
                let offset = if which == Handle::In {
                    p.in_handle
                } else {
                    p.out_handle
                };
                let at = to_screen(p.x + offset.0, p.y + offset.1);
                let r = ui.interact(
                    Rect::from_center_size(at, Vec2::splat(GRAB)),
                    id.with((i, which == Handle::In)),
                    Sense::drag(),
                );
                if r.dragged()
                    && let Some(pos) = r.interact_pointer_pos()
                {
                    let (x, y) = from_screen(pos);
                    curve.move_handle(i, which, x, y);
                    moved = true;
                }
                finished |= r.drag_stopped();
            }
        }
        for (i, p) in points.iter().enumerate() {
            let r = ui
                .interact(
                    Rect::from_center_size(to_screen(p.x, p.y), Vec2::splat(GRAB)),
                    id.with(("anchor", i)),
                    Sense::click_and_drag(),
                )
                .on_hover_text("Drag to move. Double-click or right-click to delete.");
            if r.dragged()
                && i != 0
                && i != last
                && let Some(pos) = r.interact_pointer_pos()
            {
                let (x, y) = from_screen(pos);
                curve.move_point(i, x, y);
                moved = true;
            }
            finished |= r.drag_stopped();
            if r.double_clicked() || r.secondary_clicked() {
                deleted = Some(i);
            }
        }

        if let Some(i) = deleted {
            if curve.remove(i) {
                edits.push(Edit::Apply(set(&curve)));
            }
        } else if background.clicked() && !moved {
            if let Some(pos) = background.interact_pointer_pos() {
                let (x, _) = from_screen(pos);
                let hit = (to_screen(x, self.curve.eval(x)).y - pos.y).abs() <= CLICK_DISTANCE;
                if hit {
                    curve.insert_at(x);
                    edits.push(Edit::Apply(set(&curve)));
                }
            }
        } else if moved {
            edits.push(Edit::Drag(set(&curve)));
        }
        if finished {
            edits.push(Edit::EndDrag);
        }

        self.paint(ui, rect, &curve, to_screen);
        edits
    }

    fn paint(&self, ui: &Ui, rect: Rect, curve: &Curve, to_screen: impl Fn(f32, f32) -> Pos2) {
        let painter = ui.painter_at(rect.expand(6.0));
        painter.rect_filled(rect, 4.0, theme::CANVAS);
        let grid = Stroke::new(1.0, theme::editor::GRID_MAJOR);
        for i in 0..=4 {
            let t = i as f32 / 4.0;
            painter.line_segment([to_screen(t, 0.0), to_screen(t, 1.0)], grid);
            painter.line_segment([to_screen(0.0, t), to_screen(1.0, t)], grid);
        }
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, theme::editor::NODE_OUTLINE),
            StrokeKind::Inside,
        );
        // The identity line, to see how far the curve bends from it.
        painter.line_segment(
            [to_screen(0.0, 0.0), to_screen(1.0, 1.0)],
            Stroke::new(1.0, theme::editor::NODE_OUTLINE),
        );

        let line: Vec<Pos2> = (0..=128)
            .map(|i| {
                let x = i as f32 / 128.0;
                to_screen(x, curve.eval(x))
            })
            .collect();
        painter.add(egui::Shape::line(line, Stroke::new(2.0, theme::ACCENT)));

        let last = curve.points().len() - 1;
        let weak = theme::editor::TEXT_WEAK;
        for (i, p) in curve.points().iter().enumerate() {
            let anchor = to_screen(p.x, p.y);
            for (which, offset) in [(Handle::In, p.in_handle), (Handle::Out, p.out_handle)] {
                if (which == Handle::In && i == 0) || (which == Handle::Out && i == last) {
                    continue;
                }
                let h = to_screen(p.x + offset.0, p.y + offset.1);
                painter.line_segment([anchor, h], Stroke::new(1.0, weak));
                painter.circle_filled(h, 3.5, weak);
            }
        }
        for p in curve.points() {
            let at = to_screen(p.x, p.y);
            painter.circle_filled(at, 5.0, Color32::WHITE);
            painter.circle_stroke(at, 5.0, Stroke::new(1.5, theme::CANVAS));
        }
    }
}
