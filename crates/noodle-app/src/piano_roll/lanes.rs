//! The lanes under the piano roll's notes: one for each controller the clip
//! uses (with "+" to add one), and a lane for the expressions of the
//! selected notes.
//!
//! Like the notes, each gesture works out its result from the clip's data as
//! it was when the gesture began, and is one undo step. Draw sets a value
//! along the drag, Line stores a ramp as two points, and Select picks points
//! to move, delete, copy and paste. Positions are ticks from the clip's start
//! (an expression's lane also counts from the clip's start; the points are
//! stored from the note's).

use std::collections::{BTreeMap, BTreeSet};

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, vec2};
use noodle_core::{
    Clip, ClipId, ControlKind, ControlPoint, ExpressionKind, MidiClip, MidiNote, NoteExpression,
    TempoMap, Tick,
};

use super::{Axes, Output, set_controls, set_notes};
use crate::session::Edit;
use crate::theme::{self, timeline as colors};
use crate::timeline::ClipAction;

/// The tallest a lane is.
pub const LANE: f32 = 48.0;
/// The shortest, when many lanes share the window.
const MIN_LANE: f32 = 22.0;
/// How near a press must be to a point, in points, to take it.
const REACH: f32 = 7.0;

/// What dragging in a lane does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Sets the value along the drag.
    Draw,
    /// A straight ramp from the press to the pointer, stored as two points.
    Line,
    /// Picks controller points, to move, delete and copy.
    Select,
}

/// A controller point, named by where it is: its channel, kind and tick.
pub type PointId = (u8, ControlKind, i64);

pub fn id(point: &ControlPoint) -> PointId {
    (point.channel, point.kind, point.tick.0)
}

/// The state of the lanes, between frames.
pub struct Lanes {
    /// Lanes added with "+" that the clip has no points for yet.
    pub extra: Vec<ControlKind>,
    pub tool: Tool,
    pub picked: BTreeSet<PointId>,
    /// Whether the expression lane is shown.
    pub expressions: bool,
    pub expression: ExpressionKind,
    /// Whether the pointer is over the lanes, which is where Delete, copy and
    /// paste mean points rather than notes.
    pub hover: bool,
    /// The controller number "Add CC" in the menu adds.
    next_cc: u8,
    drag: Option<LaneDrag>,
}

impl Default for Lanes {
    fn default() -> Self {
        Self {
            extra: Vec::new(),
            tool: Tool::Draw,
            picked: BTreeSet::new(),
            expressions: false,
            expression: ExpressionKind::Pressure,
            hover: false,
            next_cc: 1,
            drag: None,
        }
    }
}

impl Lanes {
    /// Forgets what belongs to the clip that was open.
    pub fn reset(&mut self) {
        self.extra.clear();
        self.picked.clear();
        self.hover = false;
        self.drag = None;
    }

    pub fn add(&mut self, kind: ControlKind) {
        if !self.extra.contains(&kind) {
            self.extra.push(kind);
        }
    }

    /// Drops picked points that are no longer in the clip, e.g. after undo.
    pub fn retain_existing(&mut self, midi: &MidiClip) {
        let there: BTreeSet<PointId> = midi.controls.iter().map(id).collect();
        self.picked.retain(|p| there.contains(p));
    }
}

#[derive(Clone, Debug)]
enum Gesture {
    Draw {
        path: Vec<(i64, f32)>,
    },
    Line {
        from: (i64, f32),
    },
    /// Moving the picked points; `grabbed` is the tick of the one pressed.
    Move {
        grabbed: i64,
        press_value: f32,
        picked: BTreeSet<PointId>,
    },
    Marquee {
        from: Pos2,
        base: BTreeSet<PointId>,
    },
}

struct LaneDrag {
    gesture: Gesture,
    /// The lane: a controller, or `None` for the expression lane.
    target: Option<ControlKind>,
    channel: u8,
    controls: Vec<ControlPoint>,
    notes: Vec<MidiNote>,
}

/// The lanes to show, and how tall each is.
pub struct Layout {
    pub kinds: Vec<ControlKind>,
    pub expression: bool,
    pub height: f32,
}

impl Layout {
    pub fn new(lanes: &Lanes, midi: &MidiClip, body_height: f32, fixed: f32) -> Self {
        let mut kinds: Vec<ControlKind> = midi
            .controls
            .iter()
            .map(|p| p.kind)
            .filter(|k| *k != ControlKind::ProgramChange)
            .collect();
        kinds.extend(lanes.extra.iter().copied());
        kinds.sort();
        kinds.dedup();
        let count = kinds.len() + usize::from(lanes.expressions);
        let height = if count == 0 {
            LANE
        } else {
            ((body_height - fixed) / count as f32).clamp(MIN_LANE, LANE)
        };
        Self {
            kinds,
            expression: lanes.expressions,
            height,
        }
    }

    pub fn total(&self) -> f32 {
        self.height * (self.kinds.len() + usize::from(self.expression)) as f32
    }

    /// The rectangles of the lanes, stacked down from `top` between `left`
    /// and `right`.
    pub fn rects(
        &self,
        top: f32,
        left: f32,
        right: f32,
    ) -> (Vec<(ControlKind, Rect)>, Option<Rect>) {
        let mut y = top;
        let mut next = || {
            let rect = Rect::from_min_max(Pos2::new(left, y), Pos2::new(right, y + self.height));
            y += self.height;
            rect
        };
        let kinds = self.kinds.iter().map(|k| (*k, next())).collect();
        let expression = self.expression.then(&mut next);
        (kinds, expression)
    }
}

/// The name of a lane.
pub fn name(kind: ControlKind) -> String {
    match kind {
        ControlKind::Controller(1) => "Mod wheel".into(),
        ControlKind::Controller(2) => "Breath".into(),
        ControlKind::Controller(7) => "Volume".into(),
        ControlKind::Controller(10) => "Pan".into(),
        ControlKind::Controller(11) => "Expression".into(),
        ControlKind::Controller(64) => "Sustain".into(),
        ControlKind::Controller(66) => "Sostenuto".into(),
        ControlKind::Controller(n) => format!("CC {n}"),
        ControlKind::PitchBend => "Pitch bend".into(),
        ControlKind::ChannelPressure => "Pressure".into(),
        ControlKind::ProgramChange => "Program".into(),
    }
}

fn expression_name(kind: ExpressionKind) -> &'static str {
    match kind {
        ExpressionKind::Pitch => "Pitch",
        ExpressionKind::Pressure => "Pressure",
        ExpressionKind::Timbre => "Timbre",
        ExpressionKind::Volume => "Volume",
        ExpressionKind::Pan => "Pan",
    }
}

/// The least and greatest value of a lane.
pub fn range(kind: ControlKind) -> (f32, f32) {
    match kind {
        ControlKind::PitchBend => (-1.0, 1.0),
        _ => (0.0, 1.0),
    }
}

fn expression_range(kind: ExpressionKind) -> (f32, f32) {
    match kind {
        ExpressionKind::Pitch => (-12.0, 12.0),
        ExpressionKind::Pan => (-1.0, 1.0),
        _ => (0.0, 1.0),
    }
}

/// A controller value on its 7-bit steps, so equal drawings are equal.
fn quantised(kind: ControlKind, value: f32) -> f32 {
    match kind {
        ControlKind::Controller(_) => (value * 127.0).round() / 127.0,
        _ => value,
    }
}

/// The screen y of `value` in a lane with `range`.
pub fn value_y(rect: Rect, range: (f32, f32), value: f32) -> f32 {
    let along = (value - range.0) / (range.1 - range.0);
    rect.bottom() - 4.0 - along * (rect.height() - 8.0)
}

fn value_at(rect: Rect, range: (f32, f32), y: f32) -> f32 {
    let along = ((rect.bottom() - 4.0 - y) / (rect.height() - 8.0)).clamp(0.0, 1.0);
    range.0 + along * (range.1 - range.0)
}

// What each gesture does to the data.

fn sorted(mut points: Vec<ControlPoint>) -> Vec<ControlPoint> {
    points.sort_by_key(|p| p.tick);
    points
}

/// `original` with the lane's points under the `path` replaced by the path's
/// points (the last value at a tick wins).
pub fn drawn(
    original: &[ControlPoint],
    channel: u8,
    kind: ControlKind,
    path: &[(i64, f32)],
) -> Vec<ControlPoint> {
    let (Some(lo), Some(hi)) = (
        path.iter().map(|p| p.0).min(),
        path.iter().map(|p| p.0).max(),
    ) else {
        return original.to_vec();
    };
    let mut out: Vec<ControlPoint> = original
        .iter()
        .filter(|p| !(p.channel == channel && p.kind == kind && (lo..=hi).contains(&p.tick.0)))
        .copied()
        .collect();
    let by_tick: BTreeMap<i64, f32> = path.iter().copied().collect();
    out.extend(
        by_tick
            .into_iter()
            .map(|(tick, value)| ControlPoint::new(Tick(tick), channel, kind, value)),
    );
    sorted(out)
}

/// `original` with a ramp from `a` to `b` on the lane: two points, the first
/// ramping, replacing the lane's points between them.
pub fn lined(
    original: &[ControlPoint],
    channel: u8,
    kind: ControlKind,
    a: (i64, f32),
    b: (i64, f32),
) -> Vec<ControlPoint> {
    let (a, b) = if a.0 <= b.0 { (a, b) } else { (b, a) };
    if a.0 == b.0 {
        return drawn(original, channel, kind, &[b]);
    }
    let mut out: Vec<ControlPoint> = original
        .iter()
        .filter(|p| !(p.channel == channel && p.kind == kind && (a.0..=b.0).contains(&p.tick.0)))
        .copied()
        .collect();
    out.push(ControlPoint {
        ramp: true,
        ..ControlPoint::new(Tick(a.0), channel, kind, a.1)
    });
    out.push(ControlPoint::new(Tick(b.0), channel, kind, b.1));
    sorted(out)
}

/// `points` without the picked ones.
pub fn removed(points: &[ControlPoint], picked: &BTreeSet<PointId>) -> Vec<ControlPoint> {
    points
        .iter()
        .filter(|p| !picked.contains(&id(p)))
        .copied()
        .collect()
}

/// The picked points moved `d_tick` ticks (the group stops at the clip's
/// ends), and those of `kind` moved `d_value` in value as well. Moving onto a
/// point replaces it. Returns the points and the new ids of the picked.
pub fn moved(
    points: &[ControlPoint],
    picked: &BTreeSet<PointId>,
    d_tick: i64,
    kind: ControlKind,
    d_value: f32,
    clip_length: i64,
) -> (Vec<ControlPoint>, BTreeSet<PointId>) {
    let chosen = || points.iter().filter(|p| picked.contains(&id(p)));
    let earliest = chosen().map(|p| p.tick.0).min().unwrap_or(0);
    let latest = chosen().map(|p| p.tick.0).max().unwrap_or(0);
    let d_tick = d_tick.clamp(-earliest, (clip_length - 1 - latest).max(-earliest));
    let moved: Vec<ControlPoint> = chosen()
        .map(|p| {
            let (lo, hi) = range(p.kind);
            let value = if p.kind == kind {
                quantised(kind, (p.value + d_value).clamp(lo, hi))
            } else {
                p.value
            };
            ControlPoint {
                tick: Tick(p.tick.0 + d_tick),
                value,
                ..*p
            }
        })
        .collect();
    let ids: BTreeSet<PointId> = moved.iter().map(id).collect();
    let mut out: Vec<ControlPoint> = points
        .iter()
        .filter(|p| !picked.contains(&id(p)) && !ids.contains(&id(p)))
        .copied()
        .collect();
    out.extend(moved);
    (sorted(out), ids)
}

/// The picked points as they are copied: ticks counted from the earliest.
pub fn copied(points: &[ControlPoint], picked: &BTreeSet<PointId>) -> Vec<ControlPoint> {
    let mut chosen: Vec<ControlPoint> = points
        .iter()
        .filter(|p| picked.contains(&id(p)))
        .copied()
        .collect();
    let earliest = chosen.iter().map(|p| p.tick.0).min().unwrap_or(0);
    for point in &mut chosen {
        point.tick = Tick(point.tick.0 - earliest);
    }
    sorted(chosen)
}

/// `points` with the copied ones put down so the earliest is at `at`, and the
/// ids of those. Points past the clip's end are dropped; `None` if none fit.
pub fn pasted(
    points: &[ControlPoint],
    copied: &[ControlPoint],
    at: i64,
    clip_length: i64,
) -> Option<(Vec<ControlPoint>, BTreeSet<PointId>)> {
    let put: Vec<ControlPoint> = copied
        .iter()
        .filter(|p| (0..clip_length).contains(&(at + p.tick.0)))
        .map(|p| ControlPoint {
            tick: Tick(at + p.tick.0),
            ..*p
        })
        .collect();
    if put.is_empty() {
        return None;
    }
    let ids: BTreeSet<PointId> = put.iter().map(id).collect();
    let mut out: Vec<ControlPoint> = points
        .iter()
        .filter(|p| !ids.contains(&id(p)))
        .copied()
        .collect();
    out.extend(put);
    Some((sorted(out), ids))
}

fn by_start(mut expressions: Vec<NoteExpression>) -> Vec<NoteExpression> {
    expressions.sort_by_key(|e| e.tick);
    expressions
}

/// The selected notes with `kind` set along `path` (ticks from the clip's
/// start) on each note the path crosses.
pub fn expression_drawn(
    notes: &[MidiNote],
    selected: &BTreeSet<u32>,
    kind: ExpressionKind,
    path: &[(i64, f32)],
) -> Vec<MidiNote> {
    let (Some(lo), Some(hi)) = (
        path.iter().map(|p| p.0).min(),
        path.iter().map(|p| p.0).max(),
    ) else {
        return notes.to_vec();
    };
    let by_tick: BTreeMap<i64, f32> = path.iter().copied().collect();
    notes
        .iter()
        .map(|note| {
            if !selected.contains(&note.id) {
                return note.clone();
            }
            let inside = |at: i64| at >= note.start.0 && at < note.end().0;
            let mut expressions: Vec<NoteExpression> = note
                .expressions
                .iter()
                .filter(|e| !(e.kind == kind && (lo..=hi).contains(&(note.start.0 + e.tick.0))))
                .copied()
                .collect();
            expressions.extend(by_tick.iter().filter(|(t, _)| inside(**t)).map(|(t, v)| {
                NoteExpression {
                    tick: Tick(*t - note.start.0),
                    kind,
                    value: *v,
                    ramp: false,
                }
            }));
            MidiNote {
                expressions: by_start(expressions),
                ..note.clone()
            }
        })
        .collect()
}

/// The selected notes with a ramp of `kind` from `a` to `b` (ticks from the
/// clip's start), cut to each note.
pub fn expression_lined(
    notes: &[MidiNote],
    selected: &BTreeSet<u32>,
    kind: ExpressionKind,
    a: (i64, f32),
    b: (i64, f32),
) -> Vec<MidiNote> {
    let (a, b) = if a.0 <= b.0 { (a, b) } else { (b, a) };
    if a.0 == b.0 {
        return expression_drawn(notes, selected, kind, &[b]);
    }
    let at = |t: i64| a.1 + (b.1 - a.1) * (t - a.0) as f32 / (b.0 - a.0) as f32;
    notes
        .iter()
        .map(|note| {
            let (from, to) = (a.0.max(note.start.0), b.0.min(note.end().0 - 1));
            if !selected.contains(&note.id) || from > to {
                return note.clone();
            }
            let mut expressions: Vec<NoteExpression> = note
                .expressions
                .iter()
                .filter(|e| !(e.kind == kind && (from..=to).contains(&(note.start.0 + e.tick.0))))
                .copied()
                .collect();
            let point = |t: i64, ramp: bool| NoteExpression {
                tick: Tick(t - note.start.0),
                kind,
                value: at(t),
                ramp,
            };
            if from == to {
                expressions.push(point(to, false));
            } else {
                expressions.push(point(from, true));
                expressions.push(point(to, false));
            }
            MidiNote {
                expressions: by_start(expressions),
                ..note.clone()
            }
        })
        .collect()
}

// The toolbar.

/// The lane controls for the piano roll's toolbar.
pub fn controls(ui: &mut egui::Ui, lanes: &mut Lanes) {
    ui.separator();
    ui.label("Lanes:");
    for (tool, label) in [
        (Tool::Draw, "Draw"),
        (Tool::Line, "Line"),
        (Tool::Select, "Select"),
    ] {
        ui.selectable_value(&mut lanes.tool, tool, label);
    }
    ui.menu_button("+", |ui| {
        let common = [
            ControlKind::Controller(1),
            ControlKind::Controller(7),
            ControlKind::Controller(10),
            ControlKind::Controller(11),
            ControlKind::Controller(64),
            ControlKind::PitchBend,
            ControlKind::ChannelPressure,
        ];
        for kind in common {
            if ui.button(name(kind)).clicked() {
                lanes.add(kind);
                ui.close();
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut lanes.next_cc).range(0..=127));
            if ui.button("Add CC").clicked() {
                lanes.add(ControlKind::Controller(lanes.next_cc));
                ui.close();
            }
        });
    });
    ui.checkbox(&mut lanes.expressions, "Expression");
    if lanes.expressions {
        egui::ComboBox::from_id_salt("expression kind")
            .selected_text(expression_name(lanes.expression))
            .show_ui(ui, |ui| {
                for kind in [
                    ExpressionKind::Pitch,
                    ExpressionKind::Pressure,
                    ExpressionKind::Timbre,
                    ExpressionKind::Volume,
                    ExpressionKind::Pan,
                ] {
                    ui.selectable_value(&mut lanes.expression, kind, expression_name(kind));
                }
            });
    }
}

// The lanes.

/// What the lanes need to know about the roll.
pub struct Ctx<'a> {
    pub id: ClipId,
    pub clip: &'a Clip,
    pub midi: &'a MidiClip,
    pub axes: Axes,
    pub map: &'a TempoMap,
    pub snap: &'a dyn Fn(i64) -> i64,
    pub shift: bool,
    pub selected: &'a BTreeSet<u32>,
}

/// Draws the lanes and handles the pointer in them. `labels` is the column
/// to the left of the grid.
pub fn show(
    ui: &mut egui::Ui,
    lanes: &mut Lanes,
    out: &mut Output,
    ctx: &Ctx,
    layout: &Layout,
    rects: &(Vec<(ControlKind, Rect)>, Option<Rect>),
    labels: f32,
) {
    let whole = rects
        .0
        .iter()
        .map(|(_, r)| *r)
        .chain(rects.1)
        .reduce(|a, b| a.union(b));
    lanes.hover = whole.is_some_and(|w| {
        let w = Rect::from_min_max(Pos2::new(labels, w.top()), w.max);
        ui.rect_contains_pointer(w)
    });
    for (kind, rect) in &rects.0 {
        controller_lane(ui, lanes, out, ctx, *kind, *rect, labels);
    }
    if let Some(rect) = rects.1 {
        expression_lane(ui, lanes, out, ctx, rect, labels);
    }
    let _ = layout;
}

fn lane_background(ui: &egui::Ui, ctx: &Ctx, rect: Rect, labels: f32, title: &str) {
    let painter = ui.painter_at(Rect::from_min_max(Pos2::new(labels, rect.top()), rect.max));
    painter.rect_filled(rect, 0.0, colors::HEADER);
    painter.hline(
        rect.x_range(),
        rect.top(),
        Stroke::new(1.0, colors::BAR_LINE),
    );
    for (tick, _) in super::beat_lines(ctx.axes, ctx.clip, ctx.map) {
        let x = ctx.axes.x(tick);
        painter.vline(
            x,
            rect.y_range(),
            Stroke::new(1.0, colors::BAR_LINE.gamma_multiply(0.5)),
        );
    }
    let labels_painter = ui.painter_at(Rect::from_min_max(
        Pos2::new(labels - super::KEYBOARD, rect.top()),
        Pos2::new(labels, rect.bottom()),
    ));
    labels_painter.text(
        Pos2::new(labels - super::KEYBOARD + 4.0, rect.top() + 4.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(10.0),
        Color32::GRAY,
    );
}

/// Draws a value line through `points` (sorted by tick) holding or ramping
/// to each next one, and on to `end`.
fn draw_line(painter: &egui::Painter, points: &[(f32, f32, bool)], end: f32, stroke: Stroke) {
    for (i, (x, y, ramp)) in points.iter().enumerate() {
        match points.get(i + 1) {
            Some((nx, ny, _)) if *ramp => {
                painter.line_segment([Pos2::new(*x, *y), Pos2::new(*nx, *ny)], stroke);
            }
            Some((nx, ny, _)) => {
                painter.line_segment([Pos2::new(*x, *y), Pos2::new(*nx, *y)], stroke);
                painter.line_segment([Pos2::new(*nx, *y), Pos2::new(*nx, *ny)], stroke);
            }
            None => {
                painter.line_segment([Pos2::new(*x, *y), Pos2::new(end.max(*x), *y)], stroke);
            }
        }
    }
}

fn controller_lane(
    ui: &mut egui::Ui,
    lanes: &mut Lanes,
    out: &mut Output,
    ctx: &Ctx,
    kind: ControlKind,
    rect: Rect,
    labels: f32,
) {
    let Ctx {
        axes, midi, clip, ..
    } = ctx;
    let length = midi.length.0;
    let range = range(kind);
    lane_background(ui, ctx, rect, labels, &name(kind));
    let in_lane: Vec<&ControlPoint> = midi.controls.iter().filter(|p| p.kind == kind).collect();
    let channel = in_lane.first().map_or(0, |p| p.channel);
    let painter = ui.painter_at(Rect::from_min_max(Pos2::new(labels, rect.top()), rect.max));

    // A lane the clip has no points in can be taken away again.
    if in_lane.is_empty() {
        let close =
            Rect::from_min_size(Pos2::new(labels - 16.0, rect.top() + 2.0), vec2(14.0, 14.0));
        let r = ui.interact(close, ui.id().with(("close lane", kind)), Sense::click());
        ui.painter().text(
            close.center(),
            Align2::CENTER_CENTER,
            "×",
            FontId::proportional(12.0),
            Color32::GRAY,
        );
        if r.clicked() {
            lanes.extra.retain(|k| *k != kind);
        }
    }

    let mut channels: BTreeMap<u8, Vec<(f32, f32, bool)>> = BTreeMap::new();
    let mut sorted_points = in_lane.clone();
    sorted_points.sort_by_key(|p| (p.channel, p.tick));
    for p in &sorted_points {
        channels.entry(p.channel).or_default().push((
            axes.x(p.tick.0),
            value_y(rect, range, p.value),
            p.ramp,
        ));
    }
    let stroke = Stroke::new(1.5, theme::ACCENT);
    for line in channels.values() {
        draw_line(&painter, line, axes.x(length), stroke);
    }
    for p in &in_lane {
        let colour = if lanes.picked.contains(&id(p)) {
            colors::SELECTED
        } else {
            theme::ACCENT
        };
        painter.circle_filled(
            Pos2::new(axes.x(p.tick.0), value_y(rect, range, p.value)),
            3.0,
            colour,
        );
    }

    let response = ui.interact(
        Rect::from_min_max(Pos2::new(labels, rect.top()), rect.max),
        ui.id().with(("lane", kind)),
        Sense::click_and_drag(),
    );
    let to_point = |pos: Pos2| {
        (
            (ctx.snap)(axes.tick(pos.x)).clamp(0, length - 1),
            quantised(kind, value_at(rect, range, pos.y)),
        )
    };
    let point_at = |pos: Pos2| {
        in_lane
            .iter()
            .map(|p| {
                let at = Pos2::new(axes.x(p.tick.0), value_y(rect, range, p.value));
                (**p, at.distance(pos))
            })
            .filter(|(_, d)| *d <= REACH)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(p, _)| p)
    };
    let shift = ctx.shift;

    if response.drag_started()
        && let Some(press) = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos())
    {
        let gesture = match lanes.tool {
            Tool::Draw => Some(Gesture::Draw {
                path: vec![to_point(press)],
            }),
            Tool::Line => Some(Gesture::Line {
                from: to_point(press),
            }),
            Tool::Select => Some(match point_at(press) {
                Some(p) => {
                    if !lanes.picked.contains(&id(&p)) && !shift {
                        lanes.picked.clear();
                    }
                    lanes.picked.insert(id(&p));
                    Gesture::Move {
                        grabbed: p.tick.0,
                        press_value: value_at(rect, range, press.y),
                        picked: lanes.picked.clone(),
                    }
                }
                None => {
                    let base = if shift {
                        lanes.picked.clone()
                    } else {
                        BTreeSet::new()
                    };
                    lanes.picked = base.clone();
                    Gesture::Marquee { from: press, base }
                }
            }),
        };
        lanes.drag = gesture.map(|gesture| LaneDrag {
            gesture,
            target: Some(kind),
            channel,
            controls: midi.controls.clone(),
            notes: Vec::new(),
        });
    } else if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        match lanes.tool {
            Tool::Draw | Tool::Line => {
                let changed = drawn(&midi.controls, channel, kind, &[to_point(pos)]);
                out.edits.push(set_controls(ctx.id, clip, changed, false));
            }
            Tool::Select => match point_at(pos) {
                Some(p) if shift => {
                    if !lanes.picked.remove(&id(&p)) {
                        lanes.picked.insert(id(&p));
                    }
                }
                Some(p) => lanes.picked = BTreeSet::from([id(&p)]),
                None => lanes.picked.clear(),
            },
        }
    }

    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(drag) = &mut lanes.drag
        && drag.target == Some(kind)
    {
        let mut picked_now = None;
        let changed = match &mut drag.gesture {
            Gesture::Draw { path } => {
                let (tick, value) = to_point(pos);
                match path.last_mut() {
                    Some(last) if last.0 == tick => last.1 = value,
                    _ => path.push((tick, value)),
                }
                Some(drawn(&drag.controls, drag.channel, kind, path))
            }
            Gesture::Line { from } => Some(lined(
                &drag.controls,
                drag.channel,
                kind,
                *from,
                to_point(pos),
            )),
            Gesture::Move {
                grabbed,
                press_value,
                picked,
            } => {
                let raw = axes.tick(pos.x);
                let d_tick = (ctx.snap)(raw).clamp(i64::MIN / 2, i64::MAX / 2) - *grabbed;
                let d_value = value_at(rect, range, pos.y) - *press_value;
                let (points, ids) = moved(&drag.controls, picked, d_tick, kind, d_value, length);
                picked_now = Some(ids);
                Some(points)
            }
            Gesture::Marquee { from, base } => {
                let area = Rect::from_two_pos(*from, pos);
                let mut picked = base.clone();
                picked.extend(
                    drag.controls
                        .iter()
                        .filter(|p| p.kind == kind)
                        .filter(|p| {
                            area.contains(Pos2::new(
                                axes.x(p.tick.0),
                                value_y(rect, range, p.value),
                            ))
                        })
                        .map(id),
                );
                lanes.picked = picked;
                ui.painter().rect_stroke(
                    area,
                    0.0,
                    Stroke::new(1.0, colors::SELECTED),
                    StrokeKind::Inside,
                );
                None
            }
        };
        if let Some(ids) = picked_now {
            lanes.picked = ids;
        }
        if let Some(changed) = changed
            && changed != midi.controls
        {
            out.edits.push(set_controls(ctx.id, clip, changed, true));
        }
    }
    if response.drag_stopped()
        && let Some(drag) = lanes.drag.take_if(|d| d.target == Some(kind))
        && !matches!(drag.gesture, Gesture::Marquee { .. })
    {
        out.edits.push(Edit::EndDrag);
    }
}

fn expression_lane(
    ui: &mut egui::Ui,
    lanes: &mut Lanes,
    out: &mut Output,
    ctx: &Ctx,
    rect: Rect,
    labels: f32,
) {
    let Ctx {
        axes, midi, clip, ..
    } = ctx;
    let kind = lanes.expression;
    let range = expression_range(kind);
    lane_background(
        ui,
        ctx,
        rect,
        labels,
        &format!("Note {}", expression_name(kind).to_lowercase()),
    );
    let painter = ui.painter_at(Rect::from_min_max(Pos2::new(labels, rect.top()), rect.max));
    let chosen: Vec<&MidiNote> = midi
        .notes
        .iter()
        .filter(|n| ctx.selected.contains(&n.id))
        .collect();
    if chosen.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Select notes to edit their expression",
            FontId::proportional(11.0),
            Color32::GRAY,
        );
    }
    for note in &chosen {
        let span = Rect::from_min_max(
            Pos2::new(axes.x(note.start.0), rect.top() + 1.0),
            Pos2::new(axes.x(note.end().0), rect.bottom()),
        );
        painter.rect_filled(span, 0.0, theme::ACCENT.gamma_multiply(0.12));
        let mut along: Vec<&NoteExpression> =
            note.expressions.iter().filter(|e| e.kind == kind).collect();
        along.sort_by_key(|e| e.tick);
        let points: Vec<(f32, f32, bool)> = along
            .iter()
            .map(|e| {
                (
                    axes.x(note.start.0 + e.tick.0),
                    value_y(rect, range, e.value),
                    e.ramp,
                )
            })
            .collect();
        draw_line(
            &painter,
            &points,
            axes.x(note.end().0),
            Stroke::new(1.5, theme::ACCENT),
        );
        for (x, y, _) in &points {
            painter.circle_filled(Pos2::new(*x, *y), 3.0, theme::ACCENT);
        }
    }

    let response = ui.interact(
        Rect::from_min_max(Pos2::new(labels, rect.top()), rect.max),
        ui.id().with("expression lane"),
        Sense::click_and_drag(),
    );
    let length = midi.length.0;
    let to_point = |pos: Pos2| {
        (
            (ctx.snap)(axes.tick(pos.x)).clamp(0, length - 1),
            value_at(rect, range, pos.y),
        )
    };
    if chosen.is_empty() {
        return;
    }
    if response.drag_started()
        && let Some(press) = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos())
    {
        let gesture = if lanes.tool == Tool::Line {
            Gesture::Line {
                from: to_point(press),
            }
        } else {
            Gesture::Draw {
                path: vec![to_point(press)],
            }
        };
        lanes.drag = Some(LaneDrag {
            gesture,
            target: None,
            channel: 0,
            controls: Vec::new(),
            notes: midi.notes.clone(),
        });
    } else if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let changed = expression_drawn(&midi.notes, ctx.selected, kind, &[to_point(pos)]);
        out.edits.push(set_notes(ctx.id, clip, changed, false));
    }
    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(drag) = &mut lanes.drag
        && drag.target.is_none()
    {
        let changed = match &mut drag.gesture {
            Gesture::Draw { path } => {
                let (tick, value) = to_point(pos);
                match path.last_mut() {
                    Some(last) if last.0 == tick => last.1 = value,
                    _ => path.push((tick, value)),
                }
                expression_drawn(&drag.notes, ctx.selected, kind, path)
            }
            Gesture::Line { from } => {
                expression_lined(&drag.notes, ctx.selected, kind, *from, to_point(pos))
            }
            _ => return,
        };
        if changed != midi.notes {
            out.edits.push(set_notes(ctx.id, clip, changed, true));
        }
    }
    if response.drag_stopped() && lanes.drag.take_if(|d| d.target.is_none()).is_some() {
        out.edits.push(Edit::EndDrag);
    }
}

/// Delete, for the picked points.
pub fn delete(lanes: &mut Lanes, out: &mut Output, id: ClipId, clip: &Clip, midi: &MidiClip) {
    let changed = removed(&midi.controls, &lanes.picked);
    lanes.picked.clear();
    if changed != midi.controls {
        out.edits.push(set_controls(id, clip, changed, false));
    }
}

/// Copy, cut, paste and duplicate for points. Returns whether it did the
/// action: copying and the like only with points picked, pasting only with
/// points on the clipboard.
#[allow(clippy::too_many_arguments)]
pub fn clipboard(
    action: ClipAction,
    lanes: &mut Lanes,
    step: i64,
    out: &mut Output,
    id: ClipId,
    clip: &Clip,
    midi: &MidiClip,
    playhead: i64,
) -> bool {
    use crate::clipboard::{CONTROL_MIME, copy, paste};
    let held = copied(&midi.controls, &lanes.picked);
    let summary = || {
        let n = held.len();
        format!("{n} controller point{}", if n == 1 { "" } else { "s" })
    };
    let mut put = |lanes: &mut Lanes, placed: Option<(Vec<ControlPoint>, BTreeSet<PointId>)>| {
        if let Some((changed, ids)) = placed {
            lanes.picked = ids;
            out.edits.push(set_controls(id, clip, changed, false));
        }
    };
    match action {
        ClipAction::Copy | ClipAction::Cut if !held.is_empty() => {
            copy(CONTROL_MIME, &held, &summary());
            if action == ClipAction::Cut {
                let changed = removed(&midi.controls, &lanes.picked);
                lanes.picked.clear();
                out.edits.push(set_controls(id, clip, changed, false));
            }
            true
        }
        ClipAction::Duplicate if !held.is_empty() => {
            let earliest = held.first().map_or(0, |_| {
                midi.controls
                    .iter()
                    .filter(|p| lanes.picked.contains(&super::lanes::id(p)))
                    .map(|p| p.tick.0)
                    .min()
                    .unwrap_or(0)
            });
            let span = held.iter().map(|p| p.tick.0).max().unwrap_or(0);
            let placed = pasted(
                &midi.controls,
                &held,
                earliest + span + step.max(1),
                midi.length.0,
            );
            put(lanes, placed);
            true
        }
        ClipAction::Paste => {
            let Some(held) = paste::<Vec<ControlPoint>>(CONTROL_MIME) else {
                return false;
            };
            let placed = pasted(&midi.controls, &held, playhead, midi.length.0);
            put(lanes, placed);
            true
        }
        _ => false,
    }
}
