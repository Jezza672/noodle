//! A saved project with one audio clip on a track, for routes that start
//! from recorded audio.

use std::path::Path;

use noodle_core::{Clip, ClipId, Command, Node, Project, Tick};
use noodle_io::write_wav;
use noodle_nodes::TRACK_INPUT_ID;

use super::{finished, virtual_session};
use crate::app::App;
use crate::session::{Edit, Session};

const RATE: u32 = 48_000;

/// Six seconds of clicks at `bpm`, on a track, as `phrase.wav`.
pub fn clicks_at(dir: &Path, bpm: f32) -> App {
    let mut session = virtual_session(dir);
    add_phrase(&mut session, dir, bpm);
    finished(session, dir)
}

/// Six seconds of clicks at `bpm`, on a new track of `session`, as
/// `phrase.wav` in `dir`: the clip that gives a project a length to export.
pub fn add_phrase(session: &mut Session, dir: &Path, bpm: f32) {
    let frames = 6 * RATE as usize;
    let gap = (60.0 / bpm * RATE as f32) as usize;
    let samples: Vec<f32> = (0..frames)
        .map(|i| {
            let k = i % gap;
            if k < 480 {
                0.8 * (-(k as f32) / 120.0).exp() * (k as f32 * 0.4).sin()
            } else {
                0.0
            }
        })
        .collect();
    write_wav(&dir.join("phrase.wav"), &samples, 1, RATE).expect("the audio is written");
    let track = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: track,
        node: Node::new(TRACK_INPUT_ID),
    })]);
    let id = session.project().next_clip_id();
    let clip = Clip::audio(track, Tick(0), "phrase.wav", frames as u64);
    session.edit([Edit::Apply(Command::AddClip { id, clip })]);
}

/// A story that exports needs something to export: fails if the project has
/// no audio clip, so a fixture that lost its clip cannot pass.
pub fn has_audio_clip(p: &Project) -> Result<(), String> {
    if p.timeline_clips().any(|(_, c)| c.as_audio().is_some()) {
        Ok(())
    } else {
        Err("the project has no audio clip to export: the fixture is not the story's".into())
    }
}

pub fn clip_of(p: &Project) -> ClipId {
    p.clips()
        .find(|(_, c)| c.as_audio().is_some())
        .map(|(id, _)| id)
        .expect("the audio clip")
}
