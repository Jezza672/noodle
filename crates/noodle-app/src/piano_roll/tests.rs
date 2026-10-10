//! The piano roll driven like a user would: pointer and keyboard events into
//! a real [`Session`], checking the notes that result.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Vec2};
use egui_kittest::Harness;
use noodle_core::{Clip, ClipId, Command, MidiNote, Node, NodeId, Tick};

use super::*;
use crate::session::{Nodes, Session};
use crate::timeline::TRACK_INPUT;

struct Rig {
    session: Session,
    roll: PianoRoll,
    clip: ClipId,
    auditioned: Vec<(u8, bool)>,
}

type H = Harness<'static, Rig>;

fn note(start: i64, length: i64, key: u8) -> MidiNote {
    MidiNote::new(Tick(start), Tick(length), key)
}

/// A four beat clip (3840 ticks) holding `notes`, open in the roll.
fn rig(notes: &[MidiNote]) -> H {
    let mut session = Session::new(Nodes::all());
    session.edit([Edit::Apply(Command::AddNode {
        id: NodeId(1),
        node: Node::new(TRACK_INPUT),
    })]);
    let clip = session.project().next_clip_id();
    let mut midi = Clip::midi(NodeId(1), Tick(960), Tick(3840));
    let ClipContent::Midi(content) = &mut midi.content else {
        unreachable!()
    };
    content.notes = notes.to_vec();
    session.edit([Edit::Apply(Command::AddClip {
        id: clip,
        clip: midi,
    })]);
    let mut roll = PianoRoll::default();
    roll.open(clip);
    let rig = Rig {
        session,
        roll,
        clip,
        auditioned: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(Vec2::new(900.0, 420.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let out = show(ui, &mut rig.roll, &rig.session, None);
                rig.session.edit(out.edits);
                rig.auditioned.extend(out.audition);
            },
            rig,
        );
    h.run();
    h
}

/// The clip's notes as stored, with their ids.
fn stored_notes(h: &H) -> Vec<MidiNote> {
    let rig = h.state();
    rig.session
        .project()
        .clip(rig.clip)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .clone()
}

/// The clip's notes with the ids zeroed, to compare with `note`.
fn notes_of(h: &H) -> Vec<MidiNote> {
    let mut notes = stored_notes(h);
    for note in &mut notes {
        note.id = 0;
    }
    notes
}

fn geometry(h: &H) -> Geometry {
    h.state().roll.geometry().expect("drawn")
}

/// The screen position of a tick and key, in the middle of its row.
fn at(h: &H, tick: i64, key: u8) -> Pos2 {
    let g = geometry(h);
    Pos2::new(g.x(tick), g.y(key))
}

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

fn drag_between(h: &mut H, modifiers: Modifiers, from: Pos2, to: Pos2) {
    let path: Vec<Pos2> = (0..=6)
        .map(|i| from + (to - from) * (i as f32 / 6.0))
        .collect();
    drag(h, modifiers, &path);
}

fn click(h: &mut H, pos: Pos2) {
    drag(h, Modifiers::NONE, &[pos]);
}

#[test]
fn dragging_on_empty_space_draws_a_note_snapped_to_the_grid() {
    let mut h = rig(&[]);
    // From just after a sixteenth to a bit over three of them: snaps to
    // 240..960 on key 64.
    let from = at(&h, 250, 64);
    let to = at(&h, 940, 64);
    drag_between(&mut h, Modifiers::NONE, from, to);
    assert_eq!(notes_of(&h), [note(240, 720, 64)]);
    assert_eq!(h.state().roll.selected().len(), 1);
}

#[test]
fn a_whole_gesture_is_one_undo_step() {
    let mut h = rig(&[]);
    let (from, to) = (at(&h, 20, 60), at(&h, 960, 60));
    drag_between(&mut h, Modifiers::NONE, from, to);
    assert_eq!(notes_of(&h).len(), 1);
    h.state_mut().session.undo();
    assert!(notes_of(&h).is_empty(), "one undo removes the drawn note");
}

#[test]
fn dragging_a_note_moves_it_in_time_and_pitch() {
    let mut h = rig(&[note(480, 480, 60), note(2000, 240, 70)]);
    let from = at(&h, 700, 60);
    let to = at(&h, 700 + 480, 62);
    drag_between(&mut h, Modifiers::NONE, from, to);
    assert_eq!(notes_of(&h), [note(960, 480, 62), note(2000, 240, 70)]);
}

#[test]
fn alt_moves_without_snapping() {
    let mut h = rig(&[note(480, 480, 60)]);
    let from = at(&h, 700, 60);
    let to = at(&h, 700 + 100, 60);
    drag_between(&mut h, Modifiers::ALT, from, to);
    let moved = notes_of(&h)[0];
    assert!((moved.start.0 - 580).abs() <= 8, "{moved:?}");
}

#[test]
fn dragging_the_end_of_a_note_resizes_it() {
    let mut h = rig(&[note(480, 480, 60)]);
    let g = geometry(&h);
    let from = Pos2::new(g.x(960) - 2.0, g.y(60));
    let to = Pos2::new(g.x(1440), g.y(60));
    drag_between(&mut h, Modifiers::NONE, from, to);
    assert_eq!(notes_of(&h), [note(480, 960, 60)]);
}

#[test]
fn a_selected_group_moves_together() {
    let mut h = rig(&[note(0, 240, 60), note(480, 240, 64), note(960, 240, 67)]);
    // Select the first two with shift, then drag one.
    let first = at(&h, 100, 60);
    let second = at(&h, 580, 64);
    click(&mut h, first);
    drag(&mut h, Modifiers::SHIFT, &[second]);
    assert_eq!(h.state().roll.selected().len(), 2);
    let target = at(&h, 580 + 480, 64);
    drag_between(&mut h, Modifiers::NONE, second, target);
    assert_eq!(
        notes_of(&h),
        [note(480, 240, 60), note(960, 240, 64), note(960, 240, 67)]
    );
}

#[test]
fn delete_removes_the_selected_notes_and_undo_restores_them() {
    let mut h = rig(&[note(0, 240, 60), note(480, 240, 64)]);
    let pos = at(&h, 100, 60);
    click(&mut h, pos);
    assert_eq!(h.state().roll.selected().len(), 1);
    h.key_press(Key::Delete);
    h.run();
    assert_eq!(notes_of(&h), [note(480, 240, 64)]);
    assert!(h.state().roll.selected().is_empty());
    h.state_mut().session.undo();
    assert_eq!(notes_of(&h).len(), 2);
}

#[test]
fn arrows_transpose_and_nudge() {
    let mut h = rig(&[note(480, 240, 60)]);
    let pos = at(&h, 540, 60);
    click(&mut h, pos);
    h.key_press(Key::ArrowUp);
    h.run();
    h.key_press(Key::ArrowRight);
    h.run();
    assert_eq!(notes_of(&h), [note(720, 240, 61)]);
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.run();
    assert_eq!(notes_of(&h)[0].key, 49);
}

#[test]
fn double_clicking_empty_space_adds_a_note() {
    let mut h = rig(&[]);
    let pos = at(&h, 1000, 65);
    drag(&mut h, Modifiers::NONE, &[pos]);
    drag(&mut h, Modifiers::NONE, &[pos]);
    // Two quick clicks may not register as a double click in the harness;
    // send the double click explicitly if they did not.
    if notes_of(&h).is_empty() {
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
    }
    let notes = notes_of(&h);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].key, 65);
    assert_eq!(notes[0].start.0 % 240, 0);
}

#[test]
fn dragging_a_velocity_bar_sets_the_velocity() {
    let mut h = rig(&[note(480, 480, 60)]);
    let g = geometry(&h);
    let x = g.x(480) + 2.5;
    let bottom = g.velocity.bottom() - 4.0;
    let height = g.velocity.height() - 8.0;
    // From the top of the 0.8 bar to the middle of the lane.
    drag_between(
        &mut h,
        Modifiers::NONE,
        Pos2::new(x, bottom - height * 0.8),
        Pos2::new(x, bottom - height * 0.25),
    );
    let velocity = notes_of(&h)[0].velocity;
    assert!((velocity - 0.25).abs() < 0.02, "{velocity}");
    h.state_mut().session.undo();
    assert_eq!(notes_of(&h)[0].velocity, 0.8);
}

#[test]
fn pressing_a_key_on_the_keyboard_plays_it() {
    let mut h = rig(&[]);
    let g = geometry(&h);
    let pos = Pos2::new(g.keyboard.center().x, g.y(62));
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run();
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
    assert_eq!(h.state().auditioned, [(62, true), (62, false)]);
}

#[test]
fn a_key_held_when_the_roll_closes_is_let_go() {
    let mut h = rig(&[]);
    let g = geometry(&h);
    let pos = Pos2::new(g.keyboard.center().x, g.y(62));
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run();
    assert_eq!(h.state().auditioned, [(62, true)]);
    // The clip is deleted under the held key.
    let clip = h.state().clip;
    h.state_mut()
        .session
        .edit([Edit::Apply(Command::RemoveClip { id: clip })]);
    h.run();
    assert_eq!(h.state_mut().roll.take_release(), Some(62));
    assert_eq!(h.state_mut().roll.take_release(), None);
}

#[test]
fn the_roll_closes_when_its_clip_goes() {
    let mut h = rig(&[note(0, 240, 60)]);
    let clip = h.state().clip;
    h.state_mut()
        .session
        .edit([Edit::Apply(Command::RemoveClip { id: clip })]);
    h.run();
    assert_eq!(h.state().roll.clip(), None);
}

#[test]
fn key_names() {
    assert_eq!(key_name(60), "C4");
    assert_eq!(key_name(69), "A4");
    assert_eq!(key_name(0), "C-1");
    assert_eq!(key_name(127), "G9");
    assert!(is_black(61) && !is_black(60));
}

#[test]
fn the_selection_follows_its_notes_through_undo_and_redo() {
    let mut h = rig(&[note(0, 240, 60), note(480, 240, 64), note(960, 240, 67)]);
    let ids: Vec<u32> = stored_notes(&h).iter().map(|n| n.id).collect();
    assert_eq!(ids.len(), 3);
    assert!(
        ids.iter()
            .all(|id| ids.iter().filter(|i| *i == id).count() == 1)
    );
    // Select the last note, then delete the first: the last is now at
    // index 1, and undoing puts the first back, at index 0.
    let first = at(&h, 100, 60);
    let last = at(&h, 1060, 67);
    click(&mut h, first);
    h.key_press(Key::Delete);
    h.run();
    click(&mut h, last);
    assert_eq!(h.state().roll.selected(), &BTreeSet::from([ids[2]]));
    h.state_mut().session.undo();
    h.run();
    assert_eq!(stored_notes(&h).len(), 3);
    assert_eq!(
        h.state().roll.selected(),
        &BTreeSet::from([ids[2]]),
        "still the same note, though its index changed"
    );
    // A note that undo removes drops out of the selection.
    let first = at(&h, 100, 60);
    click(&mut h, first);
    assert_eq!(h.state().roll.selected(), &BTreeSet::from([ids[0]]));
    h.state_mut().session.undo();
    h.run();
    assert!(h.state().roll.selected().is_empty());
}
