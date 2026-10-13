//! The tempo lane driven like a user would: pointer and keyboard events into
//! a real [`Session`], checking the tempo map that results.

use crate::testkit;
use egui::{Key, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::Harness;
use noodle_core::{Tick, TimeSignature};

use super::{TimelineState, show};
use crate::session::Session;
use crate::session::{Edit, Nodes};

struct Rig {
    session: Session,
    timeline: TimelineState,
}

type H = Harness<'static, Rig>;

fn rig() -> H {
    let mut h = Harness::builder()
        .with_size(Vec2::new(900.0, 300.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let out = show(ui, &mut rig.timeline, &rig.session, None);
                rig.session.edit(out.edits);
            },
            Rig {
                session: Session::new(Nodes::all()),
                timeline: TimelineState::default(),
            },
        );
    h.state_mut().timeline.set_tempo_lane(true);
    h.run();
    h
}

fn tempos(h: &H) -> Vec<(i64, f64)> {
    h.state()
        .session
        .project()
        .tempo_map()
        .tempos()
        .iter()
        .map(|t| (t.tick.0, t.bpm))
        .collect()
}

/// A spot in the lane `beats` quarter notes along, at `bpm`.
fn spot(h: &H, beats: f32, bpm: f64) -> Pos2 {
    let state = h.state();
    let row = state.timeline.tempo().row().expect("the lane is shown");
    let first = state.timeline.tempo().points()[0];
    // 60 bpm to 180 bpm span the row less its padding.
    let along = ((bpm - 60.0) / 120.0) as f32;
    Pos2::new(
        first.x + beats * state.timeline.pps / 2.0,
        row.bottom() - 8.0 - along * (row.height() - 16.0),
    )
}

#[test]
fn the_lane_starts_with_the_one_tempo_and_is_off_until_asked_for() {
    let mut h = rig();
    assert_eq!(tempos(&h), vec![(0, 120.0)]);
    assert_eq!(h.state().timeline.tempo().points().len(), 1);
    h.state_mut().timeline.set_tempo_lane(false);
    h.run();
    assert!(h.state().timeline.tempo().row().is_some(), "last drawn");
    h.step();
    assert!(!h.state().timeline.tempo_lane());
}

#[test]
fn clicking_the_lane_adds_a_tempo_change_on_the_beat() {
    let mut h = rig();
    let at = spot(&h, 4.1, 90.0);
    testkit::click(&mut h, at, Modifiers::NONE);
    let map = tempos(&h);
    assert_eq!(map.len(), 2, "{map:?}");
    assert_eq!(map[1].0, 4 * 960, "snapped to the beat");
    assert!((map[1].1 - 90.0).abs() < 3.0, "{map:?}");
    assert_eq!(h.state().timeline.tempo().selected(), Some(1));
    // It is one undo step.
    h.state_mut().session.undo();
    assert_eq!(tempos(&h).len(), 1);
}

#[test]
fn dragging_a_point_moves_the_change_and_changes_its_tempo_in_one_step() {
    let mut h = rig();
    let at = spot(&h, 4.0, 90.0);
    testkit::click(&mut h, at, Modifiers::NONE);
    h.run();
    let from = h.state().timeline.tempo().points()[1];
    let to = spot(&h, 8.0, 150.0);
    testkit::drag_between(&mut h, Modifiers::NONE, from, to);
    let map = tempos(&h);
    assert_eq!(map.len(), 2);
    assert_eq!(map[1].0, 8 * 960);
    assert!((map[1].1 - 150.0).abs() < 3.0, "{map:?}");
    // The undo goes back to where the point was put, not through the drag.
    h.state_mut().session.undo();
    assert_eq!(tempos(&h)[1].0, 4 * 960);
}

#[test]
fn the_first_point_only_changes_tempo_and_cant_be_removed() {
    let mut h = rig();
    let from = h.state().timeline.tempo().points()[0];
    // Dragged along and up: it stays at the start, and the tempo rises.
    let to = spot(&h, 5.0, 150.0);
    testkit::drag_between(&mut h, Modifiers::NONE, from, to);
    let map = tempos(&h);
    assert_eq!(map[0].0, 0);
    assert!((map[0].1 - 150.0).abs() < 3.0, "{map:?}");
    let first = h.state().timeline.tempo().points()[0];
    testkit::click_with(&mut h, PointerButton::Secondary, Modifiers::NONE, first);
    assert_eq!(tempos(&h).len(), 1);
}

#[test]
fn a_point_is_removed_by_right_click_or_delete() {
    let mut h = rig();
    for beat in [4.0, 8.0] {
        let at = spot(&h, beat, 100.0);
        testkit::click(&mut h, at, Modifiers::NONE);
    }
    assert_eq!(tempos(&h).len(), 3);
    let second = h.state().timeline.tempo().points()[1];
    testkit::click_with(&mut h, PointerButton::Secondary, Modifiers::NONE, second);
    assert_eq!(tempos(&h).len(), 2);
    // A picked point goes with Delete.
    let last = h.state().timeline.tempo().points()[1];
    testkit::click(&mut h, last, Modifiers::NONE);
    assert_eq!(h.state().timeline.tempo().selected(), Some(1));
    testkit::press(&mut h, last, Modifiers::NONE, Key::Delete);
    assert_eq!(tempos(&h).len(), 1);
}

#[test]
fn a_tempo_change_moves_the_clips_and_the_grid() {
    let mut h = rig();
    let at = spot(&h, 4.0, 60.0);
    testkit::click(&mut h, at, Modifiers::NONE);
    let map = h.state().session.project().tempo_map().clone();
    // Four quarters at 120 take 2 s, then the lane holds 60: the next
    // quarter takes a second.
    assert!(
        (map.seconds_at(Tick(5 * 960)) - 3.0).abs() < 0.05,
        "{map:?}"
    );
}

#[test]
fn the_signature_is_part_of_the_map_the_lane_keeps() {
    let mut h = rig();
    let sig = TimeSignature {
        numerator: 3,
        denominator: 4,
    };
    let map = crate::tempo::with_signature(h.state().session.project().tempo_map(), sig).unwrap();
    h.state_mut()
        .session
        .edit([Edit::Apply(noodle_core::Command::SetTempoMap(map))]);
    let at = spot(&h, 3.0, 100.0);
    testkit::click(&mut h, at, Modifiers::NONE);
    let map = h.state().session.project().tempo_map();
    assert_eq!(map.signatures()[0].signature, sig);
    assert_eq!(map.tempos().len(), 2);
}

#[test]
fn dragging_a_slow_point_right_lands_under_the_pointer() {
    let mut h = rig();
    let at = spot(&h, 4.0, 60.0);
    testkit::click(&mut h, at, Modifiers::NONE);
    h.run();
    let from = h.state().timeline.tempo().points()[1];
    // Four beats at 120 bpm, which is what is before the point.
    let to = Pos2::new(from.x + 4.0 * h.state().timeline.pps / 2.0, from.y);
    testkit::drag_between(&mut h, Modifiers::NONE, from, to);
    assert_eq!(tempos(&h)[1].0, 8 * 960);
}
