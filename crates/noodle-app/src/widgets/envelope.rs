//! The envelope editor: draws an [`EnvelopeCurve`] and edits it with the
//! mouse.
//!
//! Drag an anchor to move it (the two ends only move up and down). Drag a
//! stretch of the line between two anchors to bend it, and click the line to
//! add an anchor there. Double-click or right-click an inner anchor to delete
//! it. Like the other widgets it reports edits and leaves the project to the
//! caller: a drag is a run of [`Edit::Drag`]s ended by [`Edit::EndDrag`], so
//! it undoes as one step.

use egui::{Color32, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2};
use noodle_core::{Command, NodeId, Value};
use noodle_nodes::EnvelopeCurve;

use crate::session::Edit;
use crate::theme;

/// How close, in points, the pointer must be to an anchor or the line to
/// grab it.
const REACH: f32 = 8.0;
/// The editor's height.
const HEIGHT: f32 = 120.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Anchor(usize),
    /// The stretch of line after an anchor.
    Segment(usize),
}

/// What a drag started on, and the curve it started from.
#[derive(Clone)]
struct Drag {
    target: Target,
    start: EnvelopeCurve,
}

#[must_use = "call `show` to draw the editor"]
pub struct EnvelopeEditor<'a> {
    node: NodeId,
    key: &'a str,
    curve: &'a EnvelopeCurve,
}

impl<'a> EnvelopeEditor<'a> {
    pub fn new(node: NodeId, key: &'a str, curve: &'a EnvelopeCurve) -> Self {
        Self { node, key, curve }
    }

    pub fn show(self, ui: &mut Ui) -> Vec<Edit> {
        let width = ui.available_width().clamp(160.0, 320.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::hover());
        let id = Id::new((self.node, self.key, "envelope"));
        let to_screen = |x: f32, y: f32| pos2(rect.left() + x * width, rect.bottom() - y * HEIGHT);
        let from_screen = |p: Pos2| {
            (
                ((p.x - rect.left()) / width).clamp(0.0, 1.0),
                ((rect.bottom() - p.y) / HEIGHT).clamp(0.0, 1.0),
            )
        };
        let set = |curve: &EnvelopeCurve| Command::SetConfig {
            node: self.node,
            key: self.key.to_owned(),
            value: Some(Value::Text(curve.to_text())),
        };

        let response = ui.interact(rect, id, Sense::click_and_drag());
        let polyline: Vec<Pos2> = (0..=128)
            .map(|i| {
                let x = i as f32 / 128.0;
                to_screen(x, self.curve.eval(x))
            })
            .collect();
        let mut edits = Vec::new();
        let drag_id = id.with("drag");

        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            let target = origin.and_then(|at| self.target_at(at, &to_screen, &polyline));
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
                Target::Segment(i) => curve.set_bend(i, drag.start.bend_for_middle(i, y)),
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
        let anchor_at = |at: Pos2| match self.target_at(at, &to_screen, &polyline) {
            Some(Target::Anchor(i)) => Some(i),
            _ => None,
        };
        let clicked_at = response.interact_pointer_pos();
        if response.double_clicked() || response.secondary_clicked() {
            if let Some(i) = clicked_at
                .and_then(anchor_at)
                .filter(|&i| i != 0 && i != last)
            {
                let mut curve = self.curve.clone();
                if curve.remove(i) {
                    edits.push(Edit::Apply(set(&curve)));
                }
            }
        } else if response.clicked()
            && let Some(at) = clicked_at
            && let Some(Target::Segment(_)) = self.target_at(at, &to_screen, &polyline)
        {
            let mut curve = self.curve.clone();
            curve.insert_at(from_screen(at).0);
            edits.push(Edit::Apply(set(&curve)));
        }
        if let Some(at) = response.hover_pos() {
            match self.target_at(at, &to_screen, &polyline) {
                Some(Target::Anchor(i)) if i != 0 && i != last => {
                    response.on_hover_text("Drag to move. Double-click or right-click to delete.");
                }
                Some(Target::Anchor(_)) => {
                    response.on_hover_text("Drag up or down to move.");
                }
                Some(Target::Segment(_)) => {
                    response.on_hover_text("Drag to bend. Click to add a point.");
                }
                None => {}
            }
        }

        self.paint(ui, rect, &polyline, to_screen);
        edits
    }

    /// The anchor or stretch of line nearest `at` and within reach, if any.
    /// An anchor wins over the line beside it.
    fn target_at(
        &self,
        at: Pos2,
        to_screen: &impl Fn(f32, f32) -> Pos2,
        polyline: &[Pos2],
    ) -> Option<Target> {
        let points = self.curve.points();
        let anchor = points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, at.distance(to_screen(p.x, p.y))))
            .filter(|&(_, d)| d <= REACH)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = anchor {
            return Some(Target::Anchor(i));
        }
        if !near_line(polyline, at) {
            return None;
        }
        // Which stretch the pointer is over, by where it is along x.
        let x = (at.x - to_screen(0.0, 0.0).x) / (to_screen(1.0, 0.0).x - to_screen(0.0, 0.0).x);
        let segment = points
            .windows(2)
            .position(|w| x <= w[1].x)
            .unwrap_or(points.len() - 2);
        Some(Target::Segment(segment))
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
        painter.add(egui::Shape::line(
            line.to_vec(),
            Stroke::new(2.0, theme::ACCENT),
        ));
        let points = self.curve.points();
        // A small dot mid-segment to show it can be bent.
        for w in points.windows(2) {
            let x = 0.5 * (w[0].x + w[1].x);
            painter.circle_filled(
                to_screen(x, self.curve.eval(x)),
                2.5,
                theme::editor::TEXT_WEAK,
            );
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
        at.distance(a + ab * t) <= REACH
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 200 by 100 point box with its corner at the origin.
    fn screen(x: f32, y: f32) -> Pos2 {
        pos2(x * 200.0, (1.0 - y) * 100.0)
    }

    fn line(curve: &EnvelopeCurve) -> Vec<Pos2> {
        (0..=128)
            .map(|i| {
                let x = i as f32 / 128.0;
                screen(x, curve.eval(x))
            })
            .collect()
    }

    fn target(curve: &EnvelopeCurve, at: Pos2) -> Option<Target> {
        EnvelopeEditor::new(NodeId(1), "curve", curve).target_at(at, &screen, &line(curve))
    }

    #[test]
    fn an_anchor_beats_the_line_through_it_and_the_ends_can_be_grabbed() {
        let curve = EnvelopeCurve::default();
        let peak = curve.points()[1];
        assert_eq!(
            target(&curve, screen(peak.x, peak.y)),
            Some(Target::Anchor(1))
        );
        assert_eq!(target(&curve, screen(0.0, 0.0)), Some(Target::Anchor(0)));
        assert_eq!(target(&curve, screen(1.0, 0.0)), Some(Target::Anchor(2)));
    }

    #[test]
    fn the_line_is_a_segment_and_empty_space_is_nothing() {
        let curve = EnvelopeCurve::default();
        let on_fall = screen(0.5, curve.eval(0.5));
        assert_eq!(target(&curve, on_fall), Some(Target::Segment(1)));
        let on_rise = screen(0.04, curve.eval(0.04));
        assert_eq!(target(&curve, on_rise), Some(Target::Segment(0)));
        assert_eq!(target(&curve, screen(0.5, 0.95)), None);
    }
}
