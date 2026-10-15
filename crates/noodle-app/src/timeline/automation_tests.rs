//! Automation lanes in the arrangement, driven like a user would: pointer and
//! keyboard events into a real [`Session`], checking the lanes that result.

use crate::testkit;
use egui::{Key, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use noodle_core::group::{self, GROUP_OUTPUT, TRACK_INPUT, create_track};
use noodle_core::{
    AutomationLane, AutomationPoint, Command, Curve, Endpoint, LaneId, NodeId, Position, Tick,
};

use super::{TimelineState, show};
use crate::session::{Edit, Nodes, Session};

struct Rig {
    session: Session,
    timeline: TimelineState,
}

type H = Harness<'static, Rig>;

/// One track, with its group's output node.
/// A harness over one new track, and its track input, which holds the
/// track's gain and mute.
fn rig() -> (H, NodeId) {
    let mut session = Session::new(Nodes::all());
    let (_, command) = create_track(None, Position { x: 0.0, y: 0.0 }, || session.new_node_id());
    session.edit([Edit::Apply(command)]);
    let output = session
        .project()
        .graph()
        .nodes()
        .find(|(_, node)| node.type_id == TRACK_INPUT)
        .map(|(id, _)| id)
        .unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(900.0, 400.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let out = show(ui, &mut rig.timeline, &rig.session, None);
                rig.session.edit(out.edits);
            },
            Rig {
                session,
                timeline: TimelineState::default(),
            },
        );
    h.run();
    (h, output)
}

fn point(beat: i64, value: f32, curve: Curve) -> AutomationPoint {
    AutomationPoint {
        tick: Tick(beat * 960),
        value,
        curve,
    }
}

fn add_lane(h: &mut H, target: Endpoint, points: Vec<AutomationPoint>) -> LaneId {
    let id = h.state().session.project().next_lane_id();
    h.state_mut().session.edit([Edit::Apply(Command::AddLane {
        id,
        lane: AutomationLane::new(target, points),
    })]);
    h.run();
    id
}

fn lane(h: &H, id: LaneId) -> Option<AutomationLane> {
    h.state().session.project().lane(id).cloned()
}

fn ticks(h: &H, id: LaneId) -> Vec<i64> {
    lane(h, id)
        .unwrap()
        .points
        .iter()
        .map(|p| p.tick.0)
        .collect()
}

fn point_at(h: &H, id: LaneId, index: usize) -> Pos2 {
    h.state().timeline.automation().point_at(id, index).unwrap()
}

fn row(h: &H, id: LaneId) -> egui::Rect {
    h.state().timeline.automation().row(id).unwrap()
}

/// The screen position of `beat` on a lane's row, at `along` from the
/// bottom (0) to the top (1).
fn spot(h: &H, id: LaneId, beat: f32, along: f32) -> Pos2 {
    let row = row(h, id);
    // The view starts at 60 points a beat from the row's left edge.
    Pos2::new(
        row.left() + beat * 60.0,
        row.bottom() - 8.0 - along * (row.height() - 16.0),
    )
}

fn gain(output: NodeId) -> Endpoint {
    Endpoint::new(output, group::TRACK_GAIN)
}

#[test]
fn a_lane_is_drawn_under_its_track_with_its_points() {
    let (mut h, output) = rig();
    let id = add_lane(
        &mut h,
        gain(output),
        vec![point(0, 0.0, Curve::Linear), point(2, -12.0, Curve::Linear)],
    );
    let row = row(&h, id);
    assert_eq!(row.height(), super::automation::HEIGHT);
    // Two beats along is 120 points further than the first.
    assert_eq!(point_at(&h, id, 1).x - point_at(&h, id, 0).x, 120.0);
    // A quieter point is lower.
    assert!(point_at(&h, id, 1).y > point_at(&h, id, 0).y);
    assert!(row.contains(point_at(&h, id, 0)));
}

#[test]
fn the_track_header_adds_a_gain_lane_that_changes_nothing() {
    let (mut h, output) = rig();
    // Set the gain first, so the new lane has something to match.
    h.state_mut().session.edit([Edit::Apply(Command::SetParam {
        node: output,
        key: group::TRACK_GAIN.into(),
        value: Some(-6.0),
    })]);
    h.run();
    h.get_by_label("~").click();
    h.run();
    h.get_by_label("Automate Gain").click();
    h.run();
    let (id, added) = {
        let project = h.state().session.project();
        let (id, lane) = project.lane_for(&gain(output)).unwrap();
        (id, lane.clone())
    };
    assert_eq!(
        added.points,
        vec![AutomationPoint {
            tick: Tick(0),
            value: -6.0,
            curve: Curve::Linear
        }]
    );
    assert!(h.state().timeline.automation().row(id).is_some());
    // One undo takes it away again.
    h.state_mut().session.undo();
    h.run();
    assert!(lane(&h, id).is_none());
    // A lane that already exists can't be added twice.
    let id = add_lane(&mut h, gain(output), vec![point(0, 0.0, Curve::Linear)]);
    h.get_by_label("~").click();
    h.run();
    assert!(
        h.get_by_label("Automate Gain")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        !h.get_by_label("Automate Mute")
            .accesskit_node()
            .is_disabled()
    );
    assert!(lane(&h, id).is_some());
}

#[test]
fn clicking_an_empty_spot_adds_a_point_on_the_nearest_beat() {
    let (mut h, output) = rig();
    let id = add_lane(&mut h, gain(output), vec![point(0, 0.0, Curve::Linear)]);
    // A bit past two beats, in the top quarter of the row.
    let at = spot(&h, id, 2.2, 0.9);
    click(&mut h, at);
    assert_eq!(ticks(&h, id), vec![0, 1920]);
    let added = lane(&h, id).unwrap().points[1];
    assert_eq!(added.curve, Curve::Linear);
    assert!(added.value > 12.0, "{}", added.value);
    // It is selected, and one undo step removes it.
    assert_eq!(h.state().timeline.automation().selected(), Some((id, 1)));
    h.state_mut().session.undo();
    h.run();
    assert_eq!(ticks(&h, id), vec![0]);
}

#[test]
fn a_mute_lane_holds_and_snaps_to_off_or_on() {
    let (mut h, output) = rig();
    let id = add_lane(
        &mut h,
        Endpoint::new(output, group::TRACK_MUTE),
        vec![point(0, 0.0, Curve::Hold)],
    );
    let at = spot(&h, id, 1.0, 0.8);
    click(&mut h, at);
    let at = spot(&h, id, 3.0, 0.2);
    click(&mut h, at);
    let points = lane(&h, id).unwrap().points;
    assert_eq!(
        points
            .iter()
            .map(|p| (p.tick.0, p.value))
            .collect::<Vec<_>>(),
        vec![(0, 0.0), (960, 1.0), (2880, 0.0)]
    );
    assert!(points.iter().all(|p| p.curve == Curve::Hold));
}

#[test]
fn dragging_a_point_moves_it_in_time_and_value_as_one_undo_step() {
    let (mut h, output) = rig();
    let id = add_lane(
        &mut h,
        gain(output),
        vec![
            point(0, 0.0, Curve::Linear),
            point(2, 0.0, Curve::Linear),
            point(4, 0.0, Curve::Linear),
        ],
    );
    let before = lane(&h, id).unwrap();
    let from = point_at(&h, id, 1);
    let path: Vec<Pos2> = (0..=6)
        .map(|i| from + Vec2::new(70.0, -15.0) * (i as f32 / 6.0))
        .collect();
    drag(&mut h, &path);
    let after = lane(&h, id).unwrap();
    // A bit over a beat later snaps to the third beat, and it went up.
    assert_eq!(ticks(&h, id), vec![0, 2880, 3840]);
    assert!(after.points[1].value > 0.0);
    // The whole drag is one undo step.
    h.state_mut().session.undo();
    h.run();
    assert_eq!(lane(&h, id).unwrap(), before);
}

#[test]
fn a_point_cannot_be_dragged_past_its_neighbours_or_before_the_start() {
    let (mut h, output) = rig();
    let id = add_lane(
        &mut h,
        gain(output),
        vec![
            point(1, 0.0, Curve::Linear),
            point(2, 0.0, Curve::Linear),
            point(3, 0.0, Curve::Linear),
        ],
    );
    let from = point_at(&h, id, 1);
    let path: Vec<Pos2> = (0..=6)
        .map(|i| from + Vec2::new(300.0, 0.0) * (i as f32 / 6.0))
        .collect();
    drag(&mut h, &path);
    // Held just short of the next point.
    assert_eq!(ticks(&h, id), vec![960, 2879, 2880]);

    let from = point_at(&h, id, 0);
    let path: Vec<Pos2> = (0..=6)
        .map(|i| from - Vec2::new(300.0, 0.0) * (i as f32 / 6.0))
        .collect();
    drag(&mut h, &path);
    assert_eq!(ticks(&h, id)[0], 0);
}

#[test]
fn right_clicking_a_point_removes_it() {
    let (mut h, output) = rig();
    let id = add_lane(
        &mut h,
        gain(output),
        vec![point(0, 0.0, Curve::Linear), point(2, -6.0, Curve::Linear)],
    );
    let at = point_at(&h, id, 1);
    press(&mut h, at, PointerButton::Secondary, Modifiers::NONE);
    assert_eq!(ticks(&h, id), vec![0]);
    h.state_mut().session.undo();
    h.run();
    assert_eq!(ticks(&h, id), vec![0, 1920]);
}

#[test]
fn delete_removes_the_selected_point() {
    let (mut h, output) = rig();
    let id = add_lane(
        &mut h,
        gain(output),
        vec![point(0, 0.0, Curve::Linear), point(2, -6.0, Curve::Linear)],
    );
    let at = point_at(&h, id, 1);
    click(&mut h, at);
    assert_eq!(h.state().timeline.automation().selected(), Some((id, 1)));
    h.key_press(Key::Delete);
    h.run();
    assert_eq!(ticks(&h, id), vec![0]);
    assert_eq!(h.state().timeline.automation().selected(), None);
}

#[test]
fn the_lane_header_removes_the_lane() {
    let (mut h, output) = rig();
    let id = add_lane(&mut h, gain(output), vec![point(0, 0.0, Curve::Linear)]);
    h.get_by_label("×").click();
    h.run();
    assert!(lane(&h, id).is_none());
    h.state_mut().session.undo();
    h.run();
    assert!(lane(&h, id).is_some());
}

#[test]
fn a_lane_on_solo_is_shown_as_unable_to_work() {
    let (mut h, _) = rig();
    let output = h
        .state()
        .session
        .project()
        .graph()
        .nodes()
        .find(|(_, node)| node.type_id == GROUP_OUTPUT)
        .map(|(id, _)| id)
        .unwrap();
    let id = add_lane(
        &mut h,
        Endpoint::new(output, group::control_key(group::SOLO, 1)),
        vec![point(0, 1.0, Curve::Hold)],
    );
    assert!(h.state().timeline.automation().row(id).is_some());
    assert!(
        h.state()
            .timeline
            .automation()
            .texts()
            .iter()
            .any(|text| text.contains("Solo can't be automated"))
    );
}

#[test]
fn delete_removes_only_the_point_when_a_clip_was_selected_before() {
    let (mut h, output) = rig();
    let input = h
        .state()
        .session
        .project()
        .graph()
        .nodes()
        .find(|(_, node)| node.type_id == super::TRACK_INPUT)
        .map(|(id, _)| id)
        .unwrap();
    let clip_id = h.state().session.project().next_clip_id();
    h.state_mut().session.edit([Edit::Apply(Command::AddClip {
        id: clip_id,
        clip: noodle_core::Clip::audio(input, Tick(0), "a.wav", 96_000),
    })]);
    h.run();
    let id = add_lane(
        &mut h,
        gain(output),
        vec![point(2, 0.0, Curve::Linear), point(4, -6.0, Curve::Linear)],
    );
    let on_clip = h.state().timeline.clip_rect(clip_id).unwrap().center();
    click(&mut h, on_clip);
    assert!(h.state().timeline.selected().contains(&clip_id));
    let at = point_at(&h, id, 1);
    click(&mut h, at);
    assert_eq!(h.state().timeline.automation().selected(), Some((id, 1)));
    assert!(h.state().timeline.selected().is_empty());
    h.key_press(Key::Delete);
    h.run();
    assert_eq!(ticks(&h, id), vec![1920]);
    assert!(h.state().session.project().clip(clip_id).is_some());

    // And the other way round: picking a clip drops the point selection, so
    // the next Delete removes the clip and leaves the lane alone.
    click(&mut h, on_clip);
    assert_eq!(h.state().timeline.automation().selected(), None);
    h.key_press(Key::Delete);
    h.run();
    assert!(h.state().session.project().clip(clip_id).is_none());
    assert_eq!(ticks(&h, id), vec![1920]);
}

#[test]
fn a_track_control_with_a_lane_is_greyed_and_says_so() {
    let (mut h, output) = rig();
    assert!(!h.get_by_label("M").accesskit_node().is_disabled());
    add_lane(&mut h, gain(output), vec![point(0, 0.0, Curve::Linear)]);
    // Gain is automated now, mute isn't.
    assert!(!h.get_by_label("M").accesskit_node().is_disabled());
    let slider = h.get_by_role(egui::accesskit::Role::Slider);
    assert!(slider.accesskit_node().is_disabled());
    add_lane(
        &mut h,
        Endpoint::new(output, group::TRACK_MUTE),
        vec![point(0, 0.0, Curve::Hold)],
    );
    assert!(h.get_by_label("M").accesskit_node().is_disabled());
}

/// The Automation node the "+ Auto" button made, and its lane.
fn free_lane(h: &H) -> (NodeId, LaneId) {
    let project = h.state().session.project();
    let (id, _) = project
        .graph()
        .nodes()
        .find(|(_, n)| n.type_id == noodle_nodes::AUTOMATION_NODE_ID)
        .expect("the button added an Automation node");
    let (lane, _) = project
        .lane_for(&Endpoint::new(id, noodle_nodes::AUTOMATION_VALUE))
        .expect("and a lane on its value");
    (id, lane)
}

#[test]
fn the_auto_button_makes_a_free_automation_track_in_one_undo_step() {
    let (mut h, _) = rig();
    h.get_by_label("+ Auto").click();
    h.run();
    let (node, id) = free_lane(&h);
    assert_eq!(
        lane(&h, id).unwrap().points,
        vec![point(0, 0.0, Curve::Linear)]
    );
    // A free lane has a row of its own, below the track's, and the node is at
    // the top level so it can be wired anywhere.
    let free = row(&h, id);
    assert_eq!(free.height(), super::automation::HEIGHT);
    assert_eq!(
        h.state()
            .session
            .project()
            .graph()
            .node(node)
            .unwrap()
            .parent,
        None
    );

    h.state_mut().session.undo();
    h.run();
    assert!(lane(&h, id).is_none());
    assert!(h.state().session.project().graph().node(node).is_none());
}

#[test]
fn points_on_a_free_track_run_from_0_to_1_and_slide() {
    let (mut h, _) = rig();
    h.get_by_label("+ Auto").click();
    h.run();
    let (_, id) = free_lane(&h);

    // The top of the row is 1, the bottom 0, and a point there is linear.
    let top = spot(&h, id, 2.0, 1.0);
    click(&mut h, top);
    h.run();
    let low = spot(&h, id, 4.0, 0.25);
    click(&mut h, low);
    h.run();
    let points = lane(&h, id).unwrap().points;
    assert_eq!(points.len(), 3);
    assert_eq!(points[1].tick, Tick(2 * 960));
    assert!((points[1].value - 1.0).abs() < 1e-3, "{:?}", points[1]);
    assert!((points[2].value - 0.25).abs() < 0.02, "{:?}", points[2]);
    assert!(points.iter().all(|p| p.curve == Curve::Linear));

    // Dragging a point moves it freely between 0 and 1, not to a switch.
    let from = point_at(&h, id, 2);
    let to = spot(&h, id, 4.0, 0.6);
    drag(&mut h, &[from, to]);
    h.run();
    let moved = lane(&h, id).unwrap().points[2];
    assert!((moved.value - 0.6).abs() < 0.03, "{moved:?}");
}

#[test]
fn an_automation_node_in_a_tracks_group_shows_under_that_track() {
    let (mut h, output) = rig();
    let group = h
        .state()
        .session
        .project()
        .graph()
        .node(output)
        .and_then(|n| n.parent)
        .unwrap();
    let node = h.state().session.new_node_id();
    h.state_mut().session.edit([Edit::Apply(Command::AddNode {
        id: node,
        node: noodle_core::Node::new(noodle_nodes::AUTOMATION_NODE_ID).in_group(group),
    })]);
    let id = add_lane(
        &mut h,
        Endpoint::new(node, noodle_nodes::AUTOMATION_VALUE),
        vec![point(0, 0.5, Curve::Linear)],
    );
    let project = h.state().session.project();
    let tracks = super::tracks(project);
    let rows = super::automation::Rows::new(project, &tracks);
    assert_eq!(rows.lanes(0), [id]);
    assert!(rows.free_lanes().is_empty());
    assert_eq!(
        super::automation::label(project, project.lane(id).unwrap()),
        "Automation 1"
    );
}

#[test]
fn free_tracks_are_numbered_in_node_order() {
    let (mut h, _) = rig();
    h.get_by_label("+ Auto").click();
    h.run();
    h.get_by_label("+ Auto").click();
    h.run();
    let project = h.state().session.project();
    let labels: Vec<String> = project
        .lanes()
        .filter(|(_, lane)| lane.target.port == noodle_nodes::AUTOMATION_VALUE)
        .map(|(_, lane)| super::automation::label(project, lane))
        .collect();
    assert_eq!(labels, ["Automation 1", "Automation 2"]);
    // The gain lane keeps its old name.
    let gain_lane = AutomationLane::new(gain(NodeId(1)), vec![]);
    assert_eq!(super::automation::label(project, &gain_lane), "Gain lane");
}

fn press(h: &mut H, pos: Pos2, button: PointerButton, modifiers: Modifiers) {
    testkit::click_with(h, button, modifiers, pos);
}

fn click(h: &mut H, pos: Pos2) {
    testkit::click(h, pos, Modifiers::NONE);
}

fn drag(h: &mut H, path: &[Pos2]) {
    testkit::drag(h, Modifiers::NONE, path);
}
