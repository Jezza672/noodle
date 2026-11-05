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
//!
//! A layout with height speakers (7.1.2, 7.1.4) also has a height strip down
//! the right edge, whose dot is the node's `elevation` (the bottom is ear
//! level, the top overhead), and draws its height speakers on a smaller ring,
//! as in a view from above. Dragging in the strip sets the elevation.

use egui::{Align2, FontId, Id, Painter, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use noodle_core::{Command, NodeId};
use noodle_engine::ChannelLayout;

use crate::session::Edit;
use crate::theme::editor as colors;

/// The height of the full-size display.
const FULL_HEIGHT: f32 = 180.0;

/// The width of the height strip, which a layout with height speakers has down
/// its right edge.
const STRIP: f32 = 18.0;

/// Splits `area` into the part the ring is in and, for a layout with height
/// speakers, the height strip.
fn split(area: Rect, layout: ChannelLayout, zoom: f32) -> (Rect, Option<Rect>) {
    if !layout.speakers().iter().any(|s| s.is_top()) {
        return (area, None);
    }
    let width = (STRIP * zoom).min(area.width() * 0.25);
    let (ring, strip) = (
        Rect::from_min_max(area.min, pos2(area.max.x - width, area.max.y)),
        Rect::from_min_max(pos2(area.max.x - width, area.min.y), area.max),
    );
    (ring, Some(strip))
}

/// The elevation of a source dragged to `pos` in a height strip, from 0 to 90.
pub fn elevation_at(strip: Rect, pos: Pos2) -> f32 {
    let height = (strip.height() - 16.0).max(1.0);
    (((strip.bottom() - 8.0 - pos.y) / height).clamp(0.0, 1.0)) * 90.0
}

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

/// Where the source is: its angle round the listener, its height and how
/// much of it plays from every speaker.
#[derive(Clone, Copy, Debug)]
pub struct Source {
    pub azimuth: f32,
    pub elevation: f32,
    pub spread: f32,
}

/// Draws the panner in `area`. `labels` names the speakers, which only a
/// full-size drawing has room for.
pub fn paint(
    painter: &Painter,
    area: Rect,
    zoom: f32,
    layout: ChannelLayout,
    source: Source,
    labels: bool,
) {
    let Source {
        azimuth,
        elevation,
        spread,
    } = source;
    painter.rect_filled(area, 2.0 * zoom, colors::BODY);
    let (plan, strip) = split(area, layout, zoom);
    if let Some(strip) = strip {
        let x = strip.center().x;
        let (bottom, top) = (strip.bottom() - 8.0, strip.top() + 8.0);
        let axis = Stroke::new(zoom.max(1.0), colors::SCOPE_AXIS);
        painter.line_segment([pos2(x, bottom), pos2(x, top)], axis);
        // Where the height speakers stand.
        let tops = bottom - (bottom - top) * (noodle_engine::TOP_ELEVATION / 90.0);
        painter.line_segment(
            [pos2(x - 4.0 * zoom, tops), pos2(x + 4.0 * zoom, tops)],
            axis,
        );
        let e = elevation.clamp(0.0, 90.0) / 90.0;
        painter.circle_filled(
            pos2(x, bottom - (bottom - top) * e),
            3.5 * zoom,
            colors::METER_RMS,
        );
    }
    let area = plan;
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
    layout.pan_gains_3d(azimuth, elevation, spread, gains);
    for (speaker, &gain) in layout.speakers().iter().zip(gains.iter()) {
        // The LFE has no direction: it sits at the listener.
        // A height speaker is drawn on a smaller ring, nearer the middle.
        let at = layout
            .azimuth(*speaker)
            .map_or(pos2(center.x, center.y + radius * 0.25), |a| {
                on_ring(
                    center,
                    if speaker.is_top() {
                        radius * 0.7
                    } else {
                        radius
                    },
                    a,
                )
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
    elevation: f32,
    spread: f32,
    /// Whether each of the two parameters can be set (neither is wired).
    editable: (bool, bool),
    /// Whether the elevation can be set (it is not wired).
    elevation_editable: bool,
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
            elevation: 0.0,
            spread,
            editable,
            elevation_editable: false,
            azimuth_range: (-180.0, 180.0),
            spread_range: (0.0, 1.0),
            label: "Surround panner",
        }
    }

    /// Gives the elevation and whether it can be set.
    pub fn elevation(mut self, elevation: f32, editable: bool) -> Self {
        self.elevation = elevation;
        self.elevation_editable = editable;
        self
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
        let (plan, strip) = split(area, self.layout, 1.0);
        let editable =
            self.editable.0 || self.editable.1 || (strip.is_some() && self.elevation_editable);
        let drag_id = Id::new((self.node, "panner drag"));
        paint(
            &painter,
            area,
            1.0,
            self.layout,
            Source {
                azimuth: self.azimuth,
                elevation: self.elevation,
                spread: self.spread,
            },
            true,
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, self.label));
        let response = response.on_hover_text(if editable {
            "Drag the dot: its angle is the azimuth, its distance from the middle the spread. \
             The strip on the right sets the height."
        } else {
            "The azimuth and spread are wired."
        });
        if !editable {
            return edits;
        }
        let set = |edits: &mut Vec<Edit>, pos: Pos2, in_strip: bool| {
            if in_strip {
                if let (Some(strip), true) = (strip, self.elevation_editable) {
                    edits.push(Edit::Drag(Command::SetParam {
                        node: self.node,
                        key: "elevation".to_owned(),
                        value: Some(elevation_at(strip, pos)),
                    }));
                }
                return;
            }
            let (azimuth, spread) = source_at(plan, pos);
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
            // Which part the drag began in decides what it sets.
            let in_strip = response
                .interact_pointer_pos()
                .zip(strip)
                .is_some_and(|(pos, strip)| strip.contains(pos));
            ui.data_mut(|d| d.insert_temp(drag_id, in_strip));
        }
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let in_strip = ui.data(|d| d.get_temp::<bool>(drag_id)).unwrap_or(false);
            set(&mut edits, pos, in_strip);
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
