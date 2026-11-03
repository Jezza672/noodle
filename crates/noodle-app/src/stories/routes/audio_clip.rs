//! A saved project with one audio clip on a track, for routes that start
//! from recorded audio.

use std::path::Path;

use noodle_core::{Clip, ClipId, Command, Node, Project, Tick};
use noodle_io::write_wav;
use noodle_nodes::TRACK_INPUT_ID;

use super::{finished, virtual_session};
use crate::app::App;
use crate::session::Edit;

const RATE: u32 = 48_000;

/// Six seconds of clicks at `bpm`, on a track, as `phrase.wav`.
pub fn clicks_at(dir: &Path, bpm: f32) -> App {
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
    let mut session = virtual_session(dir);
    let track = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode {
        id: track,
        node: Node::new(TRACK_INPUT_ID),
    })]);
    let id = session.project().next_clip_id();
    let clip = Clip::audio(track, Tick(0), "phrase.wav", frames as u64);
    session.edit([Edit::Apply(Command::AddClip { id, clip })]);
    finished(session, dir)
}

pub fn clip_of(p: &Project) -> ClipId {
    p.clips()
        .find(|(_, c)| c.as_audio().is_some())
        .map(|(id, _)| id)
        .expect("the audio clip")
}
