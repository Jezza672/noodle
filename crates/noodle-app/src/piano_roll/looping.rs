//! A MIDI clip's loop in the piano roll: the Loop toggle with its start and
//! length in the toolbar, and a brace on the ruler whose ends and body drag.

use egui::{Pos2, Rect, Sense, Stroke, vec2};
use noodle_core::{Clip, ClipContent, ClipId, MIN_LOOP_LEN, MidiClip, Tick};

use super::{Axes, EDGE, Output, PianoRoll};
use crate::session::Edit;
use crate::theme;
use noodle_core::Command;

/// Which part of the brace is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    Start,
    End,
    Body,
}

/// A drag on the brace: the loop as it was, and where the press was.
#[derive(Clone, Copy, Debug)]
pub(super) struct LoopDrag {
    part: Part,
    start: i64,
    end: i64,
    press_tick: i64,
}

fn set_midi(id: ClipId, clip: &Clip, midi: MidiClip, drag: bool) -> Edit {
    let command = Command::SetClip {
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

/// The Loop checkbox and, while looping, the loop's start and length in beats.
pub(super) fn controls(
    ui: &mut egui::Ui,
    out: &mut Output,
    id: ClipId,
    clip: &Clip,
    midi: &MidiClip,
) {
    let mut on = midi.loop_on;
    if ui
        .checkbox(&mut on, "Loop")
        .on_hover_text("Repeat a section of the clip until its end")
        .changed()
    {
        let mut changed = midi.clone();
        changed.set_looping(on);
        out.edits.push(set_midi(id, clip, changed, false));
    }
    if !midi.loop_on {
        return;
    }
    let beat = noodle_core::TICKS_PER_QUARTER as f64;
    let min = MIN_LOOP_LEN as f64 / beat;
    let length = midi.length.0 as f64 / beat;
    let mut start = midi.loop_start.0 as f64 / beat;
    let mut len = midi.loop_len.0 as f64 / beat;
    ui.label("Start");
    let a = ui.add(
        egui::DragValue::new(&mut start)
            .speed(0.05)
            .range(0.0..=(length - min).max(0.0))
            .suffix(" beats"),
    );
    ui.label("Length");
    let b = ui.add(
        egui::DragValue::new(&mut len)
            .speed(0.05)
            .range(min..=(length - start).max(min))
            .suffix(" beats"),
    );
    for response in [&a, &b] {
        if response.changed() {
            let mut changed = midi.clone();
            let from = Tick((start * beat).round() as i64);
            changed.set_loop(from, Tick(from.0 + (len * beat).round() as i64));
            out.edits
                .push(set_midi(id, clip, changed, response.dragged()));
        }
        if response.drag_stopped() {
            out.edits.push(Edit::EndDrag);
        }
    }
}

/// The brace on the ruler, with its drag handles. Does nothing unless the
/// clip loops.
#[allow(clippy::too_many_arguments)]
pub(super) fn brace(
    ui: &egui::Ui,
    state: &mut PianoRoll,
    out: &mut Output,
    id: ClipId,
    clip: &Clip,
    midi: &MidiClip,
    ruler: Rect,
    axes: Axes,
    snap: &dyn Fn(i64) -> i64,
) {
    if !midi.loop_on {
        state.loop_drag = None;
        return;
    }
    let x0 = axes.x(midi.loop_start.0);
    let x1 = axes.x(midi.loop_end().0);
    let painter = ui.painter_at(ruler);
    let body = Rect::from_x_y_ranges(x0..=x1, ruler.y_range());
    painter.rect_filled(body, 0.0, theme::ACCENT.gamma_multiply(0.35));
    for x in [x0, x1] {
        painter.vline(x, ruler.y_range(), Stroke::new(2.0, theme::ACCENT));
    }
    // The body first, so the ends are on top of it.
    let parts = [
        (Part::Body, body.shrink2(vec2(EDGE, 0.0))),
        (
            Part::Start,
            Rect::from_x_y_ranges(x0 - EDGE..=x0 + EDGE, ruler.y_range()),
        ),
        (
            Part::End,
            Rect::from_x_y_ranges(x1 - EDGE..=x1 + EDGE, ruler.y_range()),
        ),
    ];
    for (part, rect) in parts {
        if rect.width() <= 0.0 {
            continue;
        }
        let response = ui.interact(
            rect,
            ui.id().with(("piano roll loop", part as u8)),
            Sense::drag(),
        );
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(match part {
                Part::Body => egui::CursorIcon::Grab,
                _ => egui::CursorIcon::ResizeHorizontal,
            });
        }
        if response.drag_started()
            && let Some(press) = ui
                .input(|i| i.pointer.press_origin())
                .or(response.interact_pointer_pos())
        {
            state.loop_drag = Some(LoopDrag {
                part,
                start: midi.loop_start.0,
                end: midi.loop_end().0,
                press_tick: axes.tick(press.x),
            });
        }
        if response.dragged()
            && let (Some(drag), Some(pos)) = (state.loop_drag, response.interact_pointer_pos())
            && drag.part == part
        {
            let at = axes.tick(pos.x);
            let (start, end) = match part {
                Part::Start => (snap(at).min(drag.end - MIN_LOOP_LEN), drag.end),
                Part::End => (drag.start, snap(at).max(drag.start + MIN_LOOP_LEN)),
                Part::Body => {
                    let len = drag.end - drag.start;
                    let start = snap(drag.start + at - drag.press_tick)
                        .clamp(0, (midi.length.0 - len).max(0));
                    (start, start + len)
                }
            };
            let mut changed = midi.clone();
            changed.set_loop(Tick(start), Tick(end));
            if changed != *midi {
                out.edits.push(set_midi(id, clip, changed, true));
            }
        }
        if response.drag_stopped() && state.loop_drag.take().is_some() {
            out.edits.push(Edit::EndDrag);
        }
    }
}

/// Lines where the loop starts and ends, across the grid.
pub(super) fn draw_lines(ui: &egui::Ui, axes: Axes, midi: &MidiClip) {
    if !midi.loop_on {
        return;
    }
    let painter = ui.painter_at(axes.grid);
    for tick in [midi.loop_start.0, midi.loop_end().0] {
        painter.vline(
            axes.x(tick),
            axes.grid.y_range(),
            Stroke::new(1.5, theme::ACCENT.gamma_multiply(0.8)),
        );
    }
    // Notes that start after the loop's end are never played.
    let from = axes.x(midi.loop_end().0).max(axes.grid.left());
    let to = axes.x(midi.length.0).min(axes.grid.right());
    if to > from {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(from, axes.grid.top()),
                Pos2::new(to, axes.grid.bottom()),
            ),
            0.0,
            egui::Color32::BLACK.gamma_multiply(0.3),
        );
    }
}

/// The later passes through the loop, drawn faintly where they sound.
pub(super) fn draw_repeats(ui: &egui::Ui, axes: Axes, midi: &MidiClip) {
    let painter = ui.painter_at(axes.grid);
    for note in midi.repeats() {
        let rect = axes.note_rect(&note);
        if rect.intersects(axes.grid) {
            painter.rect_filled(rect, 2.0, theme::ACCENT.gamma_multiply(0.22));
        }
    }
}
