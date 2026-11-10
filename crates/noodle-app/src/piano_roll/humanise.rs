//! Humanise in the piano roll: a button that gives the selected notes stored
//! random offsets, and the clip's two amounts that scale them when it plays.
//!
//! The notes keep the places they are drawn at; only the clip's scheduling
//! moves them, so halving an amount is one change, a fresh Humanise re-rolls
//! just the selection, and undo restores it all. Each humanised note is drawn
//! with a ghost where it will sound.

use std::collections::BTreeSet;

use egui::{Stroke, StrokeKind};
use noodle_core::{Clip, ClipContent, ClipId, MidiClip, Tick};

use super::{Axes, Output};
use crate::session::Edit;
use crate::theme;
use crate::toolbar::{Icon, Mode, Opens, Pill, pill_with_popover};

/// What the amounts become the first time Humanise is used on a clip whose
/// amounts are both zero: about 15 ms at 120 bpm, and a tenth of full scale.
const FIRST_TIMING: i64 = 30;
const FIRST_VELOCITY: f32 = 0.1;
/// The most the toolbar lets the amounts go: a sixteenth note, half of full
/// scale.
const MAX_TIMING: i64 = 240;
const MAX_VELOCITY: f32 = 0.5;

fn set_midi(id: ClipId, clip: &Clip, midi: MidiClip, drag: bool) -> Edit {
    let command = noodle_core::Command::SetClip {
        id,
        clip: Clip {
            content: ClipContent::Midi(midi),
            ..clip.clone()
        },
    };
    if drag {
        Edit::Drag(command)
    } else {
        Edit::Apply(command)
    }
}

/// The Humanise pill, which re-rolls the selected notes, and its popover with
/// the clip's Timing and Velocity amounts.
pub(super) fn controls(
    ui: &mut egui::Ui,
    mode: Mode,
    out: &mut Output,
    selected: &BTreeSet<u32>,
    id: ClipId,
    clip: &Clip,
    midi: &MidiClip,
) {
    let spec = Pill {
        icon: Icon::Dice,
        label: "Humanise",
        active: false,
        enabled: !selected.is_empty(),
        tip: "Give the selected notes a new random feel. The notes stay where they are drawn; \
              the amounts in the arrow beside it set how far they play from there.",
    };
    let pressed = pill_with_popover(ui, mode, &spec, Opens::Chevron, "Humanise amounts", |ui| {
        crate::inline_result::show(ui, &format!("humanise {id:?}"));
        amounts(ui, out, id, clip, midi);
    })
    .pressed;
    if pressed {
        let mut changed = midi.clone();
        let ids: Vec<u32> = selected.iter().copied().collect();
        let seed = ui.input(|i| i.time).to_bits() ^ (id.0 << 32);
        changed.roll_humanise(&ids, seed);
        if changed.humanise_timing == Tick::ZERO && changed.humanise_velocity == 0.0 {
            changed.humanise_timing = Tick(FIRST_TIMING);
            changed.humanise_velocity = FIRST_VELOCITY;
        }
        crate::inline_result::remember(
            ui,
            &format!("humanise {id:?}"),
            format!(
                "Humanised {} note{}: up to {} ticks and {:.0}% velocity either way",
                ids.len(),
                if ids.len() == 1 { "" } else { "s" },
                changed.humanise_timing.0,
                changed.humanise_velocity * 100.0,
            ),
        );
        out.edits.push(set_midi(id, clip, changed, false));
    }
}

/// The clip's Timing and Velocity amounts.
fn amounts(ui: &mut egui::Ui, out: &mut Output, id: ClipId, clip: &Clip, midi: &MidiClip) {
    let mut timing = midi.humanise_timing.0;
    let mut velocity = midi.humanise_velocity * 100.0;
    let t = ui
        .horizontal(|ui| {
            ui.label("Timing");
            ui.add(
                egui::DragValue::new(&mut timing)
                    .range(0..=MAX_TIMING)
                    .suffix(" ticks"),
            )
        })
        .inner;
    let v = ui
        .horizontal(|ui| {
            ui.label("Velocity");
            ui.add(
                egui::DragValue::new(&mut velocity)
                    .range(0.0..=MAX_VELOCITY * 100.0)
                    .suffix("%"),
            )
        })
        .inner;
    for response in [&t, &v] {
        if response.changed() {
            let mut changed = midi.clone();
            changed.humanise_timing = Tick(timing);
            changed.humanise_velocity = velocity / 100.0;
            out.edits
                .push(set_midi(id, clip, changed, response.dragged()));
        }
        if response.drag_stopped() {
            out.edits.push(Edit::EndDrag);
        }
    }
}

/// A faint outline at where each humanised note will play.
pub(super) fn ghosts(ui: &egui::Ui, axes: Axes, midi: &MidiClip) {
    let painter = ui.painter_at(axes.grid);
    for note in &midi.notes {
        let shift = midi.humanise_shift(note);
        if shift == 0 {
            continue;
        }
        let mut ghost = note.clone();
        ghost.start = Tick(note.start.0 + shift);
        let rect = axes.note_rect(&ghost);
        if rect.intersects(axes.grid) {
            painter.rect_stroke(
                rect,
                2.0,
                Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.7)),
                StrokeKind::Inside,
            );
        }
    }
}
