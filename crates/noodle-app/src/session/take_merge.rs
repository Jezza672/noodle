//! Recording again over a take folder adds takes to it.
//!
//! A take folder's takes are windows into the clip's one file, so a new
//! recording can't be a window of the old file. Instead the old file and the
//! new recording are written out together as one new file, the old one
//! first, so every frame of the old file is where it was and the folder's
//! `offset`s and warp still point at the same audio. The folder's clip then
//! names the new file, and the new takes are windows that start after the
//! old ones. The old file is left alone, for undo and for any clip that
//! still plays it.

use std::collections::HashMap;
use std::path::Path;

use noodle_core::{Clip, ClipId, Command, NodeId, Project, Take, Tick};

use super::{Session, next_take_path};

/// What one recording adds to the folders it lands on.
pub(super) struct Recording<'a> {
    /// Where the take's part of the timeline begins.
    pub at: Tick,
    /// The new recording's file, relative to the project.
    pub relative: &'a str,
    /// The first frame of the file that is the take.
    pub skip: u64,
    /// How long the take lasts, in frames: every window of a folder it joins
    /// has to be this long.
    pub window: u64,
    /// The takes of the recording: `delta` frames on from `skip`.
    pub takes: &'a [Take],
}

/// The files made so far for one recording: the old file's name (as the
/// folder had it), and the combined file and how long the old one was.
pub(super) type Combined = HashMap<String, (String, u64)>;

/// Writes `old` and then `new` into `out`, and returns how many frames `old`
/// had. Fails if the two aren't the same kind of audio.
fn combine(old: &Path, new: &Path, out: &Path) -> Result<u64, String> {
    let first = noodle_io::decode_file(old).map_err(|e| e.to_string())?;
    let second = noodle_io::decode_file(new).map_err(|e| e.to_string())?;
    if first.channels != second.channels || first.sample_rate != second.sample_rate {
        return Err("the recordings differ in channels or rate".into());
    }
    let frames = (first.samples.len() / first.channels.max(1)) as u64;
    let mut samples = first.samples;
    samples.extend_from_slice(&second.samples);
    noodle_io::write_wav(out, &samples, first.channels, first.sample_rate)
        .map_err(|e| e.to_string())?;
    Ok(frames)
}

impl Session {
    /// The command that adds `recording`'s takes to the take folder on
    /// `track` that covers the same stretch (it starts where the recording
    /// does and is as long), or `None` if there isn't one or the files can't
    /// be joined. `combined` remembers the files already written, so a folder
    /// shared by several tracks' clips is joined once.
    pub(super) fn add_to_folder(
        &self,
        track: NodeId,
        recording: &Recording<'_>,
        combined: &mut Combined,
    ) -> Option<(ClipId, Command)> {
        let (id, clip) = folder_on(&self.project, track, recording)?;
        let audio = clip.as_audio()?;
        let base = self.path.as_deref()?;
        let dir = base.parent()?;
        let (name, old_frames) = if let Some(done) = combined.get(audio.key()) {
            done.clone()
        } else {
            let (out, relative) = next_take_path(base).ok()?;
            let frames = combine(&audio.file(dir), &dir.join(recording.relative), &out)
                .inspect_err(|_| {
                    let _ = std::fs::remove_file(&out);
                })
                .ok()?;
            combined.insert(audio.key().to_owned(), (relative.clone(), frames));
            (relative, frames)
        };
        let mut changed: Clip = clip.clone();
        let noodle_core::ClipContent::Audio(joined) = &mut changed.content else {
            return None;
        };
        joined.source = name;
        joined.hash = None;
        let folder = joined.takes.as_mut()?;
        // The new file's frame `old_frames + skip` plays what the old file's
        // frame `audio.offset` does, so a take's delta from there is this.
        let shift = old_frames + recording.skip - audio.offset;
        folder.takes.extend(recording.takes.iter().map(|take| Take {
            delta: shift + take.delta,
        }));
        Some((id, Command::SetClip { id, clip: changed }))
    }
}

/// The take folder on `track` that `recording` covers exactly.
fn folder_on<'a>(
    project: &'a Project,
    track: NodeId,
    recording: &Recording<'_>,
) -> Option<(ClipId, &'a Clip)> {
    project.clips_on(track).find(|(_, clip)| {
        clip.start == recording.at
            && clip
                .as_audio()
                .is_some_and(|a| a.takes.is_some() && a.length == recording.window)
    })
}
