//! The piano roll: the notes of one MIDI clip on a keyboard and a grid.
//!
//! It works in ticks from the clip's start, and every change is an
//! [`Edit`] that sets the clip's notes, so it undoes like anything else. A
//! gesture (dragging notes, drawing one, setting a velocity) works out its
//! result from the notes as they were when it began, so it is one undo step
//! and snapping can't accumulate error.
//!
//! Clicking a key on the keyboard plays it through the MIDI In nodes (see
//! [`Output::audition`]).

mod notes;

use std::collections::BTreeSet;

use egui::{Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, vec2};
use noodle_core::{Clip, ClipContent, ClipId, MidiClip, MidiNote, Project, Tick};

use crate::session::{Edit, Session};
use crate::theme::{self, timeline as colors};
use crate::timeline::grid;
use noodle_core::Command;

const TOOLBAR: f32 = 26.0;
const RULER: f32 = 18.0;
const KEYBOARD: f32 = 64.0;
const VELOCITY: f32 = 56.0;
const KEY_HEIGHT: f32 = 14.0;
/// How far from a note's right end a press resizes it instead of moving it.
const EDGE: f32 = 6.0;
/// Notes narrower than this can only be moved.
const MIN_RESIZABLE: f32 = 14.0;
const MIN_PPQ: f32 = 12.0;
const MAX_PPQ: f32 = 800.0;
/// The velocity a new note gets.
const NEW_VELOCITY: f32 = 0.8;
/// The height the panel opens at.
pub const DEFAULT_HEIGHT: f32 = 300.0;

/// What a drag on the grid is doing.
#[derive(Clone, Debug, PartialEq)]
enum Gesture {
    Move,
    /// Dragging the end of the grabbed note.
    Resize,
    /// Drawing a new note, which is the last of the notes.
    Draw {
        start: i64,
    },
    /// Setting the grabbed note's velocity.
    Velocity,
}

struct Drag {
    gesture: Gesture,
    /// The notes when the gesture began.
    original: Vec<MidiNote>,
    selected: BTreeSet<usize>,
    grabbed: usize,
    press: Pos2,
}

/// The grid step notes snap to, in ticks. 1 is free.
const STEPS: [(&str, i64); 6] = [
    ("Beat", 960),
    ("1/2 beat", 480),
    ("1/4 beat", 240),
    ("1/8 beat", 120),
    ("1/16 beat", 60),
    ("Off", 1),
];

pub struct PianoRoll {
    clip: Option<ClipId>,
    /// Points per quarter note.
    ppq: f32,
    scroll_x: f32,
    /// How far down the keyboard is scrolled, in points from the top note.
    scroll_y: f32,
    /// Fit the clip to the width, and centre on its notes, on the next frame.
    focus: bool,
    step: i64,
    selected: BTreeSet<usize>,
    drag: Option<Drag>,
    /// The key being auditioned from the keyboard.
    sounding: Option<u8>,
    /// The length the last note had, which the next one starts with.
    last_length: Option<i64>,
    #[cfg(test)]
    geometry: Option<Geometry>,
}

/// Where things were drawn last frame, for tests to point at.
#[cfg(test)]
#[derive(Clone, Copy)]
pub struct Geometry {
    pub grid: Rect,
    pub velocity: Rect,
    pub keyboard: Rect,
    origin: f32,
    ppq: f32,
    scroll_y: f32,
}

#[cfg(test)]
impl Geometry {
    pub fn x(&self, tick: i64) -> f32 {
        self.origin + tick as f32 / 960.0 * self.ppq
    }

    /// The vertical centre of `key`'s row.
    pub fn y(&self, key: u8) -> f32 {
        self.grid.top() - self.scroll_y + (127.0 - f32::from(key) + 0.5) * KEY_HEIGHT
    }
}

impl Default for PianoRoll {
    fn default() -> Self {
        Self {
            clip: None,
            ppq: 120.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            focus: false,
            step: 240,
            selected: BTreeSet::new(),
            drag: None,
            sounding: None,
            last_length: None,
            #[cfg(test)]
            geometry: None,
        }
    }
}

impl PianoRoll {
    /// Shows the notes of `clip`.
    pub fn open(&mut self, clip: ClipId) {
        if self.clip != Some(clip) {
            self.clip = Some(clip);
            self.selected.clear();
            self.drag = None;
            self.focus = true;
        }
    }

    pub fn close(&mut self) {
        self.clip = None;
        self.drag = None;
        self.selected.clear();
    }

    /// The clip being edited, if the roll is open.
    pub fn clip(&self) -> Option<ClipId> {
        self.clip
    }

    /// Closes the roll if its clip has gone (or isn't a MIDI clip any more),
    /// and drops selected notes that aren't there, e.g. after an undo.
    pub fn retain_existing(&mut self, project: &Project) {
        let Some(id) = self.clip else { return };
        match project.clip(id).and_then(Clip::as_midi) {
            None => self.close(),
            Some(midi) => {
                self.selected.retain(|&i| i < midi.notes.len());
                if self
                    .drag
                    .as_ref()
                    .is_some_and(|d| d.original.len() > midi.notes.len() + 1)
                {
                    self.drag = None;
                }
            }
        }
    }

    #[cfg(test)]
    pub fn selected(&self) -> &BTreeSet<usize> {
        &self.selected
    }

    #[cfg(test)]
    pub fn geometry(&self) -> Option<Geometry> {
        self.geometry
    }
}

/// What the user did in the piano roll.
#[derive(Default)]
pub struct Output {
    pub edits: Vec<Edit>,
    /// Keys pressed (`true`) or released on the keyboard strip, to play
    /// through the MIDI In nodes.
    pub audition: Vec<(u8, bool)>,
}

/// The piano roll's name for a key: C4 is MIDI 60.
pub fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[usize::from(key % 12)],
        i32::from(key / 12) - 1
    )
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// Where the grid is in time and pitch.
#[derive(Clone, Copy)]
struct Axes {
    grid: Rect,
    /// The screen x of the clip's start.
    origin: f32,
    ppq: f32,
    scroll_y: f32,
}

impl Axes {
    fn x(self, tick: i64) -> f32 {
        self.origin + tick as f32 / 960.0 * self.ppq
    }

    fn tick(self, x: f32) -> i64 {
        ((x - self.origin) / self.ppq * 960.0).round() as i64
    }

    fn y_top(self, key: u8) -> f32 {
        self.grid.top() - self.scroll_y + (127.0 - f32::from(key)) * KEY_HEIGHT
    }

    fn key(self, y: f32) -> u8 {
        let row = ((y - self.grid.top() + self.scroll_y) / KEY_HEIGHT).floor();
        (127.0 - row).clamp(0.0, 127.0) as u8
    }

    fn note_rect(self, note: &MidiNote) -> Rect {
        let top = self.y_top(note.key);
        Rect::from_min_max(
            Pos2::new(self.x(note.start.0), top + 1.0),
            Pos2::new(
                self.x(note.end().0).max(self.x(note.start.0) + 2.0),
                top + KEY_HEIGHT - 1.0,
            ),
        )
    }

    /// The note under `pos`, the last drawn if several overlap.
    fn note_at(self, notes: &[MidiNote], pos: Pos2) -> Option<usize> {
        notes
            .iter()
            .rposition(|note| self.note_rect(note).expand2(vec2(1.0, 0.0)).contains(pos))
    }
}

/// Draws the piano roll for the open clip into `ui` and returns what the user
/// did. `playhead` is the transport position, drawn when it is inside the
/// clip.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut PianoRoll,
    session: &Session,
    playhead: Option<Tick>,
) -> Output {
    let mut out = Output::default();
    let project = session.project();
    state.retain_existing(project);
    let Some(id) = state.clip else { return out };
    let Some(clip) = project.clip(id) else {
        return out;
    };
    let Some(midi) = clip.as_midi() else {
        return out;
    };
    let map = project.tempo_map();
    let length = midi.length.0;
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());

    // The toolbar.
    let toolbar = Rect::from_min_size(rect.min, vec2(rect.width(), TOOLBAR));
    let mut closed = false;
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(toolbar.shrink2(vec2(8.0, 2.0))),
        |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(format!("Piano roll · {id}"));
                ui.separator();
                let label = STEPS
                    .iter()
                    .find(|(_, step)| *step == state.step)
                    .map_or("Grid", |(name, _)| *name);
                egui::ComboBox::from_id_salt("piano roll grid")
                    .selected_text(format!("Grid: {label}"))
                    .show_ui(ui, |ui| {
                        for (name, step) in STEPS {
                            ui.selectable_value(&mut state.step, step, name);
                        }
                    });
                ui.add_space(8.0);
                ui.weak("Drag to draw · Alt: no snap · Delete removes");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        closed = true;
                    }
                });
            });
        },
    );
    if closed {
        state.close();
        return out;
    }

    let body = Rect::from_min_max(Pos2::new(rect.left(), toolbar.bottom()), rect.max);
    let grid_rect = Rect::from_min_max(
        Pos2::new(body.left() + KEYBOARD, body.top() + RULER),
        Pos2::new(
            body.right(),
            (body.bottom() - VELOCITY).max(body.top() + RULER + 40.0),
        ),
    );
    let velocity_rect = Rect::from_min_max(
        Pos2::new(grid_rect.left(), grid_rect.bottom()),
        Pos2::new(grid_rect.right(), body.bottom().max(grid_rect.bottom())),
    );
    let keyboard_rect = Rect::from_min_max(
        Pos2::new(body.left(), grid_rect.top()),
        Pos2::new(grid_rect.left(), grid_rect.bottom()),
    );
    let ruler_rect = Rect::from_min_max(
        Pos2::new(grid_rect.left(), body.top()),
        Pos2::new(grid_rect.right(), grid_rect.top()),
    );

    if state.focus {
        state.focus = false;
        let beats = (length as f32 / 960.0).max(1.0);
        state.ppq = ((grid_rect.width() - 24.0) / beats).clamp(MIN_PPQ, MAX_PPQ);
        state.scroll_x = 0.0;
        let middle = match (
            midi.notes.iter().map(|n| n.key).min(),
            midi.notes.iter().map(|n| n.key).max(),
        ) {
            (Some(low), Some(high)) => (f32::from(low) + f32::from(high)) / 2.0,
            _ => 60.0,
        };
        state.scroll_y = (127.0 - middle) * KEY_HEIGHT - grid_rect.height() / 2.0;
    }
    let hovered = ui.rect_contains_pointer(body);
    if hovered {
        navigate(ui, state, grid_rect, length);
    }
    state.scroll_y = state
        .scroll_y
        .clamp(0.0, (128.0 * KEY_HEIGHT - grid_rect.height()).max(0.0));
    let axes = Axes {
        grid: grid_rect,
        origin: grid_rect.left() - state.scroll_x,
        ppq: state.ppq,
        scroll_y: state.scroll_y,
    };
    #[cfg(test)]
    {
        state.geometry = Some(Geometry {
            grid: grid_rect,
            velocity: velocity_rect,
            keyboard: keyboard_rect,
            origin: axes.origin,
            ppq: axes.ppq,
            scroll_y: axes.scroll_y,
        });
    }

    draw_background(ui, axes, clip, length, map);
    draw_ruler(ui, ruler_rect, axes, clip, map);

    // Notes and the gestures on them.
    let response = ui.interact(
        grid_rect,
        ui.id().with("piano roll grid"),
        Sense::click_and_drag(),
    );
    let free = ui.input(|i| i.modifiers.alt);
    let snap = |tick: i64| notes::snap(tick, if free { 1 } else { state.step });
    let min_length = if free { 1 } else { state.step.max(1) };
    let shift = ui.input(|i| i.modifiers.shift);
    let mut hit_note = false;
    if let Some(pos) = response.hover_pos() {
        let cursor = match axes.note_at(&midi.notes, pos) {
            Some(i) => {
                hit_note = true;
                let r = axes.note_rect(&midi.notes[i]);
                if r.width() >= MIN_RESIZABLE && r.right() - pos.x < EDGE {
                    CursorIcon::ResizeHorizontal
                } else {
                    CursorIcon::Grab
                }
            }
            None => CursorIcon::Crosshair,
        };
        ui.ctx().set_cursor_icon(cursor);
    }

    if response.drag_started()
        && let Some(press) = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos())
    {
        let original = midi.notes.clone();
        match axes.note_at(&original, press) {
            Some(i) => {
                if !state.selected.contains(&i) && !shift {
                    state.selected = BTreeSet::from([i]);
                }
                state.selected.insert(i);
                let r = axes.note_rect(&original[i]);
                let resize = r.width() >= MIN_RESIZABLE && r.right() - press.x < EDGE;
                if resize {
                    state.last_length = None;
                }
                state.drag = Some(Drag {
                    gesture: if resize {
                        Gesture::Resize
                    } else {
                        Gesture::Move
                    },
                    original,
                    selected: state.selected.clone(),
                    grabbed: i,
                    press,
                });
            }
            None => {
                let start = snap(axes.tick(press.x)).max(0);
                state.drag = Some(Drag {
                    gesture: Gesture::Draw { start },
                    original,
                    selected: BTreeSet::new(),
                    grabbed: 0,
                    press,
                });
            }
        }
    } else if response.double_clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && axes.note_at(&midi.notes, pos).is_none()
        && let Some((changed, index)) = notes::added(
            &midi.notes,
            snap(axes.tick(pos.x)),
            state.last_length.unwrap_or(state.step.max(240)),
            axes.key(pos.y),
            NEW_VELOCITY,
            length,
        )
    {
        state.selected = BTreeSet::from([index]);
        out.edits.push(set_notes(id, clip, changed, false));
    } else if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        match axes.note_at(&midi.notes, pos) {
            Some(i) if shift => {
                if !state.selected.remove(&i) {
                    state.selected.insert(i);
                }
            }
            Some(i) => state.selected = BTreeSet::from([i]),
            None => state.selected.clear(),
        }
    }
    if response.dragged()
        && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
    {
        let changed = match &drag.gesture {
            Gesture::Move => {
                let d_tick = if free {
                    axes.tick(pos.x) - axes.tick(drag.press.x)
                } else {
                    // Move by whole grid steps from where the note began.
                    let grabbed = &drag.original[drag.grabbed];
                    snap(grabbed.start.0 + axes.tick(pos.x) - axes.tick(drag.press.x))
                        - grabbed.start.0
                };
                let d_key = i32::from(axes.key(pos.y)) - i32::from(axes.key(drag.press.y));
                Some(notes::moved(
                    &drag.original,
                    &drag.selected,
                    d_tick,
                    d_key,
                    length,
                ))
            }
            Gesture::Resize => {
                let end = snap(axes.tick(pos.x));
                Some(notes::resized(
                    &drag.original,
                    &drag.selected,
                    drag.grabbed,
                    end,
                    min_length,
                    length,
                ))
            }
            Gesture::Draw { start } => {
                let end = snap(axes.tick(pos.x));
                let key = axes.key(drag.press.y);
                notes::added(
                    &drag.original,
                    *start,
                    (end - start).max(min_length),
                    key,
                    NEW_VELOCITY,
                    length,
                )
                .map(|(changed, index)| {
                    state.selected = BTreeSet::from([index]);
                    changed
                })
            }
            Gesture::Velocity => None,
        };
        if let Some(changed) = changed
            && changed != midi.notes
        {
            if let Gesture::Draw { .. } = drag.gesture {
                state.last_length = changed.last().map(|n| n.length.0);
            }
            out.edits.push(set_notes(id, clip, changed, true));
        }
    }
    if response.drag_stopped()
        && let Some(drag) = state.drag.take()
        && !matches!(drag.gesture, Gesture::Velocity)
    {
        out.edits.push(Edit::EndDrag);
    }

    draw_notes(ui, axes, midi, &state.selected);

    // The velocity lane, with a bar per note.
    draw_velocity(ui, state, &mut out, id, clip, midi, velocity_rect, axes);

    // The keyboard, which plays what is clicked.
    draw_keyboard(ui, state, &mut out, keyboard_rect, axes);

    // The playhead.
    if let Some(playhead) = playhead {
        let x = axes.x(playhead.0 - clip.start.0);
        if x >= grid_rect.left() && x <= grid_rect.right() && playhead >= clip.start {
            ui.painter_at(Rect::from_min_max(ruler_rect.min, velocity_rect.max))
                .vline(
                    x,
                    ruler_rect.top()..=velocity_rect.bottom(),
                    Stroke::new(1.5, colors::PLAYHEAD),
                );
        }
    }

    // Keys.
    let typing = ui.ctx().egui_wants_keyboard_input();
    if hovered && !typing && !state.selected.is_empty() {
        keyboard_edits(ui, state, &mut out, id, clip, midi);
    }
    if response.contains_pointer() || hit_note {
        // Keep the pointer's note hot for the cursor above.
    }
    out
}

/// The command that sets a clip's notes.
fn set_notes(id: ClipId, clip: &Clip, notes: Vec<MidiNote>, drag: bool) -> Edit {
    let Some(midi) = clip.as_midi() else {
        unreachable!("the piano roll only edits MIDI clips")
    };
    let command = Command::SetClip {
        id,
        clip: Clip {
            content: ClipContent::Midi(MidiClip {
                notes,
                ..midi.clone()
            }),
            ..clip.clone()
        },
    };
    if drag {
        Edit::Drag(command)
    } else {
        Edit::Apply(command)
    }
}

/// Delete, and the arrow keys: up and down transpose (a semitone, or an
/// octave with Shift), left and right nudge by a grid step.
fn keyboard_edits(
    ui: &egui::Ui,
    state: &mut PianoRoll,
    out: &mut Output,
    id: ClipId,
    clip: &Clip,
    midi: &MidiClip,
) {
    let (delete, up, down, left, right, octave) = ui.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::Delete)
                || i.consume_key(egui::Modifiers::NONE, Key::Backspace),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp)
                || i.consume_key(egui::Modifiers::SHIFT, Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown)
                || i.consume_key(egui::Modifiers::SHIFT, Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowRight),
            i.modifiers.shift,
        )
    });
    let step = state.step.max(1);
    let changed = if delete {
        let changed = notes::removed(&midi.notes, &state.selected);
        state.selected.clear();
        Some(changed)
    } else {
        let d_key = i32::from(up) - i32::from(down);
        let d_tick = (i64::from(right) - i64::from(left)) * step;
        if d_key == 0 && d_tick == 0 {
            None
        } else {
            let d_key = d_key * if octave { 12 } else { 1 };
            Some(notes::moved(
                &midi.notes,
                &state.selected,
                d_tick,
                d_key,
                midi.length.0,
            ))
        }
    };
    if let Some(changed) = changed
        && changed != midi.notes
    {
        out.edits.push(set_notes(id, clip, changed, false));
    }
}

/// Scroll and zoom while the pointer is over the roll.
fn navigate(ui: &egui::Ui, state: &mut PianoRoll, grid: Rect, length: i64) {
    let (scroll, zoom, pointer) =
        ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
    if zoom != 1.0 {
        let anchor = pointer.map_or(grid.left(), |p| p.x.max(grid.left()));
        let quarters = (anchor - grid.left() + state.scroll_x) / state.ppq;
        state.ppq = (state.ppq * zoom).clamp(MIN_PPQ, MAX_PPQ);
        state.scroll_x = quarters * state.ppq - (anchor - grid.left());
    } else {
        state.scroll_x -= scroll.x;
        state.scroll_y -= scroll.y;
    }
    let width = length as f32 / 960.0 * state.ppq;
    state.scroll_x = state
        .scroll_x
        .clamp(0.0, (width - grid.width() * 0.5).max(0.0));
}

fn draw_background(
    ui: &egui::Ui,
    axes: Axes,
    clip: &Clip,
    length: i64,
    map: &noodle_core::TempoMap,
) {
    let painter = ui.painter_at(axes.grid);
    painter.rect_filled(axes.grid, 0.0, colors::BACKGROUND);
    let first = axes.key(axes.grid.bottom());
    let last = axes.key(axes.grid.top());
    for key in first..=last {
        let top = axes.y_top(key);
        let row = Rect::from_min_size(
            Pos2::new(axes.grid.left(), top),
            vec2(axes.grid.width(), KEY_HEIGHT),
        );
        if is_black(key) {
            painter.rect_filled(row, 0.0, Color32::BLACK.gamma_multiply(0.18));
        }
        let line = if key % 12 == 0 {
            colors::BAR_LINE
        } else {
            colors::BEAT_LINE
        };
        painter.hline(
            axes.grid.x_range(),
            top + KEY_HEIGHT,
            Stroke::new(1.0, line),
        );
    }
    // Past the clip's end is dimmed.
    let end = axes.x(length);
    if end < axes.grid.right() {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(end.max(axes.grid.left()), axes.grid.top()),
                axes.grid.max,
            ),
            0.0,
            Color32::BLACK.gamma_multiply(0.45),
        );
    }
    for (tick, bar) in beat_lines(axes, clip, map) {
        let color = if bar.is_some() {
            colors::BAR_LINE
        } else {
            colors::BEAT_LINE
        };
        painter.vline(axes.x(tick), axes.grid.y_range(), Stroke::new(1.0, color));
    }
}

/// The beat lines in view as (tick from the clip's start, the bar number if
/// it is a bar line).
fn beat_lines(axes: Axes, clip: &Clip, map: &noodle_core::TempoMap) -> Vec<(i64, Option<u32>)> {
    let from = clip.start.0 + axes.tick(axes.grid.left());
    let to = clip.start.0 + axes.tick(axes.grid.right());
    grid::lines(map, Tick(from.max(0)), Tick(to.max(0)))
        .into_iter()
        .map(|line| {
            (
                line.tick.0 - clip.start.0,
                match line.kind {
                    grid::Kind::Bar(bar) => Some(bar),
                    grid::Kind::Beat => None,
                },
            )
        })
        .collect()
}

fn draw_ruler(ui: &egui::Ui, rect: Rect, axes: Axes, clip: &Clip, map: &noodle_core::TempoMap) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, colors::RULER);
    for (tick, bar) in beat_lines(axes, clip, map) {
        let x = axes.x(tick);
        let colour = if bar.is_some() {
            colors::BAR_LINE
        } else {
            colors::BEAT_LINE
        };
        painter.vline(x, rect.y_range(), Stroke::new(1.0, colour));
        if let Some(bar) = bar {
            painter.text(
                Pos2::new(x + 4.0, rect.center().y),
                Align2::LEFT_CENTER,
                (bar + 1).to_string(),
                FontId::proportional(10.0),
                colors::TEXT_WEAK,
            );
        }
    }
}

fn draw_notes(ui: &egui::Ui, axes: Axes, midi: &MidiClip, selected: &BTreeSet<usize>) {
    let painter = ui.painter_at(axes.grid);
    for (i, note) in midi.notes.iter().enumerate() {
        let rect = axes.note_rect(note);
        if !rect.intersects(axes.grid) {
            continue;
        }
        let fill = theme::ACCENT.gamma_multiply(0.45 + 0.55 * note.velocity);
        painter.rect_filled(rect, 2.0, fill);
        let stroke = if selected.contains(&i) {
            Stroke::new(1.5, colors::SELECTED)
        } else {
            Stroke::new(1.0, Color32::BLACK.gamma_multiply(0.5))
        };
        painter.rect_stroke(rect, 2.0, stroke, StrokeKind::Inside);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_velocity(
    ui: &mut egui::Ui,
    state: &mut PianoRoll,
    out: &mut Output,
    id: ClipId,
    clip: &Clip,
    midi: &MidiClip,
    rect: Rect,
    axes: Axes,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, colors::HEADER);
    painter.hline(
        rect.x_range(),
        rect.top(),
        Stroke::new(1.0, colors::BAR_LINE),
    );
    let bar_height = rect.height() - 8.0;
    let bar_rect = |note: &MidiNote| {
        let x = axes.x(note.start.0);
        let h = bar_height * note.velocity;
        Rect::from_min_max(
            Pos2::new(x, rect.bottom() - 4.0 - h),
            Pos2::new(x + 5.0, rect.bottom() - 4.0),
        )
    };
    for (i, note) in midi.notes.iter().enumerate() {
        let r = bar_rect(note);
        if !r.intersects(rect) {
            continue;
        }
        let colour = if state.selected.contains(&i) {
            colors::SELECTED
        } else {
            theme::ACCENT
        };
        painter.rect_filled(r, 1.0, colour);
    }
    let response = ui.interact(
        rect,
        ui.id().with("piano roll velocity"),
        Sense::click_and_drag(),
    );
    let velocity_at = |y: f32| ((rect.bottom() - 4.0 - y) / bar_height).clamp(0.0, 1.0);
    if response.drag_started()
        && let Some(press) = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos())
    {
        // The note whose bar is nearest the press.
        let nearest = midi
            .notes
            .iter()
            .enumerate()
            .map(|(i, note)| (i, (axes.x(note.start.0) + 2.5 - press.x).abs()))
            .filter(|(_, d)| *d <= 6.0)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = nearest {
            state.selected = BTreeSet::from([i]);
            state.drag = Some(Drag {
                gesture: Gesture::Velocity,
                original: midi.notes.clone(),
                selected: BTreeSet::from([i]),
                grabbed: i,
                press,
            });
        }
    }
    if response.dragged()
        && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
        && drag.gesture == Gesture::Velocity
    {
        let changed = notes::with_velocity(&drag.original, drag.grabbed, velocity_at(pos.y));
        if changed != midi.notes {
            out.edits.push(set_notes(id, clip, changed, true));
        }
    }
    if response.drag_stopped()
        && state
            .drag
            .as_ref()
            .is_some_and(|d| d.gesture == Gesture::Velocity)
    {
        state.drag = None;
        out.edits.push(Edit::EndDrag);
    }
}

fn draw_keyboard(
    ui: &mut egui::Ui,
    state: &mut PianoRoll,
    out: &mut Output,
    rect: Rect,
    axes: Axes,
) {
    let painter = ui.painter_at(rect);
    let first = axes.key(rect.bottom());
    let last = axes.key(rect.top());
    for key in first..=last {
        let top = axes.y_top(key);
        let row = Rect::from_min_max(
            Pos2::new(rect.left(), top),
            Pos2::new(rect.right(), top + KEY_HEIGHT),
        );
        let pressed = state.sounding == Some(key);
        let fill = match (is_black(key), pressed) {
            (_, true) => theme::ACCENT,
            (true, _) => Color32::from_rgb(30, 33, 40),
            (false, _) => Color32::from_rgb(214, 218, 226),
        };
        painter.rect_filled(row.shrink2(vec2(0.0, 0.5)), 0.0, fill);
        if key % 12 == 0 {
            painter.text(
                Pos2::new(rect.right() - 4.0, row.center().y),
                Align2::RIGHT_CENTER,
                key_name(key),
                FontId::proportional(9.0),
                Color32::BLACK.gamma_multiply(0.7),
            );
        }
    }
    let response = ui.interact(
        rect,
        ui.id().with("piano roll keys"),
        Sense::click_and_drag(),
    );
    let down = response.is_pointer_button_down_on();
    let under = response
        .interact_pointer_pos()
        .or(response.hover_pos())
        .map(|pos| axes.key(pos.y));
    let wanted = if down { under } else { None };
    if wanted != state.sounding {
        if let Some(old) = state.sounding {
            out.audition.push((old, false));
        }
        if let Some(new) = wanted {
            out.audition.push((new, true));
        }
        state.sounding = wanted;
    }
}

#[cfg(test)]
mod tests;
