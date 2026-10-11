//! The take being recorded, drawn on the arrangement as a clip that grows
//! with the playhead: its notes or waveform as they come in.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, vec2};
use noodle_core::NodeId;
use noodle_io::Peak;

use super::{Axis, colors, midi_preview, waveform};
use crate::session::TakePreview;

/// Draws `take` on a lane `top` pixels down, if it records on that track.
/// Says what it drew, for tests.
pub fn draw(
    painter: &Painter,
    axis: Axis,
    top: f32,
    take: &TakePreview,
    track: NodeId,
    waveform_of: impl FnOnce(usize) -> Vec<Peak>,
) -> Option<(Rect, String)> {
    let full = Rect::from_min_max(
        Pos2::new(axis.x(take.start), top + 3.0),
        Pos2::new(
            axis.x(take.end).max(axis.x(take.start) + 2.0),
            top + colors::LANE_HEIGHT - 3.0,
        ),
    );
    let visible = full.intersect(painter.clip_rect());
    if visible.width() <= 0.0 || visible.height() <= 0.0 {
        return None;
    }
    let red = crate::theme::RECORD;
    painter.rect_filled(
        full,
        f32::from(crate::theme::RADIUS),
        red.gamma_multiply(0.35),
    );
    let (audio, midi) = (
        take.audio && take.audio_tracks.contains(&track),
        take.midi_tracks.contains(&track),
    );
    if !audio && !midi {
        return None;
    }
    if audio {
        let area = Rect::from_min_max(full.min + vec2(0.0, waveform::LABEL), full.max);
        let width = full.width();
        if area.height() >= 4.0 && width > 0.0 {
            // One column per pixel of the whole take, however much is in view.
            let columns = (width.ceil() as usize).clamp(1, 4096);
            let peaks = waveform_of(columns);
            let stroke = Stroke::new(width / columns as f32, waveform::ink(red));
            let middle = area.center().y;
            let half = area.height() / 2.0 - 1.0;
            for (i, peak) in peaks.iter().enumerate() {
                let x = full.left() + (i as f32 + 0.5) * width / columns as f32;
                if x < visible.left() - 1.0 || x > visible.right() + 1.0 {
                    continue;
                }
                let top = middle - peak.max.clamp(-1.0, 1.0) * half - 0.5;
                let bottom = middle - peak.min.clamp(-1.0, 1.0) * half + 0.5;
                painter.line_segment([Pos2::new(x, top), Pos2::new(x, bottom)], stroke);
            }
        }
    }
    let mut notes = 0;
    if midi && let Some((start, clip)) = &take.midi {
        midi_preview::draw(painter, axis, full, *start, clip);
        notes = clip.notes.len();
    }
    let label = match (audio, notes) {
        (true, 0) => "Recording".to_owned(),
        (_, n) => format!("Recording ({n} notes)"),
    };
    painter.text(
        full.left_top() + vec2(6.0, 4.0),
        Align2::LEFT_TOP,
        &label,
        FontId::proportional(11.0),
        Color32::WHITE.gamma_multiply(0.85),
    );
    painter.rect_stroke(
        full,
        f32::from(crate::theme::RADIUS),
        Stroke::new(1.5, red),
        egui::StrokeKind::Inside,
    );
    Some((full, label))
}
