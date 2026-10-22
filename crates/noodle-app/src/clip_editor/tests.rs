//! The clip editor driven like a user would: pointer and keyboard events into
//! a real [`Session`], checking the warp that results.

use egui::{Modifiers, Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use noodle_core::{Clip, ClipId, ClipWarp, Command, LoopRegion, Node, NodeId, Tick, WarpMarker};
use noodle_io::write_wav;

use super::*;
use crate::clip_panel::{self, ClipPanel};
use crate::session::{Edit, Nodes, Session};
use crate::testkit::{double_click, drag_between, press};
use crate::timeline::{TRACK_INPUT, TimelineState};

const RATE: u32 = 48_000;

struct Rig {
    session: Session,
    editor: ClipEditor,
    panel: ClipPanel,
    /// Where the editor and the panel learn about the file.
    timeline: TimelineState,
    clip: ClipId,
    _dir: tempfile::TempDir,
}

type H = Harness<'static, Rig>;

/// A decaying 200 Hz click every half second for four seconds.
fn clicks() -> Vec<f32> {
    let mut out = vec![0.0f32; 4 * RATE as usize];
    for hit in 0..8 {
        let start = hit * RATE as usize / 2;
        for i in 0..12_000 {
            let x = i as f32 / RATE as f32;
            out[start + i] += (x * 200.0 * std::f32::consts::TAU).sin() * (-x * 25.0).exp();
        }
    }
    out
}

/// A four second clip of clicks at 120 bpm, warped to the timeline's tempo
/// (so it sounds as recorded) if `warped`.
fn rig(warped: bool) -> H {
    let dir = tempfile::tempdir().unwrap();
    write_wav(&dir.path().join("hits.wav"), &clicks(), 1, RATE).unwrap();
    let mut session = Session::new(Nodes::all());
    // Quarter beats, as these tests were written for.
    session.set_snap(240);
    assert!(session.save_as(&dir.path().join("p.ron")));
    session.edit([Edit::Apply(Command::AddNode {
        id: NodeId(1),
        node: Node::new(TRACK_INPUT),
    })]);
    let clip = session.project().next_clip_id();
    let mut audio = Clip::audio(NodeId(1), Tick(0), "hits.wav", 4 * u64::from(RATE));
    let mut commands = Vec::new();
    if warped {
        let warp = session.project().next_warp_id();
        if let noodle_core::ClipContent::Audio(a) = &mut audio.content {
            a.warp = Some(warp);
        }
        commands.push(Command::AddWarp {
            id: warp,
            warp: ClipWarp::at_tempo(120.0),
        });
    }
    commands.push(Command::AddClip {
        id: clip,
        clip: audio,
    });
    session.edit([Edit::Apply(Command::Batch(commands))]);
    let mut editor = ClipEditor::default();
    editor.open(clip);
    let rig = Rig {
        session,
        editor,
        panel: ClipPanel::default(),
        timeline: TimelineState::default(),
        clip,
        _dir: dir,
    };
    let mut h = Harness::builder()
        .with_size(Vec2::new(900.0, 260.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let directory = rig.session.directory().map(std::path::Path::to_path_buf);
                let loaded = rig
                    .timeline
                    .source(ui.ctx(), directory.as_deref(), "hits.wav");
                let out = show(ui, &mut rig.editor, &rig.session, loaded.as_ref(), None);
                rig.session.edit(out.edits);
            },
            rig,
        );
    h.run();
    // The file's waveform and transients arrive from a background worker.
    for _ in 0..50 {
        if h.state().editor.geometry().is_some() {
            h.run();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h
}

fn warp(h: &H) -> ClipWarp {
    let rig = h.state();
    rig.session.project().warp_of(rig.clip).unwrap().1.clone()
}

fn geometry(h: &H) -> Geometry {
    h.state().editor.geometry().expect("drawn")
}

/// The screen position of a beat, on the flags' row.
fn flag(h: &H, beat: f64) -> Pos2 {
    let g = geometry(h);
    Pos2::new(g.x(beat), g.flags.center().y)
}

fn with_markers(h: &mut H, markers: &[(f64, f64)]) {
    let rig = h.state_mut();
    let command = rig
        .session
        .project()
        .warp_edit(rig.clip, |warp| {
            for &(seconds, beat) in markers {
                warp.set_marker(WarpMarker { seconds, beat });
            }
        })
        .unwrap();
    rig.session.edit([Edit::Apply(command)]);
    h.run();
}

#[test]
fn double_clicking_near_a_transient_pins_it_to_the_nearest_beat() {
    let mut h = rig(true);
    // The click at half a second is at beat 1; press a little after it.
    let g = geometry(&h);
    double_click(&mut h, Pos2::new(g.x(1.08), g.wave.center().y));
    let markers = warp(&h).markers;
    assert_eq!(markers.len(), 1, "{markers:?}");
    assert!((markers[0].seconds - 0.5).abs() < 0.01, "{markers:?}");
    assert_eq!(markers[0].beat, 1.0);
    assert_eq!(h.state().editor.selected(), vec![0]);
}

#[test]
fn double_clicking_away_from_a_transient_pins_the_moment_where_it_is() {
    let mut h = rig(true);
    let g = geometry(&h);
    // Beat 1.75 is a quarter of a second past the click at 0.5 s... and a
    // quarter before the next, far from both.
    double_click(&mut h, Pos2::new(g.x(1.75), g.wave.center().y));
    let markers = warp(&h).markers;
    assert_eq!(markers.len(), 1);
    assert!((markers[0].beat - 1.75).abs() < 0.02, "{markers:?}");
    assert!((markers[0].seconds - 0.875).abs() < 0.01, "{markers:?}");
}

#[test]
fn dragging_a_flag_moves_its_beat_and_stretches_the_audio_before_it() {
    let mut h = rig(true);
    with_markers(&mut h, &[(2.0, 4.0)]);
    let (from, to) = (flag(&h, 4.0), flag(&h, 5.0));
    drag_between(&mut h, Modifiers::NONE, from, to);
    let markers = warp(&h).markers;
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].seconds, 2.0);
    assert!((markers[0].beat - 5.0).abs() < 1e-9, "{markers:?}");
    // The whole drag is one undo step.
    h.state_mut().session.undo();
    assert_eq!(warp(&h).markers[0].beat, 4.0);
}

#[test]
fn a_flag_cant_be_dragged_across_its_neighbour() {
    let mut h = rig(true);
    with_markers(&mut h, &[(1.0, 2.0), (2.0, 4.0)]);
    let (from, to) = (flag(&h, 4.0), flag(&h, 1.0));
    drag_between(&mut h, Modifiers::NONE, from, to);
    let markers = warp(&h).markers;
    assert!(markers[1].beat > markers[0].beat, "{markers:?}");
    assert!(warp(&h).problem().is_none());
}

#[test]
fn dragging_a_flag_plays_the_warp_as_preview_until_it_is_let_go() {
    use egui::{Event, PointerButton};
    let mut h = rig(true);
    with_markers(&mut h, &[(2.0, 4.0)]);
    assert!(!h.state().session.warp_preview());
    let (from, middle, to) = (flag(&h, 4.0), flag(&h, 4.25), flag(&h, 4.5));
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    for p in [middle, to] {
        h.event(Event::PointerMoved(p));
        h.step();
    }
    assert!(h.state().session.warp_preview(), "preview is on mid-drag");
    h.event(Event::PointerButton {
        pos: to,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.step();
    h.run();
    assert!(
        !h.state().session.warp_preview(),
        "and off once it is let go"
    );
}

#[test]
fn a_drag_cut_off_by_the_clip_going_ends_and_turns_the_preview_off() {
    use egui::{Event, PointerButton};
    let mut h = rig(true);
    with_markers(&mut h, &[(2.0, 4.0)]);
    let (from, to) = (flag(&h, 4.0), flag(&h, 4.5));
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    h.event(Event::PointerMoved(to));
    h.step();
    assert!(h.state().session.warp_preview(), "preview is on mid-drag");
    // The clip goes (as an undo of its creation would do) with the button
    // still down: the flag's widget is never drawn again to end the drag.
    let id = h.state().clip;
    h.state_mut()
        .session
        .edit([Edit::Apply(Command::RemoveClip { id })]);
    h.run();
    assert!(!h.state().session.warp_preview(), "the drag was ended");
}

#[test]
fn delete_removes_the_picked_flag() {
    let mut h = rig(true);
    with_markers(&mut h, &[(1.0, 2.0), (2.0, 4.0)]);
    let at = flag(&h, 2.0);
    crate::testkit::click(&mut h, at, Modifiers::NONE);
    assert_eq!(h.state().editor.selected(), vec![0]);
    press(&mut h, at, Modifiers::NONE, egui::Key::Delete);
    let markers = warp(&h).markers;
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].seconds, 2.0);
}

#[test]
fn the_loop_brace_is_dragged_by_its_ends_in_whole_beats() {
    let mut h = rig(true);
    {
        let rig = h.state_mut();
        let command = rig
            .session
            .project()
            .warp_edit(rig.clip, |warp| {
                warp.loop_region = Some(LoopRegion {
                    start: 0.0,
                    end: 4.0,
                })
            })
            .unwrap();
        rig.session.edit([Edit::Apply(command)]);
    }
    h.run();
    let g = geometry(&h);
    let y = g.brace.center().y;
    drag_between(
        &mut h,
        Modifiers::NONE,
        Pos2::new(g.x(4.0), y),
        Pos2::new(g.x(6.1), y),
    );
    let region = warp(&h).loop_region.unwrap();
    assert_eq!((region.start, region.end), (0.0, 6.0));
    // Shorter than a beat isn't a loop: the end stays at the last good place.
    let g = geometry(&h);
    drag_between(
        &mut h,
        Modifiers::NONE,
        Pos2::new(g.x(6.0), y),
        Pos2::new(g.x(0.2), y),
    );
    let region = warp(&h).loop_region.unwrap();
    assert!(region.end - region.start >= noodle_core::MIN_LOOP_BEATS - 1e-9);
}

#[test]
fn an_unwarped_clip_offers_to_be_warped_from_the_tempo_of_its_hits() {
    let mut h = rig(false);
    assert!(
        h.state()
            .session
            .project()
            .warp_of(h.state().clip)
            .is_none()
    );
    h.get_by_label("Warp").click();
    h.run();
    let w = warp(&h);
    assert!(w.tempo_conversion);
    // Hits every half second are 120 bpm, and close enough for Beats.
    assert!(
        (w.tempo.points()[0].bpm - 120.0).abs() < 1.0,
        "{:?}",
        w.tempo
    );
    assert_eq!(w.mode, noodle_core::WarpMode::Beats);
}

#[test]
fn the_clip_panel_chooses_the_mode_and_the_switches() {
    let mut h = rig(true);
    let mut panel = Harness::builder()
        .with_size(Vec2::new(260.0, 400.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let directory = rig.session.directory().map(std::path::Path::to_path_buf);
                let loaded = rig
                    .timeline
                    .source(ui.ctx(), directory.as_deref(), "hits.wav");
                let edits =
                    clip_panel::show(ui, &mut rig.panel, &rig.session, rig.clip, loaded.as_ref());
                rig.session.edit(edits);
            },
            std::mem::replace(h.state_mut(), blank_rig()),
        );
    panel.run();
    panel.get_by_label("Follow tempo").click();
    panel.run();
    panel.get_by_label("Warp markers").click();
    panel.run();
    panel.get_by_label("Loop").click();
    panel.run();
    let rig = panel.state();
    let (_, w) = rig.session.project().warp_of(rig.clip).unwrap();
    assert!(!w.tempo_conversion);
    assert!(!w.use_markers);
    // Four seconds at 120 bpm: a loop of the eight beats.
    let region = w.loop_region.expect("loop is on");
    assert_eq!((region.start, region.end), (0.0, 8.0));
}

fn blank_rig() -> Rig {
    Rig {
        session: Session::new(Nodes::all()),
        editor: ClipEditor::default(),
        panel: ClipPanel::default(),
        timeline: TimelineState::default(),
        clip: ClipId(0),
        _dir: tempfile::tempdir().unwrap(),
    }
}

#[test]
fn the_playhead_goes_round_inside_the_loop_brace() {
    let mut warp = ClipWarp::at_tempo(120.0);
    warp.loop_region = Some(LoopRegion {
        start: 1.0,
        end: 3.0,
    });
    // Before the loop's end it is where it is; after, it goes round 1 to 3.
    assert_eq!(fold_into_loop(&warp, 2.5), 2.5);
    assert!((fold_into_loop(&warp, 3.5) - 1.5).abs() < 1e-9);
    assert!((fold_into_loop(&warp, 5.5) - 1.5).abs() < 1e-9);
    assert!((fold_into_loop(&warp, 4.99) - 2.99).abs() < 1e-9);
    warp.loop_region = None;
    assert_eq!(fold_into_loop(&warp, 5.5), 5.5);
}

fn apply(h: &mut H, command: Option<Command>) {
    h.state_mut()
        .session
        .edit([Edit::Apply(command.expect("a command"))]);
    h.run();
}

#[test]
fn quantise_moves_the_flag_that_plays_and_keeps_the_one_that_was_set() {
    let mut h = rig(true);
    with_markers(&mut h, &[(2.0, 4.1)]);
    let clip = h.state().clip;
    let command = clip_panel::change_quantise(h.state().session.project(), clip, |q| {
        q.grid = 1.0;
    });
    apply(&mut h, command);
    let w = warp(&h);
    assert_eq!(w.markers[0].beat, 4.1, "the stored marker is untouched");
    assert_eq!(w.marker_beat(0), Some(4.0));

    // The flag is where it plays, so that is where it is picked up.
    let (from, to) = (flag(&h, 4.0), flag(&h, 5.0));
    drag_between(&mut h, Modifiers::NONE, from, to);
    let w = warp(&h);
    assert_eq!(w.markers[0].beat, 5.0);
    assert!(w.quantise.is_some());

    // Baking keeps what played; turning quantise off brings the stored beat.
    let clip = h.state().clip;
    let baked = clip_panel::bake_quantise(h.state().session.project(), clip);
    apply(&mut h, baked);
    let w = warp(&h);
    assert_eq!((w.quantise.clone(), w.markers[0].beat), (None, 5.0));
}

#[test]
fn pinning_transients_marks_each_without_changing_the_sound() {
    let mut h = rig(true);
    let before = warp(&h);
    let onsets: Vec<f64> = (0..8).map(|i| i as f64 * 0.5).collect();
    let clip = h.state().clip;
    let command = clip_panel::pin_transients(h.state().session.project(), clip, &onsets);
    apply(&mut h, command);
    let after = warp(&h);
    assert_eq!(after.markers.len(), 8);
    for t in [0.0, 0.3, 0.77, 1.5, 3.9] {
        assert!((after.beat_at(t) - before.beat_at(t)).abs() < 1e-9);
    }
    // Nothing left to pin.
    let clip = h.state().clip;
    assert!(clip_panel::pin_transients(h.state().session.project(), clip, &onsets).is_none());
}

#[test]
fn a_groove_can_be_copied_or_followed_from_another_clip() {
    let mut h = rig(true);
    let source_clip = {
        let rig = h.state_mut();
        let warp = rig.session.project().next_warp_id();
        let clip = rig.session.project().next_clip_id();
        let mut audio = Clip::audio(NodeId(1), Tick(0), "other.wav", u64::from(RATE));
        if let noodle_core::ClipContent::Audio(a) = &mut audio.content {
            a.warp = Some(warp);
        }
        let mut swung = ClipWarp::at_tempo(120.0);
        swung.markers = vec![WarpMarker {
            seconds: 0.28125,
            beat: 0.5625,
        }];
        rig.session.edit([Edit::Apply(Command::Batch(vec![
            Command::AddWarp {
                id: warp,
                warp: swung,
            },
            Command::AddClip {
                id: clip,
                clip: audio,
            },
        ]))]);
        clip
    };
    let (this, other) = (h.state().clip, source_clip);
    let sources = clip_panel::groove_sources(h.state().session.project(), this);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].0, other);

    let command = clip_panel::take_groove(h.state().session.project(), this, other, true);
    apply(&mut h, command);
    let q = warp(&h).quantise.unwrap();
    assert_eq!(q.groove_from, Some(other));
    assert!((q.groove.unwrap().offset_at(0.5) - 0.0625).abs() < 1e-9);

    // The source's markers change; the follower's groove follows.
    let command = h
        .state()
        .session
        .project()
        .warp_edit(source_clip, |w| w.markers[0].beat = 0.5);
    apply(&mut h, command.ok());
    let q = warp(&h).quantise.unwrap();
    assert!(q.groove.unwrap().offset_at(0.5).abs() < 1e-9);

    // A copy does not follow.
    let command = clip_panel::take_groove(h.state().session.project(), this, other, false);
    apply(&mut h, command);
    assert_eq!(warp(&h).quantise.unwrap().groove_from, None);
}

#[test]
fn pinning_transients_with_quantise_on_quantises_the_new_markers() {
    let mut h = rig(true);
    let clip = h.state().clip;
    let command = clip_panel::change_quantise(h.state().session.project(), clip, |q| q.grid = 4.0);
    apply(&mut h, command);
    let onsets = [0.0, 0.7];
    let command = clip_panel::pin_transients(h.state().session.project(), clip, &onsets);
    apply(&mut h, command);
    let w = warp(&h);
    // Stored where they play without quantise; both quantise onto the bar
    // line, so only the first plays.
    assert!((w.markers[1].beat - 1.4).abs() < 1e-9);
    assert!(w.marker_plays(0) && !w.marker_plays(1));
}

/// A mouse press, a drag along `path` and a release, with the key `modifiers` held.
fn offset_of(h: &H) -> f64 {
    warp(h).grid_offset
}

#[test]
fn dragging_the_waveform_slides_the_beat_lines_against_the_audio() {
    let mut h = rig(true);
    assert_eq!(offset_of(&h), 0.0);
    let g = geometry(&h);
    let y = g.wave.center().y;
    // Grab the clip and pull it a quarter of a beat later: the lines go the
    // other way.
    drag_between(
        &mut h,
        Modifiers::NONE,
        Pos2::new(g.x(2.1), y),
        Pos2::new(g.x(2.35), y),
    );
    assert!((offset_of(&h) + 0.25).abs() < 0.01, "{}", offset_of(&h));
    assert!(warp(&h).markers.is_empty(), "no marker was added");
    // One undo step.
    h.state_mut().session.undo();
    assert_eq!(offset_of(&h), 0.0);
}

#[test]
fn dragging_the_ruler_moves_the_beat_lines_with_the_pointer() {
    let mut h = rig(true);
    let g = geometry(&h);
    let y = g.ruler.center().y;
    drag_between(
        &mut h,
        Modifiers::NONE,
        Pos2::new(g.x(2.0), y),
        Pos2::new(g.x(2.2), y),
    );
    assert!((offset_of(&h) - 0.2).abs() < 0.01, "{}", offset_of(&h));
}

/// Where in the file the clip's section starts, in frames.
fn window_start(h: &H) -> u64 {
    let project = h.state().session.project();
    project
        .clip(h.state().clip)
        .and_then(|c| c.as_audio())
        .map(|a| a.offset)
        .unwrap()
}

#[test]
fn sliding_the_beat_lines_slips_what_plays() {
    let mut h = rig(true);
    let clip = h.state().clip;
    let before = window_start(&h);
    let tempo = warp(&h).tempo.points()[0].bpm;
    let command = clip_panel::set_grid_offset(
        h.state().session.project(),
        clip,
        0.5,
        clip_panel::Span {
            rate: RATE,
            frames: 4 * u64::from(RATE),
        },
    );
    apply(&mut h, command);
    // Half a beat of audio.
    let rate = f64::from(RATE);
    let moved = (window_start(&h) as f64 - before as f64) / rate;
    let want = 0.5 * 60.0 / tempo;
    assert!((moved - want).abs() < 0.001, "{moved} vs {want}");
    // Undo puts both back in one step.
    h.state_mut().session.undo();
    assert_eq!(window_start(&h), before);
    assert_eq!(offset_of(&h), 0.0);
}

#[test]
fn markers_snap_to_the_slid_beat_lines() {
    let mut h = rig(true);
    let clip = h.state().clip;
    let command = clip_panel::set_grid_offset(
        h.state().session.project(),
        clip,
        0.3,
        clip_panel::Span::of(None),
    );
    apply(&mut h, command);
    with_markers(&mut h, &[(2.0, 4.3)]);
    // Pulled to about 5.1, it lands on the line at 5.05 (a quarter of a beat
    // from 4.3, the lines being 0.3 past whole quarters), not on 5.0.
    let (from, to) = (flag(&h, 4.3), flag(&h, 5.12));
    drag_between(&mut h, Modifiers::NONE, from, to);
    let beat = warp(&h).markers[0].beat;
    let off_grid = ((beat - 0.3) / 0.25).round() * 0.25 + 0.3 - beat;
    assert!(
        off_grid.abs() < 1e-9 && (beat - 5.05).abs() < 1e-9,
        "{beat}"
    );
}

#[test]
fn a_box_in_the_flags_row_picks_the_flags_in_it_and_they_move_together() {
    let mut h = rig(true);
    with_markers(&mut h, &[(0.5, 1.0), (1.0, 2.0), (1.5, 3.0), (3.0, 6.0)]);
    let g = geometry(&h);
    let y = g.flags.bottom() - 2.0;
    // From just before the first flag to just after the third.
    drag_between(
        &mut h,
        Modifiers::NONE,
        Pos2::new(g.x(0.6), y),
        Pos2::new(g.x(3.4), y),
    );
    assert_eq!(h.state().editor.selected(), vec![0, 1, 2]);
    assert_eq!(warp(&h).markers.len(), 4, "a box edits nothing");
    // Pulling one of them takes the rest along.
    let (a, b) = (flag(&h, 2.0), flag(&h, 2.5));
    drag_between(&mut h, Modifiers::NONE, a, b);
    let beats: Vec<f64> = warp(&h).markers.iter().map(|m| m.beat).collect();
    assert_eq!(beats, vec![1.5, 2.5, 3.5, 6.0]);
    // One undo step for the group.
    h.state_mut().session.undo();
    let beats: Vec<f64> = warp(&h).markers.iter().map(|m| m.beat).collect();
    assert_eq!(beats, vec![1.0, 2.0, 3.0, 6.0]);
    // Delete removes the group.
    let at = flag(&h, 2.0);
    press(&mut h, at, Modifiers::NONE, egui::Key::Delete);
    assert_eq!(warp(&h).markers.len(), 1);
    assert_eq!(warp(&h).markers[0].seconds, 3.0);
}

#[test]
fn shift_click_adds_a_flag_to_the_pick_and_takes_one_out() {
    let mut h = rig(true);
    with_markers(&mut h, &[(0.5, 1.0), (1.0, 2.0), (1.5, 3.0)]);
    let one = flag(&h, 1.0);
    crate::testkit::click(&mut h, one, Modifiers::NONE);
    let three = flag(&h, 3.0);
    crate::testkit::click(&mut h, three, Modifiers::SHIFT);
    assert_eq!(h.state().editor.selected(), vec![0, 2]);
    crate::testkit::click(&mut h, one, Modifiers::SHIFT);
    assert_eq!(h.state().editor.selected(), vec![2]);
    // Select all picks every flag.
    press(&mut h, one, Modifiers::COMMAND, egui::Key::A);
    assert_eq!(h.state().editor.selected(), vec![0, 1, 2]);
    press(&mut h, one, Modifiers::NONE, egui::Key::Escape);
    assert!(h.state().editor.selected().is_empty());
}

#[test]
fn double_clicking_a_transients_handle_pins_it_where_it_plays() {
    let mut h = rig(true);
    // Stretch the clip with a marker so a transient plays off the grid.
    with_markers(&mut h, &[(1.0, 2.3)]);
    let g = geometry(&h);
    let before = warp(&h);
    let beat = before.beat_at(1.5);
    assert!((beat * 4.0).fract().abs() > 0.01, "{beat} is on the grid");
    double_click(&mut h, Pos2::new(g.x(beat), g.handles.center().y));
    let after = warp(&h);
    assert_eq!(after.markers.len(), 2, "{:?}", after.markers);
    let pinned = after
        .markers
        .iter()
        .find(|m| (m.seconds - 1.5).abs() < 0.02);
    let pinned = pinned.expect("a marker on the transient");
    // Where it already played, not moved to the grid; so nothing changes.
    assert!((pinned.beat - beat).abs() < 0.02, "{pinned:?} vs {beat}");
    assert!((after.beat_at(3.0) - before.beat_at(3.0)).abs() < 0.05);
    assert_eq!(h.state().editor.selected().len(), 1);
}

#[test]
fn detect_finds_the_tempo_and_where_the_beat_falls() {
    let mut h = rig(true);
    // A wrong tempo and a slid grid to begin with.
    let clip = h.state().clip;
    let command = h
        .state()
        .session
        .project()
        .warp_edit(clip, |w| {
            w.tempo = noodle_core::TempoCurve::constant(90.0);
            w.grid_offset = 0.4;
        })
        .unwrap();
    apply(&mut h, Some(command));
    let mut panel = Harness::builder()
        .with_size(Vec2::new(260.0, 500.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let directory = rig.session.directory().map(std::path::Path::to_path_buf);
                let loaded = rig
                    .timeline
                    .source(ui.ctx(), directory.as_deref(), "hits.wav");
                let edits =
                    clip_panel::show(ui, &mut rig.panel, &rig.session, rig.clip, loaded.as_ref());
                rig.session.edit(edits);
            },
            std::mem::replace(h.state_mut(), blank_rig()),
        );
    // The analysis arrives from a background worker; Detect is on once it has.
    for _ in 0..100 {
        panel.run();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panel.get_by_label("Detect").click();
    panel.run();
    let rig = panel.state();
    let (_, w) = rig.session.project().warp_of(rig.clip).unwrap();
    assert!(
        (w.tempo.points()[0].bpm - 120.0).abs() < 1.0,
        "{:?}",
        w.tempo
    );
    // The clicks start on the beat, so the lines come back to (about) whole
    // beats.
    assert!(w.grid_offset.abs() < 0.06, "{}", w.grid_offset);
}

/// An unwarped rig whose clip plays only the first two seconds of its file,
/// once the file's waveform has arrived.
fn plain_rig() -> H {
    let mut h = rig(false);
    let rig = h.state_mut();
    let mut clip = rig.session.project().clip(rig.clip).unwrap().clone();
    if let noodle_core::ClipContent::Audio(audio) = &mut clip.content {
        audio.length = 2 * u64::from(RATE);
    }
    let id = rig.clip;
    rig.session
        .edit([Edit::Apply(Command::SetClip { id, clip })]);
    for _ in 0..50 {
        h.run();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h
}

fn offset(h: &H) -> u64 {
    let rig = h.state();
    rig.session
        .project()
        .clip(rig.clip)
        .unwrap()
        .as_audio()
        .unwrap()
        .offset
}

#[test]
fn an_unwarped_clip_shows_its_waveform_and_the_drag_slips_the_audio_under_it() {
    let mut h = plain_rig();
    assert_eq!(offset(&h), 0);
    let g = h.state().editor.plain_geometry().expect("drawn");
    let from = Pos2::new(g.window + g.pps * 1.0, g.wave.center().y);
    // The audio follows the pointer: half a second to the left brings the
    // file's later audio under the window.
    drag_between(
        &mut h,
        Modifiers::NONE,
        from,
        from - Vec2::new(g.pps * 0.5, 0.0),
    );
    let slipped = offset(&h);
    assert!(
        (slipped as i64 - i64::from(RATE) / 2).abs() < i64::from(RATE) / 50,
        "{slipped}"
    );
    // One undo step, and the clip itself stays where it is.
    h.state_mut().session.undo();
    assert_eq!(offset(&h), 0);
    let rig = h.state();
    assert_eq!(rig.session.project().clip(rig.clip).unwrap().start, Tick(0));
}

#[test]
fn pulling_the_end_edge_of_an_unwarped_clip_trims_it() {
    let mut h = plain_rig();
    let g = h.state().editor.plain_geometry().expect("drawn");
    let length = |h: &H| {
        let rig = h.state();
        rig.session
            .project()
            .clip(rig.clip)
            .unwrap()
            .as_audio()
            .unwrap()
            .length
    };
    assert_eq!(length(&h), 2 * u64::from(RATE));
    let y = g.wave.center().y;
    drag_between(
        &mut h,
        Modifiers::NONE,
        Pos2::new(g.window + g.pps * 2.0 - 1.0, y),
        Pos2::new(g.window + g.pps * 1.0, y),
    );
    let trimmed = length(&h);
    assert!(
        (trimmed as i64 - i64::from(RATE)).abs() < i64::from(RATE) / 50,
        "{trimmed}"
    );
    // Pulling an edge trims; it does not slip the audio.
    assert_eq!(offset(&h), 0);
    h.state_mut().session.undo();
    assert_eq!(length(&h), 2 * u64::from(RATE));
}

/// Lets the double-click window pass, so the next click starts a new count.
fn wait_out_double_click(h: &mut H) {
    for _ in 0..30 {
        h.step();
    }
}

fn gain_points(h: &H) -> Vec<(i64, f32)> {
    let rig = h.state();
    rig.session
        .project()
        .clip(rig.clip)
        .unwrap()
        .as_audio()
        .unwrap()
        .gain_points
        .iter()
        .map(|p| (p.tick.0, p.db))
        .collect()
}

#[test]
fn the_gain_line_takes_points_by_double_click_and_drags_and_removes_them() {
    let mut h = plain_rig();
    let g = h.state().editor.plain_geometry().expect("drawn");
    let at = |seconds: f32, db: f32| {
        Pos2::new(
            g.window + g.pps * seconds,
            super::gain_line::y_of(g.wave, db),
        )
    };
    // A double-click on the waveform adds a point where it is.
    double_click(&mut h, at(0.5, -12.0));
    let points = gain_points(&h);
    assert_eq!(points.len(), 1);
    // Half a second at 120 beats per minute is a beat.
    assert!((points[0].0 - 960).abs() <= 2, "{points:?}");
    assert!((points[0].1 + 12.0).abs() < 0.5, "{points:?}");
    wait_out_double_click(&mut h);
    double_click(&mut h, at(1.5, 6.0));
    assert_eq!(gain_points(&h).len(), 2);
    // Dragging the first point down and left moves it and nothing else, and
    // leaves the audio under the window where it was.
    drag_between(&mut h, Modifiers::NONE, at(0.5, -12.0), at(0.25, -30.0));
    let moved = gain_points(&h);
    assert_eq!(moved.len(), 2);
    assert!(
        (moved[0].0 - 480).abs() <= 2 && (moved[0].1 + 30.0).abs() < 0.5,
        "{moved:?}"
    );
    assert_eq!(offset(&h), 0);
    // A point stops at its neighbour.
    drag_between(&mut h, Modifiers::NONE, at(0.25, -30.0), at(1.9, -30.0));
    let stopped = gain_points(&h);
    assert!(stopped[0].0 <= stopped[1].0, "{stopped:?}");
    // One undo undoes the drag; double-clicking a point removes it.
    h.state_mut().session.undo();
    assert_eq!(gain_points(&h), moved);
    wait_out_double_click(&mut h);
    double_click(&mut h, at(0.25, -30.0));
    assert_eq!(gain_points(&h).len(), 1);
}

#[test]
fn the_toolbar_shows_the_title_and_a_close_button_inside_the_panel() {
    for warped in [false, true] {
        let h = rig(warped);
        h.get_by_label("Clip editor · hits.wav");
        let close = h.get_by_label("Close").rect();
        assert!(
            close.right() <= 900.0 && close.left() > 400.0,
            "{warped}: the Close button is at {close:?}"
        );
    }
}

#[test]
fn ctrl_scroll_zooms_the_clip_editor_for_any_clip() {
    for warped in [false, true] {
        let mut h = if warped { rig(true) } else { plain_rig() };
        let wave = if warped {
            geometry(&h).wave
        } else {
            h.state().editor.plain_geometry().expect("drawn").wave
        };
        let before = h.state().editor.view.ppb;
        crate::testkit::wheel_with(
            &mut h,
            wave.center(),
            egui::MouseWheelUnit::Line,
            Vec2::new(0.0, 3.0),
            Modifiers::COMMAND,
        );
        h.step();
        let after = h.state().editor.view.ppb;
        assert!(after > before * 1.2, "warped {warped}: {before} -> {after}");
    }
}

#[test]
fn dragging_the_waveform_moves_the_audio_on_screen_with_the_beat_lines_and_clip_start_fixed() {
    let mut h = rig(true);
    let rate = f64::from(RATE);
    let g0 = geometry(&h);
    let y = g0.wave.center().y;
    // Where a beat line, the clip's start and a click in the audio are.
    let line = |h: &H| {
        let g = geometry(h);
        g.x(4.0 + warp(h).grid_offset)
    };
    let start = |h: &H| {
        let g = geometry(h);
        let offset = window_start(h) as f64 / rate;
        g.x(warp(h).beat_at(offset))
    };
    let click = |h: &H| {
        let g = geometry(h);
        g.x(warp(h).beat_at(2.0))
    };
    let (line0, start0, click0) = (line(&h), start(&h), click(&h));
    drag_between(
        &mut h,
        Modifiers::NONE,
        Pos2::new(g0.x(2.35), y),
        Pos2::new(g0.x(2.1), y),
    );
    assert!((line(&h) - line0).abs() < 0.5, "the beat line stayed");
    assert!(
        (start(&h) - start0).abs() < 0.5,
        "the clip start stayed {} {} off {}",
        start0,
        start(&h),
        window_start(&h)
    );
    // The click followed the pointer by a quarter of a beat to the left.
    let moved = (click(&h) - click0) / geometry(&h).ppb;
    assert!((moved + 0.25).abs() < 0.02, "{moved}");
}
