//! Crossfades between audio clips that are butted together.
//!
//! Where one audio clip ends as the next begins, a handle sits on the cut,
//! halfway down the lane. Dragging it sideways makes a crossfade lasting as
//! far before the cut as after it. Each side stops where its clip runs out of
//! audio, and the other side carries on growing. Once there is a crossfade
//! its edges resize it, one side at a time, and its middle slides it along the
//! cut, keeping its length. The crossfade is stored on the later clip (see
//! [`Crossfade`]) and plays in timeline time, so warped clips need no care.

use std::collections::HashMap;

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, vec2};
use noodle_core::{BUTT_SECONDS, Clip, ClipId, Command, Crossfade, NodeId, Project, Tick};

use super::{Axis, clips, colors, warp_of};
use crate::session::Edit;

/// How wide an edge of the crossfade is to grab, and the handle's radius.
const GRAB: f32 = 8.0;
const HANDLE: f32 = 6.0;
const LANE_PAD: f32 = 3.0;

/// What a drag has hold of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Grab {
    /// The handle on the cut: makes a crossfade.
    Create,
    Left,
    Right,
    Middle,
}

/// Two audio clips butted together, and how far a crossfade can reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Butt {
    pub earlier: ClipId,
    pub later: ClipId,
    /// Where the later clip starts.
    pub cut: Tick,
    /// The longest the crossfade can be before the cut and after it: what the
    /// clips have to spare there, and no more than half of either clip.
    pub room: (Tick, Tick),
}

struct Drag {
    later: ClipId,
    grab: Grab,
    original: Clip,
    /// Where the pointer was when the drag began.
    press: Tick,
    butt: Butt,
}

#[derive(Default)]
pub struct State {
    drag: Option<Drag>,
    /// The widgets drawn last frame, for tests to point at.
    #[cfg(test)]
    pub(super) widgets: HashMap<(ClipId, Grab), Rect>,
}

/// What the layout of a clip needs from its file: its sample rate, and its
/// length in frames if known.
pub type Infos = HashMap<ClipId, (u32, Option<u64>)>;

/// The pairs of audio clips on `track` that are butted together.
pub fn butts(project: &Project, track: NodeId, infos: &Infos, fallback_rate: u32) -> Vec<Butt> {
    let map = project.tempo_map();
    let info = |id: ClipId| infos.get(&id).copied().unwrap_or((fallback_rate, None));
    // Each audio clip that plays, with the tick it ends on.
    let clips: Vec<(ClipId, &Clip, Tick)> = project
        .clips_on(track)
        .filter(|(_, clip)| clip.as_audio().is_some_and(|audio| !audio.muted))
        .map(|(id, clip)| {
            let end = clips::clip_end(map, clip, info(id).0, warp_of(project, clip));
            (id, clip, end)
        })
        .collect();
    let mut found = Vec::new();
    for &(later, clip, _) in &clips {
        let cut = clip.start;
        let gap = |end: Tick| (map.seconds_at(end) - map.seconds_at(cut)).abs();
        let Some(&(earlier, before, end)) = clips
            .iter()
            .filter(|&&(id, other, end)| {
                id != later && other.start < cut && gap(end) <= BUTT_SECONDS
            })
            .min_by(|a, b| gap(a.2).total_cmp(&gap(b.2)))
        else {
            continue;
        };
        let (rate_l, _) = info(later);
        let (rate_e, frames_e) = info(earlier);
        let end_later = clips::clip_end(map, clip, rate_l, warp_of(project, clip));
        let head = clips::head_room(map, clip, rate_l, warp_of(project, clip));
        let tail = clips::tail_room(map, before, rate_e, frames_e, warp_of(project, before));
        // A clip that ends past the cut lets the crossfade reach that far
        // with nothing to spare.
        let overrun = (end.0 - cut.0).max(0);
        let half = |from: Tick, to: Tick| Tick((to.0 - from.0).max(0) / 2);
        found.push(Butt {
            earlier,
            later,
            cut,
            room: (
                head.min(half(before.start, cut)),
                Tick(tail.0 + overrun).min(half(cut, end_later)),
            ),
        });
    }
    found
}

/// The crossfade that is heard on a butt: what is stored, held to the room.
pub fn effective(butt: &Butt, stored: Crossfade) -> Crossfade {
    Crossfade {
        before: stored.before.clamp(Tick::ZERO, butt.room.0.max(Tick::ZERO)),
        after: stored.after.clamp(Tick::ZERO, butt.room.1.max(Tick::ZERO)),
    }
}

/// The later clip with `crossfade` (none if it lasts no time).
fn with_crossfade(clip: &Clip, crossfade: Crossfade) -> Option<Clip> {
    let mut clip = clip.clone();
    let noodle_core::ClipContent::Audio(audio) = &mut clip.content else {
        return None;
    };
    audio.crossfade = (!crossfade.is_empty()).then_some(crossfade);
    Some(clip)
}

/// The later clip with its crossfade changed by a drag that took `grab` at
/// `press` and has the pointer at `pointer`. `original` is the clip as it was
/// when the drag began.
pub fn dragged(
    original: &Clip,
    butt: &Butt,
    grab: Grab,
    press: Tick,
    pointer: Tick,
) -> Option<Clip> {
    let was = effective(butt, original.as_audio()?.crossfade.unwrap_or_default());
    let (room_before, room_after) = (butt.room.0.0.max(0), butt.room.1.0.max(0));
    let cut = butt.cut.0;
    let (before, after) = match grab {
        Grab::Create => {
            // The same either side, each stopping at its own room.
            let reach = (pointer.0 - cut).abs();
            (reach.min(room_before), reach.min(room_after))
        }
        Grab::Left => ((cut - pointer.0).clamp(0, room_before), was.after.0),
        Grab::Right => (was.before.0, (pointer.0 - cut).clamp(0, room_after)),
        Grab::Middle => {
            // Slide: what one side gains the other loses.
            let (b, a) = (was.before.0, was.after.0);
            let lowest = (-a).max(b - room_before);
            let highest = b.min(room_after - a);
            let slide = (pointer.0 - press.0).clamp(lowest.min(highest), highest.max(lowest));
            (b - slide, a + slide)
        }
    };
    with_crossfade(
        original,
        Crossfade {
            before: Tick(before),
            after: Tick(after),
        },
    )
}

/// Where the lanes are, for [`show`].
pub struct View<'a> {
    pub project: &'a Project,
    pub axis: Axis<'a>,
    pub content: Rect,
    /// Each track, with the screen height of the top of its lane.
    pub tracks: &'a [(NodeId, f32)],
    pub infos: &'a Infos,
    pub lane_height: f32,
}

/// Draws the handles and crossfades of every visible lane and returns the
/// edits their drags make.
pub fn show(ui: &mut egui::Ui, state: &mut State, view: &View<'_>) -> Vec<Edit> {
    #[cfg(test)]
    state.widgets.clear();
    let mut edits = Vec::new();
    let painter = ui.painter_at(view.content);
    for &(track, top) in view.tracks {
        if top > view.content.bottom() || top + view.lane_height < view.content.top() {
            continue;
        }
        let (y0, y1) = (top + LANE_PAD, top + view.lane_height - LANE_PAD);
        for butt in butts(view.project, track, view.infos, super::FALLBACK_RATE) {
            let Some(later) = view.project.clip(butt.later) else {
                continue;
            };
            let Some(audio) = later.as_audio() else {
                continue;
            };
            let cut_x = view.axis.x(butt.cut);
            if cut_x < view.content.left() - 50.0 || cut_x > view.content.right() + 50.0 {
                continue;
            }
            let mid = (y0 + y1) / 2.0;
            let creating = state
                .drag
                .as_ref()
                .is_some_and(|d| d.later == butt.later && d.grab == Grab::Create);
            let fade = audio
                .crossfade
                .map(|stored| effective(&butt, stored))
                .filter(|fade| !fade.is_empty());
            let region = fade.map(|fade| {
                Rect::from_min_max(
                    Pos2::new(view.axis.x(Tick(butt.cut.0 - fade.before.0)), y0),
                    Pos2::new(view.axis.x(Tick(butt.cut.0 + fade.after.0)), y1),
                )
            });
            if let Some(region) = region {
                draw_region(&painter, region, cut_x);
            }
            // The handle on the cut makes a crossfade, and, once the drag that
            // made it is under way, keeps hold of it.
            if fade.is_none() || creating {
                let rect = Rect::from_center_size(
                    Pos2::new(cut_x, mid),
                    vec2(HANDLE * 2.0 + 6.0, HANDLE * 2.0 + 6.0),
                );
                let response = ui.interact(
                    rect,
                    ui.id().with(("crossfade handle", butt.later)),
                    Sense::click_and_drag(),
                );
                #[cfg(test)]
                state.widgets.insert((butt.later, Grab::Create), rect);
                let lit = response.hovered() || response.dragged();
                painter.circle(
                    Pos2::new(cut_x, mid),
                    if lit { HANDLE + 1.0 } else { HANDLE },
                    if lit {
                        Color32::WHITE
                    } else {
                        colors::BACKGROUND
                    },
                    Stroke::new(1.5, Color32::WHITE),
                );
                if lit {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                edits.extend(drive(
                    ui,
                    state,
                    &response,
                    view,
                    &butt,
                    later,
                    Grab::Create,
                ));
            } else if let Some(region) = region {
                let edge = |x: f32| {
                    Rect::from_min_max(Pos2::new(x - GRAB / 2.0, y0), Pos2::new(x + GRAB / 2.0, y1))
                };
                let (left, right) = (edge(region.left()), edge(region.right()));
                // The middle is registered first, so the edges win where they
                // overlap it on a narrow crossfade.
                let middle = ui.interact(
                    region,
                    ui.id().with(("crossfade middle", butt.later)),
                    Sense::click_and_drag(),
                );
                #[cfg(test)]
                state.widgets.insert((butt.later, Grab::Middle), region);
                if middle.hovered() || middle.dragged() {
                    ui.ctx().set_cursor_icon(CursorIcon::Grab);
                }
                edits.extend(drive(ui, state, &middle, view, &butt, later, Grab::Middle));
                let mut removed = false;
                middle.context_menu(|ui| {
                    if ui.button("Remove crossfade").clicked() {
                        ui.close();
                        removed = true;
                    }
                });
                if removed || middle.double_clicked() {
                    let none = Crossfade::default();
                    edits.extend(with_crossfade(later, none).map(|clip| {
                        Edit::Apply(Command::SetClip {
                            id: butt.later,
                            clip,
                        })
                    }));
                }
                for (side, rect) in [(Grab::Left, left), (Grab::Right, right)] {
                    let response = ui.interact(
                        rect,
                        ui.id()
                            .with(("crossfade edge", butt.later, side == Grab::Left)),
                        Sense::click_and_drag(),
                    );
                    #[cfg(test)]
                    state.widgets.insert((butt.later, side), rect);
                    if response.hovered() || response.dragged() {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                        painter.rect_filled(rect, 2.0, Color32::WHITE.gamma_multiply(0.6));
                    }
                    edits.extend(drive(ui, state, &response, view, &butt, later, side));
                }
            }
        }
    }
    // A drag whose clips have gone never sees its widget's end.
    if let Some(drag) = &state.drag
        && (view.project.clip(drag.later).is_none() || !ui.input(|i| i.pointer.any_down()))
    {
        state.drag = None;
        if !edits.contains(&Edit::EndDrag) {
            edits.push(Edit::EndDrag);
        }
    }
    edits
}

/// Starts, follows and ends a drag of `grab` on `response`.
fn drive(
    ui: &egui::Ui,
    state: &mut State,
    response: &egui::Response,
    view: &View<'_>,
    butt: &Butt,
    later: &Clip,
    grab: Grab,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    if response.drag_started() {
        let press = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos())
            .map_or(butt.cut, |pos| view.axis.tick(pos.x));
        state.drag = Some(Drag {
            later: butt.later,
            grab,
            original: later.clone(),
            press,
            butt: *butt,
        });
    }
    if response.dragged()
        && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
        && drag.later == butt.later
        && drag.grab == grab
        && let Some(changed) = dragged(
            &drag.original,
            &drag.butt,
            grab,
            drag.press,
            view.axis.tick(pos.x),
        )
        && changed != *later
    {
        edits.push(Edit::Drag(Command::SetClip {
            id: butt.later,
            clip: changed,
        }));
    }
    if response.drag_stopped()
        && state
            .drag
            .as_ref()
            .is_some_and(|d| d.later == butt.later && d.grab == grab)
    {
        state.drag = None;
        edits.push(Edit::EndDrag);
    }
    edits
}

/// The crossfade's span, shaded, with the two fades drawn across it: the
/// earlier clip falling and the later one rising.
fn draw_region(painter: &egui::Painter, region: Rect, cut_x: f32) {
    painter.rect_filled(region, 2.0, Color32::WHITE.gamma_multiply(0.18));
    painter.rect_stroke(
        region,
        2.0,
        Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.7)),
        egui::StrokeKind::Inside,
    );
    let steps = 12;
    let curve = |rising: bool| {
        (0..=steps)
            .map(|i| {
                let t = i as f32 / steps as f32;
                // Equal power: the level is the sine of a quarter turn.
                let level = if rising { t } else { 1.0 - t };
                let level = (level * std::f32::consts::FRAC_PI_2).sin();
                Pos2::new(
                    region.left() + t * region.width(),
                    region.bottom() - level * region.height(),
                )
            })
            .collect::<Vec<_>>()
    };
    for rising in [false, true] {
        painter.add(egui::Shape::line(
            curve(rising),
            Stroke::new(1.5, Color32::WHITE),
        ));
    }
    painter.vline(
        cut_x,
        region.y_range(),
        Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.4)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{ClipContent, History};

    fn butt(before: i64, after: i64) -> Butt {
        Butt {
            earlier: ClipId(1),
            later: ClipId(2),
            cut: Tick(1000),
            room: (Tick(before), Tick(after)),
        }
    }

    fn later(fade: Option<(i64, i64)>) -> Clip {
        let mut clip = Clip::audio(NodeId(1), Tick(1000), "b.wav", 48_000);
        if let ClipContent::Audio(audio) = &mut clip.content {
            audio.crossfade = fade.map(|(before, after)| Crossfade {
                before: Tick(before),
                after: Tick(after),
            });
        }
        clip
    }

    fn fade_of(clip: &Clip) -> Option<(i64, i64)> {
        let x = clip.as_audio()?.crossfade?;
        Some((x.before.0, x.after.0))
    }

    fn drag(clip: &Clip, butt: &Butt, grab: Grab, press: i64, pointer: i64) -> Option<(i64, i64)> {
        fade_of(&dragged(clip, butt, grab, Tick(press), Tick(pointer)).unwrap())
    }

    #[test]
    fn the_handle_grows_both_sides_equally_until_one_runs_out() {
        let butt = butt(100, 1000);
        let clip = later(None);
        assert_eq!(drag(&clip, &butt, Grab::Create, 1000, 1060), Some((60, 60)));
        // Left of the cut is the same as right of it.
        assert_eq!(drag(&clip, &butt, Grab::Create, 1000, 940), Some((60, 60)));
        // The earlier side stops at its room; the other carries on.
        assert_eq!(
            drag(&clip, &butt, Grab::Create, 1000, 1500),
            Some((100, 500))
        );
        assert_eq!(
            drag(&clip, &butt, Grab::Create, 1000, 400),
            Some((100, 600))
        );
        // Back at the cut there is nothing.
        assert_eq!(drag(&clip, &butt, Grab::Create, 1000, 1000), None);
    }

    #[test]
    fn the_edges_resize_one_side_each_within_the_room() {
        let butt = butt(200, 300);
        let clip = later(Some((100, 100)));
        assert_eq!(drag(&clip, &butt, Grab::Left, 900, 850), Some((150, 100)));
        assert_eq!(drag(&clip, &butt, Grab::Left, 900, 100), Some((200, 100)));
        assert_eq!(
            drag(&clip, &butt, Grab::Right, 1100, 1250),
            Some((100, 250))
        );
        assert_eq!(
            drag(&clip, &butt, Grab::Right, 1100, 9000),
            Some((100, 300))
        );
        // Dragging an edge across the cut takes that side to nothing, and
        // both to nothing removes the crossfade.
        assert_eq!(drag(&clip, &butt, Grab::Left, 900, 1100), Some((0, 100)));
        let one_side = later(Some((100, 0)));
        assert_eq!(drag(&one_side, &butt, Grab::Left, 900, 1100), None);
    }

    #[test]
    fn the_middle_slides_the_crossfade_along_the_cut_at_the_same_length() {
        let butt = butt(150, 400);
        let clip = later(Some((100, 100)));
        // Right: what the front loses the back gains.
        assert_eq!(drag(&clip, &butt, Grab::Middle, 1000, 1100), Some((0, 200)));
        assert_eq!(drag(&clip, &butt, Grab::Middle, 1000, 1500), Some((0, 200)));
        // Left, held to the earlier clip's room.
        assert_eq!(drag(&clip, &butt, Grab::Middle, 1000, 970), Some((130, 70)));
        assert_eq!(drag(&clip, &butt, Grab::Middle, 1000, 0), Some((150, 50)));
    }

    #[test]
    fn what_is_heard_is_held_to_the_room() {
        let fade = effective(
            &butt(50, 70),
            Crossfade {
                before: Tick(200),
                after: Tick(30),
            },
        );
        assert_eq!((fade.before.0, fade.after.0), (50, 30));
    }

    fn butted_project() -> (Project, Infos) {
        let mut project = Project::new();
        let mut history = History::new();
        let node = noodle_core::Node::new(noodle_core::group::TRACK_INPUT);
        history
            .apply(
                &mut project,
                Command::AddNode {
                    id: NodeId(1),
                    node,
                },
            )
            .unwrap();
        // At 120 beats per minute a second is 1920 ticks. A is a second of a
        // 1.4 s file; B starts a second in and plays from 0.25 s into its file.
        let a = project.new_clip_id();
        let b = project.new_clip_id();
        let mut second = Clip::audio(NodeId(1), Tick(1920), "b.wav", 48_000);
        if let ClipContent::Audio(audio) = &mut second.content {
            audio.offset = 12_000;
        }
        for (id, clip) in [
            (a, Clip::audio(NodeId(1), Tick(0), "a.wav", 48_000)),
            (b, second),
        ] {
            history
                .apply(&mut project, Command::AddClip { id, clip })
                .unwrap();
        }
        let infos = Infos::from([(a, (48_000, Some(67_200))), (b, (48_000, Some(96_000)))]);
        (project, infos)
    }

    #[test]
    fn butted_clips_are_found_with_the_room_each_has() {
        let (project, infos) = butted_project();
        let found = butts(&project, NodeId(1), &infos, 48_000);
        assert_eq!(found.len(), 1);
        let butt = found[0];
        assert_eq!(
            (butt.earlier, butt.later, butt.cut),
            (ClipId(1), ClipId(2), Tick(1920))
        );
        // B has a quarter second (480 ticks) behind its start. A's file goes
        // on 0.4 s (768 ticks) past its end.
        assert_eq!((butt.room.0.0, butt.room.1.0), (480, 768));
    }

    #[test]
    fn clips_with_a_gap_between_them_are_not_butted() {
        let (mut project, infos) = butted_project();
        let mut history = History::new();
        let mut clip = project.clip(ClipId(2)).unwrap().clone();
        clip.start = Tick(2100);
        history
            .apply(
                &mut project,
                Command::SetClip {
                    id: ClipId(2),
                    clip,
                },
            )
            .unwrap();
        assert!(butts(&project, NodeId(1), &infos, 48_000).is_empty());
    }
}
