//! The gain envelope of an audio clip or a sample, drawn as a line over its waveform with
//! a handle on each point. Double-click the waveform to add a point, drag a
//! point to move it (it stops at its neighbours), double-click a point to
//! remove it.

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, vec2};
use noodle_core::{Command, GAIN_POINT_MAX_DB, GAIN_POINT_MIN_DB, GainPoint, Tick};

use super::subject::Subject;
use crate::session::Edit;
use crate::theme::timeline as colors;

const HANDLE: f32 = 5.0;

/// How the line sits on the screen: the clip's window in x, the dB range in y.
pub(super) struct Frame<'a> {
    pub wave: Rect,
    /// The clip's length in ticks, which bounds the points.
    pub length: Tick,
    pub x_of: &'a dyn Fn(Tick) -> f32,
    pub tick_at: &'a dyn Fn(f32) -> Tick,
}

/// The screen height of `db` in `wave`: the bottom of the range at the
/// bottom.
pub(super) fn y_of(wave: Rect, db: f32) -> f32 {
    let along = (db.clamp(GAIN_POINT_MIN_DB, GAIN_POINT_MAX_DB) - GAIN_POINT_MIN_DB)
        / (GAIN_POINT_MAX_DB - GAIN_POINT_MIN_DB);
    wave.bottom() - along * wave.height()
}

fn db_at(wave: Rect, y: f32) -> f32 {
    let along = ((wave.bottom() - y) / wave.height().max(1.0)).clamp(0.0, 1.0);
    GAIN_POINT_MIN_DB + along * (GAIN_POINT_MAX_DB - GAIN_POINT_MIN_DB)
}

/// Draws the envelope `points` of `id` and handles its edits, which `set`
/// turns into commands. `waveform` is the response of the waveform behind it,
/// for a double-click that adds a point.
pub(super) fn show(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    frame: &Frame<'_>,
    id: Subject,
    points: &[GainPoint],
    set: &dyn Fn(Vec<GainPoint>) -> Option<Command>,
    waveform: &egui::Response,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    let colour = Color32::from_rgb(0xF0, 0xB4, 0x4A);
    let at = |p: &GainPoint| Pos2::new((frame.x_of)(p.tick), y_of(frame.wave, p.db));
    if let (Some(first), Some(last)) = (points.first(), points.last()) {
        let mut line = vec![Pos2::new(frame.wave.left(), at(first).y)];
        line.extend(points.iter().map(at));
        line.push(Pos2::new(frame.wave.right(), at(last).y));
        painter.add(egui::Shape::line(line, Stroke::new(1.5, colour)));
    }
    let neighbours = |i: usize| {
        (
            i.checked_sub(1).map_or(Tick::ZERO, |k| points[k].tick),
            points.get(i + 1).map_or(frame.length, |p| p.tick),
        )
    };
    let mut handled = false;
    for (i, point) in points.iter().enumerate() {
        let centre = at(point);
        if !frame.wave.expand(HANDLE).contains(centre) {
            continue;
        }
        let rect = Rect::from_center_size(centre, vec2(HANDLE * 3.0, HANDLE * 3.0));
        let response = ui.interact(
            rect,
            ui.id().with(("gain point", id, i)),
            Sense::click_and_drag(),
        );
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        let lit = response.hovered() || response.dragged();
        painter.circle(
            centre,
            if lit { HANDLE + 1.0 } else { HANDLE },
            if lit { colour } else { colors::BACKGROUND },
            Stroke::new(1.5, colour),
        );
        if response.double_clicked() {
            let mut rest = points.to_vec();
            rest.remove(i);
            edits.extend(set(rest).map(Edit::Apply));
            handled = true;
        } else if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let (lo, hi) = neighbours(i);
            let mut moved = points.to_vec();
            moved[i] = GainPoint {
                tick: (frame.tick_at)(pos.x).clamp(lo, hi),
                db: db_at(frame.wave, pos.y),
            };
            if moved != *points {
                edits.extend(set(moved).map(Edit::Drag));
            }
            handled = true;
        } else if response.drag_stopped() {
            edits.push(Edit::EndDrag);
            handled = true;
        }
    }
    if !handled
        && waveform.double_clicked()
        && let Some(pos) = waveform.interact_pointer_pos()
    {
        let tick = (frame.tick_at)(pos.x).clamp(Tick::ZERO, frame.length);
        let mut added = points.to_vec();
        let place = added.partition_point(|p| p.tick <= tick);
        added.insert(
            place,
            GainPoint {
                tick,
                db: db_at(frame.wave, pos.y),
            },
        );
        edits.extend(set(added).map(Edit::Apply));
    }
    edits
}
