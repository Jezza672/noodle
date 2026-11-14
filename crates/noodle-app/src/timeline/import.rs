//! Adding audio files to the arrangement as clips.

use std::path::{Path, PathBuf};

use noodle_core::{Clip, Command, NodeId, Tick};
use noodle_io::Decoder;

use super::clips;
use crate::session::Session;

/// Clips for `files`, laid end to end from `at` on `track`, as one command.
/// Files that can't be used are left out and explained in the notice.
pub struct Import {
    pub command: Option<Command>,
    pub notice: Option<String>,
}

pub fn import(session: &Session, files: &[PathBuf], track: NodeId, at: Tick) -> Import {
    let project = session.project();
    let map = project.tempo_map();
    let mut next = project.next_clip_id();
    let mut start = at;
    let mut commands = Vec::new();
    let mut problems = Vec::new();
    for path in files {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        let info = match Decoder::open(path) {
            Ok(decoder) => decoder.info(),
            Err(error) => {
                problems.push(format!("{name}: {error}"));
                continue;
            }
        };
        let Some(frames) = info.frames.filter(|&frames| frames > 0) else {
            problems.push(format!("{name}: it doesn't say how long it is"));
            continue;
        };
        // The clip names the audio by its hash, so it needs no copy of the file
        // and is the same clip wherever the file is (ADR 0004).
        let hash = match session.samples().register_file(path) {
            Ok(hash) => hash,
            Err(error) => {
                problems.push(format!("{name}: {error}"));
                continue;
            }
        };
        let clip = stored_clip(track, start, &name, &hash, frames);
        start = clips::end_tick(map, start, frames, info.sample_rate);
        commands.push(Command::AddClip { id: next, clip });
        next.0 += 1;
    }
    let notice = (!problems.is_empty()).then(|| format!("Couldn't import {}", problems.join("; ")));
    let command = match commands.len() {
        0 => None,
        1 => commands.pop(),
        _ => Some(Command::Batch(commands)),
    };
    Import { command, notice }
}

/// A clip of `frames` of the stored audio `hash`, called `name`.
fn stored_clip(track: NodeId, start: Tick, name: &str, hash: &str, frames: u64) -> Clip {
    Clip {
        node: track,
        start,
        content: noodle_core::ClipContent::Audio(
            noodle_core::AudioClip::new(name, frames).with_hash(hash),
        ),
    }
}

/// A clip for audio that is already in the store, such as a sound from a
/// source, with the attribution that goes with it. `file` is where the store
/// has the audio now, read only for its length.
pub fn import_stored(
    session: &Session,
    file: &Path,
    name: &str,
    hash: &str,
    track: NodeId,
    at: Tick,
    credit: Option<String>,
) -> Import {
    let frames = Decoder::open(file)
        .ok()
        .and_then(|d| d.info().frames)
        .filter(|&frames| frames > 0);
    let Some(frames) = frames else {
        return Import {
            command: None,
            notice: Some(format!(
                "Couldn't import {name}: it doesn't say how long it is"
            )),
        };
    };
    noodle_core::register_audio_file(hash, file);
    let mut clip = stored_clip(track, at, name, hash, frames);
    if let noodle_core::ClipContent::Audio(audio) = &mut clip.content {
        audio.credit = credit;
    }
    let id = session.project().next_clip_id();
    Import {
        command: Some(Command::AddClip { id, clip }),
        notice: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Nodes, Session};

    #[test]
    fn an_imported_file_becomes_a_clip_that_names_its_audio_by_hash() {
        let dir = std::env::temp_dir().join(format!("noodle-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tone.wav");
        let tone: Vec<f32> = (0..4_410).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        noodle_io::write_wav(&file, &tone, 1, 44_100).unwrap();
        let session = Session::new(Nodes::all());
        let added = import(&session, &[file.clone()], NodeId(1), Tick::ZERO);
        let Some(Command::AddClip { clip, .. }) = added.command else {
            panic!("no clip: {:?}", added.notice);
        };
        let audio = clip.as_audio().unwrap();
        assert_eq!(audio.source, "tone.wav");
        let hash = audio.hash.clone().expect("a hash");
        assert_eq!(audio.length, 4_410);
        // It plays from where it is until a save copies it into the store.
        assert_eq!(audio.file(std::path::Path::new("/nowhere")), file);
        assert_eq!(noodle_core::registered_audio_file(&hash), Some(file));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
