//! Automation lanes in the arrangement: under each track, a row for every
//! lane that drives one of its group's boundary nodes (gain and mute) or one
//! of its group's Automation nodes, with the lane's points drawn as a curve.
//! Lanes on an Automation node outside any track's group get rows of their
//! own after the last track: free automation tracks, whose node's output is
//! wired wherever the signal is wanted.
//!
//! Click an empty spot to add a point, drag a point to move it, and
//! right-click it (or press Delete with it selected) to remove it. Each is
//! one undo step. A track's header has a menu to add a gain or mute lane, and
//! each lane's header has a button to remove it. A lane on solo can't work,
//! because solo is read when the project is compiled; it is drawn greyed out
//! and says so.

use egui::{Align2, FontId, Key, PointerButton, Pos2, Rect, Sense, Stroke, vec2};
use noodle_core::group::{self, Controls};
use noodle_core::{
    AutomationLane, AutomationPoint, Command, Curve, Endpoint, LaneId, NodeId, Project, Tick,
};

use super::Axis;
use crate::session::Edit;
use crate::theme::timeline as colors;

/// A lane row's height.
pub const HEIGHT: f32 = 44.0;
/// Space above and below the curve's extremes.
const PAD: f32 = 8.0;
/// How near a press has to be to grab a point.
const GRAB: f32 = 8.0;
/// What a lane on solo says.
const REFUSED: &str = "Solo can't be automated";
/// The gain range, matching the track headers' slider.
const GAIN_RANGE: (f32, f32) = (-60.0, 24.0);

/// What the user is doing to which point.
#[derive(Default)]
pub struct State {
    /// The selected point: a lane and an index into its points.
    selected: Option<(LaneId, usize)>,
    drag: Option<PointDrag>,
    /// Where each point was drawn last frame, for tests to aim at.
    #[cfg(test)]
    points: std::collections::BTreeMap<(LaneId, usize), Pos2>,
    /// The text drawn on the rows last frame; painted text isn't in the
    /// accessibility tree.
    #[cfg(test)]
    texts: Vec<String>,
    /// Where each lane row was drawn last frame.
    #[cfg(test)]
    rows: std::collections::BTreeMap<LaneId, Rect>,
}

struct PointDrag {
    lane: LaneId,
    index: usize,
    /// The lane as it was when the drag began.
    original: AutomationLane,
}

impl State {
    /// Forgets points that no longer exist, e.g. after an undo.
    pub fn retain_existing(&mut self, project: &Project) {
        let exists = |lane: LaneId, index: usize| {
            project
                .lane(lane)
                .is_some_and(|lane| index < lane.points.len())
        };
        if self.selected.is_some_and(|(lane, i)| !exists(lane, i)) {
            self.selected = None;
        }
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| project.lane(drag.lane).is_none())
        {
            self.drag = None;
        }
    }

    #[cfg(test)]
    pub fn point_at(&self, lane: LaneId, index: usize) -> Option<Pos2> {
        self.points.get(&(lane, index)).copied()
    }

    #[cfg(test)]
    pub fn row(&self, lane: LaneId) -> Option<Rect> {
        self.rows.get(&lane).copied()
    }

    #[cfg(test)]
    pub fn texts(&self) -> &[String] {
        &self.texts
    }

    pub fn deselect(&mut self) {
        self.selected = None;
    }

    pub fn selected(&self) -> Option<(LaneId, usize)> {
        self.selected
    }
}

/// Whether `node` is an Automation node: a free control signal drawn on the
/// timeline.
pub(super) fn is_automation_node(project: &Project, node: NodeId) -> bool {
    project
        .graph()
        .node(node)
        .is_some_and(|n| n.type_id == noodle_nodes::AUTOMATION_NODE_ID)
}

/// The lanes that belong to a track: those driving a boundary node, the
/// track input (its gain and mute) or an Automation node of the group its track input sits in. Empty for a track
/// outside any group.
pub fn lanes_for(project: &Project, input: NodeId) -> Vec<LaneId> {
    let graph = project.graph();
    let Some(group) = graph.node(input).and_then(|node| node.parent) else {
        return Vec::new();
    };
    project
        .lanes()
        .filter(|(_, lane)| {
            graph.node(lane.target.node).is_some_and(|node| {
                node.parent == Some(group)
                    && matches!(
                        node.type_id.as_str(),
                        group::GROUP_INPUT
                            | group::GROUP_OUTPUT
                            | group::TRACK_INPUT
                            | noodle_nodes::AUTOMATION_NODE_ID
                    )
            })
        })
        .map(|(id, _)| id)
        .collect()
}

/// Where each track's block starts, in points below the top of the lanes:
/// the track's own lane, then a row for each of its automation lanes.
pub struct Rows {
    tops: Vec<f32>,
    /// Each track's own lane height.
    heights: Vec<f32>,
    lanes: Vec<Vec<LaneId>>,
    /// Lanes on Automation nodes that no track claims, after the last track.
    free: Vec<LaneId>,
    free_top: f32,
    total: f32,
}

impl Rows {
    /// `heights` is each track's own lane height, parallel to `tracks`.
    pub fn new(project: &Project, tracks: &[NodeId], heights: &[f32]) -> Self {
        let mut tops = Vec::with_capacity(tracks.len());
        let mut lanes = Vec::with_capacity(tracks.len());
        let mut at = 0.0;
        for (&track, &height) in tracks.iter().zip(heights) {
            let own = lanes_for(project, track);
            tops.push(at);
            at += height + own.len() as f32 * HEIGHT;
            lanes.push(own);
        }
        let claimed: std::collections::BTreeSet<LaneId> = lanes.iter().flatten().copied().collect();
        let free: Vec<LaneId> = project
            .lanes()
            .filter(|(id, lane)| {
                !claimed.contains(id) && is_automation_node(project, lane.target.node)
            })
            .map(|(id, _)| id)
            .collect();
        let free_top = at;
        at += free.len() as f32 * HEIGHT;
        Self {
            tops,
            heights: heights.to_vec(),
            lanes,
            free,
            free_top,
            total: at,
        }
    }

    /// The free automation lanes, top to bottom.
    pub fn free_lanes(&self) -> &[LaneId] {
        &self.free
    }

    /// Where the row for the `k`th free automation lane begins.
    pub fn free_lane_top(&self, k: usize) -> f32 {
        self.free_top + k as f32 * HEIGHT
    }

    /// Every lane row with the point it begins at, top to bottom.
    fn placed(&self) -> Vec<(LaneId, f32)> {
        let mut placed = Vec::new();
        for (index, lanes) in self.lanes.iter().enumerate() {
            for (k, &id) in lanes.iter().enumerate() {
                placed.push((id, self.lane_top(index, k)));
            }
        }
        placed.extend(
            self.free
                .iter()
                .enumerate()
                .map(|(k, &id)| (id, self.free_lane_top(k))),
        );
        placed
    }

    /// Where track `index`'s block begins.
    pub fn top(&self, index: usize) -> f32 {
        self.tops[index]
    }

    /// The height of track `index`'s block, automation included.
    pub fn height(&self, index: usize) -> f32 {
        self.heights[index] + self.lanes[index].len() as f32 * HEIGHT
    }

    /// The height of track `index`'s own lane, without its automation.
    pub fn lane_height(&self, index: usize) -> f32 {
        self.heights[index]
    }

    /// The height of everything.
    pub fn total(&self) -> f32 {
        self.total
    }

    /// Track `index`'s automation lanes, top to bottom.
    pub fn lanes(&self, index: usize) -> &[LaneId] {
        &self.lanes[index]
    }

    /// Where the row for the `k`th automation lane of track `index` begins.
    pub fn lane_top(&self, index: usize, k: usize) -> f32 {
        self.tops[index] + self.heights[index] + k as f32 * HEIGHT
    }

    /// The track whose block contains `y`, in points below the top of the
    /// lanes. Past the ends it is the first or last.
    pub fn track_at(&self, y: f32) -> usize {
        self.tops
            .iter()
            .rposition(|&top| top <= y)
            .unwrap_or(0)
            .min(self.tops.len().saturating_sub(1))
    }
}

/// The control ([`group::GAIN`], [`group::MUTE`] or [`group::SOLO`]) that a
/// lane's target key drives.
fn control_of(key: &str) -> &str {
    group::parse_control_key(key).map_or(key, |(control, _)| control)
}

/// What a lane's value means on screen.
fn range(key: &str) -> (f32, f32) {
    if control_of(key) == group::GAIN {
        GAIN_RANGE
    } else {
        (0.0, 1.0)
    }
}

/// Switches (mute, solo) hold their value and snap to off or on; gain and
/// an Automation node's value slide.
fn is_switch(key: &str) -> bool {
    matches!(control_of(key), group::MUTE | group::SOLO)
}

fn name(key: &str) -> &str {
    match control_of(key) {
        group::GAIN => "Gain",
        group::MUTE => "Mute",
        group::SOLO => "Solo",
        noodle_nodes::AUTOMATION_VALUE => "Automation",
        other => other,
    }
}

/// What a lane's header calls it: an Automation node's lane is numbered by
/// the order of the Automation nodes, so two of them can be told apart.
pub fn label(project: &Project, lane: &AutomationLane) -> String {
    if !is_automation_node(project, lane.target.node) {
        return format!("{} lane", name(&lane.target.port));
    }
    let number = project
        .graph()
        .nodes()
        .filter(|(_, node)| node.type_id == noodle_nodes::AUTOMATION_NODE_ID)
        .position(|(id, _)| id == lane.target.node)
        .map_or(0, |i| i + 1);
    format!("Automation {number}")
}

fn y_of(row: Rect, key: &str, value: f32) -> f32 {
    let (low, high) = range(key);
    let along = ((value - low) / (high - low)).clamp(0.0, 1.0);
    row.bottom() - PAD - along * (row.height() - 2.0 * PAD)
}

fn value_at(row: Rect, key: &str, y: f32) -> f32 {
    let (low, high) = range(key);
    let along = ((row.bottom() - PAD - y) / (row.height() - 2.0 * PAD)).clamp(0.0, 1.0);
    let value = low + along * (high - low);
    if is_switch(key) {
        f32::from(value >= 0.5)
    } else {
        value
    }
}

/// Whether the compiler will refuse the lane: solo is read when the project
/// is compiled, so a lane on it never does anything.
pub fn refused(lane: &AutomationLane) -> bool {
    control_of(&lane.target.port) == group::SOLO
}

/// Everything the lanes need to know about where they are drawn.
pub struct View<'a> {
    pub project: &'a Project,
    pub rows: &'a Rows,
    /// The area the lanes are drawn in.
    pub content: Rect,
    pub scroll_y: f32,
    pub axis: Axis<'a>,
}

/// Draws every lane row and handles the pointer on them.
pub fn show(ui: &mut egui::Ui, state: &mut State, view: &View<'_>) -> Vec<Edit> {
    let mut edits = Vec::new();
    #[cfg(test)]
    {
        state.points.clear();
        state.rows.clear();
        state.texts.clear();
    }
    {
        for (id, lane_top) in view.rows.placed() {
            let Some(lane) = view.project.lane(id) else {
                continue;
            };
            let top = view.content.top() - view.scroll_y + lane_top;
            let row = Rect::from_min_size(
                Pos2::new(view.content.left(), top),
                vec2(view.content.width(), HEIGHT),
            );
            let visible = row.intersect(view.content);
            if visible.width() <= 0.0 || visible.height() <= 0.0 {
                continue;
            }
            let key = lane.target.port.as_str();
            let dimmed = refused(lane);
            let screen: Vec<Pos2> = lane
                .points
                .iter()
                .map(|p| Pos2::new(view.axis.x(p.tick), y_of(row, key, p.value)))
                .collect();
            #[cfg(test)]
            {
                state.rows.insert(id, visible);
                for (i, &p) in screen.iter().enumerate() {
                    state.points.insert((id, i), p);
                }
            }

            let response =
                ui.interact(visible, ui.id().with(("lane", id)), Sense::click_and_drag());
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
                let tick = view.axis.tick(x).max(Tick::ZERO);
                if free { tick } else { view.axis.snap(tick) }
            };

            if response.drag_started_by(PointerButton::Primary) {
                let press = ui
                    .input(|i| i.pointer.press_origin())
                    .or(response.interact_pointer_pos());
                if let Some(i) = press.and_then(near) {
                    state.selected = Some((id, i));
                    state.drag = Some(PointDrag {
                        lane: id,
                        index: i,
                        original: lane.clone(),
                    });
                }
            }
            if response.dragged()
                && let (Some(drag), Some(pos)) = (&state.drag, response.interact_pointer_pos())
                && drag.lane == id
            {
                let moved = moved_point(
                    &drag.original,
                    drag.index,
                    snap(pos.x),
                    value_at(row, key, pos.y),
                );
                if let Some(moved) = moved
                    && moved != *lane
                {
                    edits.push(Edit::Drag(Command::SetLane { id, lane: moved }));
                }
            }
            if response.drag_stopped() && state.drag.as_ref().is_some_and(|d| d.lane == id) {
                state.drag = None;
                edits.push(Edit::EndDrag);
            }
            if response.clicked_by(PointerButton::Primary)
                && let Some(pos) = response.interact_pointer_pos()
            {
                match near(pos) {
                    Some(i) => state.selected = Some((id, i)),
                    None => {
                        let point = AutomationPoint {
                            tick: snap(pos.x),
                            value: value_at(row, key, pos.y),
                            curve: if is_switch(key) {
                                Curve::Hold
                            } else {
                                Curve::Linear
                            },
                        };
                        let (lane, at) = with_point(lane, point);
                        state.selected = Some((id, at));
                        edits.push(Edit::Apply(Command::SetLane { id, lane }));
                    }
                }
            }
            let delete = if response.secondary_clicked() {
                response.interact_pointer_pos().and_then(near)
            } else if response.hovered()
                && ui.ctx().memory(|m| m.focused()).is_none()
                && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace))
            {
                state.selected.filter(|&(l, _)| l == id).map(|(_, i)| i)
            } else {
                None
            };
            if let Some(i) = delete.filter(|&i| i < lane.points.len()) {
                let mut lane = lane.clone();
                lane.points.remove(i);
                state.selected = None;
                edits.push(Edit::Apply(Command::SetLane { id, lane }));
            }

            let selected = state.selected.filter(|&(l, _)| l == id).map(|(_, i)| i);
            #[cfg(test)]
            if dimmed {
                state.texts.push(REFUSED.into());
            }
            paint(ui, row, visible, lane, &screen, selected, dimmed);
        }
    }
    // A drag whose widget never saw its release would leave the undo group
    // open.
    if state.drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
        state.drag = None;
        if !edits.contains(&Edit::EndDrag) {
            edits.push(Edit::EndDrag);
        }
    }
    edits
}

fn paint(
    ui: &egui::Ui,
    row: Rect,
    visible: Rect,
    lane: &AutomationLane,
    screen: &[Pos2],
    selected: Option<usize>,
    dimmed: bool,
) {
    let painter = ui.painter_at(visible);
    painter.rect_filled(row, 0.0, colors::AUTOMATION);
    painter.hline(
        row.x_range(),
        row.bottom() - 0.5,
        Stroke::new(1.0, colors::LANE_EVEN),
    );
    let line = if dimmed {
        colors::TEXT_WEAK
    } else {
        colors::AUTOMATION_LINE
    };
    let stroke = Stroke::new(1.5, line);
    let mut shape = Vec::new();
    // Before the first point the lane holds its first value, and after the
    // last it holds the last.
    if let Some(first) = screen.first() {
        shape.push(Pos2::new(row.left(), first.y));
    }
    for (i, &p) in screen.iter().enumerate() {
        let hold = lane.points[i].curve == Curve::Hold;
        if hold && let Some(next) = screen.get(i + 1) {
            shape.push(p);
            shape.push(Pos2::new(next.x, p.y));
        } else {
            shape.push(p);
        }
    }
    if let Some(last) = screen.last() {
        shape.push(Pos2::new(row.right(), last.y));
    }
    painter.add(egui::Shape::line(shape, stroke));
    for (i, &p) in screen.iter().enumerate() {
        let on = selected == Some(i);
        painter.circle_filled(
            p,
            if on { 5.0 } else { 3.5 },
            if on { colors::SELECTED } else { line },
        );
    }
    if dimmed {
        painter.text(
            row.left_top() + vec2(8.0, 4.0),
            Align2::LEFT_TOP,
            REFUSED,
            FontId::proportional(11.0),
            colors::MISSING,
        );
    }
}

/// `lane` with `point` in it, at its place in tick order, and that place. A
/// point already at the same tick is replaced.
fn with_point(lane: &AutomationLane, point: AutomationPoint) -> (AutomationLane, usize) {
    let mut lane = lane.clone();
    match lane.points.binary_search_by_key(&point.tick, |p| p.tick) {
        Ok(at) => {
            lane.points[at].value = point.value;
            (lane, at)
        }
        Err(at) => {
            lane.points.insert(at, point);
            (lane, at)
        }
    }
}

/// `lane` with point `index` moved to `tick` and `value`. Points can't pass
/// their neighbours or go before the beginning.
fn moved_point(
    lane: &AutomationLane,
    index: usize,
    tick: Tick,
    value: f32,
) -> Option<AutomationLane> {
    let mut moved = lane.clone();
    let before = index
        .checked_sub(1)
        .map_or(Tick::ZERO, |i| Tick(lane.points[i].tick.0 + 1));
    let point = moved.points.get_mut(index)?;
    point.tick = tick.max(before);
    if let Some(next) = lane.points.get(index + 1) {
        point.tick = point.tick.min(Tick(next.tick.0 - 1));
    }
    point.value = value;
    Some(moved)
}

/// A lane's header: its name and a button to remove it.
pub fn header(ui: &mut egui::Ui, rect: Rect, id: LaneId, label: &str, refused: bool) -> Vec<Edit> {
    let mut edits = Vec::new();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, colors::AUTOMATION_HEADER);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, colors::LANE_EVEN),
    );
    painter.text(
        Pos2::new(rect.left() + 16.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(11.0),
        if refused {
            colors::MISSING
        } else {
            colors::TEXT
        },
    );
    let button = Rect::from_center_size(
        Pos2::new(rect.right() - 16.0, rect.center().y),
        vec2(20.0, 18.0),
    );
    if ui
        .put(button, egui::Button::new("×"))
        .on_hover_text("Remove this lane")
        .clicked()
    {
        edits.push(Edit::Apply(Command::RemoveLane { id }));
    }
    edits
}

/// A track header's menu for adding a lane for the track's gain or mute,
/// which starts as one point holding the control's value now, so adding it
/// changes nothing you hear.
pub fn add_menu(
    ui: &mut egui::Ui,
    spot: Rect,
    project: &Project,
    gain: &(NodeId, String),
    mute: &(NodeId, String),
    controls: Controls,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(spot)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.menu_button("~", |ui| {
        for ((key, (node, port)), now, curve) in [
            ((group::GAIN, gain), controls.gain_db, Curve::Linear),
            ((group::MUTE, mute), f32::from(controls.mute), Curve::Hold),
        ] {
            let target = Endpoint::new(*node, port.as_str());
            let taken = project.lane_for(&target).is_some();
            let item = ui.add_enabled(!taken, egui::Button::new(format!("Automate {}", name(key))));
            if item.on_disabled_hover_text("Already automated").clicked() {
                let point = AutomationPoint {
                    tick: Tick::ZERO,
                    value: now,
                    curve,
                };
                edits.push(Edit::Apply(Command::AddLane {
                    id: project.next_lane_id(),
                    lane: AutomationLane::new(target, vec![point]),
                }));
                ui.close();
            }
        }
    });
    edits
}
