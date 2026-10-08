//! A clip's waveform, drawn from the file's peaks.

use egui::{Color32, Painter, Pos2, Rect, Stroke, vec2};
use noodle_core::AudioClip;
use noodle_io::{Peak, Peaks};

/// The strip along the top of a clip that its name goes in.
const LABEL: f32 = 15.0;
/// At most this many columns, however wide the clip is zoomed.
const MAX_COLUMNS: usize = 4096;

/// The waveform's colour on a clip of `clip_colour`.
fn ink(clip_colour: Color32) -> Color32 {
    clip_colour.gamma_multiply(0.45)
}

/// Draws the part of the clip that's in view, `visible` of `full`, one pixel
/// column per peak. Says whether it drew anything.
pub fn draw(
    painter: &Painter,
    full: Rect,
    visible: Rect,
    peaks: &Peaks,
    audio: &AudioClip,
    clip_colour: Color32,
) -> bool {
    let area = Rect::from_min_max(full.min + vec2(0.0, LABEL), full.max);
    if area.height() < 4.0 || full.width() <= 0.0 {
        return false;
    }
    // The visible part of the clip as a part of its audio.
    let from = f64::from((visible.left() - full.left()) / full.width());
    let to = f64::from((visible.right() - full.left()) / full.width());
    let length = audio.length as f64;
    let start = audio.offset + (length * from).round() as u64;
    let end = audio.offset + (length * to).round() as u64;
    let columns = (visible.width().ceil() as usize).clamp(1, MAX_COLUMNS);
    let stroke = Stroke::new(visible.width() / columns as f32, ink(clip_colour));
    let middle = area.center().y;
    let half = area.height() / 2.0 - 1.0;
    for (i, peak) in peaks
        .columns(None, start, end, columns)
        .into_iter()
        .enumerate()
    {
        let Peak { min, max } = scaled(peak, audio.gain);
        let x = visible.left() + (i as f32 + 0.5) * visible.width() / columns as f32;
        // At least a hairline, so silence still shows the clip has audio.
        let top = middle - max * half - 0.5;
        let bottom = middle - min * half + 0.5;
        painter.line_segment([Pos2::new(x, top), Pos2::new(x, bottom)], stroke);
    }
    true
}

/// The peak at the clip's gain, kept on the screen.
fn scaled(peak: Peak, gain: f32) -> Peak {
    Peak {
        min: (peak.min * gain).clamp(-1.0, 1.0),
        max: (peak.max * gain).clamp(-1.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clips_gain_scales_a_peak_but_not_off_the_screen() {
        let p = scaled(
            Peak {
                min: -0.25,
                max: 0.5,
            },
            2.0,
        );
        assert_eq!((p.min, p.max), (-0.5, 1.0));
        let p = scaled(
            Peak {
                min: -0.9,
                max: 0.9,
            },
            4.0,
        );
        assert_eq!((p.min, p.max), (-1.0, 1.0));
    }
}
