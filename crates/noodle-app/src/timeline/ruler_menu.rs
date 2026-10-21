//! The ruler's right-click menu: add a tempo change or a time signature
//! change at the bar nearest the click, or change or remove a time signature
//! change by right-clicking its marker. Signature changes sit on bar lines
//! only (see docs/architecture/data-model.md), so every spot snaps to a bar.

use egui::{Align2, FontId, Pos2, Rect, Response, Stroke};
use noodle_core::{Command, SignatureChange, TempoMap, TimeSignature};

use super::Axis;
use crate::session::Edit;
use crate::tempo;
use crate::theme::timeline as colors;

/// How near a click has to be to a marker to be on it.
const GRAB: f32 = 8.0;

/// Where the ruler was last right-clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Spot {
    /// The bar nearest the click, counting from 0.
    bar: u32,
    /// The time signature change the click was on, if any.
    marker: Option<usize>,
}

#[derive(Default)]
pub struct State {
    spot: Option<Spot>,
}

/// The time signature as a marker on the ruler reads it.
fn label(signature: TimeSignature) -> String {
    signature.to_string()
}

/// The marker of a time signature change `pos` is on.
fn marker_at(map: &TempoMap, axis: Axis<'_>, pos: Pos2) -> Option<usize> {
    map.signatures()
        .iter()
        .enumerate()
        .skip(1)
        .map(|(i, change)| (i, (axis.x(map.bar_start(change.bar)) - pos.x).abs()))
        .filter(|&(_, distance)| distance <= GRAB)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Opens the menu on a right-click of `ruler` and runs it.
pub fn show(state: &mut State, ruler: &Response, map: &TempoMap, axis: Axis<'_>) -> Vec<Edit> {
    if ruler.secondary_clicked()
        && let Some(pos) = ruler.interact_pointer_pos()
    {
        let tick = axis.tick(pos.x);
        state.spot = Some(Spot {
            bar: map.nearest_bar(tick),
            marker: marker_at(map, axis, pos),
        });
    }
    let mut edits = Vec::new();
    let Some(spot) = state.spot else {
        return edits;
    };
    ruler.clone().context_menu(|ui| {
        let at = map.bar_start(spot.bar);
        if let Some(index) = spot.marker.filter(|&i| i < map.signatures().len()) {
            let change = map.signatures()[index];
            ui.label(format!(
                "{} from bar {}",
                label(change.signature),
                change.bar + 1
            ));
            ui.menu_button("Change time signature", |ui| {
                for (numerator, denominator) in tempo::SIGNATURES {
                    let signature = TimeSignature {
                        numerator,
                        denominator,
                    };
                    if ui.button(label(signature)).clicked() {
                        ui.close();
                        edits.extend(
                            tempo::with_signature_at(map, change.bar, signature)
                                .map(|(new, _)| Edit::Apply(Command::SetTempoMap(new))),
                        );
                    }
                }
            });
            if ui.button("Remove time signature change").clicked() {
                ui.close();
                edits.extend(
                    tempo::signature_removed(map, index)
                        .map(|new| Edit::Apply(Command::SetTempoMap(new))),
                );
            }
            ui.separator();
        }
        if ui
            .button(format!("Add tempo change at bar {}", spot.bar + 1))
            .clicked()
        {
            ui.close();
            edits.extend(
                tempo::with_change(map, at, map.bpm_at(at))
                    .filter(|(new, _)| new != map)
                    .map(|(new, _)| Edit::Apply(Command::SetTempoMap(new))),
            );
        }
        ui.menu_button(
            format!("Add time signature change at bar {}", spot.bar + 1),
            |ui| {
                for (numerator, denominator) in tempo::SIGNATURES {
                    let signature = TimeSignature {
                        numerator,
                        denominator,
                    };
                    if ui.button(label(signature)).clicked() {
                        ui.close();
                        edits.extend(
                            tempo::with_signature_at(map, spot.bar, signature)
                                .map(|(new, _)| Edit::Apply(Command::SetTempoMap(new))),
                        );
                    }
                }
            },
        );
    });
    edits
}

/// A marker on `strip` at every time signature change after the first, which
/// the transport shows.
pub fn draw_markers(ui: &egui::Ui, strip: Rect, map: &TempoMap, axis: Axis<'_>) -> Vec<String> {
    let painter = ui.painter_at(strip);
    let mut drawn = Vec::new();
    for &SignatureChange { bar, signature } in map.signatures().iter().skip(1) {
        let x = axis.x(map.bar_start(bar));
        painter.vline(
            x,
            strip.y_range(),
            Stroke::new(1.5, colors::AUTOMATION_LINE),
        );
        painter.text(
            Pos2::new(x + 4.0, strip.bottom() - 2.0),
            Align2::LEFT_BOTTOM,
            label(signature),
            FontId::proportional(10.0),
            colors::AUTOMATION_LINE,
        );
        drawn.push(label(signature));
    }
    drawn
}
