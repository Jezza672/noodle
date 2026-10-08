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
        let clip = Clip::audio(track, start, source_name(session.directory(), path), frames);
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

/// How a clip refers to `path`: relative to the project's folder when it is
/// inside it, so the project moves with its audio, and absolute otherwise.
fn source_name(directory: Option<&Path>, path: &Path) -> String {
    directory
        .and_then(|dir| path.strip_prefix(dir).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_inside_the_project_folder_are_named_relative_to_it() {
        let dir = Path::new("/music/song");
        assert_eq!(
            source_name(Some(dir), Path::new("/music/song/a/b.wav")),
            "a/b.wav"
        );
        assert_eq!(
            source_name(Some(dir), Path::new("/elsewhere/b.wav")),
            "/elsewhere/b.wav"
        );
        assert_eq!(source_name(None, Path::new("/x/b.wav")), "/x/b.wav");
    }
}
