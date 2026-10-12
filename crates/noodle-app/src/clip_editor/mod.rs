//! The clip editor: an audio clip's own timeline, in the panel the piano roll
//! uses for MIDI clips.
//!
//! The horizontal axis is the beats of the clip (the beats its warp markers
//! are given in), so the waveform is drawn as warped: pulling a marker to a
//! later beat stretches the audio before it and squeezes the audio after. Along
//! the top are the beat ruler, the warp markers as flags, and the loop's brace.
//! Transients found in the audio are drawn as thin lines, for markers to snap to.
//!
//! Every change is an [`Edit`] made with [`Project::warp_edit`], so a warp that
//! other sections share is forked for this one. Dragging a flag or the brace
//! is one undo step, played as cheap Repitch while it lasts (the session does
//! that for any warp being dragged).

use egui::{Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, vec2};
use noodle_core::{
    ClipId, ClipWarp, Command, LoopRegion, MIN_LOOP_BEATS, Project, TICKS_PER_QUARTER, Tick,
    WarpAxis, WarpMarker,
};

use crate::clip_panel;
use crate::session::{Edit, Session};
use crate::theme::{self, timeline as colors};
use crate::timeline::Loaded;

const TOOLBAR: f32 = 26.0;
const RULER: f32 = 18.0;
const FLAGS: f32 = 22.0;
const BRACE: f32 = 14.0;
/// How close, in points, a press is to a flag or a brace end to take it.
const GRAB: f32 = 7.0;
/// How close, in points, a transient has to be to pull a new marker onto it.
const SNAP_TO_TRANSIENT: f32 = 10.0;
const MIN_PPB: f32 = 4.0;
const MAX_PPB: f32 = 600.0;
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
}

struct Drag {
    gesture: Gesture,
    /// The warp when the gesture began; every step is worked out from it.
    original: ClipWarp,
}

pub struct ClipEditor {
    clip: Option<ClipId>,
    /// Points per beat.
    ppb: f32,
    scroll_x: f32,
    /// Fit the clip to the width on the next frame.
    focus: bool,
    /// The flag last pressed, by its place among the markers.
    selected: Option<usize>,
    drag: Option<Drag>,
    /// Where the editor was last right-clicked, in beats.
    menu_at: Option<f64>,
    #[cfg(test)]
    geometry: Option<Geometry>,
}

/// Where things were drawn last frame, for tests to point at.
#[cfg(test)]
#[derive(Clone, Copy)]
pub struct Geometry {
    pub flags: Rect,
    pub brace: Rect,
    pub wave: Rect,
    origin: f32,
    ppb: f32,
}

#[cfg(test)]
impl Geometry {
    pub fn x(&self, beat: f64) -> f32 {
        self.origin + beat as f32 * self.ppb
    }
}

impl Default for ClipEditor {
    fn default() -> Self {
        Self {
            clip: None,
            ppb: 60.0,
            scroll_x: 0.0,
            focus: false,
            selected: None,
            drag: None,
            menu_at: None,
            #[cfg(test)]
            geometry: None,
        }
    }
}

impl ClipEditor {
    /// Shows the audio clip `clip`.
    pub fn open(&mut self, clip: ClipId) {
        if self.clip != Some(clip) {
            self.clip = Some(clip);
            self.selected = None;
            self.drag = None;
            self.focus = true;
        }
    }

    pub fn close(&mut self) {
        self.clip = None;
        self.drag = None;
        self.selected = None;
    }

    /// The clip being edited, if the editor is open.
    pub fn clip(&self) -> Option<ClipId> {
        self.clip
    }

    /// Closes the editor if its clip has gone (or isn't an audio clip any
    /// more).
    pub fn retain_existing(&mut self, project: &Project) {
        if let Some(id) = self.clip
            && project.clip(id).and_then(|c| c.as_audio()).is_none()
        {
            self.close();
        }
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    #[cfg(test)]
    pub fn geometry(&self) -> Option<Geometry> {
        self.geometry
    }
}

/// What the user did in the clip editor.
#[derive(Default)]
pub struct Output {
    pub edits: Vec<Edit>,
}

/// Where the beats are on the screen.
#[derive(Clone, Copy)]
struct Axis {
    area: Rect,
    origin: f32,
    ppb: f32,
}

impl Axis {
    fn x(self, beat: f64) -> f32 {
        self.origin + beat as f32 * self.ppb
    }

    fn beat(self, x: f32) -> f64 {
        f64::from((x - self.origin) / self.ppb)
    }
}

/// The grid a marker snaps to, in beats, at this zoom.
fn grid(ppb: f32) -> f64 {
    if ppb >= 80.0 {
        0.25
    } else if ppb >= 30.0 {
        0.5
    } else {
        1.0
    }
}

fn snap(beat: f64, step: f64) -> f64 {
    (beat / step).round() * step
}

/// Draws the clip editor for the open clip into `ui` and returns what the
/// user did. `loaded` is what is known about the clip's file. `playhead` is
/// the transport position, drawn when it is inside the section.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut ClipEditor,
    session: &Session,
    loaded: Option<&Loaded>,
    playhead: Option<Tick>,
) -> Output {
    let mut out = Output::default();
    let project = session.project();
    let dragging = state.drag.is_some();
    state.retain_existing(project);
    let Some(id) = state.clip else {
        // The clip went (an undo) mid-drag: nothing will send the drag's end.
        if dragging {
            out.edits.push(Edit::EndDrag);
        }
        return out;
    };
    let Some(clip) = project.clip(id) else {
        return out;
    };
    let Some(audio) = clip.as_audio() else {
        return out;
    };
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let name = std::path::Path::new(&audio.source).file_name().map_or_else(
        || audio.source.clone(),
        |n| n.to_string_lossy().into_owned(),
    );

    let toolbar = Rect::from_min_size(rect.min, vec2(rect.width(), TOOLBAR));
    let mut closed = false;
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(toolbar.shrink2(vec2(8.0, 2.0))),
        |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(format!("Clip editor · {name}"));
                ui.separator();
                ui.weak("Double-click to add a marker · drag a flag to move it · Delete removes · Alt: no snap");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        closed = true;
                    }
                });
            });
        },
    );
    if closed {
        if state.drag.take().is_some() {
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
            "The audio file can't be read",
            FontId::proportional(13.0),
            colors::TEXT_WEAK,
        );
        return out;
    };
    let Some((_, warp)) = project.warp_of(id) else {
        let message = Rect::from_center_size(body.center(), vec2(360.0, 56.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(message), |ui| {
            ui.vertical_centered(|ui| {
                ui.label("This clip isn't warped, so it has no beats to pin.");
                if ui.button("Warp").clicked() {
                    out.edits.extend(
                        clip_panel::start_warp(project, id, loaded.analysis.as_deref())
                            .map(Edit::Apply),
                    );
                }
            });
        });
        return out;
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

    let ruler = Rect::from_min_size(body.min, vec2(body.width(), RULER));
    let flags = Rect::from_min_size(
        Pos2::new(body.left(), ruler.bottom()),
        vec2(body.width(), FLAGS),
    );
    let brace = Rect::from_min_size(
        Pos2::new(body.left(), flags.bottom()),
        vec2(body.width(), BRACE),
    );
    let wave = Rect::from_min_max(Pos2::new(body.left(), brace.bottom()), body.max);
    let area = Rect::from_min_max(ruler.min, body.max);

    let (first, last) = (warp.beat_at(0.0).min(0.0), warp.beat_at(file_secs));
    if state.focus {
        state.focus = false;
        state.ppb =
            ((area.width() - 24.0) / (last - first).max(1.0) as f32).clamp(MIN_PPB, MAX_PPB);
        state.scroll_x = first as f32 * state.ppb - 12.0;
    }
    if ui.rect_contains_pointer(area) {
        navigate(ui, state, area, (first, last));
    }
    let axis = Axis {
        area,
        origin: area.left() - state.scroll_x,
        ppb: state.ppb,
    };
    #[cfg(test)]
    {
        state.geometry = Some(Geometry {
            flags,
            brace,
            wave,
            origin: axis.origin,
            ppb: axis.ppb,
        });
    }

    draw_waveform(&painter, wave, axis, warp, loaded, rate, file_secs);
    draw_window(&painter, wave, axis, warp, audio.offset, audio.length, rate);
    draw_ruler(&painter, ruler, wave, axis);
    if let Some(analysis) = &loaded.analysis {
        for &onset in &analysis.onsets {
            let x = axis.x(warp.beat_at(onset));
            if (area.left()..=area.right()).contains(&x) {
                painter.vline(
                    x,
                    wave.y_range(),
                    Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.22)),
                );
            }
        }
    }
    draw_loop(&painter, brace, wave, axis, warp);
    draw_flags(&painter, flags, wave, axis, warp, state.selected);
    draw_playhead(
        &painter,
        wave,
        axis,
        warp,
        clip.start,
        playhead,
        audio.offset,
        rate,
    );

    // A flag that has gone (after an undo) can't stay picked.
    if state.selected.is_some_and(|i| i >= warp.markers.len()) {
        state.selected = None;
    }

    // Gestures.
    let response = ui.interact(area, ui.id().with("clip editor"), Sense::click_and_drag());
    let free = ui.input(|i| i.modifiers.alt);
    let step = grid(state.ppb);
    let onsets: &[f64] = loaded.analysis.as_ref().map_or(&[], |a| &a.onsets);
    let hit = |pos: Pos2| hit_test(pos, flags, brace, wave, axis, warp);
    if let Some(pos) = response.hover_pos() {
        ui.ctx().set_cursor_icon(match hit(pos) {
            Some(Gesture::Marker(_) | Gesture::LoopStart | Gesture::LoopEnd) => {
                CursorIcon::ResizeHorizontal
            }
            Some(Gesture::Loop { .. }) => CursorIcon::Grab,
            None => CursorIcon::Default,
        });
    }
    if response.drag_started()
        && let Some(press) = ui.input(|i| i.pointer.press_origin())
        && let Some(gesture) = hit(press)
    {
        let gesture = match gesture {
            Gesture::Loop { .. } => {
                let start = loop_beats(warp).map_or(0.0, |(start, _)| start);
                Gesture::Loop {
                    grab: axis.beat(press.x) - start,
                }
            }
            other => other,
        };
        if let Gesture::Marker(i) = gesture {
            state.selected = Some(i);
        }
        state.drag = Some(Drag {
            gesture,
            original: warp.clone(),
        });
    }
    if response.dragged()
        && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
    {
        let beat = axis.beat(pos.x);
        let snapped = if free { beat } else { snap(beat, step) };
        if let Some(candidate) = drag_step(drag, beat, snapped, free)
            && let Some(edit) = warp_change(project, id, &candidate, warp)
        {
            out.edits.push(Edit::Drag(edit));
        }
    }
    if response.drag_stopped() {
        state.drag = None;
        out.edits.push(Edit::EndDrag);
    }
    // Clicking picks a flag; clicking elsewhere lets go.
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        state.selected = match hit(pos) {
            Some(Gesture::Marker(i)) => Some(i),
            _ => None,
        };
    }
    // Double-click adds a marker.
    if response.double_clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && !matches!(hit(pos), Some(Gesture::Marker(_)))
    {
        let marker = new_marker(pos.x, axis, warp, onsets, step, free);
        if let Some(candidate) = with_marker(warp, marker) {
            state.selected = candidate
                .markers
                .iter()
                .position(|m| m.seconds == marker.seconds);
            out.edits
                .extend(warp_change(project, id, &candidate, warp).map(Edit::Apply));
        }
    }
    // The right-click menu.
    if response.secondary_clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        state.menu_at = Some(axis.beat(pos.x));
        if let Some(Gesture::Marker(i)) = hit(pos) {
            state.selected = Some(i);
        }
    }
    let menu_x = state.menu_at.map(|beat| axis.x(beat));
    let on_marker = menu_x.and_then(|x| marker_at_x(x, axis, warp));
    response.context_menu(|ui| {
        if let Some(i) = on_marker {
            if ui.button("Delete marker").clicked() {
                out.edits
                    .extend(removal(project, id, warp, i).map(Edit::Apply));
                state.selected = None;
                ui.close();
            }
        } else if let Some(x) = menu_x
            && ui.button("Add marker here").clicked()
        {
            let marker = new_marker(x, axis, warp, onsets, step, free);
            if let Some(candidate) = with_marker(warp, marker) {
                out.edits
                    .extend(warp_change(project, id, &candidate, warp).map(Edit::Apply));
            }
            ui.close();
        }
    });
    // Delete removes the picked flag.
    if ui.rect_contains_pointer(area)
        && !ui.ctx().egui_wants_keyboard_input()
        && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace))
        && let Some(i) = state.selected
    {
        out.edits
            .extend(removal(project, id, warp, i).map(Edit::Apply));
        state.selected = None;
    }
    out
}

fn navigate(ui: &egui::Ui, state: &mut ClipEditor, area: Rect, beats: (f64, f64)) {
    let (scroll, zoom, pointer) =
        ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
    if zoom != 1.0 {
        let anchor = pointer.map_or(area.left(), |p| p.x.max(area.left()));
        let beat = (anchor - area.left() + state.scroll_x) / state.ppb;
        state.ppb = (state.ppb * zoom).clamp(MIN_PPB, MAX_PPB);
        state.scroll_x = beat * state.ppb - (anchor - area.left());
    } else {
        state.scroll_x -= scroll.x;
    }
    let (lo, hi) = (
        beats.0 as f32 * state.ppb - 12.0,
        beats.1 as f32 * state.ppb,
    );
    state.scroll_x = state
        .scroll_x
        .clamp(lo.min(hi), (hi - area.width() * 0.5).max(lo));
}

/// The loop's start and end in beats of the clip (warped), if it loops.
fn loop_beats(warp: &ClipWarp) -> Option<(f64, f64)> {
    let (from, to) = warp.loop_seconds()?;
    Some((warp.beat_at(from), warp.beat_at(to)))
}

/// What is under `pos`: a flag, a brace end or the brace.
fn hit_test(
    pos: Pos2,
    flags: Rect,
    brace: Rect,
    wave: Rect,
    axis: Axis,
    warp: &ClipWarp,
) -> Option<Gesture> {
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
    None
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
                beat: if free { on_grid } else { snap(on_grid, step) },
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

/// The command that gives clip `id` the markers and loop of `candidate`,
/// if that is a change.
fn warp_change(
    project: &Project,
    id: ClipId,
    candidate: &ClipWarp,
    now: &ClipWarp,
) -> Option<Command> {
    if candidate.markers == now.markers && candidate.loop_region == now.loop_region {
        return None;
    }
    let (markers, region) = (candidate.markers.clone(), candidate.loop_region);
    project
        .warp_edit(id, move |warp| {
            warp.markers = markers;
            warp.loop_region = region;
        })
        .ok()
}

/// The command that removes marker `index` of clip `id`.
fn removal(project: &Project, id: ClipId, warp: &ClipWarp, index: usize) -> Option<Command> {
    let mut candidate = warp.clone();
    candidate.markers.get(index)?;
    candidate.markers.remove(index);
    warp_change(project, id, &candidate, warp)
}

/// The warp a drag has made by the time the pointer is at `beat` (`snapped`
/// onto the grid), if that is a warp that can play.
fn drag_step(drag: &Drag, beat: f64, snapped: f64, free: bool) -> Option<ClipWarp> {
    let mut warp = drag.original.clone();
    match drag.gesture {
        Gesture::Marker(i) => {
            warp.markers.get_mut(i)?.beat = snapped;
        }
        Gesture::LoopStart | Gesture::LoopEnd | Gesture::Loop { .. } => {
            warp.loop_region?;
            let (from, to) = loop_beats(&warp)?;
            // Whole beats of the clip, unless snapping is off.
            let whole = |b: f64| {
                if free { b } else { snap(b, 1.0) }
            };
            let (new_from, new_to) = match drag.gesture {
                Gesture::LoopStart => (whole(beat), to),
                Gesture::LoopEnd => (from, whole(beat)),
                Gesture::Loop { grab } => {
                    let start = whole(beat - grab);
                    (start, start + (to - from))
                }
                Gesture::Marker(_) => unreachable!(),
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
fn draw_waveform(
    painter: &egui::Painter,
    wave: Rect,
    axis: Axis,
    warp: &ClipWarp,
    loaded: &Loaded,
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

fn draw_ruler(painter: &egui::Painter, ruler: Rect, wave: Rect, axis: Axis) {
    painter.rect_filled(ruler, 0.0, colors::RULER);
    let (first, last) = (
        axis.beat(ruler.left()).floor() as i64,
        axis.beat(ruler.right()).ceil() as i64,
    );
    // Label every beat that has room.
    let every = [1, 2, 4, 8, 16, 32]
        .into_iter()
        .find(|n| *n as f32 * axis.ppb >= 28.0)
        .unwrap_or(64);
    for beat in first..=last {
        let x = axis.x(beat as f64);
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
                beat.to_string(),
                FontId::proportional(10.0),
                colors::TEXT_WEAK,
            );
        }
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
    selected: Option<usize>,
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
        } else if selected == Some(i) {
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
