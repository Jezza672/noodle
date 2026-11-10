//! The piano roll: the notes of one MIDI clip on a keyboard and a grid.
//!
//! It works in ticks from the clip's start, and every change is an
//! [`Edit`] that sets the clip's notes, so it undoes like anything else. A
//! gesture (dragging notes, drawing one, setting a velocity) works out its
//! result from the notes as they were when it began, so it is one undo step
//! and snapping can't accumulate error.
//!
//! Clicking a key on the keyboard plays it on the clip's track (see
//! [`Output::audition`]).

mod articulations;
mod humanise;
mod lanes;
mod live;
mod looping;
mod notes;

use std::collections::BTreeSet;

use egui::{
    Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, vec2,
};
use noodle_core::{
    Clip, ClipContent, ClipId, ControlPoint, MidiClip, MidiNote, Project, TICKS_PER_QUARTER, Tick,
};

use crate::session::{Edit, Session};
use crate::theme::{self, timeline as colors};
use crate::timeline::{ClipAction, grid};
use noodle_core::Command;

const TOOLBAR: f32 = crate::toolbar::HEIGHT;
const TITLE_WIDTH: f32 = 150.0;
const HINT: &str =
    "Drag to draw · Shift-drag selects · Cmd-drag copies · Alt: no snap · Alt-scroll zooms keys";

// The toolbar's slots, in the order they are drawn. `keep` is how long each
// survives a narrow panel (see `toolbar::fit`): what edits the clip stays
// longest, what describes the selection goes before it, and the hint first.
const TOOLS: usize = 0;
const LOOP: usize = 1;
const LANES: usize = 2;
const STATUS: usize = 3;
const HUMANISE: usize = 4;
const ARTICULATION: usize = 5;
const SNAP: usize = 6;
const HELP: usize = 7;
const SLOTS: [crate::toolbar::Slot; 8] = [
    crate::toolbar::Slot::left(crate::toolbar::segment_width(3), 3),
    crate::toolbar::Slot::left(88.0, 2),
    crate::toolbar::Slot::left(84.0, 4),
    crate::toolbar::Slot::left(112.0, 6).after_divider(),
    crate::toolbar::Slot::left(114.0, 5),
    crate::toolbar::Slot::left(122.0, 5),
    crate::toolbar::Slot::right(112.0, 7),
    crate::toolbar::Slot::right(crate::toolbar::SQUARE, 8),
];
const RULER: f32 = 18.0;
const KEYBOARD: f32 = 64.0;
const VELOCITY: f32 = 56.0;
/// The least height the grid keeps when lanes crowd it.
const MIN_GRID: f32 = 100.0;
/// The height of a key's row at the default vertical zoom.
const KEY_HEIGHT: f32 = 14.0;
const MIN_KEY_HEIGHT: f32 = 6.0;
const MAX_KEY_HEIGHT: f32 = 40.0;
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
    /// Sweeping a box over the notes to select them; `selected` is what was
    /// selected before, which the box adds to.
    Marquee,
}

struct Drag {
    gesture: Gesture,
    /// The notes when the gesture began.
    original: Vec<MidiNote>,
    selected: BTreeSet<u32>,
    grabbed: usize,
    press: Pos2,
}

pub struct PianoRoll {
    clip: Option<ClipId>,
    /// Points per quarter note.
    ppq: f32,
    /// Points per key row (the vertical zoom).
    key_h: f32,
    scroll_x: f32,
    /// How far down the keyboard is scrolled, in points from the top note.
    scroll_y: f32,
    /// Fit the clip to the width, and centre on its notes, on the next frame.
    focus: bool,
    step: i64,
    selected: BTreeSet<u32>,
    /// Asked for by the Edit menu, done on the next frame.
    pending: Option<ClipAction>,
    drag: Option<Drag>,
    /// A drag on the loop brace on the ruler.
    loop_drag: Option<looping::LoopDrag>,
    /// The key being auditioned from the keyboard.
    sounding: Option<u8>,
    /// The notes being played now on any MIDI input, to light on the keys.
    live: live::Live,
    /// A key that was down when the roll closed, still to be let go.
    release: Option<u8>,
    /// The length the last note had, which the next one starts with.
    last_length: Option<i64>,
    /// The controller and expression lanes under the notes.
    lanes: lanes::Lanes,
    #[cfg(test)]
    geometry: Option<Geometry>,
}

/// Where things were drawn last frame, for tests to point at.
#[cfg(test)]
#[derive(Clone)]
pub struct Geometry {
    pub grid: Rect,
    pub velocity: Rect,
    pub keyboard: Rect,
    pub lanes: Vec<(noodle_core::ControlKind, Rect)>,
    pub expression: Option<Rect>,
    origin: f32,
    ppq: f32,
    scroll_y: f32,
    key_h: f32,
}

#[cfg(test)]
impl Geometry {
    pub fn x(&self, tick: i64) -> f32 {
        self.origin + tick as f32 / TICKS_PER_QUARTER as f32 * self.ppq
    }

    /// The vertical centre of `key`'s row.
    pub fn y(&self, key: u8) -> f32 {
        self.grid.top() - self.scroll_y + (127.0 - f32::from(key) + 0.5) * self.key_h
    }
}

impl Default for PianoRoll {
    fn default() -> Self {
        Self {
            clip: None,
            ppq: 120.0,
            key_h: KEY_HEIGHT,
            scroll_x: 0.0,
            scroll_y: 0.0,
            focus: false,
            step: 240,
            selected: BTreeSet::new(),
            pending: None,
            drag: None,
            loop_drag: None,
            sounding: None,
            live: live::Live::default(),
            release: None,
            last_length: None,
            lanes: lanes::Lanes::default(),
            #[cfg(test)]
            geometry: None,
        }
    }
}

impl PianoRoll {
    /// Asks for `action` to be done to the selected notes on the next frame.
    pub fn request(&mut self, action: ClipAction) {
        self.pending = Some(action);
    }

    /// Whether there are selected notes to copy, cut or duplicate.
    pub fn can_copy(&self) -> bool {
        !self.selected.is_empty() || !self.lanes.picked.is_empty()
    }

    /// Whether there are copied notes to paste.
    pub fn can_paste(&self) -> bool {
        self.clip.is_some()
            && (crate::clipboard::holds(crate::clipboard::NOTE_MIME)
                || crate::clipboard::holds(crate::clipboard::CONTROL_MIME))
    }

    /// Shows the notes of `clip`.
    pub fn open(&mut self, clip: ClipId) {
        if self.clip != Some(clip) {
            self.clip = Some(clip);
            self.lanes.reset();
            self.selected.clear();
            self.drag = None;
            self.focus = true;
        }
    }

    pub fn close(&mut self) {
        self.release = self.release.or(self.sounding.take());
        self.clip = None;
        self.drag = None;
        self.live.stop();
        self.lanes.reset();
        self.selected.clear();
    }

    /// A held audition key to let go, because the roll closed (or its clip
    /// went) with the key down. The app asks every frame, open or not.
    pub fn take_release(&mut self) -> Option<u8> {
        self.release.take()
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
                self.lanes.retain_existing(midi);
                self.selected
                    .retain(|id| midi.notes.iter().any(|n| n.id == *id));
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
    pub fn selected(&self) -> &BTreeSet<u32> {
        &self.selected
    }

    #[cfg(test)]
    pub fn geometry(&self) -> Option<Geometry> {
        self.geometry.clone()
    }
}

/// What the user did in the piano roll.
#[derive(Default)]
pub struct Output {
    pub edits: Vec<Edit>,
    /// Keys pressed (`true`) or released on the keyboard strip, to play
    /// through the clip's track input.
    pub audition: Vec<(u8, bool)>,
    /// A click or drag on the ruler: where to put the playhead.
    pub seek: Option<Tick>,
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
    /// Points per key row.
    key_h: f32,
}

impl Axes {
    fn x(self, tick: i64) -> f32 {
        self.origin + tick as f32 / TICKS_PER_QUARTER as f32 * self.ppq
    }

    fn tick(self, x: f32) -> i64 {
        ((x - self.origin) / self.ppq * TICKS_PER_QUARTER as f32).round() as i64
    }

    fn y_top(self, key: u8) -> f32 {
        self.grid.top() - self.scroll_y + (127.0 - f32::from(key)) * self.key_h
    }

    fn key(self, y: f32) -> u8 {
        let row = ((y - self.grid.top() + self.scroll_y) / self.key_h).floor();
        (127.0 - row).clamp(0.0, 127.0) as u8
    }

    fn note_rect(self, note: &MidiNote) -> Rect {
        let top = self.y_top(note.key);
        Rect::from_min_max(
            Pos2::new(self.x(note.start.0), top + 1.0),
            Pos2::new(
                self.x(note.end().0).max(self.x(note.start.0) + 2.0),
                top + self.key_h - 1.0,
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
    let playing = state.live.poll(session.midi_bus());
    // MIDI arriving doesn't wake the window by itself, so look again soon.
    // (A test steps frames by hand, and a pending repaint would stop it.)
    #[cfg(not(test))]
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(if playing {
            30
        } else {
            100
        }));
    #[cfg(test)]
    let _ = playing;
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());

    // The toolbar.
    let toolbar = Rect::from_min_size(rect.min, vec2(rect.width(), TOOLBAR));
    // The snap is the app-wide setting, in the menu bar.
    state.step = session.snap();
    let selected = state.selected.len();
    let status =
        crate::inline_result::recall(ui, &format!("humanise {id:?}")).unwrap_or_else(|| {
            match selected {
                0 => "No notes selected".to_owned(),
                1 => "1 note selected".to_owned(),
                n => format!("{n} notes selected"),
            }
        });
    let snap = crate::snap::label(state.step);
    let closed = crate::toolbar::show(
        ui,
        toolbar,
        &format!("Piano roll · {id}"),
        TITLE_WIDTH,
        &SLOTS,
        &mut |ui, slot, mode| match slot {
            TOOLS => lanes::tools(ui, mode, &mut state.lanes),
            LOOP => looping::controls(ui, mode, &mut out, id, clip, midi),
            LANES => lanes::controls(ui, mode, &mut state.lanes),
            STATUS => crate::toolbar::status(ui, mode, &status, selected > 0),
            HUMANISE => {
                humanise::controls(ui, mode, &mut out, &state.selected, id, clip, midi);
            }
            ARTICULATION => articulations::controls(
                ui,
                mode,
                &mut out,
                project,
                &state.selected,
                id,
                clip,
                midi,
            ),
            SNAP => crate::toolbar::chip(
                ui,
                mode,
                "Snap",
                snap,
                "The snap step is set in the menu bar",
            ),
            HELP => crate::toolbar::hint_popover(ui, mode, HINT),
            _ => {}
        },
    );
    if closed {
        state.close();
        return out;
    }

    let body = Rect::from_min_max(Pos2::new(rect.left(), toolbar.bottom()), rect.max);
    let lane_layout = lanes::Layout::new(
        &state.lanes,
        midi,
        body.height(),
        RULER + VELOCITY + MIN_GRID,
    );
    let grid_rect = Rect::from_min_max(
        Pos2::new(body.left() + KEYBOARD, body.top() + RULER),
        Pos2::new(
            body.right(),
            (body.bottom() - VELOCITY - lane_layout.total()).max(body.top() + RULER + 40.0),
        ),
    );
    let velocity_rect = Rect::from_min_max(
        Pos2::new(grid_rect.left(), grid_rect.bottom()),
        Pos2::new(grid_rect.right(), grid_rect.bottom() + VELOCITY),
    );
    let lane_rects = lane_layout.rects(velocity_rect.bottom(), grid_rect.left(), grid_rect.right());
    let lanes_bottom = velocity_rect.bottom() + lane_layout.total();
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
        let beats = (length as f32 / TICKS_PER_QUARTER as f32).max(1.0);
        state.ppq = ((grid_rect.width() - 24.0) / beats).clamp(MIN_PPQ, MAX_PPQ);
        state.scroll_x = 0.0;
        let middle = match (
            midi.notes.iter().map(|n| n.key).min(),
            midi.notes.iter().map(|n| n.key).max(),
        ) {
            (Some(low), Some(high)) => (f32::from(low) + f32::from(high)) / 2.0,
            _ => 60.0,
        };
        state.scroll_y = (127.0 - middle) * state.key_h - grid_rect.height() / 2.0;
    }
    let hovered = ui.rect_contains_pointer(body);
    if hovered {
        navigate(ui, state, grid_rect, length);
    }
    state.scroll_y = state
        .scroll_y
        .clamp(0.0, (128.0 * state.key_h - grid_rect.height()).max(0.0));
    let axes = Axes {
        grid: grid_rect,
        origin: grid_rect.left() - state.scroll_x,
        ppq: state.ppq,
        scroll_y: state.scroll_y,
        key_h: state.key_h,
    };
    #[cfg(test)]
    {
        state.geometry = Some(Geometry {
            grid: grid_rect,
            velocity: velocity_rect,
            keyboard: keyboard_rect,
            lanes: lane_rects.0.clone(),
            expression: lane_rects.1,
            origin: axes.origin,
            ppq: axes.ppq,
            scroll_y: axes.scroll_y,
            key_h: axes.key_h,
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
    let snap = |tick: i64| notes::snap(map, clip.start, tick, if free { 1 } else { state.step });
    let min_length = if free { 1 } else { state.step.max(1) };
    let shift = ui.input(|i| i.modifiers.shift);
    // Pressing or dragging along the ruler puts the playhead there. The loop
    // brace's handles are registered after this, so they win where they are.
    let scrub = ui.interact(
        ruler_rect,
        ui.id().with("piano roll ruler"),
        Sense::click_and_drag(),
    );
    if scrub.is_pointer_button_down_on()
        && let Some(pos) = scrub.interact_pointer_pos()
    {
        let at = snap(axes.tick(pos.x).clamp(0, length));
        out.seek = Some(Tick((clip.start.0 + at).max(0)));
    }
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
                let note_id = original[i].id;
                if !state.selected.contains(&note_id) && !shift {
                    state.selected = BTreeSet::from([note_id]);
                }
                state.selected.insert(note_id);
                let r = axes.note_rect(&original[i]);
                let resize = r.width() >= MIN_RESIZABLE && r.right() - press.x < EDGE;
                if resize {
                    state.last_length = None;
                }
                let (mut original, mut grabbed) = (original, i);
                // Cmd/Ctrl-drag drags copies and leaves the notes where they are.
                if !resize && ui.input(|i| i.modifiers.command) {
                    let mut next = original.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                    let mut copies = BTreeSet::new();
                    for (at, note) in original.clone().iter().enumerate() {
                        if state.selected.contains(&note.id) {
                            if at == i {
                                grabbed = original.len();
                            }
                            original.push(MidiNote {
                                id: next,
                                ..note.clone()
                            });
                            copies.insert(next);
                            next += 1;
                        }
                    }
                    state.selected = copies;
                }
                state.drag = Some(Drag {
                    gesture: if resize {
                        Gesture::Resize
                    } else {
                        Gesture::Move
                    },
                    original,
                    selected: state.selected.clone(),
                    grabbed,
                    press,
                });
            }
            None if shift => {
                state.drag = Some(Drag {
                    gesture: Gesture::Marquee,
                    original,
                    selected: state.selected.clone(),
                    grabbed: 0,
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
                let note_id = midi.notes[i].id;
                if !state.selected.remove(&note_id) {
                    state.selected.insert(note_id);
                }
            }
            Some(i) => state.selected = BTreeSet::from([midi.notes[i].id]),
            None if shift => {}
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
            Gesture::Marquee => {
                let sweep = Rect::from_two_pos(drag.press, pos);
                state.selected = drag.selected.clone();
                state.selected.extend(
                    midi.notes
                        .iter()
                        .filter(|n| axes.note_rect(n).intersects(sweep))
                        .map(|n| n.id),
                );
                None
            }
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
        && !matches!(drag.gesture, Gesture::Velocity | Gesture::Marquee)
    {
        out.edits.push(Edit::EndDrag);
    }

    draw_notes(ui, axes, midi, &state.selected, project);
    if let Some(drag) = &state.drag
        && drag.gesture == Gesture::Marquee
        && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
    {
        let sweep = Rect::from_two_pos(drag.press, pos).intersect(grid_rect);
        let painter = ui.painter_at(grid_rect);
        painter.rect_filled(sweep, 0.0, theme::ACCENT.gamma_multiply(0.15));
        painter.rect_stroke(
            sweep,
            0.0,
            Stroke::new(1.0, theme::ACCENT),
            StrokeKind::Inside,
        );
    }
    humanise::ghosts(ui, axes, midi);
    looping::draw_repeats(ui, axes, midi);
    looping::draw_lines(ui, axes, midi);
    let step = if free { 1 } else { state.step };
    looping::brace(
        ui,
        state,
        &mut out,
        id,
        clip,
        midi,
        ruler_rect,
        axes,
        &|t| notes::snap(map, clip.start, t, step),
    );

    // The velocity lane, with a bar per note.
    draw_velocity(ui, state, &mut out, id, clip, midi, velocity_rect, axes);

    // The controller and expression lanes under it.
    lanes::show(
        ui,
        &mut state.lanes,
        &mut out,
        &lanes::Ctx {
            id,
            clip,
            midi,
            axes,
            map,
            snap: &|t| notes::snap(map, clip.start, t, step),
            shift,
            selected: &state.selected,
        },
        &lane_layout,
        &lane_rects,
        grid_rect.left(),
    );

    // The keyboard, which plays what is clicked.
    draw_keyboard(ui, state, &mut out, keyboard_rect, axes);

    // The playhead.
    if let Some(playhead) = playhead {
        let x = axes.x(playhead.0 - clip.start.0);
        if x >= grid_rect.left() && x <= grid_rect.right() && playhead >= clip.start {
            ui.painter_at(Rect::from_min_max(
                ruler_rect.min,
                Pos2::new(velocity_rect.right(), lanes_bottom),
            ))
            .vline(
                x,
                ruler_rect.top()..=lanes_bottom,
                Stroke::new(1.5, colors::PLAYHEAD),
            );
        }
    }

    // Keys.
    let typing = ui.ctx().egui_wants_keyboard_input();
    if hovered && !typing {
        if state.lanes.hover && !state.lanes.picked.is_empty() {
            if ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, Key::Delete)
                    || i.consume_key(egui::Modifiers::NONE, Key::Backspace)
            }) {
                lanes::delete(&mut state.lanes, &mut out, id, clip, midi);
            }
        } else if !state.selected.is_empty() {
            keyboard_edits(ui, state, &mut out, id, clip, midi);
        }
    }
    if !typing {
        let chord = |ui: &egui::Ui| {
            ui.input_mut(|i| {
                let event = |wanted: fn(&egui::Event) -> bool| i.events.iter().any(wanted);
                let copy = event(|e| matches!(e, egui::Event::Copy));
                let cut = event(|e| matches!(e, egui::Event::Cut));
                let paste = event(|e| matches!(e, egui::Event::Paste(_)));
                let command = egui::Modifiers::COMMAND;
                if i.consume_key(command, Key::X) | cut {
                    Some(ClipAction::Cut)
                } else if i.consume_key(command, Key::C) | copy {
                    Some(ClipAction::Copy)
                } else if i.consume_key(command, Key::V) | paste {
                    Some(ClipAction::Paste)
                } else if i.consume_key(command, Key::D) {
                    Some(ClipAction::Duplicate)
                } else {
                    None
                }
            })
        };
        if hovered && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::A)) {
            state.selected = midi.notes.iter().map(|n| n.id).collect();
        }
        if hovered && let Some(action) = chord(ui) {
            state.pending = Some(action);
        }
        if let Some(action) = state.pending.take() {
            // Notes paste at the playhead; with it outside the clip, at the
            // left edge of what is in view, so a paste is never out of sight.
            let in_view = notes::snap(
                map,
                clip.start,
                axes.tick(grid_rect.left()).max(0),
                state.step,
            );
            let local = playhead
                .map(|p| p.0 - clip.start.0)
                .filter(|at| (0..length).contains(at))
                .unwrap_or(in_view.clamp(0, (length - 1).max(0)));
            clipboard_edits(action, state, &mut out, id, clip, midi, local);
        }
    }
    if response.contains_pointer() || hit_note {
        // Keep the pointer's note hot for the cursor above.
    }
    out
}

/// The command that sets a clip's controller points.
fn set_controls(id: ClipId, clip: &Clip, controls: Vec<ControlPoint>, drag: bool) -> Edit {
    let Some(midi) = clip.as_midi() else {
        unreachable!("the piano roll only edits MIDI clips")
    };
    let command = Command::SetClip {
        id,
        clip: Clip {
            content: ClipContent::Midi(MidiClip {
                controls,
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

/// Copy, cut, paste and duplicate: the notes go on the clipboard with their
/// spacing, and come off it as one edit, selected.
fn clipboard_edits(
    action: ClipAction,
    state: &mut PianoRoll,
    out: &mut Output,
    id: ClipId,
    clip: &Clip,
    midi: &MidiClip,
    playhead: i64,
) {
    use crate::clipboard::{CONTROL_MIME, NOTE_MIME, copy, holds, paste};
    // Points when the pointer is over the lanes or no notes are selected; a
    // paste goes by what is on the clipboard.
    let points_first = state.lanes.hover || state.selected.is_empty();
    let for_points = if action == ClipAction::Paste {
        holds(CONTROL_MIME)
    } else {
        points_first && !state.lanes.picked.is_empty()
    };
    if for_points
        && lanes::clipboard(
            action,
            &mut state.lanes,
            state.step,
            out,
            id,
            clip,
            midi,
            playhead,
        )
    {
        return;
    }
    let held = notes::copied(&midi.notes, &state.selected);
    let summary = || {
        let n = held.len();
        format!("{n} note{}", if n == 1 { "" } else { "s" })
    };
    match action {
        ClipAction::Copy | ClipAction::Cut if !held.is_empty() => {
            copy(NOTE_MIME, &held, &summary());
            if action == ClipAction::Cut {
                let changed = notes::removed(&midi.notes, &state.selected);
                state.selected.clear();
                out.edits.push(set_notes(id, clip, changed, false));
            }
        }
        ClipAction::Duplicate if !held.is_empty() => {
            let earliest = midi
                .notes
                .iter()
                .filter(|n| state.selected.contains(&n.id))
                .map(|n| n.start.0)
                .min()
                .unwrap_or(0);
            let at = earliest + notes::span(&held);
            if let Some((changed, ids)) = notes::pasted(&midi.notes, &held, at, midi.length.0) {
                state.selected = ids;
                out.edits.push(set_notes(id, clip, changed, false));
            }
        }
        ClipAction::Paste => {
            let Some(held) = paste::<Vec<MidiNote>>(NOTE_MIME) else {
                return;
            };
            if let Some((changed, ids)) = notes::pasted(&midi.notes, &held, playhead, midi.length.0)
            {
                state.selected = ids;
                out.edits.push(set_notes(id, clip, changed, false));
            }
        }
        _ => {}
    }
}

/// Scroll and zoom while the pointer is over the roll. A pinch or Ctrl-scroll
/// zooms time; with Alt held it zooms the key rows instead, and a pinch that
/// is clearly across or along (on a touch screen) zooms only that way.
fn navigate(ui: &egui::Ui, state: &mut PianoRoll, grid: Rect, length: i64) {
    let (scroll, zoom, pointer) = ui.input(|i| {
        (
            i.smooth_scroll_delta,
            i.zoom_delta_2d(),
            i.pointer.hover_pos(),
        )
    });
    if zoom != Vec2::splat(1.0) {
        // Don't let the same turn pan as well.
        let anchor = pointer.map_or(grid.left(), |p| p.x.max(grid.left()));
        let quarters = (anchor - grid.left() + state.scroll_x) / state.ppq;
        // With no modifier a wheel or trackpad pinch is one number for both
        // ways, and means time.
        let uniform = zoom.x == zoom.y;
        let across = if uniform && !ui.input(|i| i.modifiers.alt) {
            zoom.x
        } else if uniform {
            1.0
        } else {
            zoom.x
        };
        let along = if uniform && !ui.input(|i| i.modifiers.alt) {
            1.0
        } else {
            zoom.y
        };
        state.ppq = (state.ppq * across).clamp(MIN_PPQ, MAX_PPQ);
        state.scroll_x = quarters * state.ppq - (anchor - grid.left());
        if along != 1.0 {
            let anchor = pointer.map_or(grid.center().y, |p| p.y.clamp(grid.top(), grid.bottom()));
            let rows = (anchor - grid.top() + state.scroll_y) / state.key_h;
            state.key_h = (state.key_h * along).clamp(MIN_KEY_HEIGHT, MAX_KEY_HEIGHT);
            state.scroll_y = rows * state.key_h - (anchor - grid.top());
        }
    } else if ui.input(|i| i.modifiers.alt) && scroll.y != 0.0 {
        // Alt and the wheel: the key rows grow or shrink.
        let anchor = pointer.map_or(grid.center().y, |p| p.y.clamp(grid.top(), grid.bottom()));
        let rows = (anchor - grid.top() + state.scroll_y) / state.key_h;
        state.key_h =
            (state.key_h * (scroll.y * 0.005).exp()).clamp(MIN_KEY_HEIGHT, MAX_KEY_HEIGHT);
        state.scroll_y = rows * state.key_h - (anchor - grid.top());
    } else {
        state.scroll_x -= scroll.x;
        state.scroll_y -= scroll.y;
    }
    // The roll has had the wheel turn, whether it panned or zoomed, so the
    // delta is spent: a scroll area around the roll must not act on it as
    // well.
    ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
    let width = length as f32 / TICKS_PER_QUARTER as f32 * state.ppq;
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
            vec2(axes.grid.width(), axes.key_h),
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
            top + axes.key_h,
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

fn draw_notes(
    ui: &egui::Ui,
    axes: Axes,
    midi: &MidiClip,
    selected: &BTreeSet<u32>,
    project: &Project,
) {
    let painter = ui.painter_at(axes.grid);
    for note in &midi.notes {
        let rect = axes.note_rect(note);
        if !rect.intersects(axes.grid) {
            continue;
        }
        let base = articulations::tint(project, note.articulation).unwrap_or(theme::ACCENT);
        let fill = base.gamma_multiply(0.45 + 0.55 * note.velocity);
        painter.rect_filled(rect, 2.0, fill);
        let stroke = if selected.contains(&note.id) {
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
    // The bar being dragged goes last, outlined, so it shows on top of any
    // bar at the same time; the selected ones go over the rest.
    let active = state
        .drag
        .as_ref()
        .filter(|d| d.gesture == Gesture::Velocity)
        .and_then(|d| d.original.get(d.grabbed))
        .map(|n| n.id);
    let mut order: Vec<&MidiNote> = midi.notes.iter().collect();
    order.sort_by_key(|n| (Some(n.id) == active, state.selected.contains(&n.id)));
    for note in order {
        let r = bar_rect(note);
        if !r.intersects(rect) {
            continue;
        }
        let colour = if state.selected.contains(&note.id) {
            colors::SELECTED
        } else {
            theme::ACCENT
        };
        painter.rect_filled(r, 1.0, colour);
        if Some(note.id) == active {
            painter.rect_stroke(
                r.expand(1.5),
                2.0,
                Stroke::new(1.5, Color32::WHITE),
                StrokeKind::Outside,
            );
            painter.text(
                Pos2::new(r.right() + 5.0, r.top().max(rect.top() + 6.0)),
                Align2::LEFT_CENTER,
                format!("{}", (note.velocity * 127.0).round()),
                FontId::proportional(10.0),
                colors::TEXT_WEAK,
            );
        }
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
            // Dragging a bar of the selection edits the whole selection.
            let note_id = midi.notes[i].id;
            if !state.selected.contains(&note_id) {
                state.selected = BTreeSet::from([note_id]);
            }
            state.drag = Some(Drag {
                gesture: Gesture::Velocity,
                original: midi.notes.clone(),
                selected: state.selected.clone(),
                grabbed: i,
                press,
            });
        }
    }
    if response.dragged()
        && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
        && drag.gesture == Gesture::Velocity
    {
        let changed = notes::with_group_velocity(
            &drag.original,
            &drag.selected,
            drag.grabbed,
            velocity_at(pos.y),
        );
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
            Pos2::new(rect.right(), top + axes.key_h),
        );
        let pressed = state.sounding == Some(key) || state.live.is_down(key);
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
