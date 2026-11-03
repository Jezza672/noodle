//! S01: comp a vocal from multiple takes.
//!
//! The takes are recorded for real: the fixture is an empty project with a short loop,
//! and the virtual input plays eight different tones, one per pass, into the
//! recorder (see `testkit::virtual_input`). The loop is a bar long, so
//! the recording takes about seventeen seconds. From there on everything is the
//! user's: open the takes, swipe the best phrases, change their mind.

use egui::Pos2;
use noodle_core::{Clip, ClipId, Command, Project, Tick};
use noodle_io::write_wav;
use std::path::Path;

use super::super::handle::{Handle, Probe};
use super::super::{Keep, LateChange, Route, Tag};
use super::{finished_recording, virtual_session};
use crate::app::App;
use crate::session::Edit;

/// One loop cycle: a bar of four beats at 120 bpm.
const CYCLE: u64 = 96_000;
const TAKES: u64 = 8;

/// An empty project with a loop over its first bar, with the virtual
/// input set to play a different tone on each pass.
fn eight_takes(dir: &Path) -> App {
    let tone = |take: u64| {
        let hz = 220.0 * 2f32.powf(take as f32 / 12.0);
        (0..CYCLE).map(move |i| 0.4 * (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin())
    };
    // Two passes more than the takes, since the input starts a moment before
    // the recording does.
    let samples: Vec<f32> = (0..TAKES + 2).flat_map(tone).collect();
    let wav = dir.join("input.wav");
    write_wav(&wav, &samples, 1, 48_000).expect("the input is written");
    let mut session = virtual_session(dir);
    session.edit([Edit::Apply(Command::SetLoop(noodle_core::TransportLoop {
        on: true,
        start: Tick(0),
        end: Tick(3840),
    }))]);
    finished_recording(session, dir, &wav)
}

/// Audio clips on the project: the recorded folder, and a later recording.
fn audio_clips(p: &Project) -> usize {
    p.clips().filter(|(_, c)| c.as_audio().is_some()).count()
}

pub fn route() -> Route {
    Route {
        story: "S01",
        tag: Tag::Default,
        name: "swipe two phrases from eight takes",
        fixture: eight_takes,
        size: None,
        steps,
        done,
        late: &[
            LateChange {
                name: "record a ninth take",
                steps: ninth,
                done: ninth_done,
                keep: &[Keep {
                    name: "the first phrase",
                    of: first_phrase,
                }],
            },
            LateChange {
                name: "re-pick the verse 2 phrase",
                steps: repick,
                done: repicked,
                keep: &[Keep {
                    name: "the first phrase",
                    of: first_phrase,
                }],
            },
        ],
    }
}

fn folder(p: &Project) -> (ClipId, &Clip) {
    p.clips()
        .find(|(_, c)| c.as_audio().is_some_and(|a| a.takes.is_some()))
        .expect("the take folder")
}

/// The takes the comp plays, in order, with where each starts as a fraction
/// of the cycle.
fn comp(p: &Project) -> Vec<(usize, f64)> {
    let (_, clip) = folder(p);
    let takes = clip.as_audio().and_then(|a| a.takes.as_ref()).unwrap();
    takes
        .comp
        .iter()
        .map(|s| (s.take, s.at as f64 / CYCLE as f64))
        .collect()
}

fn first_phrase(p: &Project) -> String {
    format!("{:?}", comp(p).first())
}

/// Drags across take lane `k` from `from` to `to`, as fractions of the clip.
fn swipe(h: &mut Handle, id: ClipId, k: usize, from: f32, to: f32) {
    // Lanes below the fold are scrolled to, as a user must.
    // Scroll only when the lane is off screen or cut short: towards it, which
    // is up when the lanes below it are showing.
    let mut tries = 0;
    while {
        h.probe().settle();
        h.probe()
            .try_take_lane(id, k)
            .is_none_or(|r| r.height() < 30.0)
    } && tries < 10
    {
        let below = (k + 1..8).any(|j| h.probe().try_take_lane(id, j).is_some());
        let dy = if below { 60.0 } else { -60.0 };
        h.wheel(
            Pos2::new(600.0, 200.0),
            egui::MouseWheelUnit::Point,
            egui::Vec2::new(0.0, dy),
        );
        tries += 1;
    }
    h.probe().settle();
    let lane = h.take_lane(id, k);
    let y = lane.center().y;
    let x = |f: f32| lane.left() + lane.width() * f;
    h.drag_between(
        egui::Modifiers::ALT,
        Pos2::new(x(from), y),
        Pos2::new(x(to), y),
    );
}

fn folder_id(h: &mut Handle) -> ClipId {
    folder(h.probe().project()).0
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("add a track");
    let add = h.find("+ Track");
    h.drag_playing(&[add]);
    h.step("arm the track");
    let arm = h.find("RA");
    h.drag_playing(&[arm]);
    h.step("record eight passes of the loop");
    let record = h.find("⏺");
    h.drag_playing(&[record]);
    // The input plays in real time, so the passes take as long as they take.
    h.probe().hear(std::time::Duration::from_millis(17600));
    h.waited(17.6);
    h.step("stop");
    h.press_playing(
        Pos2::new(640.0, 300.0),
        egui::Modifiers::NONE,
        egui::Key::Space,
    );
    h.step("turn the audio engine off");
    let engine = h.find("Audio");
    h.drag_playing(&[engine]);
    h.probe().settle();
    let id = folder_id(h);
    h.step("open the takes");
    h.probe().settle();
    let at = h.clip_rect(id).center();
    h.right_click(at);
    let show = h.find("Show takes");
    h.click(show, egui::Modifiers::NONE);
    h.step("swipe the first phrase from take 4");
    swipe(h, id, 3, 0.0, 0.5);
    h.step("swipe the second phrase from take 7");
    swipe(h, id, 6, 0.5, 1.0);
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    let c = comp(p.project());
    let takes: Vec<usize> = c.iter().map(|s| s.0).collect();
    if takes != [3, 6] {
        return Err(format!("the comp plays takes {takes:?}, wanted [3, 6]"));
    }
    if !(0.4..0.6).contains(&c[1].1) {
        return Err(format!("the join is at {:.2} of the cycle", c[1].1));
    }
    Ok(())
}

fn repick(h: &mut Handle, _size: usize) {
    let id = folder_id(h);
    h.step("swipe take 2 over the second phrase");
    swipe(h, id, 1, 0.5, 1.0);
}

fn repicked(p: &mut Probe, _size: usize) -> Result<(), String> {
    let takes: Vec<usize> = comp(p.project()).iter().map(|s| s.0).collect();
    if takes != [3, 1] {
        return Err(format!("the comp plays takes {takes:?}, wanted [3, 1]"));
    }
    Ok(())
}

/// Records again over the loop, the way the first eight passes were. The
/// virtual input starts its file again when the engine opens.
fn ninth(h: &mut Handle, _size: usize) {
    h.step("record another take");
    let record = h.find("⏺");
    h.drag_playing(&[record]);
    h.probe().hear(std::time::Duration::from_millis(2600));
    h.waited(2.6);
    h.step("stop");
    h.press_playing(
        Pos2::new(640.0, 300.0),
        egui::Modifiers::NONE,
        egui::Key::Space,
    );
    h.step("turn the audio engine off");
    let engine = h.find("Audio");
    h.drag_playing(&[engine]);
    h.probe().settle();
}

fn ninth_done(p: &mut Probe, _size: usize) -> Result<(), String> {
    // The app does not yet add a recording to the folder it records over
    // (a new clip holds it), so the take is counted as a second clip.
    let n = audio_clips(p.project());
    if n < 2 {
        return Err(format!("{n} audio clip after recording again"));
    }
    Ok(())
}
