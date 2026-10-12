//! The session's tempo as an optional lane under the ruler: each tempo change
//! is a point on a stepped line, and the line holds a tempo until the next
//! point. The lane is the tempo map; with one point there is a single tempo
//! (set in the transport), and each point added makes a change.
//!
//! Click an empty spot to add a point, drag a point to move it (the first
//! stays at the start and only changes tempo), and right-click it, or press
//! Delete with it selected, to remove it. Each is one undo step.

use egui::{Align2, FontId, Key, PointerButton, Pos2, Rect, Sense, Stroke, vec2};
use noodle_core::{Command, TempoMap, Tick};

use super::{Axis, grid};
use crate::session::Edit;
use crate::tempo;
use crate::theme::timeline as colors;

/// The lane's height.
pub const HEIGHT: f32 = 52.0;
/// Space above and below the curve's extremes.
const PAD: f32 = 8.0;
/// How near a press has to be to grab a point.
const GRAB: f32 = 8.0;
/// The tempos the lane shows at least; a tempo outside them widens it.
const SHOWN: (f64, f64) = (60.0, 180.0);

/// What the user is doing to which point.
#[derive(Default)]
pub struct State {
    selected: Option<usize>,
    drag: Option<Drag>,
    /// Where each point was drawn last frame, for tests to aim at.
    #[cfg(test)]
    points: Vec<Pos2>,
    #[cfg(test)]
    row: Option<Rect>,
}

struct Drag {
    index: usize,
    /// The map when the drag began.
    original: TempoMap,
    /// The tempos the lane showed then, kept for the whole drag so the line
    /// doesn't rescale under the pointer.
    range: (f64, f64),
}

impl State {
    /// Forgets a point that is gone, e.g. after an undo.
    pub fn retain_existing(&mut self, map: &TempoMap) {
        if self.selected.is_some_and(|i| i >= map.tempos().len()) {
            self.selected = None;
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Lets go of the picked point.
    pub fn deselect(&mut self) {
        self.selected = None;
    }

    #[cfg(test)]
    pub fn points(&self) -> &[Pos2] {
        &self.points
    }

    #[cfg(test)]
    pub fn row(&self) -> Option<Rect> {
        self.row
    }
}

/// The tempos the lane spans for `map`: the usual range, wider if a tempo
/// needs it.
fn range_of(map: &TempoMap) -> (f64, f64) {
    let lowest = map.tempos().iter().map(|t| t.bpm).fold(f64::MAX, f64::min);
    let highest = map.tempos().iter().map(|t| t.bpm).fold(f64::MIN, f64::max);
    (
        SHOWN.0.min((lowest / 10.0).floor() * 10.0),
        SHOWN.1.max((highest / 10.0).ceil() * 10.0),
    )
}

fn y_of(row: Rect, range: (f64, f64), bpm: f64) -> f32 {
    let along = ((bpm - range.0) / (range.1 - range.0)).clamp(0.0, 1.0) as f32;
    row.bottom() - PAD - along * (row.height() - 2.0 * PAD)
}

/// The tempo at screen height `y`, to a tenth of a beat per minute.
fn bpm_at(row: Rect, range: (f64, f64), y: f32) -> f64 {
    let along = f64::from(((row.bottom() - PAD - y) / (row.height() - 2.0 * PAD)).clamp(0.0, 1.0));
    let bpm = range.0 + along * (range.1 - range.0);
    ((bpm * 10.0).round() / 10.0).clamp(TempoMap::MIN_BPM, TempoMap::MAX_BPM)
}

/// Draws the lane in `row` (of which `visible` is on the screen) and handles
/// the pointer on it.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut State,
    map: &TempoMap,
    row: Rect,
    visible: Rect,
    axis: Axis,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    state.retain_existing(map);
    let range = state
        .drag
        .as_ref()
        .map_or_else(|| range_of(map), |d| d.range);
    let screen: Vec<Pos2> = map
        .tempos()
        .iter()
        .map(|t| Pos2::new(axis.x(t.tick), y_of(row, range, t.bpm)))
        .collect();
    #[cfg(test)]
    {
        state.points = screen.clone();
        state.row = Some(visible);
    }
    if visible.width() <= 0.0 || visible.height() <= 0.0 {
        return edits;
    }
    let response = ui.interact(visible, ui.id().with("tempo lane"), Sense::click_and_drag());
    let near = |pos: Pos2| {
        screen
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.distance(pos)))
            .filter(|&(_, d)| d <= GRAB)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let free = ui.input(|i| i.modifiers.alt);
    let snap = |x: f32| {
        let tick = axis.tick(x).max(Tick::ZERO);
        if free { tick } else { grid::snap(map, tick) }
    };

    if response.drag_started_by(PointerButton::Primary) {
        let press = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos());
        if let Some(i) = press.and_then(near) {
            state.selected = Some(i);
            state.drag = Some(Drag {
                index: i,
                original: map.clone(),
                range: range_of(map),
            });
        }
    }
    if response.dragged()
        && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
        && let Some(moved) = tempo::moved(
            &drag.original,
            drag.index,
            snap(pos.x),
            bpm_at(row, drag.range, pos.y),
        )
        && moved != *map
    {
        edits.push(Edit::Drag(Command::SetTempoMap(moved)));
    }
    if response.drag_stopped() && state.drag.take().is_some() {
        edits.push(Edit::EndDrag);
    }
    if response.clicked_by(PointerButton::Primary)
        && let Some(pos) = response.interact_pointer_pos()
    {
        match near(pos) {
            Some(i) => state.selected = Some(i),
            None => {
                if let Some((new, at)) =
                    tempo::with_change(map, snap(pos.x), bpm_at(row, range, pos.y))
                {
                    state.selected = Some(at);
                    edits.push(Edit::Apply(Command::SetTempoMap(new)));
                }
            }
        }
    }
    let delete = if response.secondary_clicked() {
        response.interact_pointer_pos().and_then(near)
    } else if response.hovered()
        && ui.ctx().memory(|m| m.focused()).is_none()
        && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace))
    {
        state.selected
    } else {
        None
    };
    if let Some(new) = delete.and_then(|i| tempo::removed(map, i)) {
        state.selected = None;
        edits.push(Edit::Apply(Command::SetTempoMap(new)));
    }
    // A drag whose widget never saw its release would leave the undo group
    // open.
    if state.drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
        state.drag = None;
        if !edits.contains(&Edit::EndDrag) {
            edits.push(Edit::EndDrag);
        }
    }
    paint(ui, row, visible, map, &screen, state.selected);
    edits
}

fn paint(
    ui: &egui::Ui,
    row: Rect,
    visible: Rect,
    map: &TempoMap,
    screen: &[Pos2],
    selected: Option<usize>,
) {
    let painter = ui.painter_at(visible);
    painter.rect_filled(row, 0.0, colors::AUTOMATION);
    painter.hline(
        row.x_range(),
        row.bottom() - 0.5,
        Stroke::new(1.0, colors::LANE_EVEN),
    );
    let line = colors::AUTOMATION_LINE;
    // A stepped line: each tempo holds until the next change, and the last
    // goes on for ever.
    let mut shape = Vec::new();
    for (i, &p) in screen.iter().enumerate() {
        let from = if i == 0 { row.left().min(p.x) } else { p.x };
        shape.push(Pos2::new(from, p.y));
        match screen.get(i + 1) {
            Some(next) => shape.push(Pos2::new(next.x, p.y)),
            None => shape.push(Pos2::new(row.right().max(p.x), p.y)),
        }
    }
    painter.add(egui::Shape::line(shape, Stroke::new(1.5, line)));
    for (i, (&p, change)) in screen.iter().zip(map.tempos()).enumerate() {
        let on = selected == Some(i);
        painter.circle_filled(
            p,
            if on { 5.0 } else { 3.5 },
            if on { colors::SELECTED } else { line },
        );
        painter.text(
            p + vec2(6.0, -4.0),
            Align2::LEFT_BOTTOM,
            format_bpm(change.bpm),
            FontId::proportional(10.0),
            colors::TEXT_WEAK,
        );
    }
}

/// A tempo as the lane labels it.
pub fn format_bpm(bpm: f64) -> String {
    if (bpm - bpm.round()).abs() < 0.005 {
        format!("{}", bpm.round() as i64)
    } else {
        format!("{bpm:.1}")
    }
}
