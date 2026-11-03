//! S11: turn an audio phrase into MIDI.
//!
//! Full. Right-click the clip, Convert to MIDI: the notes arrive on a new
//! track that plays them with the Mono Synth. The conversion is destructive
//! (the notes are plain notes, nothing of the analysis is kept), so the late
//! change "change the detection sensitivity after notes were edited" is a
//! second conversion that replaces the MIDI clip: select both clips, then pick
//! the other setting. For live use, Pitch Track and To Notes do the same in
//! the graph (not routed).

use std::path::Path;

use egui::Modifiers;
use noodle_core::{Clip, Command, Node, Project, Tick};
use noodle_nodes::TRACK_INPUT_ID;

use super::super::handle::{Handle, Probe};
use super::super::{LateChange, Route, Tag};
use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

const RATE: u32 = 48_000;

/// A hummed C4 and E4 with a rest between, as `hum.wav`, on a track.
fn fixture(dir: &Path) -> App {
    let voice = |hz: f32, seconds: f32| -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                0.3 * (std::f32::consts::TAU * hz * t).sin()
                    + 0.15 * (std::f32::consts::TAU * 2.0 * hz * t).sin()
            })
            .collect()
    };
    let rest = |seconds: f32| vec![0.0f32; (seconds * RATE as f32) as usize];
    let mut samples = rest(0.1);
    samples.extend(voice(261.63, 0.5));
    samples.extend(rest(0.2));
    samples.extend(voice(329.63, 0.5));
    samples.extend(rest(0.1));
    noodle_io::write_wav(&dir.join("hum.wav"), &samples, 1, RATE).expect("the audio is written");
    let mut session = virtual_session(dir);
    let track = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: track,
        node: Node::new(TRACK_INPUT_ID),
    })]);
    let id = session.project().next_clip_id();
    let clip = Clip::audio(track, Tick(0), "hum.wav", samples.len() as u64);
    session.edit([Edit::Apply(Command::AddClip { id, clip })]);
    finished(session, dir)
}

fn audio_clip(p: &Project) -> noodle_core::ClipId {
    p.clips()
        .find(|(_, c)| c.as_audio().is_some())
        .map(|(id, _)| id)
        .expect("the audio clip")
}

fn midi_clip(p: &Project) -> Option<noodle_core::ClipId> {
    p.clips()
        .find(|(_, c)| c.as_midi().is_some())
        .map(|(id, _)| id)
}

fn convert(h: &mut Handle, setting: &str) {
    let id = audio_clip(h.probe().project());
    let at = h.clip_rect(id).center();
    h.right_click(at);
    let menu = h.find_like("Convert to MIDI");
    h.click(menu, Modifiers::NONE);
    let pick = h.find(setting);
    h.click(pick, Modifiers::NONE);
    h.probe().settle();
}

fn notes_of(p: &Project) -> Result<usize, String> {
    let id = midi_clip(p).ok_or("there is no MIDI clip")?;
    Ok(p.clip(id)
        .and_then(|c| c.as_midi())
        .map_or(0, |m| m.notes.len()))
}

fn steps(h: &mut Handle, _size: usize) {
    h.step("convert the phrase to MIDI");
    convert(h, "Normal");
}

fn done(p: &mut Probe, _size: usize) -> Result<(), String> {
    match notes_of(p.project())? {
        2 => Ok(()),
        n => Err(format!("expected two notes, found {n}")),
    }
}

fn reconvert(h: &mut Handle, _size: usize) {
    h.step("select the audio and the MIDI clip");
    let p = h.probe().project().clone();
    let audio = h.clip_rect(audio_clip(&p)).center();
    h.click(audio, Modifiers::NONE);
    let midi = h.clip_rect(midi_clip(&p).expect("the MIDI clip")).center();
    h.click(midi, Modifiers::SHIFT);
    h.step("convert again at another setting, replacing the clip");
    h.right_click(audio);
    let menu = h.find_like("Convert to MIDI");
    h.click(menu, Modifiers::NONE);
    let pick = h.find("More notes");
    h.click(pick, Modifiers::NONE);
    h.probe().settle();
}

fn reconverted(p: &mut Probe, _size: usize) -> Result<(), String> {
    let clips = p
        .project()
        .clips()
        .filter(|(_, c)| c.as_midi().is_some())
        .count();
    if clips != 1 {
        return Err(format!(
            "expected the MIDI clip replaced, found {clips} MIDI clips"
        ));
    }
    notes_of(p.project()).map(|_| ())
}

pub fn route() -> Route {
    Route {
        story: "S11",
        tag: Tag::Default,
        name: "convert a hummed phrase to MIDI",
        fixture,
        size: None,
        steps,
        done,
        late: &[LateChange {
            name: "change the detection sensitivity after the notes were edited",
            steps: reconvert,
            done: reconverted,
            keep: &[],
        }],
    }
}
