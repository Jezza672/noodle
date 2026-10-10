//! A MIDI clip's notes drawn small inside its block on the arrangement, the
//! way a waveform is for audio.

use egui::{Color32, Painter, Pos2, Rect, vec2};
use noodle_core::{MidiClip, Tick};

use super::Axis;

/// The strip along the top of a clip that its name goes in.
const LABEL: f32 = 15.0;
/// The fewest keys the preview spans, so one note doesn't fill the block.
const MIN_SPAN: u8 = 12;

pub fn draw(painter: &Painter, axis: Axis, full: Rect, start: Tick, midi: &MidiClip) {
    let (Some(low), Some(high)) = (
        midi.notes.iter().map(|n| n.key).min(),
        midi.notes.iter().map(|n| n.key).max(),
    ) else {
        return;
    };
    let span = (high - low + 1).max(MIN_SPAN);
    let area = Rect::from_min_max(
        Pos2::new(full.left(), full.top() + LABEL),
        Pos2::new(full.right(), full.bottom() - 2.0),
    );
    if area.height() <= 2.0 {
        return;
    }
    let row = area.height() / f32::from(span);
    // Centre the notes' range in the block.
    let slack = f32::from(span - (high - low + 1)) / 2.0;
    let ink = Color32::BLACK.gamma_multiply(0.6);
    for note in &midi.notes {
        let x0 = axis.x(Tick(start.0 + note.start.0));
        let x1 = axis
            .x(Tick(start.0 + note.start.0 + note.length.0))
            .min(full.right());
        let y = area.bottom() - (f32::from(note.key - low) + 1.0 + slack) * row;
        let rect = Rect::from_min_size(Pos2::new(x0, y), vec2((x1 - x0).max(1.0), row.max(1.5)));
        painter.rect_filled(rect, 0.0, ink);
    }
}
