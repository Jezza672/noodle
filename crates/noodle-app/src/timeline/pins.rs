//! The warp markers of an audio clip, drawn on it: a line and a small
//! triangle where each pinned moment plays now. "Pin transients" puts one on
//! every hit, so they show where the clip will be cut for Slices mode and
//! what quantise moves; with quantise on they are drawn where the moved
//! markers play.

use egui::{Painter, Pos2, Rect, Shape, Stroke, vec2};
use noodle_core::TempoMap;
use noodle_core::{Clip, ClipWarp};

use super::{Axis, clips, colors};

/// The most markers drawn on one clip.
const MAX_DRAWN: usize = 1_000;
/// Markers closer than this on screen are not told apart, so a dense clip
/// does not turn solid.
const MIN_GAP: f32 = 3.0;
/// Half the width of the triangle.
const FLAG: f32 = 3.5;

/// Draws the markers of `warp` on the clip drawn in `full`, within `visible`.
/// Returns how many were drawn.
pub fn draw(
    painter: &Painter,
    axis: Axis,
    map: &TempoMap,
    clip: &Clip,
    rate: u32,
    warp: &ClipWarp,
    (full, visible): (Rect, Rect),
) -> usize {
    if !warp.use_markers || warp.markers.is_empty() {
        return 0;
    }
    let stroke = Stroke::new(1.0, colors::SELECTED.gamma_multiply(0.85));
    let mut drawn = 0;
    let mut last = f32::NEG_INFINITY;
    for marker in warp.markers.iter().take(MAX_DRAWN) {
        let frame = (marker.seconds * f64::from(rate)).round().max(0.0) as u64;
        let Some(tick) = clips::tick_of_frame(map, clip, frame, rate, Some(warp)) else {
            continue;
        };
        let x = axis.x(tick);
        if x < visible.left() || x > visible.right() || x < full.left() || x > full.right() {
            continue;
        }
        if x - last < MIN_GAP {
            continue;
        }
        last = x;
        painter.vline(x, full.y_range(), stroke);
        painter.add(Shape::convex_polygon(
            vec![
                Pos2::new(x, full.top() + FLAG * 2.0),
                Pos2::new(x, full.top() + FLAG * 2.0) + vec2(-FLAG, -FLAG * 2.0),
                Pos2::new(x, full.top() + FLAG * 2.0) + vec2(FLAG, -FLAG * 2.0),
            ],
            colors::SELECTED,
            Stroke::NONE,
        ));
        drawn += 1;
    }
    drawn
}
