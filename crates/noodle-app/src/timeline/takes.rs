//! Take lanes: the takes of a take folder shown one under another below its
//! track, for comping them. Dragging across a take swipes that stretch of it
//! into the comp (on every linked folder at once); clicking a take auditions
//! it alone; each join of an audio comp has a handle to drag its crossfade.

use std::collections::BTreeSet;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use noodle_core::{AudioClip, Clip, ClipId, ClipWarp, Crossfade, MidiClip, Project, Tick};

use super::{Axis, FALLBACK_RATE, TimelineState, clips, comp, midi_preview, waveform};
use crate::session::Edit;
use crate::theme::timeline as colors;

/// The height of one take's lane.
pub const LANE: f32 = 36.0;

/// The take lanes that are open, and the gesture in progress on them.
#[derive(Default)]
pub struct State {
    /// The take folders whose takes are shown.
    pub shown: BTreeSet<ClipId>,
    /// The count of the session's last recording whose folders were opened.
    opened: u64,
    swipe: Option<Swipe>,
    join: Option<(ClipId, usize)>,
}

struct Swipe {
    clip: ClipId,
    take: usize,
    from_x: f32,
}

/// How many takes a clip has, if it is a take folder.
pub fn count(clip: &Clip) -> Option<usize> {
    match &clip.content {
        noodle_core::ClipContent::Audio(audio) => audio.takes.as_ref().map(|t| t.takes.len()),
        noodle_core::ClipContent::Midi(midi) => midi.takes.as_ref().map(|t| t.content.len()),
    }
}

impl State {
    /// Opens the lanes of the take folders a recording has just made or
    /// added to, once.
    pub fn open_recorded(&mut self, session: &crate::session::Session) {
        let (count, folders) = session.opened_takes();
        if count != self.opened {
            self.opened = count;
            self.shown.extend(folders.iter().copied());
        }
    }
    /// How many take lanes each of `tracks` shows: the most of any of its
    /// open folders.
    pub fn lanes(&self, project: &Project, tracks: &[noodle_core::NodeId]) -> Vec<usize> {
        tracks
            .iter()
            .map(|&track| {
                project
                    .clips_on(track)
                    .filter(|(id, _)| self.shown.contains(id))
                    .filter_map(|(_, clip)| count(clip))
                    .max()
                    .unwrap_or(0)
            })
            .collect()
    }

    /// Forgets folders that are gone.
    pub fn retain(&mut self, project: &Project) {
        self.shown
            .retain(|&id| project.clip(id).is_some_and(|c| count(c).is_some()));
    }
}

/// What a track's take lanes are drawn against.
pub struct View<'a> {
    pub project: &'a Project,
    pub axis: Axis<'a>,
    pub content: Rect,
    pub directory: Option<&'a std::path::Path>,
    pub colour: Color32,
    /// The screen y where the track's own lane, and below it its take lanes,
    /// begin.
    pub lane_top: f32,
    pub lane_height: f32,
    /// The take being auditioned.
    pub audition: Option<(ClipId, usize)>,
}

/// What the lanes of one track asked for.
#[derive(Default)]
pub struct Output {
    pub edits: Vec<Edit>,
    /// Audition this take (or none): the answer to a click.
    pub audition: Option<Option<(ClipId, usize)>>,
}

/// The stretches of the comp as ticks: the span's index in the comp, from,
/// to, and the take.
pub fn spans(
    project: &Project,
    map: &noodle_core::TempoMap,
    clip: &Clip,
    rate: u32,
    warp: Option<&ClipWarp>,
    end: Tick,
) -> Vec<(usize, Tick, Tick, usize)> {
    let starts: Vec<(usize, Tick, usize)> = match &clip.content {
        noodle_core::ClipContent::Audio(audio) => audio
            .takes
            .as_ref()
            .map(|folder| {
                folder
                    .comp
                    .iter()
                    .enumerate()
                    .filter_map(|(i, span)| {
                        let tick = if i == 0 {
                            clip.start
                        } else {
                            clips::tick_of_frame(map, clip, span.at, rate, warp)?
                        };
                        Some((i, tick, span.take))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        noodle_core::ClipContent::Midi(midi) => midi
            .takes
            .as_ref()
            .map(|takes| {
                takes
                    .folder
                    .comp
                    .iter()
                    .enumerate()
                    .map(|(i, span)| (i, Tick(clip.start.0 + span.at as i64), span.take))
                    .collect()
            })
            .unwrap_or_default(),
    };
    let _ = project;
    starts
        .iter()
        .enumerate()
        .map(|(n, &(index, from, take))| {
            let to = starts.get(n + 1).map_or(end, |next| next.1);
            (index, from.max(clip.start), to.min(end), take)
        })
        .collect()
}

/// Draws and handles the take lanes of the open folders on one track.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut TimelineState,
    view: &View<'_>,
    track: noodle_core::NodeId,
) -> Output {
    let mut out = Output::default();
    let project = view.project;
    let map = view.axis.map;
    let folders: Vec<(ClipId, &Clip)> = project
        .clips_on(track)
        .filter(|(id, clip)| state.takes.shown.contains(id) && count(clip).is_some())
        .collect();
    for (id, clip) in folders {
        let Some(n) = count(clip) else { continue };
        let audio = clip.as_audio();
        let loaded =
            audio.and_then(|audio| state.sources.get(ui.ctx(), view.directory, &audio.source));
        let rate = loaded
            .as_ref()
            .map_or(FALLBACK_RATE, |l| l.source.sample_rate);
        let warp = audio.and_then(|a| a.warp).and_then(|w| project.warp(w));
        let end = clips::clip_end(map, clip, rate, warp);
        let (x0, x1) = (
            view.axis.x(clip.start),
            view.axis.x(end).max(view.axis.x(clip.start) + 2.0),
        );
        let comp_spans = spans(project, map, clip, rate, warp, end);
        let top = view.lane_top + view.lane_height;
        for k in 0..n {
            let row = Rect::from_min_max(
                Pos2::new(x0, top + k as f32 * LANE + 1.0),
                Pos2::new(x1, top + (k + 1) as f32 * LANE - 1.0),
            );
            let visible = row.intersect(view.content);
            if visible.width() <= 0.0 || visible.height() <= 0.0 {
                continue;
            }
            let response = ui.interact(
                visible,
                ui.id().with(("take", id, k)),
                Sense::click_and_drag(),
            );
            let painter = ui.painter_at(visible);
            painter.rect_filled(row, 2.0, view.colour.gamma_multiply(0.22));
            match (&clip.content, &loaded) {
                (noodle_core::ClipContent::Audio(audio), Some(loaded)) => {
                    if let (Some(peaks), Some(folder)) =
                        (loaded.peaks.as_ref(), audio.takes.as_ref())
                    {
                        let delta = folder.takes[k].delta;
                        let shifted = AudioClip {
                            offset: audio.offset + delta,
                            ..audio.clone()
                        };
                        let wave = waveform::Clip {
                            id,
                            peaks,
                            audio: &shifted,
                            colour: view.colour,
                        };
                        waveform::draw_range(
                            &painter,
                            &mut state.waveforms,
                            (id, 8 + k.min(200) as u8),
                            row,
                            visible,
                            &wave,
                            (shifted.offset, shifted.offset + shifted.length),
                        );
                    }
                }
                (noodle_core::ClipContent::Midi(midi), _) => {
                    if let Some(takes) = &midi.takes {
                        let mut one = MidiClip::new(midi.length);
                        one.notes = takes.content[k].notes.clone();
                        midi_preview::draw(&painter, view.axis, row, clip.start, &one);
                    }
                }
                _ => {}
            }
            // Where the comp plays this take.
            for &(_, from, to, take) in &comp_spans {
                if take != k {
                    continue;
                }
                let span = Rect::from_min_max(
                    Pos2::new(view.axis.x(from), row.top()),
                    Pos2::new(view.axis.x(to), row.bottom()),
                );
                painter.rect_filled(span, 2.0, colors::SELECTED.gamma_multiply(0.30));
                painter.rect_stroke(
                    span,
                    2.0,
                    Stroke::new(1.0, colors::SELECTED),
                    StrokeKind::Inside,
                );
            }
            let auditioned = view.audition == Some((id, k));
            let label = if auditioned {
                format!("▶ Take {}", k + 1)
            } else {
                format!("Take {}", k + 1)
            };
            #[cfg(test)]
            state.drawn_text.push(label.clone());
            painter.text(
                Pos2::new(row.left().max(view.content.left()) + 6.0, row.top() + 3.0),
                Align2::LEFT_TOP,
                label,
                FontId::proportional(11.0),
                Color32::WHITE.gamma_multiply(0.9),
            );
            #[cfg(test)]
            state.take_lane_rects.insert((id, k), visible);

            // Swiping.
            if response.drag_started() {
                let press = ui
                    .input(|i| i.pointer.press_origin())
                    .or(response.interact_pointer_pos())
                    .unwrap_or(row.center());
                state.takes.swipe = Some(Swipe {
                    clip: id,
                    take: k,
                    from_x: press.x,
                });
            }
            let swiping = state
                .takes
                .swipe
                .as_ref()
                .filter(|s| s.clip == id && s.take == k)
                .map(|s| s.from_x);
            if let Some(from_x) = swiping {
                let to_x = response.interact_pointer_pos().map_or(from_x, |p| p.x);
                let (a, b) = (from_x.min(to_x), from_x.max(to_x));
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(a, row.top()), Pos2::new(b, row.bottom())),
                    2.0,
                    colors::SELECTED.gamma_multiply(0.5),
                );
                if response.drag_stopped() {
                    state.takes.swipe = None;
                    let free = ui.input(|i| i.modifiers.alt);
                    let at = |x: f32| {
                        let tick = view.axis.tick(x);
                        if free { tick } else { view.axis.snap(tick) }
                    };
                    let range = (at(a), at(b));
                    let mut rate_of = |audio: &AudioClip| {
                        state
                            .sources
                            .get(ui.ctx(), view.directory, &audio.source)
                            .map_or(FALLBACK_RATE, |l| l.source.sample_rate)
                    };
                    out.edits.extend(
                        comp::swipe(project, map, id, range, k, &mut rate_of).map(Edit::Apply),
                    );
                }
            } else if response.clicked() {
                out.audition = Some(if auditioned { None } else { Some((id, k)) });
            }
        }
        if state.takes.swipe.is_some() && !ui.input(|i| i.pointer.any_down()) {
            state.takes.swipe = None;
        }

        // The joins of an audio comp: a handle on the clip to drag the
        // crossfade wider or narrower.
        if let Some(folder) = audio.and_then(|a| a.takes.as_ref()) {
            for &(i, from, _, _) in comp_spans.iter().filter(|s| s.0 > 0) {
                let Some(span) = folder.comp.get(i) else {
                    break;
                };
                let fade = span
                    .crossfade
                    .unwrap_or_else(|| comp::default_join(map, from));
                let x = view.axis.x(from);
                let (left, right) = (
                    view.axis.x(Tick(from.0 - fade.before.0)),
                    view.axis.x(Tick(from.0 + fade.after.0)),
                );
                let grip =
                    Rect::from_center_size(Pos2::new(x, view.lane_top + 12.0), Vec2::splat(10.0));
                if !view.content.intersects(grip) {
                    continue;
                }
                let response = ui.interact(grip, ui.id().with(("join", id, i)), Sense::drag());
                let painter = ui.painter_at(view.content);
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(left, view.lane_top + 3.0),
                        Pos2::new(
                            right.max(left + 1.0),
                            view.lane_top + view.lane_height - 3.0,
                        ),
                    ),
                    0.0,
                    Color32::WHITE.gamma_multiply(0.25),
                );
                painter.circle_filled(grip.center(), 4.5, Color32::WHITE);
                #[cfg(test)]
                state.join_rects.insert((id, i), grip);
                if response.drag_started() {
                    state.takes.join = Some((id, i));
                }
                if response.dragged()
                    && state.takes.join == Some((id, i))
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    let wide = (view.axis.tick(pos.x).0 - from.0).abs().max(0);
                    let crossfade = Crossfade {
                        before: Tick(wide),
                        after: Tick(wide),
                    };
                    out.edits
                        .extend(comp::set_join(project, id, i, Some(crossfade)).map(Edit::Drag));
                }
                if response.drag_stopped() {
                    state.takes.join = None;
                    out.edits.push(Edit::EndDrag);
                }
            }
        }
    }
    out
}
