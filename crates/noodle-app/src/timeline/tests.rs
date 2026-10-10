//! The arrangement driven like a user would: pointer and keyboard events into
//! a real [`Session`], checking the clips that result.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use noodle_core::{Clip, ClipId, Command, Node, NodeId, Tick};
use noodle_io::write_wav;

use super::{TRACK_INPUT, TimelineState, show};
use crate::session::{Edit, Nodes, Session};

struct Rig {
    session: Session,
    timeline: TimelineState,
    playhead: Option<Tick>,
    /// Every place the ruler asked the playhead to go.
    seeks: Vec<Tick>,
    /// What the arrangement told the user.
    notices: Vec<String>,
    /// Where it asked to import audio from a file.
    picks: Vec<super::Target>,
    /// The MIDI clips it asked to open in the piano roll.
    opened: Vec<ClipId>,
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
        seeks: Vec::new(),
        notices: Vec::new(),
        picks: Vec::new(),
        opened: Vec::new(),
        _dir: dir,
    };
    let mut h = Harness::builder()
        .with_size(Vec2::new(900.0, 300.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let out = show(ui, &mut rig.timeline, &rig.session, rig.playhead);
                rig.session.edit(out.edits);
                rig.seeks.extend(out.seek);
                rig.notices.extend(out.notice);
                rig.picks.extend(out.pick);
                rig.opened.extend(out.open_midi);
                for (track, on) in out.arm {
                    rig.session.arm(track, on);
                }
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

#[test]
fn clicking_the_ruler_asks_for_the_nearest_beat() {
    let (mut h, id) = rig();
    // The clip starts at tick 0; 60 points a beat, so 130 is a bit past two.
    let x = rect(&h, id).left() + 130.0;
    drag(&mut h, Modifiers::NONE, &[Pos2::new(x, 10.0)]);
    assert_eq!(h.state().seeks.last(), Some(&Tick(1920)));
}

#[test]
fn alt_clicking_the_ruler_does_not_snap() {
    let (mut h, id) = rig();
    let x = rect(&h, id).left() + 130.0;
    drag(&mut h, Modifiers::ALT, &[Pos2::new(x, 10.0)]);
    let tick = h.state().seeks.last().unwrap().0;
    assert!((2075..=2085).contains(&tick), "{tick}");
}

#[test]
fn clicking_the_ruler_leaves_clips_selected() {
    let (mut h, id) = rig();
    let c = centre(&h, id);
    drag(&mut h, Modifiers::NONE, &[c]);
    drag(&mut h, Modifiers::NONE, &[Pos2::new(400.0, 10.0)]);
    assert!(h.state().timeline.selected().contains(&id));
}

#[test]
fn holding_the_ruler_still_seeks_once() {
    let (mut h, id) = rig();
    let x = rect(&h, id).left() + 130.0;
    let at = Pos2::new(x, 10.0);
    h.event(Event::PointerMoved(at));
    h.step();
    h.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.state().seeks, [Tick(1920)]);
    // Letting go and pressing again is a new request.
    h.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
    h.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    h.step();
    assert_eq!(h.state().seeks, [Tick(1920), Tick(1920)]);
}

/// The rig with its first track inside a group that has an output node, as a
/// created track has: the group is node 10, its output node 11.
fn grouped() -> (H, ClipId) {
    use noodle_core::group::{GROUP, GROUP_OUTPUT};
    let (mut h, id) = rig();
    let inside = |type_id: &str| {
        let mut node = Node::new(type_id);
        node.parent = Some(NodeId(10));
        node
    };
    let session = &mut h.state_mut().session;
    session.edit([Edit::Apply(Command::AddNode {
        id: NodeId(10),
        node: Node::new(GROUP),
    })]);
    session.edit([Edit::Apply(Command::AddNode {
        id: NodeId(11),
        node: inside(GROUP_OUTPUT),
    })]);
    session.edit([Edit::Apply(Command::SetParent {
        node: NodeId(1),
        parent: Some(NodeId(10)),
    })]);
    h.run();
    (h, id)
}

fn param(h: &H, key: &str) -> Option<f32> {
    let graph = h.state().session.project().graph();
    graph.node(NodeId(11)).unwrap().params.get(key).copied()
}

#[test]
fn the_mute_button_sets_the_output_nodes_mute() {
    let (mut h, _) = grouped();
    h.get_by_label("M").click();
    h.run();
    assert_eq!(param(&h, "mute"), Some(1.0));
    h.get_by_label("M").click();
    h.run();
    assert_eq!(param(&h, "mute"), Some(0.0));
}

#[test]
fn the_solo_button_sets_solo() {
    let (mut h, _) = grouped();
    h.get_by_label("S").click();
    h.run();
    assert_eq!(param(&h, "solo"), Some(1.0));
}

#[test]
fn dragging_the_gain_slider_is_one_undo_step() {
    let (mut h, _) = grouped();
    let slider = h.get_by_role(egui::accesskit::Role::Slider).rect();
    // From the middle to the far left: down to the bottom of the range.
    let from = slider.center();
    let path: Vec<Pos2> = (0..=6)
        .map(|i| from + Vec2::new(-200.0 * i as f32 / 6.0, 0.0))
        .collect();
    drag(&mut h, Modifiers::NONE, &path);
    let gain = param(&h, "gain").unwrap();
    assert!(gain < -20.0, "{gain}");
    h.state_mut().session.undo();
    assert_eq!(param(&h, "gain"), None, "one undo puts it back");
}

#[test]
fn a_track_outside_a_group_has_no_controls() {
    let (h, _) = rig();
    assert!(h.query_by_label("M").is_none());
}

fn set_param(h: &mut H, node: u64, key: &str, value: f32) {
    h.state_mut().session.edit([Edit::Apply(Command::SetParam {
        node: NodeId(node),
        key: key.into(),
        value: Some(value),
    })]);
    h.run();
}

#[test]
fn turning_solo_off_clears_it_on_every_boundary_node() {
    use noodle_core::group::GROUP_INPUT;
    let (mut h, _) = grouped();
    let mut input = Node::new(GROUP_INPUT);
    input.parent = Some(NodeId(10));
    h.state_mut().session.edit([Edit::Apply(Command::AddNode {
        id: NodeId(12),
        node: input,
    })]);
    set_param(&mut h, 12, "solo", 1.0);
    set_param(&mut h, 11, "solo", 1.0);
    h.get_by_label("S").click();
    h.run();
    assert_eq!(param(&h, "solo"), Some(0.0));
    let graph = h.state().session.project().graph();
    assert_eq!(graph.node(NodeId(12)).unwrap().params["solo"], 0.0);
}

#[test]
fn solo_on_the_input_node_shows_as_soloed() {
    use noodle_core::group::GROUP_INPUT;
    let (mut h, _) = grouped();
    let mut input = Node::new(GROUP_INPUT);
    input.parent = Some(NodeId(10));
    h.state_mut().session.edit([Edit::Apply(Command::AddNode {
        id: NodeId(12),
        node: input,
    })]);
    set_param(&mut h, 12, "solo", 1.0);
    // Pressing S turns it off, rather than setting it again.
    h.get_by_label("S").click();
    h.run();
    let graph = h.state().session.project().graph();
    assert_eq!(graph.node(NodeId(12)).unwrap().params["solo"], 0.0);
}

#[test]
fn double_clicking_the_gain_slider_writes_zero_db() {
    let (mut h, _) = grouped();
    set_param(&mut h, 11, "gain", -12.0);
    let slider = h.get_by_role(egui::accesskit::Role::Slider).rect();
    h.event(Event::PointerMoved(slider.center()));
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos: slider.center(),
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        h.step();
    }
    h.run();
    assert_eq!(param(&h, "gain"), Some(0.0));
}

fn track_inputs(h: &H) -> usize {
    let graph = h.state().session.project().graph();
    graph
        .nodes()
        .filter(|(_, n)| n.type_id == noodle_core::group::TRACK_INPUT)
        .count()
}

#[test]
fn add_track_creates_a_group_with_a_track_input_in_one_undo_step() {
    let (mut h, _) = rig();
    let before = track_inputs(&h);
    h.get_by_label("+ Add track").click();
    h.run();
    assert_eq!(track_inputs(&h), before + 1);
    let groups = |h: &H| {
        let graph = h.state().session.project().graph();
        graph
            .nodes()
            .filter(|(_, n)| n.type_id == noodle_core::group::GROUP)
            .count()
    };
    assert_eq!(groups(&h), 1);
    // The new track shows its controls straight away.
    assert!(h.query_by_label("M").is_some());
    h.state_mut().session.undo();
    assert_eq!(track_inputs(&h), before);
    assert_eq!(groups(&h), 0);
}

#[test]
fn add_track_works_with_no_tracks() {
    let (mut h, _) = rig();
    for id in [1, 2] {
        h.state_mut()
            .session
            .edit([Edit::Apply(Command::RemoveNode { id: NodeId(id) })]);
    }
    h.run();
    assert_eq!(track_inputs(&h), 0);
    h.get_by_label("+ Add track").click();
    h.run();
    assert_eq!(track_inputs(&h), 1);
}

fn fades(h: &H, id: ClipId) -> (u64, u64) {
    let audio = clip(h, id).as_audio().unwrap().clone();
    (audio.fade_in, audio.fade_out)
}

fn drag_handle(h: &mut H, id: ClipId, fade_in: bool, by: f32) {
    let from = h
        .state()
        .timeline
        .fade_handle(id, fade_in)
        .unwrap()
        .center();
    let path: Vec<Pos2> = (0..=6)
        .map(|i| from + Vec2::new(by * i as f32 / 6.0, 0.0))
        .collect();
    drag(h, Modifiers::NONE, &path);
}

#[test]
fn dragging_the_fade_in_handle_sets_the_fade_without_moving_the_clip() {
    let (mut h, id) = rig();
    let before = clip(&h, id);
    drag_handle(&mut h, id, true, 60.0);
    let (fade_in, fade_out) = fades(&h, id);
    // The pointer ended 66 points in: a beat is 60 points and 24000 frames.
    assert!(fade_in.abs_diff(26_400) < 1_500, "{fade_in}");
    assert_eq!(fade_out, 0);
    let after = clip(&h, id);
    assert_eq!((after.start, length(&h, id)), (before.start, 96_000));
    h.state_mut().session.undo();
    assert_eq!(fades(&h, id), (0, 0), "one undo step");
}

#[test]
fn dragging_the_fade_out_handle_and_the_two_fades_never_cross() {
    let (mut h, id) = rig();
    drag_handle(&mut h, id, true, 150.0);
    let (fade_in, _) = fades(&h, id);
    assert!(fade_in > 55_000, "{fade_in}");
    // The fade out handle drags left past the end of the fade in.
    drag_handle(&mut h, id, false, -230.0);
    let (fade_in, fade_out) = fades(&h, id);
    assert!(fade_in + fade_out <= 96_000, "{fade_in} + {fade_out}");
    assert!(fade_out > 0);
}

#[test]
fn a_clip_too_narrow_for_trimming_has_no_fade_handles() {
    let (mut h, id) = rig();
    h.state_mut().session.edit([Edit::Apply(Command::SetClip {
        id,
        clip: Clip::audio(NodeId(1), Tick(0), "a.wav", 2_000),
    })]);
    h.run();
    assert!(h.state().timeline.fade_handle(id, true).is_none());
}

#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not needed".into())
    }
}

/// Drops `files` with the pointer at `at`.
fn drop_files(h: &mut H, at: Pos2, files: &[std::path::PathBuf]) {
    h.event(Event::PointerMoved(at));
    h.step();
    for file in files {
        h.input_mut()
            .dropped_files
            .push(std::sync::Arc::new(Dropped(file.clone())));
    }
    h.step();
    h.run();
}

/// Writes a wav of `frames` frames next to the project and returns its path.
fn wav(h: &H, name: &str, frames: usize) -> std::path::PathBuf {
    let path = h.state()._dir.path().join(name);
    write_wav(&path, &vec![0.0; frames], 1, 48_000).unwrap();
    path
}

fn clips_on(h: &H, node: u64) -> Vec<Clip> {
    let project = h.state().session.project();
    project
        .clips()
        .map(|(_, clip)| clip.clone())
        .filter(|clip| clip.node == NodeId(node))
        .collect()
}

/// A point in the second lane, `beats` quarter notes from the left.
fn lane_two(h: &H, id: ClipId, beats: f32) -> Pos2 {
    let r = rect(h, id);
    Pos2::new(
        r.left() + 60.0 * beats,
        r.center().y + super::colors::LANE_HEIGHT,
    )
}

#[test]
fn dropping_a_file_on_a_lane_adds_a_clip_there_in_one_undo_step() {
    let (mut h, id) = rig();
    let file = wav(&h, "b.wav", 48_000);
    // Just off the second beat: it snaps to it.
    let at = lane_two(&h, id, 2.2);
    drop_files(&mut h, at, &[file]);
    let added = clips_on(&h, 2);
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].start, Tick(1920));
    let audio = added[0].as_audio().unwrap();
    assert_eq!((audio.source.as_str(), audio.length), ("b.wav", 48_000));
    assert!(h.state().notices.is_empty());
    h.state_mut().session.undo();
    assert!(clips_on(&h, 2).is_empty(), "one undo step");
}

#[test]
fn several_dropped_files_are_laid_end_to_end() {
    let (mut h, id) = rig();
    let one = wav(&h, "b.wav", 48_000);
    let two = wav(&h, "c.wav", 24_000);
    // A little in from the lane's left edge, not on it: the drop only counts
    // when the pointer is inside the timeline, and a point on the very edge
    // fell either side of it depending on how the platform rounded the
    // layout (the Windows flake).
    let at = lane_two(&h, id, 0.05);
    drop_files(&mut h, at, &[one, two]);
    let mut added = clips_on(&h, 2);
    added.sort_by_key(|clip| clip.start);
    assert_eq!(added.len(), 2);
    // The first snaps to the grid line at the start; one second is two
    // beats, so the second starts at beat two.
    assert_eq!(
        added.iter().map(|c| c.start).collect::<Vec<_>>(),
        [Tick(0), Tick(1920)]
    );
    h.state_mut().session.undo();
    assert!(clips_on(&h, 2).is_empty());
}

#[test]
fn a_file_that_cant_be_read_is_reported_and_adds_nothing() {
    let (mut h, id) = rig();
    let bad = h.state()._dir.path().join("notes.txt");
    std::fs::write(&bad, "not audio").unwrap();
    let at = lane_two(&h, id, 1.0);
    drop_files(&mut h, at, &[bad]);
    assert!(clips_on(&h, 2).is_empty());
    let notices = &h.state().notices;
    assert!(
        notices.len() == 1 && notices[0].contains("notes.txt"),
        "{notices:?}"
    );
}

#[test]
fn dropping_a_file_where_there_are_no_tracks_asks_for_one() {
    let (mut h, id) = rig();
    let file = wav(&h, "b.wav", 48_000);
    let at = lane_two(&h, id, 1.0);
    for node in [1, 2] {
        h.state_mut()
            .session
            .edit([Edit::Apply(Command::RemoveNode { id: NodeId(node) })]);
    }
    h.run();
    drop_files(&mut h, at, &[file]);
    assert_eq!(h.state().session.project().clips().count(), 0);
    assert_eq!(h.state().notices.len(), 1);
}

#[test]
fn the_import_button_targets_the_selected_clips_track_at_the_playhead() {
    let (mut h, id) = rig();
    h.state_mut().playhead = Some(Tick(960));
    h.run();
    // Nothing selected: the first track.
    h.get_by_label("Import audio…").click();
    h.run();
    let first = h.state().picks.last().copied().unwrap();
    assert_eq!((first.track, first.at), (NodeId(1), Tick(960)));
    // Moving the clip to the second lane and selecting it points the import there.
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Body,
        Vec2::new(0.0, 64.0),
    );
    h.get_by_label("Import audio…").click();
    h.run();
    assert_eq!(h.state().picks.last().unwrap().track, NodeId(2));
}

fn double_click(h: &mut H, at: Pos2) {
    h.event(Event::PointerMoved(at));
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        h.step();
    }
    h.run();
}

fn type_and_press(h: &mut H, text: &str, key: Key) {
    h.event(Event::Text(text.into()));
    h.step();
    for pressed in [true, false] {
        h.event(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
    }
    h.run();
}

fn track_name(h: &H) -> Option<noodle_core::Value> {
    let graph = h.state().session.project().graph();
    graph.node(NodeId(10)).unwrap().config.get("name").cloned()
}

#[test]
fn double_clicking_a_track_name_renames_it_in_one_undo_step() {
    let (mut h, _) = grouped();
    let at = h.get_by_label("Track 1").rect().center();
    double_click(&mut h, at);
    type_and_press(&mut h, "Drums", Key::Enter);
    assert_eq!(
        track_name(&h),
        Some(noodle_core::Value::Text("Drums".into()))
    );
    assert!(h.query_by_label("Drums").is_some());
    h.state_mut().session.undo();
    assert_eq!(track_name(&h), None);
}

#[test]
fn escape_keeps_the_name() {
    let (mut h, _) = grouped();
    let at = h.get_by_label("Track 1").rect().center();
    double_click(&mut h, at);
    type_and_press(&mut h, "Nope", Key::Escape);
    assert_eq!(track_name(&h), None);
    assert!(h.query_by_label("Track 1").is_some());
    // It can be renamed again afterwards. (Wait out the double click window,
    // or the next two clicks would count as a triple click.)
    h.run_steps(60);
    double_click(&mut h, at);
    assert!(h.state().timeline.renaming.is_some());
    type_and_press(&mut h, "Bass", Key::Enter);
    assert_eq!(
        track_name(&h),
        Some(noodle_core::Value::Text("Bass".into()))
    );
}

#[test]
fn an_empty_name_puts_the_default_back() {
    let (mut h, _) = grouped();
    h.state_mut().session.edit([Edit::Apply(Command::SetConfig {
        node: NodeId(10),
        key: "name".into(),
        value: Some(noodle_core::Value::Text("Bass".into())),
    })]);
    h.run();
    let at = h.get_by_label("Bass").rect().center();
    double_click(&mut h, at);
    h.event(Event::Key {
        key: Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    });
    h.step();
    type_and_press(&mut h, "", Key::Backspace);
    type_and_press(&mut h, "", Key::Enter);
    assert_eq!(track_name(&h), None);
    assert!(h.query_by_label("Track 1").is_some());
}

#[test]
fn a_track_outside_a_group_cannot_be_renamed() {
    let (mut h, _) = rig();
    let at = h.get_by_label("Track 1").rect().center();
    double_click(&mut h, at);
    assert!(h.state().timeline.renaming.is_none());
}

#[test]
fn spaces_around_a_name_are_trimmed_and_spaces_alone_are_no_name() {
    let (mut h, _) = grouped();
    let at = h.get_by_label("Track 1").rect().center();
    double_click(&mut h, at);
    type_and_press(&mut h, "  Lead  ", Key::Enter);
    assert_eq!(
        track_name(&h),
        Some(noodle_core::Value::Text("Lead".into()))
    );
    h.run_steps(60);
    let at = h.get_by_label("Lead").rect().center();
    double_click(&mut h, at);
    h.event(Event::Key {
        key: Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    });
    h.step();
    type_and_press(&mut h, "   ", Key::Enter);
    assert_eq!(track_name(&h), None);
}

#[test]
fn drops_find_their_track_below_an_automation_lane() {
    use super::{automation, colors};
    use noodle_core::{AutomationLane, AutomationPoint, Endpoint, LaneId};
    let (mut h, id) = grouped();
    let point = |tick, value| AutomationPoint {
        tick: Tick(tick),
        value,
        curve: Default::default(),
    };
    h.state_mut().session.edit([Edit::Apply(Command::AddLane {
        id: LaneId(1),
        lane: AutomationLane::new(
            Endpoint::new(NodeId(11), "gain"),
            vec![point(0, 0.0), point(960, -6.0)],
        ),
    })]);
    h.run();
    let top = rect(&h, id).center();
    let file = wav(&h, "b.wav", 48_000);
    // The row under the first track is its automation lane: the drop still
    // belongs to the first track.
    drop_files(
        &mut h,
        top + Vec2::new(0.0, colors::LANE_HEIGHT),
        std::slice::from_ref(&file),
    );
    assert_eq!(clips_on(&h, 1).len(), 2, "the first track took it");
    assert!(clips_on(&h, 2).is_empty());
    // The second track starts below that row.
    let below = top + Vec2::new(0.0, colors::LANE_HEIGHT + automation::HEIGHT);
    drop_files(&mut h, below, &[file]);
    assert_eq!(clips_on(&h, 2).len(), 1, "the second track took it");
}

#[test]
fn the_arm_button_arms_and_disarms_its_track() {
    let (mut h, _) = grouped();
    let first = NodeId(1);
    assert!(!h.state().session.is_armed(first));
    h.get_all_by_label("R").next().unwrap().click();
    h.run();
    assert!(h.state().session.is_armed(first));
    h.get_all_by_label("R").next().unwrap().click();
    h.run();
    assert!(!h.state().session.is_armed(first));
}

#[test]
fn a_rename_is_dropped_when_its_header_scrolls_away() {
    let (mut h, _) = grouped();
    let at = h.get_by_label("Track 1").rect().center();
    double_click(&mut h, at);
    assert!(h.state().timeline.renaming.is_some());
    for n in 3..=9 {
        h.state_mut().session.edit([Edit::Apply(Command::AddNode {
            id: NodeId(n),
            node: Node::new(TRACK_INPUT),
        })]);
    }
    h.state_mut().timeline.scroll_y = 10_000.0;
    h.run();
    assert!(h.state().timeline.scroll_y > 100.0, "scrolled");
    assert!(h.state().timeline.renaming.is_none(), "dropped");
    assert_eq!(track_name(&h), None, "nothing was written");
}

#[test]
fn a_rename_is_dropped_when_its_track_is_removed() {
    let (mut h, _) = grouped();
    let at = h.get_by_label("Track 1").rect().center();
    double_click(&mut h, at);
    assert!(h.state().timeline.renaming.is_some());
    h.state_mut()
        .session
        .edit([Edit::Apply(Command::RemoveNode { id: NodeId(1) })]);
    h.run();
    assert!(h.state().timeline.renaming.is_none());
}

fn clip_count(h: &H) -> usize {
    h.state().session.project().clips().count()
}

#[test]
fn a_command_drag_copies_the_clip_and_leaves_the_original() {
    let (mut h, id) = rig();
    drag_by(
        &mut h,
        Modifiers::COMMAND,
        id,
        Grab::Body,
        Vec2::new(130.0, 0.0),
    );
    assert_eq!(clip_count(&h), 2);
    assert_eq!(start(&h, id), Tick(0), "the original stays");
    let copy = *h
        .state()
        .session
        .project()
        .clips()
        .map(|(copy, _)| copy)
        .find(|&copy| copy != id)
        .as_ref()
        .unwrap();
    assert_eq!(start(&h, copy), Tick(1920));
    assert_eq!(clip(&h, copy).as_audio().unwrap().source, "a.wav");
    // The copy is what's selected, and the whole drag is one undo step.
    assert_eq!(h.state().timeline.selected(), &[copy].into());
    h.state_mut().session.undo();
    assert_eq!(clip_count(&h), 1);
    assert_eq!(start(&h, id), Tick(0));
}

#[test]
fn a_plain_drag_still_moves_without_copying() {
    let (mut h, id) = rig();
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Body,
        Vec2::new(130.0, 0.0),
    );
    assert_eq!(clip_count(&h), 1);
}

fn key_over(h: &mut H, at: Pos2, modifiers: Modifiers, key: Key) {
    h.hover_at(at);
    h.step();
    h.key_press_modifiers(modifiers, key);
    h.run();
}

#[test]
fn command_arrows_take_the_playhead_to_the_clips_start_and_end() {
    let (mut h, id) = rig();
    // Move the clip off the start so the two edges differ.
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Body,
        Vec2::new(130.0, 0.0),
    );
    let at = centre(&h, id);
    key_over(&mut h, at, Modifiers::COMMAND, Key::ArrowLeft);
    assert_eq!(h.state().seeks.last(), Some(&Tick(1920)));
    key_over(&mut h, at, Modifiers::COMMAND, Key::ArrowRight);
    // Two seconds of audio at 120 bpm is four quarter notes (960 per beat).
    assert_eq!(h.state().seeks.last(), Some(&Tick(1920 + 4 * 960)));
}

#[test]
fn command_arrows_do_nothing_without_a_selection() {
    let (mut h, _) = rig();
    let at = Pos2::new(500.0, 200.0);
    key_over(&mut h, at, Modifiers::COMMAND, Key::ArrowRight);
    assert!(h.state().seeks.is_empty());
}

#[test]
fn command_arrows_scroll_the_playhead_into_view() {
    let (mut h, id) = rig();
    // Select the clip, then jump to its end far past the right edge.
    let at = centre(&h, id);
    drag(&mut h, Modifiers::NONE, &[at]);
    h.state_mut().timeline.ppq = 600.0;
    h.run();
    let before = h.state().timeline.scroll_x;
    key_over(&mut h, at, Modifiers::COMMAND, Key::ArrowRight);
    assert!(h.state().timeline.scroll_x > before);
}

#[test]
fn the_background_menu_offers_import_at_the_clicked_lane() {
    let (mut h, _) = rig();
    // Right-click empty space in the second track's lane.
    let lane = Pos2::new(700.0, 150.0);
    h.event(Event::PointerMoved(lane));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: lane,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    h.run();
    h.get_all_by_label("Import audio…").last().unwrap().click();
    h.run();
    let pick = h.state().picks.last().copied().unwrap();
    assert_eq!(pick.track, NodeId(2));
    assert!(pick.at.0 > 0);
}

/// A real track (a group holding a track input) added below the rig's two
/// bare track inputs, with a clip on it. Returns (group, track input, clip).
fn add_real_track(h: &mut H) -> (NodeId, NodeId, ClipId) {
    let session = &mut h.state_mut().session;
    let mut next = 100;
    let (group, create) =
        noodle_core::group::create_track(None, noodle_core::Position::default(), || {
            next += 1;
            NodeId(next)
        });
    session.edit([Edit::Apply(create)]);
    let input = session
        .project()
        .graph()
        .children(Some(group))
        .find(|(_, n)| n.type_id == TRACK_INPUT)
        .map(|(id, _)| id)
        .unwrap();
    let clip = session.project().next_clip_id();
    session.edit([Edit::Apply(Command::AddClip {
        id: clip,
        clip: Clip::audio(input, Tick(0), "a.wav", 96_000),
    })]);
    h.run();
    (group, input, clip)
}

/// The left margin of the third track's header, which has no widgets on it
/// (the panel starts 8 points in).
fn third_header(_: &H) -> Pos2 {
    Pos2::new(
        14.0,
        8.0 + super::colors::RULER_HEIGHT + 2.0 * super::colors::LANE_HEIGHT + 20.0,
    )
}

#[test]
fn delete_removes_a_selected_track_with_its_clips_in_one_undo_step() {
    let (mut h, _) = rig();
    let (group, _, clip) = add_real_track(&mut h);
    let before = h.state().session.project().clone();
    let at = third_header(&h);
    drag(&mut h, Modifiers::NONE, &[at]);
    assert!(h.state().timeline.selected_track.is_some());
    key_over(&mut h, at, Modifiers::NONE, Key::Delete);
    let project = h.state().session.project();
    assert!(project.graph().node(group).is_none());
    assert!(project.clip(clip).is_none(), "its clips went with it");
    h.state_mut().session.undo();
    assert_eq!(h.state().session.project(), &before);
}

#[test]
fn delete_with_clips_selected_removes_the_clips_not_the_track() {
    let (mut h, first) = rig();
    let (group, _, _) = add_real_track(&mut h);
    let at = third_header(&h);
    drag(&mut h, Modifiers::NONE, &[at]);
    let on_clip = centre(&h, first);
    drag(&mut h, Modifiers::NONE, &[on_clip]);
    key_over(&mut h, on_clip, Modifiers::NONE, Key::Delete);
    assert!(h.state().session.project().clip(first).is_none());
    assert!(h.state().session.project().graph().node(group).is_some());
}

#[test]
fn the_header_menu_deletes_a_track() {
    let (mut h, _) = rig();
    let (group, _, _) = add_real_track(&mut h);
    let at = third_header(&h);
    h.event(Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    h.run();
    h.get_by_label("Delete track").click();
    h.run();
    assert!(h.state().session.project().graph().node(group).is_none());
}

fn header_y(index: usize) -> f32 {
    8.0 + super::colors::RULER_HEIGHT + index as f32 * super::colors::LANE_HEIGHT + 20.0
}

#[test]
fn dragging_a_header_up_reorders_the_tracks_in_one_undo_step() {
    let (mut h, _) = rig();
    let (group, input, _) = add_real_track(&mut h);
    let before = h.state().session.project().clone();
    let shown = |h: &H| super::tracks(h.state().session.project());
    assert_eq!(shown(&h).last(), Some(&input));
    // From the third header to the top of the first.
    drag(
        &mut h,
        Modifiers::NONE,
        &[
            Pos2::new(14.0, header_y(2)),
            Pos2::new(14.0, header_y(1)),
            Pos2::new(14.0, header_y(0) - 14.0),
        ],
    );
    assert_eq!(shown(&h).first(), Some(&input));
    assert_eq!(h.state().session.project().track_order()[0], group);
    h.state_mut().session.undo();
    assert_eq!(h.state().session.project(), &before);
    assert_eq!(shown(&h).last(), Some(&input));
}

#[test]
fn dropping_a_header_where_it_was_changes_nothing() {
    let (mut h, _) = rig();
    add_real_track(&mut h);
    let before = h.state().session.project().clone();
    drag(
        &mut h,
        Modifiers::NONE,
        &[
            Pos2::new(14.0, header_y(2)),
            Pos2::new(14.0, header_y(2) + 15.0),
        ],
    );
    assert_eq!(h.state().session.project(), &before);
}

#[test]
fn dragging_a_header_down_moves_it_below_the_next_track() {
    let (mut h, _) = rig();
    add_real_track(&mut h);
    let shown = |h: &H| super::tracks(h.state().session.project());
    let before = shown(&h);
    drag(
        &mut h,
        Modifiers::NONE,
        &[
            Pos2::new(14.0, header_y(0)),
            Pos2::new(14.0, header_y(1)),
            Pos2::new(14.0, header_y(1) + 20.0),
        ],
    );
    let after = shown(&h);
    assert_eq!(after[0], before[1]);
    assert_eq!(after[1], before[0]);
    assert_eq!(after[2], before[2]);
}

/// Adds a one bar MIDI clip with two notes at beat 2 on the second track.
fn add_midi(h: &mut H) -> ClipId {
    let session = &mut h.state_mut().session;
    let id = session.project().next_clip_id();
    let mut clip = Clip::midi(NodeId(2), Tick(960), Tick(3840));
    let noodle_core::ClipContent::Midi(midi) = &mut clip.content else {
        unreachable!()
    };
    midi.notes = vec![
        noodle_core::MidiNote::new(Tick(0), Tick(480), 60),
        noodle_core::MidiNote::new(Tick(960), Tick(960), 64),
    ];
    session.edit([Edit::Apply(Command::AddClip { id, clip })]);
    h.run();
    id
}

#[test]
fn a_midi_clip_is_drawn_and_moves_like_any_clip() {
    let (mut h, _) = rig();
    let id = add_midi(&mut h);
    assert!(
        h.state()
            .timeline
            .drawn_text()
            .contains(&"MIDI (2 notes)".to_string())
    );
    // A bar at 60 points per quarter note.
    assert!((rect(&h, id).width() - 240.0).abs() < 1.0);
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Body,
        Vec2::new(60.0, 0.0),
    );
    assert_eq!(start(&h, id), Tick(1920));
    // Dragging up a lane moves it to the track above.
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Body,
        Vec2::new(0.0, -64.0),
    );
    assert_eq!(clip(&h, id).node, NodeId(1));
}

#[test]
fn trimming_a_midi_clip_keeps_its_notes_in_place_on_the_timeline() {
    let (mut h, _) = rig();
    let id = add_midi(&mut h);
    // The right edge from beat 5 to beat 4: the clip is 3 beats long.
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::End,
        Vec2::new(-60.0, 0.0),
    );
    assert_eq!(clip(&h, id).as_midi().unwrap().length, Tick(2880));
    // The left edge a beat in: the clip starts a beat later and the notes
    // before it are cut off or shortened.
    drag_by(
        &mut h,
        Modifiers::NONE,
        id,
        Grab::Start,
        Vec2::new(60.0, 0.0),
    );
    let c = clip(&h, id);
    assert_eq!(c.start, Tick(1920));
    let midi = c.as_midi().unwrap();
    assert_eq!(midi.length, Tick(1920));
    assert_eq!(
        midi.notes.len(),
        1,
        "the first note ended before the new start"
    );
    assert_eq!(midi.notes[0].start, Tick(0));
    assert_eq!(midi.notes[0].key, 64);
}

#[test]
fn double_clicking_a_midi_clip_opens_it_and_audio_clips_do_not() {
    let (mut h, audio) = rig();
    let id = add_midi(&mut h);
    let pos = centre(&h, id);
    for _ in 0..2 {
        h.event(Event::PointerMoved(pos));
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
    }
    h.run();
    assert_eq!(h.state().opened, [id]);
    let pos = centre(&h, audio);
    for _ in 0..2 {
        h.event(Event::PointerMoved(pos));
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
    }
    h.run();
    assert_eq!(h.state().opened, [id]);
}

#[test]
fn the_lane_menu_makes_a_midi_clip_where_it_was_clicked() {
    let (mut h, _) = rig();
    let before = h.state().session.project().clips().count();
    // Empty space on the second lane, a little after beat 3.
    let pos = Pos2::new(150.0 + 130.0, 22.0 + 64.0 + 30.0);
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Secondary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Secondary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
    h.get_by_label("New MIDI clip").click();
    h.run();
    let project = h.state().session.project();
    assert_eq!(project.clips().count(), before + 1);
    let (id, made) = project
        .clips()
        .find(|(_, c)| c.as_midi().is_some())
        .expect("a MIDI clip");
    assert_eq!(made.node, NodeId(2));
    // Snapped to the beat nearest the click.
    assert_eq!(made.start, Tick(1920));
    assert_eq!(made.as_midi().unwrap().length, Tick(3840));
    assert_eq!(h.state().opened, [id]);
    assert!(h.state().timeline.selected().contains(&id));
}
