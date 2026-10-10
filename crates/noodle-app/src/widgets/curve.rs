//! The curve editor: draws a [`Curve`] and edits it with the mouse.
//!
//! Drag an anchor or a handle to move it; click on the curve to add an
//! anchor there; double-click or right-click an anchor to delete it. The two
//! ends are fixed, but their handles move. Like the other widgets it reports
//! edits and leaves the project to the caller: a drag is a run of
//! [`Edit::Drag`]s ended by [`Edit::EndDrag`], so it undoes as one step.
//!
//! The whole square is one interactive area, and the target under the
//! pointer is chosen by distance, so a handle lying on its anchor can still
//! be grabbed (end anchors can't move, so a handle on one always wins).
//! Every drag frame is computed from the curve as it was when the drag
//! began, so moving a point toward a neighbour and back restores its handles.

use egui::{Color32, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2};
use noodle_core::{Command, NodeId, Value};
use noodle_nodes::{Curve, Handle, lookup};

use crate::session::Edit;
use crate::theme;

/// How close, in points, the pointer must be to the curve to add an anchor,
/// or to an anchor or handle to grab it.
const CLICK_DISTANCE: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Anchor(usize),
    Handle(usize, Handle),
}

/// What a drag started on, and the curve it started from.
#[derive(Clone)]
struct Drag {
    target: Target,
    start: Curve,
}

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
        let set = |curve: &Curve| Command::SetConfig {
            node: self.node,
            key: self.key.to_owned(),
            value: Some(Value::Text(curve.to_text())),
        };

        let response = ui.interact(rect, id, Sense::click_and_drag());
        let lut = self.curve.lut();
        let polyline: Vec<Pos2> = (0..=128)
            .map(|i| {
                let x = i as f32 / 128.0;
                to_screen(x, lookup(&lut, x))
            })
            .collect();
        let mut edits = Vec::new();
        let drag_id = id.with("drag");

        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            let target = origin.and_then(|at| self.target_at(at, &to_screen));
            let drag = target.map(|target| Drag {
                target,
                start: self.curve.clone(),
            });
            ui.data_mut(|d| d.insert_temp(drag_id, drag));
        }
        if response.dragged()
            && let Some(drag) = ui.data(|d| d.get_temp::<Option<Drag>>(drag_id)).flatten()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let (x, y) = from_screen(pos);
            let mut curve = drag.start.clone();
            match drag.target {
                Target::Anchor(i) => curve.move_point(i, x, y),
                Target::Handle(i, which) => curve.move_handle(i, which, x, y),
            }
            // A held button with the pointer still gives the same curve.
            if curve != *self.curve {
                edits.push(Edit::Drag(set(&curve)));
            }
        }
        if response.drag_stopped() {
            let took = ui.data_mut(|d| d.remove_temp::<Option<Drag>>(drag_id));
            if took.flatten().is_some() {
                edits.push(Edit::EndDrag);
            }
        }

        let last = self.curve.points().len() - 1;
        let hovered = response
            .hover_pos()
            .and_then(|at| self.target_at(at, &to_screen));
        let interior = |t: Option<Target>| match t {
            Some(Target::Anchor(i)) if i != 0 && i != last => Some(i),
            _ => None,
        };

        let clicked_at = response.interact_pointer_pos();
        if response.double_clicked() || response.secondary_clicked() {
            let target = clicked_at.and_then(|at| self.target_at(at, &to_screen));
            if let Some(i) = interior(target) {
                let mut curve = self.curve.clone();
                if curve.remove(i) {
                    edits.push(Edit::Apply(set(&curve)));
                }
            }
        } else if response.clicked()
            && let Some(at) = clicked_at
            && self.target_at(at, &to_screen).is_none()
            && near_line(&polyline, at)
        {
            let mut curve = self.curve.clone();
            curve.insert_at(from_screen(at).0);
            edits.push(Edit::Apply(set(&curve)));
        }
        if interior(hovered).is_some() {
            response.on_hover_text("Drag to move. Double-click or right-click to delete.");
        }

        self.paint(ui, rect, &polyline, to_screen);
        edits
    }

    /// The anchor or handle nearest `at` and within reach, if any. Ties go
    /// to an interior anchor over its handle, and to a handle over an end
    /// anchor, which can't be moved.
    fn target_at(&self, at: Pos2, to_screen: &impl Fn(f32, f32) -> Pos2) -> Option<Target> {
        let points = self.curve.points();
        let last = points.len() - 1;
        let mut best: Option<(f32, Target)> = None;
        let mut consider = |distance: f32, target: Target| {
            if distance <= CLICK_DISTANCE && best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, target));
            }
        };
        for (i, p) in points.iter().enumerate() {
            if i != 0 && i != last {
                consider(at.distance(to_screen(p.x, p.y)), Target::Anchor(i));
            }
            for (which, offset) in [(Handle::In, p.in_handle), (Handle::Out, p.out_handle)] {
                if (which == Handle::In && i == 0) || (which == Handle::Out && i == last) {
                    continue;
                }
                let h = to_screen(p.x + offset.0, p.y + offset.1);
                // Strictly nearer wins, so on a tie the anchor stays; an end
                // anchor is never a target, so its handle always can be.
                consider(at.distance(h), Target::Handle(i, which));
            }
        }
        best.map(|(_, target)| target)
    }

    fn paint(&self, ui: &Ui, rect: Rect, line: &[Pos2], to_screen: impl Fn(f32, f32) -> Pos2) {
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
        painter.add(egui::Shape::line(
            line.to_vec(),
            Stroke::new(2.0, theme::ACCENT),
        ));

        let points = self.curve.points();
        let last = points.len() - 1;
        let weak = theme::editor::TEXT_WEAK;
        for (i, p) in points.iter().enumerate() {
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
        for p in points {
            let at = to_screen(p.x, p.y);
            painter.circle_filled(at, 5.0, Color32::WHITE);
            painter.circle_stroke(at, 5.0, Stroke::new(1.5, theme::CANVAS));
        }
    }
}

/// Whether `at` is within reach of the drawn line.
fn near_line(line: &[Pos2], at: Pos2) -> bool {
    line.windows(2).any(|w| {
        let (a, b) = (w[0], w[1]);
        let ab = b - a;
        let len2 = ab.length_sq();
        let t = if len2 > 0.0 {
            ((at - a).dot(ab) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        at.distance(a + ab * t) <= CLICK_DISTANCE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 100-point square with its corner at the origin.
    fn screen(x: f32, y: f32) -> Pos2 {
        pos2(x * 100.0, (1.0 - y) * 100.0)
    }

    fn editor(curve: &Curve) -> CurveEditor<'_> {
        CurveEditor::new(NodeId(1), "curve", curve)
    }

    #[test]
    fn a_handle_lying_on_an_end_anchor_can_be_grabbed() {
        let mut curve = Curve::linear();
        curve.move_handle(0, Handle::Out, 0.0, 0.0); // zero length
        let target = editor(&curve).target_at(screen(0.0, 0.0), &screen);
        assert_eq!(target, Some(Target::Handle(0, Handle::Out)));
    }

    #[test]
    fn on_a_tie_the_interior_anchor_wins_and_the_nearer_target_wins_otherwise() {
        let mut curve = Curve::linear();
        let i = curve.insert_at(0.5);
        let (ax, ay) = (curve.points()[i].x, curve.points()[i].y);
        curve.move_handle(i, Handle::Out, ax, ay); // on its anchor
        let e = editor(&curve);
        assert_eq!(
            e.target_at(screen(0.5, 0.5), &screen),
            Some(Target::Anchor(i))
        );
        // Nudged toward the handle's side, the handle is as near, so a handle
        // that has been moved away from its anchor is nearer to its own spot.
        curve.move_handle(i, Handle::Out, 0.6, 0.5);
        let e = editor(&curve);
        assert_eq!(
            e.target_at(screen(0.6, 0.5), &screen),
            Some(Target::Handle(i, Handle::Out))
        );
        assert_eq!(e.target_at(screen(0.9, 0.1), &screen), None);
    }

    #[test]
    fn a_click_beside_a_steep_stretch_is_near_the_line() {
        // A near-vertical line: 30 points off vertically, 2 across.
        let line = [pos2(50.0, 0.0), pos2(52.0, 100.0)];
        assert!(near_line(&line, pos2(55.0, 50.0)));
        assert!(!near_line(&line, pos2(70.0, 50.0)));
    }
}
