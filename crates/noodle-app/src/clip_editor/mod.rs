//! The clip editor: an audio clip's own timeline, in the panel the piano roll
//! uses for MIDI clips.
//!
//! The horizontal axis is the beats of the clip (the beats its warp markers
//! are given in), so the waveform is drawn as warped: pulling a marker to a
//! later beat stretches the audio before it and squeezes the audio after. Along
//! the top are the beat ruler, the warp markers as flags, and the loop's brace.
//! Transients found in the audio are drawn as thin lines, for markers to snap
//! to, each with a handle on the row under the brace: double-click it to make
//! the transient a marker. Flags are picked one at a time (Shift or Ctrl adds
//! to the pick), by a box dragged in the flags' row, or all at once with
//! Ctrl+A, and a picked group moves and is deleted together. Dragging the
//! waveform (or the ruler) slides the clip's beat lines against its audio.
//!
//! Every change is an [`Edit`] made with [`Project::warp_edit`], so a warp that
//! other sections share is forked for this one. Dragging a flag or the brace
//! is one undo step, played as cheap Repitch while it lasts (the session does
//! that for any warp being dragged).

use std::collections::BTreeSet;

use egui::{Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, vec2};
use noodle_core::{
    ClipId, ClipWarp, Command, LoopRegion, MAX_GRID_OFFSET, MIN_LOOP_BEATS, NodeId, Project,
    TICKS_PER_QUARTER, Tick, WarpAxis, WarpMarker,
};

mod curve;
mod gain_line;
pub(crate) mod pane;
mod plain;
pub(crate) mod subject;

use crate::clip_panel;
use crate::session::{Edit, Session};
use crate::theme::{self, timeline as colors};
use crate::timeline::Loaded;
use subject::Subject;

const TOOLBAR: f32 = crate::toolbar::HEIGHT;
const HINT_PLAIN: &str =
    "Drag the waveform to slide the audio under the clip · Warp gives it beats to pin";
const HINT: &str = "Double-click to add a marker · drag a flag to move it (Shift picks several) · drag the waveform to line the beats up · Delete removes · Alt: no snap";
const RULER: f32 = 18.0;
const FLAGS: f32 = 22.0;
const BRACE: f32 = 14.0;
/// The row of handles on the detected transients.
const HANDLES: f32 = 12.0;
/// How close, in points, a press is to a flag or a brace end to take it.
const GRAB: f32 = 7.0;
/// How close, in points, a press is to a transient's handle to take it.
const GRAB_HANDLE: f32 = 5.0;
/// How close, in points, a transient has to be to pull a new marker onto it.
const SNAP_TO_TRANSIENT: f32 = 10.0;
const MIN_PPB: f32 = 4.0;
const MAX_PPB: f32 = 250_000.0;
/// The height the panel opens at.
pub const DEFAULT_HEIGHT: f32 = 240.0;
const MAX_COLUMNS: usize = 4096;

/// What a drag is moving.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Gesture {
    /// A flag, by its place among the markers.
    Marker(usize),
    LoopStart,
    LoopEnd,
    /// The whole brace, grabbed `grab` beats after its start.
    Loop {
        grab: f64,
    },
    /// A detected transient's handle, by its place among the transients.
    Transient(usize),
    /// The waveform, grabbed at `from` (beats): sliding it slides the clip
    /// against the beat lines.
    Clip {
        from: f64,
    },
    /// The beat ruler, grabbed at `from` (beats): sliding it slides the beat
    /// lines against the clip.
    Grid {
        from: f64,
    },
    /// A box dragged out in the flags' row from `from` (screen x) to pick
    /// the flags in it.
    Marquee {
        from: f32,
    },
}

struct Drag {
    gesture: Gesture,
    /// The warp when the gesture began; every step is worked out from it.
    original: ClipWarp,
    /// The flags that were picked when it began.
    picked: BTreeSet<usize>,
}

/// The rows the editor is made of, top to bottom.
struct Rows {
    ruler: Rect,
    flags: Rect,
    brace: Rect,
    handles: Rect,
    wave: Rect,
}

#[derive(Default)]
pub struct ClipEditor {
    subject: Option<Subject>,
    /// Zoom, scroll and the drag in the audio pane; points per beat when the
    /// clip is warped, per second when it isn't.
    view: pane::View,
    /// The flags picked, by their places among the markers.
    selected: BTreeSet<usize>,
    /// The box being dragged out in the flags' row, in screen x.
    marquee: Option<(f32, f32)>,
    drag: Option<Drag>,
    /// Where the editor was last right-clicked, in beats.
    menu_at: Option<f64>,
    /// The part of the file that plays when a drag of an unwarped waveform
    /// began.
    slip: Option<(noodle_core::AudioClip, Option<(f64, f64)>)>,
    /// Whether the last frame drew an unwarped clip (in seconds, not beats).
    plain: bool,
    /// The playheads drawn last frame, in seconds of the file.
    #[cfg(test)]
    playheads: Vec<f64>,
    #[cfg(test)]
    geometry: Option<Geometry>,
    #[cfg(test)]
    plain_geometry: Option<plain::Geometry>,
}

/// Where things were drawn last frame, for tests to point at.
#[cfg(test)]
#[derive(Clone, Copy)]
pub struct Geometry {
    pub flags: Rect,
    pub brace: Rect,
    pub handles: Rect,
    pub ruler: Rect,
    pub wave: Rect,
    origin: f32,
    ppb: f32,
    shift: f64,
}

#[cfg(test)]
impl Geometry {
    pub fn x(&self, beat: f64) -> f32 {
        self.origin + (beat - self.shift) as f32 * self.ppb
    }
}

impl ClipEditor {
    /// Shows the audio clip `clip`.
    pub fn open(&mut self, clip: ClipId) {
        self.open_subject(Subject::Clip(clip));
    }

    /// Shows the sample of the Sampler `node`.
    pub fn open_sampler(&mut self, node: NodeId) {
        self.open_subject(Subject::Sampler(node));
    }

    fn open_subject(&mut self, subject: Subject) {
        if self.subject != Some(subject) {
            self.subject = Some(subject);
            self.selected.clear();
            self.marquee = None;
            self.drag = None;
            self.slip = None;
            self.view.drop_grab();
            self.view.focus = true;
        }
    }

    pub fn close(&mut self) {
        self.subject = None;
        self.drag = None;
        self.slip = None;
        self.view.drop_grab();
        self.marquee = None;
        self.selected.clear();
    }

    /// What is being edited, if the editor is open.
    pub fn subject(&self) -> Option<Subject> {
        self.subject
    }

    /// The clip being edited, if the editor is open on one.
    pub fn clip(&self) -> Option<ClipId> {
        match self.subject? {
            Subject::Clip(id) => Some(id),
            Subject::Sampler(_) => None,
        }
    }

    /// Closes the editor if what it edits has gone (or isn't an audio clip
    /// or a Sampler any more).
    pub fn retain_existing(&mut self, project: &Project) {
        let gone = match self.subject {
            Some(Subject::Clip(id)) => project.clip(id).and_then(|c| c.as_audio()).is_none(),
            Some(Subject::Sampler(id)) => !project
                .graph()
                .node(id)
                .is_some_and(|node| node.type_id == noodle_nodes::SAMPLER_ID),
            None => false,
        };
        if gone {
            self.close();
        }
    }

    /// Points per second (or beat).
    #[cfg(test)]
    pub fn zoom(&self) -> f32 {
        self.view.ppb
    }

    #[cfg(test)]
    pub fn scroll(&self) -> f32 {
        self.view.scroll_x
    }

    #[cfg(test)]
    pub fn playheads(&self) -> &[f64] {
        &self.playheads
    }

    #[cfg(test)]
    pub fn selected(&self) -> Vec<usize> {
        self.selected.iter().copied().collect()
    }

    #[cfg(test)]
    pub fn geometry(&self) -> Option<Geometry> {
        self.geometry
    }

    #[cfg(test)]
    pub fn plain_geometry(&self) -> Option<plain::Geometry> {
        self.plain_geometry
    }
}

/// What the user did in the clip editor.
#[derive(Default)]
pub struct Output {
    pub edits: Vec<Edit>,
}

/// Where the beats are on the screen. The beat lines are what holds still:
/// `shift` is the clip's grid offset, so the clip's beat `n + shift` is drawn
/// where whole beat `n` is, and sliding the grid moves the audio (and the
/// markers) against fixed lines and a fixed clip start.
#[derive(Clone, Copy)]
struct Axis {
    area: Rect,
    origin: f32,
    ppb: f32,
    shift: f64,
}

impl Axis {
    fn x(self, beat: f64) -> f32 {
        self.origin + (beat - self.shift) as f32 * self.ppb
    }

    fn beat(self, x: f32) -> f64 {
        f64::from((x - self.origin) / self.ppb) + self.shift
    }

    /// This axis as it was drawn when the grid offset was `shift`.
    fn at_shift(self, shift: f64) -> Self {
        Self { shift, ..self }
    }
}

/// `beat` on the grid of lines every `step` beats, slid `origin` beats.
fn snap_to(beat: f64, step: f64, origin: f64) -> f64 {
    ((beat - origin) / step).round() * step + origin
}

/// Draws the clip editor for what is open into `ui` and returns what the
/// user did. `loaded` is what is known about the file (a Sampler's sample is
/// in memory). `playhead` is the transport position, drawn when it is inside
/// a clip's section.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut ClipEditor,
    session: &Session,
    loaded: Option<&Loaded>,
    playhead: Option<Tick>,
) -> Output {
    let mut out = Output::default();
    let project = session.project();
    let dragging = state.drag.is_some() || state.view.dragging();
    state.retain_existing(project);
    let Some(subject) = state.subject else {
        // What was edited went (an undo) mid-drag: nothing will send the
        // drag's end.
        if dragging {
            out.edits.push(Edit::EndDrag);
        }
        return out;
    };
    let span = clip_panel::Span::of(loaded);
    let Some(section) = subject.read(project, span) else {
        return out;
    };
    let audio = &section.audio;
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let name = match subject {
        Subject::Clip(_) => std::path::Path::new(&audio.source).file_name().map_or_else(
            || audio.source.clone(),
            |n| n.to_string_lossy().into_owned(),
        ),
        Subject::Sampler(_) => loaded.map_or_else(String::new, |l| {
            format!(
                "{:.2} s at {} Hz",
                span.frames as f64 / f64::from(l.source.sample_rate.max(1)),
                l.source.sample_rate
            )
        }),
    };
    let title = match subject {
        Subject::Clip(_) => format!("Clip editor · {name}"),
        Subject::Sampler(_) => format!("Sample editor · {name}"),
    };

    let toolbar = Rect::from_min_size(rect.min, vec2(rect.width(), TOOLBAR));
    let unwarped = section.is_plain();
    if state.plain != unwarped {
        // The horizontal axis changes between seconds and beats.
        state.plain = unwarped;
        state.view.focus = true;
    }
    let closed = crate::toolbar::show(
        ui,
        toolbar,
        &title,
        if unwarped { HINT_PLAIN } else { HINT },
        |ui| {
            if unwarped {
                if ui
                    .button("Warp")
                    .on_hover_text(
                        "Give this audio beats to pin: the tempo is guessed from its hits",
                    )
                    .clicked()
                {
                    let analysis = loaded.and_then(|l| l.analysis.as_deref());
                    out.edits.extend(
                        clip_panel::start_warp(project, subject, analysis).map(Edit::Apply),
                    );
                }
                plain::loop_toggle(ui, project, subject, &section, span, &mut out.edits);
            }
        },
    );
    if closed {
        if state.drag.take().is_some() | state.view.drop_grab() {
            out.edits.push(Edit::EndDrag);
        }
        state.close();
        return out;
    }
    let body = Rect::from_min_max(Pos2::new(rect.left(), toolbar.bottom()), rect.max);
    let painter = ui.painter_at(body);
    painter.rect_filled(body, 0.0, colors::BACKGROUND);

    let Some(loaded) = loaded else {
        painter.text(
            body.center(),
            Align2::CENTER_CENTER,
            match subject {
                Subject::Clip(_) => "The audio file can't be read",
                Subject::Sampler(_) => "This Sampler has no sample loaded",
            },
            FontId::proportional(13.0),
            colors::TEXT_WEAK,
        );
        return out;
    };
    let warp = match &section.warp {
        Some(warp) if !unwarped => warp,
        _ => {
            out.edits.extend(plain::show(
                ui, state, session, subject, &section, loaded, body, playhead,
            ));
            return out;
        }
    };
    let rate = f64::from(loaded.source.sample_rate);
    let Some(file_frames) = loaded
        .source
        .frames
        .or_else(|| loaded.peaks.as_ref().map(|p| p.frames()))
    else {
        return out;
    };
    let file_secs = file_frames as f64 / rate;
    let span = clip_panel::Span {
        rate: loaded.source.sample_rate,
        frames: file_frames,
    };
    let id = subject;

    let ruler = Rect::from_min_size(body.min, vec2(body.width(), RULER));
    let flags = Rect::from_min_size(
        Pos2::new(body.left(), ruler.bottom()),
        vec2(body.width(), FLAGS),
    );
    let brace = Rect::from_min_size(
        Pos2::new(body.left(), flags.bottom()),
        vec2(body.width(), BRACE),
    );
    let handles = Rect::from_min_size(
        Pos2::new(body.left(), brace.bottom()),
        vec2(body.width(), HANDLES),
    );
    let wave = Rect::from_min_max(Pos2::new(body.left(), handles.bottom()), body.max);
    let area = Rect::from_min_max(ruler.min, body.max);
    let rows = Rows {
        ruler,
        flags,
        brace,
        handles,
        wave,
    };

    let (first, last) = (warp.beat_at(0.0).min(0.0), warp.beat_at(file_secs));
    if state.view.focus {
        state.view.focus = false;
        state.view.ppb =
            ((area.width() - 24.0) / (last - first).max(1.0) as f32).clamp(MIN_PPB, MAX_PPB);
        state.view.scroll_x = first as f32 * state.view.ppb - 12.0;
    }
    if ui.rect_contains_pointer(area) {
        pane::navigate(ui, &mut state.view, area, (first, last));
    }
    let axis = Axis {
        area,
        origin: area.left() - state.view.scroll_x,
        ppb: state.view.ppb,
        shift: warp.grid_offset,
    };
    #[cfg(test)]
    {
        state.geometry = Some(Geometry {
            flags,
            brace,
            handles,
            ruler,
            wave,
            origin: axis.origin,
            ppb: axis.ppb,
            shift: axis.shift,
        });
    }
    let onsets: &[f64] = loaded.analysis.as_ref().map_or(&[], |a| &a.onsets);

    let (lo, hi) = (
        (warp
            .seconds_at_beat(axis.beat(wave.left()))
            .clamp(0.0, file_secs)
            * rate) as u64,
        (warp
            .seconds_at_beat(axis.beat(wave.right()))
            .clamp(0.0, file_secs)
            * rate) as u64
            + 1,
    );
    let memory;
    let samples = if (hi - lo) as f32 <= 2.0 * wave.width() {
        match &loaded.memory {
            Some(sample) => {
                memory = curve::Samples::in_memory(sample, lo, hi);
                Some(&memory)
            }
            None => curve::window(&mut state.view.samples, &loaded.path, lo, hi, file_frames),
        }
    } else {
        None
    };
    draw_waveform(&painter, wave, axis, warp, loaded, samples, rate, file_secs);
    draw_window(&painter, wave, axis, warp, audio.offset, audio.length, rate);
    draw_ruler(&painter, ruler, wave, axis, warp.grid_offset);
    draw_handles(&painter, handles, wave, axis, warp, onsets);
    draw_loop(&painter, brace, wave, axis, warp);
    draw_flags(&painter, flags, wave, axis, warp, &state.selected);
    if let Some((a, b)) = state.marquee {
        let band = Rect::from_x_y_ranges(a.min(b)..=a.max(b), flags.top()..=wave.bottom());
        painter.rect_filled(band, 0.0, colors::SELECTED.gamma_multiply(0.12));
        painter.rect_stroke(
            band,
            0.0,
            Stroke::new(1.0, colors::SELECTED.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
    }
    match subject {
        Subject::Clip(_) => draw_playhead(
            &painter,
            wave,
            axis,
            warp,
            section.start,
            playhead,
            audio.offset,
            rate,
        ),
        Subject::Sampler(node) => {
            let voices = session.telemetry().read_positions(node);
            #[cfg(test)]
            {
                state.playheads = voices.iter().map(|f| f64::from(*f) * file_secs).collect();
            }
            for fraction in &voices {
                let x = axis.x(warp.beat_at(f64::from(*fraction) * file_secs));
                if (wave.left()..=wave.right()).contains(&x) {
                    painter.vline(x, axis.area.y_range(), Stroke::new(1.0, colors::PLAYHEAD));
                }
            }
            if !voices.is_empty() {
                ui.ctx().request_repaint();
            }
        }
    }

    // Flags that have gone (after an undo) can't stay picked.
    state.selected.retain(|&i| i < warp.markers.len());

    // Gestures.
    let response = ui.interact(area, ui.id().with("clip editor"), Sense::click_and_drag());
    let (alt, additive) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift || i.modifiers.command));
    // Alt, or the snap setting at Off, lets a marker go anywhere. Otherwise it
    // snaps to the setting's grid, in beats.
    let free = alt || session.snap() <= 1;
    let step = session.snap() as f64 / f64::from(TICKS_PER_QUARTER as u32);
    let hit = |pos: Pos2| hit_test(pos, &rows, axis, warp, onsets);
    if let Some(pos) = response.hover_pos() {
        ui.ctx().set_cursor_icon(match hit(pos) {
            Some(Gesture::Marker(_) | Gesture::LoopStart | Gesture::LoopEnd) => {
                CursorIcon::ResizeHorizontal
            }
            Some(Gesture::Loop { .. }) => CursorIcon::Grab,
            Some(Gesture::Transient(_)) => CursorIcon::PointingHand,
            Some(Gesture::Clip { .. } | Gesture::Grid { .. }) => CursorIcon::ResizeHorizontal,
            Some(Gesture::Marquee { .. }) | None => CursorIcon::Default,
        });
    }
    if response.drag_started()
        && let Some(press) = ui.input(|i| i.pointer.press_origin())
        && let Some(gesture) = hit(press)
    {
        let at = axis.beat(press.x);
        let gesture = match gesture {
            Gesture::Loop { .. } => {
                let start = loop_beats(warp).map_or(0.0, |(start, _)| start);
                Gesture::Loop { grab: at - start }
            }
            Gesture::Clip { .. } => Gesture::Clip { from: at },
            Gesture::Grid { .. } => Gesture::Grid { from: at },
            Gesture::Marquee { .. } => Gesture::Marquee { from: press.x },
            other => other,
        };
        match gesture {
            Gesture::Marker(i) => {
                // Pulling a flag that is not picked picks it alone; one that
                // is picked takes the others with it.
                if !state.selected.contains(&i) {
                    if !additive {
                        state.selected.clear();
                    }
                    state.selected.insert(i);
                }
            }
            Gesture::Marquee { .. } if !additive => state.selected.clear(),
            _ => {}
        }
        if let Gesture::Marquee { from } = gesture {
            state.marquee = Some((from, from));
        }
        state.drag = Some(Drag {
            gesture,
            original: warp.clone(),
            picked: state.selected.clone(),
        });
    }
    if response.dragged()
        && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
    {
        // The grid slides under a drag of the clip or the ruler, so the
        // pointer is read against the grid the drag began with.
        let beat = axis.at_shift(drag.original.grid_offset).beat(pos.x);
        if let Gesture::Marquee { from } = drag.gesture {
            state.marquee = Some((from, pos.x));
            let (lo, hi) = (from.min(pos.x), from.max(pos.x));
            let inside: BTreeSet<usize> = (0..warp.markers.len())
                .filter(|&i| (lo..=hi).contains(&axis.x(flag_beat(warp, i))))
                .collect();
            state.selected = if additive {
                drag.picked.union(&inside).copied().collect()
            } else {
                inside
            };
        } else {
            let origin = drag.original.grid_offset;
            let snapped = if free {
                beat
            } else {
                snap_to(beat, step, origin)
            };
            if let Some(candidate) = drag_step(drag, beat, snapped, free)
                && let Some(edit) = warp_change(project, id, span, &candidate, warp)
            {
                out.edits.push(Edit::Drag(edit));
            }
        }
    }
    if response.drag_stopped() {
        let marquee = state.marquee.take().is_some();
        state.drag = None;
        // A box is not an edit: there is no step to close.
        if !marquee {
            out.edits.push(Edit::EndDrag);
        }
    }
    // Clicking picks a flag (with Shift or Ctrl, adds it to or takes it out
    // of the pick); clicking elsewhere lets go.
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let target = match hit(pos) {
            Some(Gesture::Marker(i)) => Some(i),
            Some(Gesture::Transient(t)) => warp.marker_at(onsets[t]),
            _ => None,
        };
        match target {
            Some(i) if additive => {
                if !state.selected.remove(&i) {
                    state.selected.insert(i);
                }
            }
            Some(i) => state.selected = BTreeSet::from([i]),
            None if additive => {}
            None => state.selected.clear(),
        }
    }
    // Double-click adds a marker, or makes a transient's handle one.
    if response.double_clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && !matches!(hit(pos), Some(Gesture::Marker(_)))
    {
        if let Some(Gesture::Transient(t)) = hit(pos) {
            let seconds = onsets[t];
            if let Some(i) = warp.marker_at(seconds) {
                state.selected = BTreeSet::from([i]);
            } else if let Some(candidate) = pinned_transient(warp, seconds) {
                state.selected = candidate
                    .markers
                    .iter()
                    .position(|m| m.seconds == seconds)
                    .into_iter()
                    .collect();
                out.edits
                    .extend(warp_change(project, id, span, &candidate, warp).map(Edit::Apply));
            }
        } else if (wave.top()..=wave.bottom()).contains(&pos.y) || flags.contains(pos) {
            let marker = new_marker(pos.x, axis, warp, onsets, step, free);
            if let Some(candidate) = with_marker(warp, marker) {
                state.selected = candidate
                    .markers
                    .iter()
                    .position(|m| m.seconds == marker.seconds)
                    .into_iter()
                    .collect();
                out.edits
                    .extend(warp_change(project, id, span, &candidate, warp).map(Edit::Apply));
            }
        }
    }
    // The right-click menu.
    if response.secondary_clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        state.menu_at = Some(axis.beat(pos.x));
        if let Some(Gesture::Marker(i)) = hit(pos)
            && !state.selected.contains(&i)
        {
            state.selected = BTreeSet::from([i]);
        }
    }
    let menu_x = state.menu_at.map(|beat| axis.x(beat));
    let on_marker = menu_x.and_then(|x| marker_at_x(x, axis, warp));
    response.context_menu(|ui| {
        if let Some(i) = on_marker {
            let picked: Vec<usize> = if state.selected.contains(&i) {
                state.selected.iter().copied().collect()
            } else {
                vec![i]
            };
            let label = if picked.len() > 1 {
                format!("Delete {} markers", picked.len())
            } else {
                "Delete marker".to_owned()
            };
            if ui.button(label).clicked() {
                out.edits
                    .extend(removal(project, id, span, warp, &picked).map(Edit::Apply));
                state.selected.clear();
                ui.close();
            }
        } else if let Some(x) = menu_x
            && ui.button("Add marker here").clicked()
        {
            let marker = new_marker(x, axis, warp, onsets, step, free);
            if let Some(candidate) = with_marker(warp, marker) {
                out.edits
                    .extend(warp_change(project, id, span, &candidate, warp).map(Edit::Apply));
            }
            ui.close();
        }
        if warp.grid_offset != 0.0 && ui.button("Reset the beat lines").clicked() {
            out.edits
                .extend(clip_panel::set_grid_offset(project, id, 0.0, span).map(Edit::Apply));
            ui.close();
        }
    });
    // Keys act on the flags while the pointer is over the editor.
    if ui.rect_contains_pointer(area) && !ui.ctx().egui_wants_keyboard_input() {
        let (delete, all, none) = ui.input(|i| {
            (
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.key_pressed(Key::A) && i.modifiers.command,
                i.key_pressed(Key::Escape),
            )
        });
        if delete && !state.selected.is_empty() {
            let picked: Vec<usize> = state.selected.iter().copied().collect();
            out.edits
                .extend(removal(project, id, span, warp, &picked).map(Edit::Apply));
            state.selected.clear();
        } else if all {
            state.selected = (0..warp.markers.len()).collect();
        } else if none {
            state.selected.clear();
        }
    }
    out
}

/// The loop's start and end in beats of the clip (warped), if it loops.
fn loop_beats(warp: &ClipWarp) -> Option<(f64, f64)> {
    let (from, to) = warp.loop_seconds()?;
    Some((warp.beat_at(from), warp.beat_at(to)))
}

/// What is under `pos`: a flag, a brace end, the brace, a transient's handle,
/// the ruler, empty room in the flags' row, or the waveform.
fn hit_test(
    pos: Pos2,
    rows: &Rows,
    axis: Axis,
    warp: &ClipWarp,
    onsets: &[f64],
) -> Option<Gesture> {
    let Rows {
        ruler,
        flags,
        brace,
        handles,
        wave,
    } = *rows;
    if (flags.top()..=wave.bottom()).contains(&pos.y)
        && let Some(i) = marker_at_x(pos.x, axis, warp)
        && (pos.y <= flags.bottom() || (pos.x - axis.x(flag_beat(warp, i))).abs() <= 3.0)
    {
        return Some(Gesture::Marker(i));
    }
    if brace.y_range().contains(pos.y)
        && let Some((start, end)) = loop_beats(warp)
    {
        let (a, b) = (axis.x(start), axis.x(end));
        if (pos.x - a).abs() <= GRAB {
            return Some(Gesture::LoopStart);
        }
        if (pos.x - b).abs() <= GRAB {
            return Some(Gesture::LoopEnd);
        }
        if (a..=b).contains(&pos.x) {
            return Some(Gesture::Loop { grab: 0.0 });
        }
    }
    if handles.y_range().contains(pos.y)
        && let Some(t) = transient_at_x(pos.x, axis, warp, onsets)
    {
        return Some(Gesture::Transient(t));
    }
    if ruler.y_range().contains(pos.y) {
        return Some(Gesture::Grid { from: 0.0 });
    }
    if flags.y_range().contains(pos.y) {
        return Some(Gesture::Marquee { from: pos.x });
    }
    if wave.y_range().contains(pos.y) {
        return Some(Gesture::Clip { from: 0.0 });
    }
    None
}

/// The transient whose handle is nearest `x`, if one is within reach.
fn transient_at_x(x: f32, axis: Axis, warp: &ClipWarp, onsets: &[f64]) -> Option<usize> {
    onsets
        .iter()
        .enumerate()
        .map(|(i, &t)| (i, (axis.x(warp.beat_at(t)) - x).abs()))
        .filter(|&(_, distance)| distance <= GRAB_HANDLE)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Where flag `index` is drawn: where its marker plays, or, for a marker warp
/// quantise leaves out (nothing plays there), where it was put.
fn flag_beat(warp: &ClipWarp, index: usize) -> f64 {
    warp.marker_beat(index)
        .unwrap_or_else(|| warp.markers[index].beat)
}

/// The flag nearest `x`, if one is within reach.
fn marker_at_x(x: f32, axis: Axis, warp: &ClipWarp) -> Option<usize> {
    (0..warp.markers.len())
        .map(|i| (i, (axis.x(flag_beat(warp, i)) - x).abs()))
        .filter(|&(_, distance)| distance <= GRAB)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The marker a press at `x` would add: on the transient close to it if there
/// is one (and then on the grid), else where the press is.
fn new_marker(
    x: f32,
    axis: Axis,
    warp: &ClipWarp,
    onsets: &[f64],
    step: f64,
    free: bool,
) -> WarpMarker {
    let beat = axis.beat(x);
    let near = onsets
        .iter()
        .map(|&t| (t, (axis.x(warp.beat_at(t)) - x).abs()))
        .filter(|&(_, distance)| distance <= SNAP_TO_TRANSIENT)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    match near {
        Some((seconds, _)) => {
            let on_grid = warp.beat_at(seconds);
            WarpMarker {
                seconds,
                beat: if free {
                    on_grid
                } else {
                    snap_to(on_grid, step, warp.grid_offset)
                },
            }
        }
        None => WarpMarker {
            seconds: warp.seconds_at_beat(beat).max(0.0),
            beat,
        },
    }
}

/// The warp with `marker` added, if that leaves a warp that can play.
fn with_marker(warp: &ClipWarp, marker: WarpMarker) -> Option<ClipWarp> {
    let mut candidate = warp.clone();
    candidate.set_marker(marker);
    candidate.problem().is_none().then_some(candidate)
}

/// The warp with a marker on the transient at `seconds`, where that moment
/// plays now (so pinning it changes nothing until it is moved or
/// quantised), if that leaves a warp that can play.
fn pinned_transient(warp: &ClipWarp, seconds: f64) -> Option<ClipWarp> {
    // Where it plays with the markers as they are stored, not as warp
    // quantise moves them.
    let plain = ClipWarp {
        quantise: None,
        ..warp.clone()
    };
    with_marker(
        warp,
        WarpMarker {
            seconds,
            beat: plain.beat_at(seconds),
        },
    )
}

/// The command that gives `id` the markers, loop and grid offset of
/// `candidate`, if that is a change.
fn warp_change(
    project: &Project,
    id: Subject,
    span: clip_panel::Span,
    candidate: &ClipWarp,
    now: &ClipWarp,
) -> Option<Command> {
    if candidate.markers == now.markers
        && candidate.loop_region == now.loop_region
        && candidate.grid_offset == now.grid_offset
    {
        return None;
    }
    let (markers, region, offset) = (
        candidate.markers.clone(),
        candidate.loop_region,
        candidate.grid_offset,
    );
    clip_panel::slide_grid(project, id, span, move |warp| {
        warp.markers = markers;
        warp.loop_region = region;
        warp.grid_offset = offset;
    })
}

/// The command that removes the markers `indices` of `id`.
fn removal(
    project: &Project,
    id: Subject,
    span: clip_panel::Span,
    warp: &ClipWarp,
    indices: &[usize],
) -> Option<Command> {
    let mut candidate = warp.clone();
    let mut gone = indices.to_vec();
    gone.sort_unstable();
    gone.dedup();
    if gone.is_empty() || gone.iter().any(|&i| i >= candidate.markers.len()) {
        return None;
    }
    for &i in gone.iter().rev() {
        candidate.markers.remove(i);
    }
    warp_change(project, id, span, &candidate, warp)
}

/// The grid offset, kept to what a warp allows and to the nearest thousandth
/// of a beat so a drag does not leave a long tail of decimals in the file.
fn clamp_offset(offset: f64) -> f64 {
    ((offset * 1000.0).round() / 1000.0).clamp(-MAX_GRID_OFFSET, MAX_GRID_OFFSET)
}

/// The warp a drag has made by the time the pointer is at `beat` (`snapped`
/// onto the grid), if that is a warp that can play.
fn drag_step(drag: &Drag, beat: f64, snapped: f64, free: bool) -> Option<ClipWarp> {
    let mut warp = drag.original.clone();
    match drag.gesture {
        Gesture::Marker(i) => {
            // The flag under the pointer goes to the snapped beat, and the
            // other picked flags keep their distance from it.
            let delta = snapped - warp.markers.get(i)?.beat;
            if drag.picked.len() > 1 && drag.picked.contains(&i) {
                for &j in &drag.picked {
                    warp.markers.get_mut(j)?.beat += delta;
                }
            } else {
                warp.markers.get_mut(i)?.beat = snapped;
            }
        }
        Gesture::Clip { from } => {
            // The audio follows the pointer, so the beat lines go the other
            // way.
            warp.grid_offset = clamp_offset(drag.original.grid_offset - (beat - from));
        }
        Gesture::Grid { from } => {
            warp.grid_offset = clamp_offset(drag.original.grid_offset + (beat - from));
        }
        Gesture::Transient(_) | Gesture::Marquee { .. } => return None,
        Gesture::LoopStart | Gesture::LoopEnd | Gesture::Loop { .. } => {
            warp.loop_region?;
            let (from, to) = loop_beats(&warp)?;
            // Whole beats of the clip, unless snapping is off.
            let whole = |b: f64| {
                if free {
                    b
                } else {
                    snap_to(b, 1.0, warp.grid_offset)
                }
            };
            let (new_from, new_to) = match drag.gesture {
                Gesture::LoopStart => (whole(beat), to),
                Gesture::LoopEnd => (from, whole(beat)),
                Gesture::Loop { grab } => {
                    let start = whole(beat - grab);
                    (start, start + (to - from))
                }
                _ => unreachable!(),
            };
            let to_clip = |warped: f64| warp.tempo.beats_at(warp.seconds_at_beat(warped));
            let moved = LoopRegion {
                start: to_clip(new_from),
                end: to_clip(new_to),
            };
            if moved.beats() < MIN_LOOP_BEATS - 1e-9 || moved.start < 0.0 {
                return None;
            }
            warp.loop_region = Some(moved);
        }
    }
    warp.problem().is_none().then_some(warp)
}

// Drawing.

/// The waveform as warped: the audio between two bends of the warp (markers,
/// tempo changes, the ends of the file) is a straight stretch, drawn one pixel
/// column per peak.
#[allow(clippy::too_many_arguments)]
fn draw_waveform(
    painter: &egui::Painter,
    wave: Rect,
    axis: Axis,
    warp: &ClipWarp,
    loaded: &Loaded,
    samples: Option<&curve::Samples>,
    rate: f64,
    file_secs: f64,
) {
    let Some(peaks) = &loaded.peaks else {
        painter.text(
            wave.center(),
            Align2::CENTER_CENTER,
            "Reading the waveform…",
            FontId::proportional(12.0),
            colors::TEXT_WEAK,
        );
        return;
    };
    let mut bends: Vec<f64> = vec![0.0, file_secs];
    bends.extend(warp.tempo.points().iter().map(|p| p.seconds));
    if warp.use_markers {
        bends.extend(warp.markers.iter().map(|m| m.seconds));
    }
    bends.retain(|t| (0.0..=file_secs).contains(t));
    bends.sort_by(f64::total_cmp);
    bends.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let middle = wave.center().y;
    let half = wave.height() / 2.0 - 2.0;
    let ink = theme::ACCENT.gamma_multiply(0.55);
    for pair in bends.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (x0, x1) = (axis.x(warp.beat_at(a)), axis.x(warp.beat_at(b)));
        let (v0, v1) = (x0.max(wave.left()), x1.min(wave.right()));
        if v1 - v0 < 0.5 {
            continue;
        }
        let across = |x: f32| a + (b - a) * f64::from((x - x0) / (x1 - x0));
        if let Some(samples) = samples
            && (x1 - x0) / ((b - a) * rate) as f32 >= curve::CURVE_AT
        {
            let frames = ((across(v0) * rate) as u64, (across(v1) * rate) as u64 + 1);
            curve::draw(painter, wave, samples, frames, 1.0, ink, |f| {
                x0 + (f / rate - a) as f32 / (b - a) as f32 * (x1 - x0)
            });
            continue;
        }
        let (from, to) = ((across(v0) * rate) as u64, (across(v1) * rate) as u64);
        let columns = ((v1 - v0).ceil() as usize).clamp(1, MAX_COLUMNS);
        let stroke = Stroke::new((v1 - v0) / columns as f32, ink);
        for (i, peak) in peaks
            .columns(None, from, to.max(from + 1), columns)
            .iter()
            .enumerate()
        {
            let x = v0 + (i as f32 + 0.5) * (v1 - v0) / columns as f32;
            let top = middle - peak.max.clamp(-1.0, 1.0) * half - 0.5;
            let bottom = middle - peak.min.clamp(-1.0, 1.0) * half + 0.5;
            painter.line_segment([Pos2::new(x, top), Pos2::new(x, bottom)], stroke);
        }
    }
    painter.hline(wave.x_range(), middle, Stroke::new(1.0, colors::BEAT_LINE));
}

/// Dims the audio the section doesn't show.
fn draw_window(
    painter: &egui::Painter,
    wave: Rect,
    axis: Axis,
    warp: &ClipWarp,
    offset: u64,
    length: u64,
    rate: f64,
) {
    let (from, to) = (
        axis.x(warp.beat_at(offset as f64 / rate)),
        axis.x(warp.beat_at((offset + length) as f64 / rate)),
    );
    let dim = Color32::BLACK.gamma_multiply(0.45);
    for (a, b) in [(wave.left(), from), (to, wave.right())] {
        let (a, b) = (a.max(wave.left()), b.min(wave.right()));
        if b > a {
            painter.rect_filled(Rect::from_x_y_ranges(a..=b, wave.y_range()), 0.0, dim);
        }
    }
}

/// The number a beat is labelled with on the ruler. Beats count from 1, and
/// there is no zero: beat 0 is 1, and the beat before it is -1, as in a DAW.
fn ruler_number(beat: i64) -> i64 {
    if beat < 0 { beat } else { beat + 1 }
}

fn draw_ruler(painter: &egui::Painter, ruler: Rect, wave: Rect, axis: Axis, offset: f64) {
    painter.rect_filled(ruler, 0.0, colors::RULER);
    // The beat lines are slid from the whole beats by the clip's grid
    // offset; the numbers still count its beats.
    let (first, last) = (
        (axis.beat(ruler.left()) - offset).floor() as i64,
        (axis.beat(ruler.right()) - offset).ceil() as i64,
    );
    // Label every beat that has room.
    let every = [1, 2, 4, 8, 16, 32]
        .into_iter()
        .find(|n| *n as f32 * axis.ppb >= 28.0)
        .unwrap_or(64);
    for beat in first..=last {
        let x = axis.x(beat as f64 + offset);
        if !(ruler.left()..=ruler.right()).contains(&x) {
            continue;
        }
        let bar = beat % 4 == 0;
        let colour = if bar {
            colors::BAR_LINE
        } else {
            colors::BEAT_LINE
        };
        if axis.ppb >= 8.0 || bar {
            painter.vline(x, wave.y_range(), Stroke::new(1.0, colour));
            painter.vline(x, ruler.y_range(), Stroke::new(1.0, colors::BAR_LINE));
        }
        if beat % every == 0 {
            painter.text(
                Pos2::new(x + 3.0, ruler.center().y),
                Align2::LEFT_CENTER,
                ruler_number(beat).to_string(),
                FontId::proportional(10.0),
                colors::TEXT_WEAK,
            );
        }
    }
}

/// The detected transients: a thin line down the waveform, and a handle on
/// the row above it to double-click to make the transient a warp marker. A
/// transient that has a marker already has a filled handle.
fn draw_handles(
    painter: &egui::Painter,
    handles: Rect,
    wave: Rect,
    axis: Axis,
    warp: &ClipWarp,
    onsets: &[f64],
) {
    painter.rect_filled(handles, 0.0, colors::HEADER.gamma_multiply(0.5));
    for &onset in onsets {
        let x = axis.x(warp.beat_at(onset));
        if !(handles.left() - 6.0..=handles.right() + 6.0).contains(&x) {
            continue;
        }
        painter.vline(
            x,
            wave.y_range(),
            Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.22)),
        );
        let pinned = warp.marker_at(onset).is_some();
        let tip = Pos2::new(x, handles.bottom() - 1.0);
        let triangle = vec![
            tip,
            Pos2::new(x - 4.0, handles.top() + 2.0),
            Pos2::new(x + 4.0, handles.top() + 2.0),
        ];
        let colour = if pinned {
            theme::ACCENT
        } else {
            Color32::WHITE.gamma_multiply(0.55)
        };
        painter.add(egui::Shape::convex_polygon(
            triangle,
            if pinned { colour } else { Color32::TRANSPARENT },
            Stroke::new(1.0, colour),
        ));
    }
}

fn draw_loop(painter: &egui::Painter, brace: Rect, wave: Rect, axis: Axis, warp: &ClipWarp) {
    painter.rect_filled(brace, 0.0, colors::HEADER);
    let Some((start, end)) = loop_beats(warp) else {
        return;
    };
    let (a, b) = (axis.x(start), axis.x(end));
    let bar = Rect::from_x_y_ranges(a.max(brace.left())..=b.min(brace.right()), brace.y_range());
    if bar.width() > 0.0 {
        painter.rect_filled(
            bar.shrink2(vec2(0.0, 2.0)),
            2.0,
            theme::ACCENT.gamma_multiply(0.7),
        );
        let tint = Rect::from_x_y_ranges(bar.x_range(), wave.y_range());
        painter.rect_filled(tint, 0.0, theme::ACCENT.gamma_multiply(0.08));
    }
    for x in [a, b] {
        if (brace.left()..=brace.right()).contains(&x) {
            painter.vline(x, brace.y_range(), Stroke::new(2.0, Color32::WHITE));
        }
    }
}

fn draw_flags(
    painter: &egui::Painter,
    flags: Rect,
    wave: Rect,
    axis: Axis,
    warp: &ClipWarp,
    selected: &BTreeSet<usize>,
) {
    painter.rect_filled(flags, 0.0, colors::HEADER.gamma_multiply(0.8));
    for (i, marker) in warp.markers.iter().enumerate() {
        // Switched off, or left out by warp quantise: nothing plays here.
        let dead = !warp.use_markers || !warp.marker_plays(i);
        // Where the marker plays: with warp quantise on, that is not where
        // it is stored.
        let played = flag_beat(warp, i);
        let x = axis.x(played);
        let stored = axis.x(marker.beat);
        if !(flags.left() - 8.0..=flags.right() + 8.0).contains(&x)
            && !(flags.left() - 8.0..=flags.right() + 8.0).contains(&stored)
        {
            continue;
        }
        let colour = if dead {
            colors::TEXT_WEAK
        } else if selected.contains(&i) {
            colors::SELECTED
        } else {
            theme::ACCENT
        };
        painter.vline(
            x,
            egui::Rangef::new(flags.top(), wave.bottom()),
            Stroke::new(1.0, colour),
        );
        let flag = Rect::from_min_size(Pos2::new(x, flags.top() + 2.0), vec2(12.0, 10.0));
        painter.rect_filled(flag, 2.0, colour);
        painter.text(
            Pos2::new(x + 15.0, flags.top() + 7.0),
            Align2::LEFT_CENTER,
            format_beat(played),
            FontId::proportional(9.0),
            colors::TEXT_WEAK,
        );
        // Back to where the marker was put: the original timing, which warp
        // quantise leaves alone.
        if warp.quantise.is_some() && !dead && (stored - x).abs() > 1.0 {
            let y = flags.bottom() - 8.0;
            painter.line_segment(
                [Pos2::new(stored, y), Pos2::new(x, y)],
                Stroke::new(1.5, colors::SELECTED.gamma_multiply(0.8)),
            );
            painter.circle_stroke(
                Pos2::new(stored, y),
                3.0,
                Stroke::new(1.5, colors::SELECTED),
            );
            painter.vline(
                stored,
                egui::Rangef::new(y, wave.bottom()),
                Stroke::new(1.0, colors::SELECTED.gamma_multiply(0.35)),
            );
        }
        // Where the pinned moment would be without the marker.
        let natural = axis.x(warp.tempo.beats_at(marker.seconds));
        if (natural - x).abs() > 1.0 {
            painter.line_segment(
                [
                    Pos2::new(natural, flags.bottom() - 3.0),
                    Pos2::new(x, flags.bottom() - 3.0),
                ],
                Stroke::new(1.0, colour.gamma_multiply(0.5)),
            );
        }
    }
    painter.rect_stroke(
        flags,
        0.0,
        Stroke::new(1.0, colors::BEAT_LINE),
        StrokeKind::Inside,
    );
}

fn format_beat(beat: f64) -> String {
    if (beat - beat.round()).abs() < 1e-6 {
        format!("{}", beat.round() as i64)
    } else {
        format!("{beat:.2}")
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_playhead(
    painter: &egui::Painter,
    wave: Rect,
    axis: Axis,
    warp: &ClipWarp,
    start: Tick,
    playhead: Option<Tick>,
    offset: u64,
    rate: f64,
) {
    let Some(at) = playhead.filter(|at| *at >= start) else {
        return;
    };
    let curve = warp.curve();
    if curve.axis != WarpAxis::Beats {
        return;
    }
    let beat = fold_into_loop(
        warp,
        curve.at(offset as f64 / rate) + (at.0 - start.0) as f64 / TICKS_PER_QUARTER as f64,
    );
    let x = axis.x(beat);
    if (wave.left()..=wave.right()).contains(&x) {
        painter.vline(x, axis.area.y_range(), Stroke::new(1.0, colors::PLAYHEAD));
    }
}

/// Where on the editor's beat axis a looping section is at `beat` of its
/// run: past the loop's end it goes round inside the brace.
fn fold_into_loop(warp: &ClipWarp, beat: f64) -> f64 {
    let curve = warp.curve();
    let Some((from, to)) = warp.loop_seconds() else {
        return beat;
    };
    let (from, to) = (curve.at(from), curve.at(to));
    if beat > to && to > from {
        from + (beat - to).rem_euclid(to - from)
    } else {
        beat
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ruler_tests {
    use super::ruler_number;

    #[test]
    fn ruler_beats_count_from_one_with_no_zero() {
        let labels: Vec<i64> = (-2..=2).map(ruler_number).collect();
        assert_eq!(labels, [-2, -1, 1, 2, 3]);
    }
}
