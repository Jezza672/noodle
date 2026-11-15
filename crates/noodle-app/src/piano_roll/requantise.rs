//! Quantise again after recording: a recorded clip keeps the raw take (each
//! note's start as played), so the grid can be changed, or taken away, once
//! the take is done. Notes moved by hand since stay where they were put.

use noodle_core::{Clip, ClipContent, ClipId, MidiClip};

use super::Output;
use crate::midi_take::Quantise;
use crate::session::Edit;
use crate::toolbar::{Icon, Mode, Opens, Pill, pill_with_popover};

/// How many of the clip's notes still have a raw take to quantise from.
fn recorded(midi: &MidiClip) -> usize {
    midi.notes.iter().filter(|n| n.played.is_some()).count()
}

/// The clip quantised to `quantise`, if that moves any note.
pub(super) fn requantised(clip: &Clip, midi: &MidiClip, quantise: Quantise) -> Option<Clip> {
    let mut changed = midi.clone();
    (changed.requantise(quantise.grid(), clip.start) > 0).then(|| Clip {
        content: ClipContent::Midi(changed),
        ..clip.clone()
    })
}

/// The Re-quantise pill: a list of grids, and Off to put the recorded notes
/// back as they were played. Disabled for a clip with no raw take.
pub(super) fn controls(
    ui: &mut egui::Ui,
    mode: Mode,
    out: &mut Output,
    id: ClipId,
    clip: &Clip,
    midi: &MidiClip,
) {
    let count = recorded(midi);
    let spec = Pill {
        icon: Icon::Grid,
        label: "Re-quantise",
        active: false,
        enabled: count > 0,
        tip: "Snap the notes you recorded to a grid again, or put them back as you played \
              them. Notes you have moved since stay where they are.",
    };
    pill_with_popover(ui, mode, &spec, Opens::Itself, "Re-quantise", |ui| {
        crate::inline_result::show(ui, &format!("requantise {id:?}"));
        ui.weak(format!(
            "{count} recorded note{}",
            if count == 1 { "" } else { "s" }
        ));
        for quantise in Quantise::ALL {
            let label = if quantise == Quantise::Off {
                "As played"
            } else {
                quantise.label()
            };
            if ui.button(label).clicked() {
                match requantised(clip, midi, quantise) {
                    Some(changed) => {
                        crate::inline_result::remember(
                            ui,
                            &format!("requantise {id:?}"),
                            format!("Notes set to {label}"),
                        );
                        out.edits.push(Edit::Apply(noodle_core::Command::SetClip {
                            id,
                            clip: changed,
                        }));
                    }
                    None => crate::inline_result::remember(
                        ui,
                        &format!("requantise {id:?}"),
                        format!("Already on {label}: nothing moved"),
                    ),
                }
            }
        }
    });
}
