//! The surround panner display: the listener at the centre of a ring, the
//! speakers of the node's layout around it, and the source between them.
//!
//! [`paint`] draws one, small for a node's body or full size in the
//! properties panel. The source sits at the node's azimuth, nearer the middle
//! the more `spread` there is (all the way in, every speaker plays it alike),
//! and each speaker's dot grows with the gain the real panner would give it
//! (`ChannelLayout::pan_gains`, the function the node runs). [`PannerEditor`]
//! is the draggable full-size version: dragging the source sets the azimuth
//! with its angle and the spread with its distance from the middle, a run of
//! [`Edit::Drag`]s ended by [`Edit::EndDrag`].

use egui::{Align2, FontId, Id, Painter, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use noodle_core::{Command, NodeId};
use noodle_engine::ChannelLayout;

use crate::session::Edit;
use crate::theme::editor as colors;

/// The height of the full-size display.
const FULL_HEIGHT: f32 = 180.0;

/// The ring: its centre and radius inside `area`, leaving room for the
/// speakers' dots.
fn ring(area: Rect) -> (Pos2, f32) {
    let radius = (area.width().min(area.height()) * 0.5 - 8.0).max(4.0);
    (area.center(), radius)
}

/// Where a speaker at `azimuth` degrees (straight ahead up, positive to the
/// right) is on the ring.
fn on_ring(center: Pos2, radius: f32, azimuth: f32) -> Pos2 {
    let a = azimuth.to_radians();
    pos2(center.x + radius * a.sin(), center.y - radius * a.cos())
}

/// The azimuth and spread of a source dragged to `pos`.
pub fn source_at(area: Rect, pos: Pos2) -> (f32, f32) {
    let (center, radius) = ring(area);
    let d = pos - center;
    let azimuth = if d.length() < 1e-3 {
        0.0
    } else {
        d.x.atan2(-d.y).to_degrees()
    };
    (azimuth, 1.0 - (d.length() / radius).clamp(0.0, 1.0))
}

/// Draws the panner in `area`. `labels` names the speakers, which only a
/// full-size drawing has room for.
pub fn paint(
    painter: &Painter,
    area: Rect,
    zoom: f32,
    layout: ChannelLayout,
    azimuth: f32,
    spread: f32,
    labels: bool,
) {
    painter.rect_filled(area, 2.0 * zoom, colors::BODY);
    let (center, radius) = ring(area);
    let axis = Stroke::new(zoom.max(1.0), colors::SCOPE_AXIS);
    painter.circle_stroke(center, radius, axis);
    painter.circle_stroke(center, radius * 0.5, axis);
    painter.line_segment(
        [
            pos2(center.x, center.y - radius),
            pos2(center.x, center.y + radius),
        ],
        axis,
    );
    painter.line_segment(
        [
            pos2(center.x - radius, center.y),
            pos2(center.x + radius, center.y),
        ],
        axis,
    );

    let mut gains = [0.0f32; noodle_engine::MAX_LAYOUT_CHANNELS];
    let gains = &mut gains[..layout.channels()];
    layout.pan_gains(azimuth, spread, gains);
    for (speaker, &gain) in layout.speakers().iter().zip(gains.iter()) {
        // The LFE has no direction: it sits at the listener.
        let at = layout
            .azimuth(*speaker)
            .map_or(pos2(center.x, center.y + radius * 0.25), |a| {
                on_ring(center, radius, a)
            });
        let size = (2.5 + 5.5 * gain.clamp(0.0, 1.0)) * zoom;
        painter.circle_filled(
            at,
            size,
            colors::SCOPE_TRACE.gamma_multiply(0.25 + 0.75 * gain.clamp(0.0, 1.0)),
        );
        if labels {
            let above = at.y < center.y;
            painter.text(
                at + vec2(0.0, if above { -size - 2.0 } else { size + 2.0 }),
                if above {
                    Align2::CENTER_BOTTOM
                } else {
                    Align2::CENTER_TOP
                },
                speaker.name(),
                FontId::proportional(10.0),
                colors::TEXT_WEAK,
            );
        }
    }
    let source = on_ring(center, radius * (1.0 - spread.clamp(0.0, 1.0)), azimuth);
    painter.line_segment(
        [center, source],
        Stroke::new(zoom.max(1.0), colors::METER_RMS.gamma_multiply(0.6)),
    );
    painter.circle_filled(source, 4.0 * zoom, colors::METER_RMS);
}

/// The draggable panner in the properties panel.
pub struct PannerEditor<'a> {
    node: NodeId,
    layout: ChannelLayout,
    azimuth: f32,
    spread: f32,
    /// Whether each of the two parameters can be set (neither is wired).
    editable: (bool, bool),
    /// The ranges of the azimuth and spread, as the node's parameters have.
    azimuth_range: (f32, f32),
    spread_range: (f32, f32),
    label: &'a str,
}

impl<'a> PannerEditor<'a> {
    pub fn new(
        node: NodeId,
        layout: ChannelLayout,
        azimuth: f32,
        spread: f32,
        editable: (bool, bool),
    ) -> Self {
        Self {
            node,
            layout,
            azimuth,
            spread,
            editable,
            azimuth_range: (-180.0, 180.0),
            spread_range: (0.0, 1.0),
            label: "Surround panner",
        }
    }

    /// Sets the ranges the azimuth and spread can be dragged through.
    pub fn ranges(mut self, azimuth: (f32, f32), spread: (f32, f32)) -> Self {
        self.azimuth_range = azimuth;
        self.spread_range = spread;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Vec<Edit> {
        let mut edits = Vec::new();
        let width = ui.available_width();
        let (response, painter) =
            ui.allocate_painter(vec2(width, FULL_HEIGHT), Sense::click_and_drag());
        let area = response.rect;
        let editable = self.editable.0 || self.editable.1;
        let drag_id = Id::new((self.node, "panner drag"));
        paint(
            &painter,
            area,
            1.0,
            self.layout,
            self.azimuth,
            self.spread,
            true,
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, self.label));
        let response = response.on_hover_text(if editable {
            "Drag the dot: its angle is the azimuth, its distance from the middle the spread."
        } else {
            "The azimuth and spread are wired."
        });
        if !editable {
            return edits;
        }
        let set = |edits: &mut Vec<Edit>, pos: Pos2| {
            let (azimuth, spread) = source_at(area, pos);
            let (lo, hi) = self.azimuth_range;
            if self.editable.0 {
                edits.push(Edit::Drag(Command::SetParam {
                    node: self.node,
                    key: "azimuth".to_owned(),
                    value: Some(azimuth.clamp(lo, hi)),
                }));
            }
            if self.editable.1 {
                edits.push(Edit::Drag(Command::SetParam {
                    node: self.node,
                    key: "spread".to_owned(),
                    value: Some(
                        self.spread_range.0 + spread * (self.spread_range.1 - self.spread_range.0),
                    ),
                }));
            }
        };
        if response.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_id, true));
        }
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            set(&mut edits, pos);
        }
        if response.drag_stopped() && ui.data_mut(|d| d.remove_temp::<bool>(drag_id)).is_some() {
            edits.push(Edit::EndDrag);
        }
        edits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_on_the_ring_maps_back_to_its_azimuth_and_spread() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 200.0));
        let (center, radius) = ring(area);
        for azimuth in [-135.0f32, -90.0, -30.0, 0.0, 45.0, 90.0, 170.0] {
            for spread in [0.0f32, 0.25, 1.0] {
                let at = on_ring(center, radius * (1.0 - spread), azimuth);
                let (a, s) = source_at(area, at);
                assert!((s - spread).abs() < 1e-3, "{azimuth} {spread}: spread {s}");
                if spread < 1.0 {
                    let wrapped = (a - azimuth + 540.0).rem_euclid(360.0) - 180.0;
                    assert!(wrapped.abs() < 1e-2, "{azimuth} {spread}: azimuth {a}");
                }
            }
        }
        // Straight ahead is up, to the right is positive, outside the ring
        // is the edge.
        assert_eq!(source_at(area, pos2(100.0, 0.0)).0, 0.0);
        assert!(source_at(area, pos2(200.0, 100.0)).0 > 0.0);
        assert_eq!(source_at(area, pos2(1000.0, 100.0)).1, 0.0);
    }
}
