//! A Looper node's live state in the properties panel: what it is doing
//! (empty, recording, playing, overdubbing), where the playhead is in the
//! loop, and whether a change waits for the loop's end.

use egui::{Color32, RichText, Sense, Ui, vec2};
use noodle_core::{Node, NodeId};
use noodle_engine::{LoopReading, LoopState};

use crate::session::{Edit, Session};
use crate::theme;

/// The words for a state.
pub fn state_text(reading: Option<LoopReading>) -> &'static str {
    match reading.map(|r| r.state) {
        None => "No signal yet",
        Some(LoopState::Empty) => "Empty",
        Some(LoopState::Recording) => "Recording",
        Some(LoopState::Playing) => "Playing",
        Some(LoopState::Overdubbing) => "Overdubbing",
    }
}

/// What the panel says about a change that is waiting.
pub fn queued_text(reading: LoopReading) -> Option<&'static str> {
    if !reading.queued {
        return None;
    }
    Some(match reading.state {
        LoopState::Recording => "Ends at the next bar line",
        LoopState::Playing => "Overdub starts at the loop's end",
        LoopState::Overdubbing => "Changes at the loop's end",
        LoopState::Empty => return None,
    })
}

pub fn show(ui: &mut Ui, session: &Session, id: NodeId, _node: &Node, _edits: &mut Vec<Edit>) {
    ui.separator();
    ui.label(RichText::new("Loop").strong());
    if !session.engine_running() {
        ui.weak("The engine is off, so there is nothing to show.");
        return;
    }
    let reading = session.telemetry().read_loop(id);
    let colour = match reading.map(|r| r.state) {
        Some(LoopState::Recording) => Color32::from_rgb(230, 80, 80),
        Some(LoopState::Overdubbing) => Color32::from_rgb(235, 170, 60),
        Some(LoopState::Playing) => theme::ACCENT,
        _ => Color32::GRAY,
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 5.0, colour);
        ui.label(state_text(reading));
        if reading.is_some_and(|r| r.has_layer) {
            ui.weak("(a layer to undo)");
        }
    });
    let width = ui.available_width();
    let (response, painter) = ui.allocate_painter(vec2(width, 14.0), Sense::hover());
    let rect = response.rect;
    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
    if let Some(r) = reading.filter(|r| r.state != LoopState::Empty) {
        let x = rect.left() + r.at.clamp(0.0, 1.0) * rect.width();
        let mut filled = rect;
        filled.set_right(x);
        painter.rect_filled(filled, 3.0, colour.gamma_multiply(0.35));
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(2.0, colour),
        );
    }
    response.on_hover_text("Where the loop is. While recording, how far through the length chosen (or the most it can hold).");
    if let Some(text) = reading.and_then(queued_text) {
        ui.label(RichText::new(text).small().weak());
    }
    // The playhead moves without any edit.
    ui.ctx().request_repaint();
}
