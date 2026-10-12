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
    assert_eq!(h.state().editor.selected(), Some(0));
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
fn delete_removes_the_picked_flag() {
    let mut h = rig(true);
    with_markers(&mut h, &[(1.0, 2.0), (2.0, 4.0)]);
    let at = flag(&h, 2.0);
    crate::testkit::click(&mut h, at, Modifiers::NONE);
    assert_eq!(h.state().editor.selected(), Some(0));
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
