//! The arrangement driven like a user would: pointer and keyboard events into
//! a real [`Session`], checking the clips that result.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::Harness;
use noodle_core::{Clip, ClipId, Command, Node, NodeId, Tick};
use noodle_io::write_wav;

use super::{TRACK_INPUT, TimelineState, show};
use crate::session::{Edit, Nodes, Session};

struct Rig {
    session: Session,
    timeline: TimelineState,
    playhead: Option<Tick>,
    /// Keeps the audio files alive.
    _dir: tempfile::TempDir,
}

type H = Harness<'static, Rig>;

/// Two tracks and a two second (four quarter notes at the default 120 bpm)
/// clip of `a.wav` at tick 0 on the first, in a project saved next to the
/// file. Returns the clip's ID.
fn rig() -> (H, ClipId) {
    let dir = tempfile::tempdir().unwrap();
    write_wav(&dir.path().join("a.wav"), &vec![0.0; 96_000], 1, 48_000).unwrap();
    let mut session = Session::new(Nodes::all());
    assert!(session.save_as(&dir.path().join("p.ron")));
    for n in 1..=2 {
        session.edit([Edit::Apply(Command::AddNode {
            id: NodeId(n),
            node: Node::new(TRACK_INPUT),
        })]);
    }
    let id = session.project().next_clip_id();
    session.edit([Edit::Apply(Command::AddClip {
        id,
        clip: Clip::audio(NodeId(1), Tick(0), "a.wav", 96_000),
    })]);
    let rig = Rig {
        session,
        timeline: TimelineState::default(),
        playhead: None,
        _dir: dir,
    };
    let mut h = Harness::builder()
        .with_size(Vec2::new(900.0, 300.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let edits = show(ui, &mut rig.timeline, &rig.session, rig.playhead);
                rig.session.edit(edits);
            },
            rig,
        );
    h.run();
    (h, id)
}

fn clip(h: &H, id: ClipId) -> Clip {
    h.state().session.project().clip(id).unwrap().clone()
}

fn centre(h: &H, id: ClipId) -> Pos2 {
    rect(h, id).center()
}

fn rect(h: &H, id: ClipId) -> egui::Rect {
    h.state().timeline.clip_rect(id).unwrap()
}

/// Presses at the first point, moves through the rest, releases at the last.
fn drag(h: &mut H, modifiers: Modifiers, path: &[Pos2]) {
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerMoved(path[0]));
    h.step();
    let button = |pos, pressed| Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    };
    h.event(button(path[0], true));
    h.step();
    for &p in &path[1..] {
        h.event(Event::PointerMoved(p));
        h.step();
    }
    h.event(button(*path.last().unwrap(), false));
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

enum Grab {
    Body,
    Start,
    End,
}

/// Drags from a place on a clip to `by` away from it, in steps.
fn drag_by(h: &mut H, modifiers: Modifiers, id: ClipId, at: Grab, by: Vec2) {
    let r = rect(h, id);
    let from = match at {
        Grab::Body => r.center(),
        Grab::Start => Pos2::new(r.left() + 2.0, r.center().y),
        Grab::End => Pos2::new(r.right() - 2.0, r.center().y),
    };
    let path: Vec<Pos2> = (0..=6).map(|i| from + by * (i as f32 / 6.0)).collect();
    drag(h, modifiers, &path);
}

fn start(h: &H, id: ClipId) -> Tick {
    clip(h, id).start
}

fn length(h: &H, id: ClipId) -> u64 {
    clip(h, id).as_audio().unwrap().length
}

#[test]
fn a_clip_is_as_wide_as_its_audio_at_the_tempo() {
    let (h, id) = rig();
    // Four quarter notes at 60 points each.
    assert_eq!(rect(&h, id).width(), 240.0);
}

#[test]
fn dragging_a_clip_moves_it_to_the_nearest_beat() {
    let (mut h, id) = rig();
    // 130 points is a bit over two beats.
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Body,
        Vec2::new(130.0, 0.0),
    );
    assert_eq!(start(&h, id), Tick(1920));
}

#[test]
fn alt_drags_without_snapping() {
    let (mut h, id) = rig();
    drag_by(
        &mut h,
        Modifiers::ALT,
        id,
        Grab::Body,
        Vec2::new(130.0, 0.0),
    );
    // 130 points is 2.1667 quarter notes.
    let tick = start(&h, id).0;
    assert!((2075..=2085).contains(&tick), "{tick}");
}

#[test]
fn a_drag_is_one_undo_step() {
    let (mut h, id) = rig();
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Body,
        Vec2::new(130.0, 0.0),
    );
    h.state_mut().session.undo();
    assert_eq!(start(&h, id), Tick(0));
}

#[test]
fn a_clip_cant_be_dragged_before_the_start() {
    let (mut h, id) = rig();
    let mut moved = clip(&h, id);
    moved.start = Tick(960);
    h.state_mut()
        .session
        .edit([Edit::Apply(Command::SetClip { id, clip: moved })]);
    h.run();
    // Nearly three beats back from a clip that's one beat in, not snapping to a
    // beat, stops at the start.
    drag_by(
        &mut h,
        Modifiers::ALT,
        id,
        Grab::Body,
        Vec2::new(-170.0, 0.0),
    );
    assert_eq!(start(&h, id), Tick(0));
}

#[test]
fn dragging_down_moves_a_clip_to_the_next_track() {
    let (mut h, id) = rig();
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Body,
        Vec2::new(0.0, 70.0),
    );
    assert_eq!(clip(&h, id).node, NodeId(2));
    assert_eq!(start(&h, id), Tick(0));
}

#[test]
fn dragging_the_right_edge_shortens_the_clip() {
    let (mut h, id) = rig();
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::End,
        Vec2::new(-60.0, 0.0),
    );
    assert_eq!(length(&h, id), 72_000);
    assert_eq!(start(&h, id), Tick(0));
}

#[test]
fn dragging_the_left_edge_cuts_the_start_off() {
    let (mut h, id) = rig();
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Start,
        Vec2::new(60.0, 0.0),
    );
    let c = clip(&h, id);
    assert_eq!(c.start, Tick(960));
    assert_eq!(c.as_audio().unwrap().offset, 24_000);
    assert_eq!(c.as_audio().unwrap().length, 72_000);
}

#[test]
fn the_right_edge_stops_at_the_end_of_the_file() {
    let (mut h, id) = rig();
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::End,
        Vec2::new(120.0, 0.0),
    );
    assert_eq!(length(&h, id), 96_000);
}

#[test]
fn clicking_selects_and_delete_removes() {
    let (mut h, id) = rig();
    let c = rect(&h, id).center();
    drag(&mut h, Modifiers::NONE, &[c]);
    assert!(h.state().timeline.selected().contains(&id));
    h.key_press(Key::Delete);
    h.run();
    assert!(h.state().session.project().clip(id).is_none());
    // And one undo brings it back.
    h.state_mut().session.undo();
    assert!(h.state().session.project().clip(id).is_some());
}

#[test]
fn clicking_empty_space_deselects() {
    let (mut h, id) = rig();
    let centre = centre(&h, id);
    drag(&mut h, Modifiers::NONE, &[centre]);
    drag(&mut h, Modifiers::NONE, &[Pos2::new(700.0, 250.0)]);
    assert!(h.state().timeline.selected().is_empty());
}

#[test]
fn a_clip_whose_file_is_missing_says_so() {
    let (mut h, _) = rig();
    let session = &mut h.state_mut().session;
    let id = session.project().next_clip_id();
    session.edit([Edit::Apply(Command::AddClip {
        id,
        clip: Clip::audio(NodeId(2), Tick(0), "gone.wav", 48_000),
    })]);
    h.run();
    assert!(
        h.state()
            .timeline
            .drawn_text()
            .contains(&"gone.wav (missing)".to_string())
    );
}

#[test]
fn no_tracks_says_so() {
    let (mut h, id) = rig();
    let session = &mut h.state_mut().session;
    session.edit([Edit::Apply(Command::RemoveNode { id: NodeId(1) })]);
    session.edit([Edit::Apply(Command::RemoveNode { id: NodeId(2) })]);
    h.run();
    assert!(h.state().timeline.clip_rect(id).is_none());
    assert!(
        h.state()
            .timeline
            .drawn_text()
            .contains(&"No tracks yet".to_string())
    );
}

#[test]
fn the_playhead_follows_the_tick_it_is_given() {
    let (mut h, id) = rig();
    h.state_mut().playhead = Some(Tick(960));
    h.run();
    // Nothing to assert on a painted line; it must at least not disturb the
    // clips.
    assert_eq!(rect(&h, id).width(), 240.0);
}

#[test]
fn a_drag_that_loses_its_clip_still_ends_its_undo_step() {
    let (mut h, id) = rig();
    let from = centre(&h, id);
    h.event(Event::PointerMoved(from));
    h.step();
    let button = |pressed| Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    h.event(button(true));
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + Vec2::new(i as f32 * 20.0, 0.0)));
        h.step();
    }
    // The clip scrolls out of view mid-drag, and the button comes up with
    // nothing under the pointer to report it.
    h.state_mut().timeline.scroll_x = 5000.0;
    h.step();
    h.event(button(false));
    h.run();
    // A later drag must be its own undo step.
    h.state_mut().session.edit([
        Edit::Drag(Command::AddNode {
            id: NodeId(9),
            node: Node::new("noodle.osc.sine"),
        }),
        Edit::EndDrag,
    ]);
    h.state_mut().session.undo();
    assert_ne!(start(&h, id), Tick(0), "the move is still there");
}

#[test]
fn undoing_a_delete_leaves_nothing_selected_that_is_gone() {
    let (mut h, id) = rig();
    let c = centre(&h, id);
    drag(&mut h, Modifiers::NONE, &[c]);
    h.key_press(Key::Delete);
    h.run();
    assert!(h.state().timeline.selected().is_empty());
    h.state_mut().session.undo();
    h.run();
    assert!(h.state().session.project().clip(id).is_some());
    assert!(h.state().timeline.selected().is_empty());
}

/// Steps until the background threads have reported, or gives up.
fn wait_for_waveforms(h: &mut H, wanted: usize) {
    for _ in 0..200 {
        h.step();
        if h.state().timeline.waveforms() >= wanted {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn a_clip_gets_its_waveform_once_the_file_is_read() {
    let (mut h, _) = rig();
    wait_for_waveforms(&mut h, 1);
    assert_eq!(h.state().timeline.waveforms(), 1);
}

#[test]
fn a_file_that_turns_up_later_replaces_the_missing_mark() {
    let (mut h, _) = rig();
    h.state_mut().timeline.sources.retry_at_once();
    let session = &mut h.state_mut().session;
    let id = session.project().next_clip_id();
    session.edit([Edit::Apply(Command::AddClip {
        id,
        clip: Clip::audio(NodeId(2), Tick(0), "later.wav", 48_000),
    })]);
    h.run_steps(3);
    assert!(
        h.state()
            .timeline
            .drawn_text()
            .contains(&"later.wav (missing)".to_string())
    );
    let dir = h.state().session.directory().unwrap().to_owned();
    write_wav(&dir.join("later.wav"), &vec![0.5; 48_000], 1, 48_000).unwrap();
    wait_for_waveforms(&mut h, 2);
    assert!(
        h.state()
            .timeline
            .drawn_text()
            .contains(&"later.wav".to_string())
    );
    assert_eq!(h.state().timeline.waveforms(), 2);
}

#[test]
fn a_waveform_is_only_worked_out_again_when_the_view_changes() {
    let (mut h, _) = rig();
    wait_for_waveforms(&mut h, 1);
    h.run_steps(5);
    let computed = h.state().timeline.columns_computed();
    assert_eq!(computed, 1);
    h.run_steps(5);
    assert_eq!(h.state().timeline.columns_computed(), computed);
    // Zooming changes the columns.
    h.state_mut().timeline.ppq = 90.0;
    h.run_steps(2);
    assert_eq!(h.state().timeline.columns_computed(), computed + 1);
}

#[test]
fn a_file_that_changes_on_disk_is_read_again() {
    let (mut h, id) = rig();
    wait_for_waveforms(&mut h, 1);
    h.state_mut().timeline.sources.retry_at_once();
    let before = rect(&h, id).width();
    // Re-exported at half the rate: the same frames take twice as long.
    let dir = h.state().session.directory().unwrap().to_owned();
    let path = dir.join("a.wav");
    write_wav(&path, &vec![0.0; 96_000], 1, 24_000).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(30);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(later)
        .unwrap();
    h.run_steps(3);
    assert_eq!(rect(&h, id).width(), before * 2.0);
}
