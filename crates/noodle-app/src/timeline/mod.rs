//! The arrangement view: a lane per track, with the clips each track plays,
//! on a ruler of bars and beats.
//!
//! The view works in musical time. Its x axis is ticks, so it doesn't stretch
//! when the tempo changes; a clip is as wide as its audio is long at that
//! tempo. Every change is an [`Edit`] using the core's clip commands, so it
//! can be undone.

mod clips;
mod grid;
mod sources;
mod waveform;

use std::collections::BTreeSet;
#[cfg(test)]
use std::collections::HashMap;

use egui::{
    Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, vec2,
};
use noodle_core::{Clip, ClipId, Command, NodeId, Project, TempoMap, Tick};

use crate::session::{Edit, Session};
use crate::theme::timeline as colors;
use clips::Source;
use sources::Sources;

/// The type of the node inside a track's group that plays its clips.
pub const TRACK_INPUT: &str = "noodle.track.input";

/// How far from a clip's edge a drag trims it instead of moving it.
const EDGE: f32 = 6.0;
/// Clips narrower than this can only be moved.
const MIN_TRIMMABLE: f32 = 24.0;
/// The sample rate to lay a clip out at when its file can't be read.
const FALLBACK_RATE: u32 = 48_000;
const MIN_PPQ: f32 = 8.0;
const MAX_PPQ: f32 = 600.0;

pub struct TimelineState {
    /// Zoom: screen points per quarter note.
    ppq: f32,
    /// How far the view is scrolled, in screen points.
    scroll_x: f32,
    scroll_y: f32,
    selected: BTreeSet<ClipId>,
    drag: Option<Drag>,
    sources: Sources,
    #[cfg(test)]
    clip_rects: HashMap<ClipId, Rect>,
    /// The text drawn on the lanes last frame; painted text isn't in the
    /// accessibility tree.
    #[cfg(test)]
    drawn_text: Vec<String>,
    /// How many clips drew a waveform last frame.
    #[cfg(test)]
    waveforms: usize,
}

impl Default for TimelineState {
    fn default() -> Self {
        Self {
            ppq: 60.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            selected: BTreeSet::new(),
            drag: None,
            sources: Sources::default(),
            #[cfg(test)]
            clip_rects: HashMap::new(),
            #[cfg(test)]
            drawn_text: Vec::new(),
            #[cfg(test)]
            waveforms: 0,
        }
    }
}

impl TimelineState {
    /// Forgets clips that no longer exist, e.g. after an undo.
    fn retain_existing(&mut self, project: &Project) {
        self.selected.retain(|&id| project.clip(id).is_some());
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| project.clip(drag.grabbed).is_none())
        {
            self.drag = None;
        }
    }

    #[cfg(test)]
    pub fn selected(&self) -> &BTreeSet<ClipId> {
        &self.selected
    }

    #[cfg(test)]
    pub fn waveforms(&self) -> usize {
        self.waveforms
    }

    #[cfg(test)]
    pub fn drawn_text(&self) -> &[String] {
        &self.drawn_text
    }

    #[cfg(test)]
    pub fn clip_rect(&self, id: ClipId) -> Option<Rect> {
        self.clip_rects.get(&id).copied()
    }
}

/// A clip being moved or trimmed. Each frame's edit is worked out from the
/// clips as they were when the drag began, so snapping can't accumulate error.
struct Drag {
    mode: Mode,
    grabbed: ClipId,
    /// Every clip the drag moves, as it was at the start.
    originals: Vec<(ClipId, Clip)>,
    press_x: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Move,
    TrimStart,
    TrimEnd,
}

/// The track lanes, top to bottom: every track input node, and any other node
/// that has clips.
fn tracks(project: &Project) -> Vec<NodeId> {
    let mut tracks: Vec<NodeId> = project
        .graph()
        .nodes()
        .filter(|(id, node)| node.type_id == TRACK_INPUT || project.clips_on(*id).next().is_some())
        .map(|(id, _)| id)
        .collect();
    tracks.sort();
    tracks
}

/// Where the screen's x axis is in time.
#[derive(Clone, Copy)]
struct Axis {
    /// The screen x of tick 0.
    origin: f32,
    ppq: f32,
}

impl Axis {
    fn x(self, tick: Tick) -> f32 {
        self.origin + tick.quarters() as f32 * self.ppq
    }

    fn tick(self, x: f32) -> Tick {
        Tick::from_quarters(f64::from((x - self.origin) / self.ppq))
    }
}

/// Draws the arrangement and returns the edits the user made.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut TimelineState,
    session: &Session,
    playhead: Option<Tick>,
) -> Vec<Edit> {
    let project = session.project();
    state.retain_existing(project);
    let tracks = tracks(project);
    let map = project.tempo_map();

    let (rect, background) = ui.allocate_exact_size(ui.available_size(), Sense::click());
    let content = Rect::from_min_max(
        rect.min + vec2(colors::HEADER_WIDTH, colors::RULER_HEIGHT),
        rect.max,
    );
    let hovered = ui.rect_contains_pointer(rect);
    let directory = session.directory().map(std::path::Path::to_path_buf);
    if hovered {
        // Room to scroll to: the end of the last clip, and some more to
        // place clips in.
        let extent = project
            .clips()
            .filter_map(|(_, clip)| {
                let audio = clip.as_audio()?;
                let source = state
                    .sources
                    .get(ui.ctx(), directory.as_deref(), &audio.source)
                    .map(|loaded| loaded.source);
                let rate = source.map_or(FALLBACK_RATE, |s| s.sample_rate);
                Some(clips::end_tick(map, clip.start, audio.length, rate).quarters())
            })
            .fold(0.0, f64::max) as f32
            + 16.0;
        navigate(ui, state, content, tracks.len(), extent);
    }
    let axis = Axis {
        origin: content.left() - state.scroll_x,
        ppq: state.ppq,
    };

    let mut edits = Vec::new();
    #[cfg(test)]
    {
        state.clip_rects.clear();
        state.drawn_text.clear();
        state.waveforms = 0;
    }

    let painter = ui.painter_at(content);
    painter.rect_filled(rect, 0.0, colors::BACKGROUND);
    let lane_top = |i: usize| content.top() - state.scroll_y + i as f32 * colors::LANE_HEIGHT;
    for i in 0..tracks.len() {
        let top = lane_top(i);
        let fill = if i % 2 == 0 {
            colors::LANE_EVEN
        } else {
            colors::LANE_ODD
        };
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(content.left(), top),
                vec2(content.width(), colors::LANE_HEIGHT),
            ),
            0.0,
            fill,
        );
    }
    let lines = grid::lines(map, axis.tick(content.left()), axis.tick(content.right()));
    for line in &lines {
        let (color, width) = match line.kind {
            grid::Kind::Bar(_) => (colors::BAR_LINE, 1.0),
            grid::Kind::Beat => (colors::BEAT_LINE, 1.0),
        };
        painter.vline(
            axis.x(line.tick),
            content.y_range(),
            Stroke::new(width, color),
        );
    }

    if tracks.is_empty() {
        #[cfg(test)]
        state.drawn_text.push("No tracks yet".into());
        painter.text(
            content.center(),
            Align2::CENTER_CENTER,
            "No tracks yet",
            FontId::proportional(14.0),
            colors::TEXT_WEAK,
        );
    }

    let mut hit_clip = false;
    for (index, &track) in tracks.iter().enumerate() {
        let top = lane_top(index);
        if top > content.bottom() || top + colors::LANE_HEIGHT < content.top() {
            continue;
        }
        let colour = colors::track_colour(index);
        for (id, clip) in project.clips_on(track) {
            let Some(audio) = clip.as_audio() else {
                continue;
            };
            let loaded = state
                .sources
                .get(ui.ctx(), directory.as_deref(), &audio.source);
            let source = loaded.as_ref().map(|loaded| loaded.source);
            let rate = source.map_or(FALLBACK_RATE, |s| s.sample_rate);
            let end = clips::end_tick(map, clip.start, audio.length, rate);
            let full = Rect::from_min_max(
                Pos2::new(axis.x(clip.start), top + 3.0),
                Pos2::new(
                    axis.x(end).max(axis.x(clip.start) + 2.0),
                    top + colors::LANE_HEIGHT - 3.0,
                ),
            );
            let visible = full.intersect(content);
            if visible.width() <= 0.0 || visible.height() <= 0.0 {
                continue;
            }
            #[cfg(test)]
            state.clip_rects.insert(id, visible);
            let response =
                ui.interact(visible, ui.id().with(("clip", id)), Sense::click_and_drag());
            hit_clip |= response.contains_pointer() || response.is_pointer_button_down_on();
            let mode_at = |x: f32| mode_for(full, x);
            if let Some(pos) = response.hover_pos() {
                ui.ctx().set_cursor_icon(match mode_at(pos.x) {
                    Mode::Move => CursorIcon::Grab,
                    _ => CursorIcon::ResizeHorizontal,
                });
            }
            let selected = state.selected.contains(&id);
            if response.drag_started() {
                let press = ui
                    .input(|i| i.pointer.press_origin())
                    .or(response.interact_pointer_pos())
                    .unwrap_or(full.center());
                if !selected && !ui.input(|i| i.modifiers.shift) {
                    state.selected = BTreeSet::from([id]);
                }
                state.selected.insert(id);
                let mode = mode_at(press.x);
                let originals = if mode == Mode::Move {
                    state
                        .selected
                        .iter()
                        .filter_map(|&sel| Some((sel, project.clip(sel)?.clone())))
                        .collect()
                } else {
                    state.selected = BTreeSet::from([id]);
                    vec![(id, clip.clone())]
                };
                state.drag = Some(Drag {
                    mode,
                    grabbed: id,
                    originals,
                    press_x: press.x,
                });
            } else if response.clicked() {
                if ui.input(|i| i.modifiers.shift) {
                    if !state.selected.remove(&id) {
                        state.selected.insert(id);
                    }
                } else {
                    state.selected = BTreeSet::from([id]);
                }
            }
            if response.dragged()
                && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
            {
                let free = ui.input(|i| i.modifiers.alt);
                let lane = ((pos.y - content.top() + state.scroll_y) / colors::LANE_HEIGHT)
                    .floor()
                    .max(0.0) as usize;
                let target = tracks[lane.min(tracks.len() - 1)];
                let command = drag_command(
                    drag,
                    DragInput {
                        map,
                        axis,
                        x: pos.x,
                        free,
                        target,
                        source,
                    },
                    project,
                );
                if let Some(command) = command {
                    edits.push(Edit::Drag(command));
                }
            }
            if response.drag_stopped() {
                state.drag = None;
                edits.push(Edit::EndDrag);
            }

            let painter = ui.painter_at(visible);
            painter.rect_filled(full, 4.0, colour);
            if let Some(peaks) = loaded.as_ref().and_then(|loaded| loaded.peaks.as_deref()) {
                let drawn = waveform::draw(&painter, full, visible, peaks, audio, colour);
                #[cfg(test)]
                {
                    state.waveforms += usize::from(drawn);
                }
                #[cfg(not(test))]
                let _ = drawn;
            }
            let name = std::path::Path::new(&audio.source).file_name().map_or_else(
                || audio.source.clone(),
                |n| n.to_string_lossy().into_owned(),
            );
            let label = if source.is_none() {
                format!("{name} (missing)")
            } else {
                name
            };
            #[cfg(test)]
            state.drawn_text.push(label.clone());
            painter.text(
                full.left_top() + vec2(6.0, 4.0),
                Align2::LEFT_TOP,
                label,
                FontId::proportional(11.0),
                Color32::BLACK.gamma_multiply(0.75),
            );
            if source.is_none() {
                painter.rect_stroke(
                    full,
                    4.0,
                    Stroke::new(1.5, colors::MISSING),
                    StrokeKind::Inside,
                );
            } else if selected {
                painter.rect_stroke(
                    full,
                    4.0,
                    Stroke::new(1.5, colors::SELECTED),
                    StrokeKind::Inside,
                );
            }
        }
    }

    // A drag whose clip has scrolled off screen or been removed never sees
    // its widget's `drag_stopped`, which would leave its undo group open.
    if state.drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
        state.drag = None;
        if !edits.contains(&Edit::EndDrag) {
            edits.push(Edit::EndDrag);
        }
    }
    if background.clicked() && !hit_clip {
        state.selected.clear();
    }
    if hovered
        && !state.selected.is_empty()
        && ui.ctx().memory(|m| m.focused()).is_none()
        && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace))
    {
        let removals = state
            .selected
            .iter()
            .map(|&id| Command::RemoveClip { id })
            .collect();
        edits.push(Edit::Apply(Command::Batch(removals)));
    }

    draw_headers(ui, rect, content, &tracks, state.scroll_y);
    draw_ruler(ui, rect, axis, &lines);
    if let Some(tick) = playhead {
        let x = axis.x(tick);
        if x >= content.left() && x <= content.right() {
            ui.painter_at(Rect::from_min_max(
                Pos2::new(content.left(), rect.top()),
                rect.max,
            ))
            .vline(x, rect.y_range(), Stroke::new(1.5, colors::PLAYHEAD));
        }
    }
    edits
}

/// Scrolling and zooming, while the pointer is over the arrangement.
fn navigate(ui: &egui::Ui, state: &mut TimelineState, content: Rect, tracks: usize, extent: f32) {
    let (scroll, zoom, pointer) =
        ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
    if zoom != 1.0 {
        // Keep the tick under the pointer where it is.
        let anchor_x = pointer.map_or(content.left(), |p| p.x.max(content.left()));
        let quarters = (anchor_x - content.left() + state.scroll_x) / state.ppq;
        state.ppq = (state.ppq * zoom).clamp(MIN_PPQ, MAX_PPQ);
        state.scroll_x = quarters * state.ppq - (anchor_x - content.left());
    } else {
        state.scroll_x -= scroll.x;
        state.scroll_y -= scroll.y;
    }
    let tall = tracks as f32 * colors::LANE_HEIGHT;
    state.scroll_x = state
        .scroll_x
        .clamp(0.0, (extent * state.ppq - content.width() * 0.5).max(0.0));
    state.scroll_y = state
        .scroll_y
        .clamp(0.0, (tall - content.height()).max(0.0));
}

/// Whether a press at `x` on a clip spanning `full` moves it or trims an edge.
fn mode_for(full: Rect, x: f32) -> Mode {
    if full.width() < MIN_TRIMMABLE {
        Mode::Move
    } else if x - full.left() < EDGE {
        Mode::TrimStart
    } else if full.right() - x < EDGE {
        Mode::TrimEnd
    } else {
        Mode::Move
    }
}

/// Where the pointer is, and what it's over, mid-drag.
struct DragInput<'a> {
    map: &'a TempoMap,
    axis: Axis,
    /// The pointer's x.
    x: f32,
    /// Alt: don't snap to beats.
    free: bool,
    /// The track under the pointer.
    target: NodeId,
    /// The grabbed clip's file, if it could be read.
    source: Option<Source>,
}

/// The command that puts the dragged clips where the pointer says, if that's
/// different from where they are.
fn drag_command(drag: &Drag, input: DragInput<'_>, project: &Project) -> Option<Command> {
    let DragInput {
        map,
        axis,
        x,
        free,
        target,
        source,
    } = input;
    let snap = |tick: Tick| if free { tick } else { grid::snap(map, tick) };
    let delta = axis.tick(x).0 - axis.tick(drag.press_x).0;
    let (_, grabbed) = drag.originals.iter().find(|(id, _)| *id == drag.grabbed)?;
    let rate = source.map_or(FALLBACK_RATE, |s| s.sample_rate);
    let commands: Vec<Command> = match drag.mode {
        Mode::Move => {
            let earliest = drag.originals.iter().map(|(_, c)| c.start.0).min()?;
            let wanted = snap(Tick(grabbed.start.0 + delta)).0 - grabbed.start.0;
            let shift = wanted.max(-earliest);
            // Dragging between lanes only makes sense for one clip.
            let single = drag.originals.len() == 1;
            drag.originals
                .iter()
                .map(|(id, original)| {
                    let mut clip = clips::moved(original, Tick(original.start.0 + shift));
                    if single {
                        clip.node = target;
                    }
                    Command::SetClip { id: *id, clip }
                })
                .collect()
        }
        Mode::TrimStart => {
            let edge = snap(Tick(grabbed.start.0 + delta));
            let clip = clips::trim_start(map, grabbed, edge, rate)?;
            vec![Command::SetClip {
                id: drag.grabbed,
                clip,
            }]
        }
        Mode::TrimEnd => {
            let audio = grabbed.as_audio()?;
            let end = clips::end_tick(map, grabbed.start, audio.length, rate);
            let edge = snap(Tick(end.0 + delta));
            let clip = clips::trim_end(map, grabbed, edge, rate, source.and_then(|s| s.frames))?;
            vec![Command::SetClip {
                id: drag.grabbed,
                clip,
            }]
        }
    };
    let changed: Vec<Command> = commands
        .into_iter()
        .filter(|command| match command {
            Command::SetClip { id, clip } => project.clip(*id) != Some(clip),
            _ => true,
        })
        .collect();
    match changed.len() {
        0 => None,
        1 => changed.into_iter().next(),
        _ => Some(Command::Batch(changed)),
    }
}

fn draw_headers(ui: &egui::Ui, rect: Rect, content: Rect, tracks: &[NodeId], scroll_y: f32) {
    let column = Rect::from_min_max(
        Pos2::new(rect.left(), content.top()),
        Pos2::new(content.left(), rect.bottom()),
    );
    let painter = ui.painter_at(column);
    painter.rect_filled(column, 0.0, colors::HEADER);
    for (index, _) in tracks.iter().enumerate() {
        let top = content.top() - scroll_y + index as f32 * colors::LANE_HEIGHT;
        let lane = Rect::from_min_size(
            Pos2::new(column.left(), top),
            vec2(colors::HEADER_WIDTH, colors::LANE_HEIGHT),
        );
        painter.rect_filled(
            Rect::from_min_size(lane.min, vec2(4.0, lane.height() - 1.0)),
            0.0,
            colors::track_colour(index),
        );
        painter.text(
            lane.left_top() + vec2(12.0, 8.0),
            Align2::LEFT_TOP,
            format!("Track {}", index + 1),
            FontId::proportional(13.0),
            colors::TEXT,
        );
        painter.hline(
            lane.x_range(),
            lane.bottom() - 0.5,
            Stroke::new(1.0, colors::LANE_EVEN),
        );
    }
}

fn draw_ruler(ui: &egui::Ui, rect: Rect, axis: Axis, lines: &[grid::Line]) {
    let strip = Rect::from_min_size(rect.min, Vec2::new(rect.width(), colors::RULER_HEIGHT));
    let painter = ui.painter_at(strip);
    painter.rect_filled(strip, 0.0, colors::RULER);
    let left = rect.left() + colors::HEADER_WIDTH;
    let painter = ui.painter_at(Rect::from_min_max(Pos2::new(left, strip.top()), strip.max));
    let mut last_label = f32::NEG_INFINITY;
    for line in lines {
        let x = axis.x(line.tick);
        match line.kind {
            grid::Kind::Bar(bar) => {
                painter.vline(
                    x,
                    strip.bottom() - 10.0..=strip.bottom(),
                    Stroke::new(1.0, colors::TEXT_WEAK),
                );
                if x - last_label >= 32.0 {
                    painter.text(
                        Pos2::new(x + 4.0, strip.top() + 2.0),
                        Align2::LEFT_TOP,
                        (bar + 1).to_string(),
                        FontId::proportional(11.0),
                        colors::TEXT,
                    );
                    last_label = x;
                }
            }
            grid::Kind::Beat if axis.ppq >= 24.0 => {
                painter.vline(
                    x,
                    strip.bottom() - 5.0..=strip.bottom(),
                    Stroke::new(1.0, colors::BAR_LINE),
                );
            }
            grid::Kind::Beat => {}
        }
    }
    painter.hline(
        strip.x_range(),
        strip.bottom() - 0.5,
        Stroke::new(1.0, colors::BACKGROUND),
    );
}

#[cfg(test)]
mod tests;
