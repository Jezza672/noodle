//! Standard MIDI Files on the timeline: import as clips on new tracks, and
//! export of clips, tracks or the arrangement.

use std::path::{Path, PathBuf};

use noodle_core::group::PORT_NAME;
use noodle_core::{Clip, ClipContent, ClipId, Command, NodeId, Tick, Value};

use super::add_track;
use super::import::Import;
use crate::session::Session;

/// Whether `path` is named like a MIDI file.
pub fn is_midi(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"))
}

/// Tracks and clips for the MIDI files in `files`, all as one command. The
/// first file track goes on `first` if there is one; every other gets a new
/// track. Clips start at `at`.
///
/// The file's tempo and meter are used when the project has no clips yet
/// (there is nothing to put out of step); otherwise the project's stay, and
/// the notice says so.
pub fn import(session: &Session, files: &[PathBuf], first: Option<NodeId>, at: Tick) -> Import {
    let project = session.project();
    let empty = project.timeline_clips().next().is_none();
    let mut scratch = project.clone();
    let mut next = project.next_clip_id();
    let mut first = first;
    let mut commands = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut tempo = None;
    let mut kept_tempo = false;
    let mut added = 0;
    for path in files {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        let imported = std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| noodle_io::read_smf(&bytes).map_err(|e| e.to_string()));
        let file = match imported {
            Ok(file) => noodle_io::import_smf(&file),
            Err(error) => {
                notes.push(format!("Couldn't import {name}: {error}"));
                continue;
            }
        };
        if file.tracks.is_empty() {
            notes.push(format!("{name} has no notes or controllers"));
            continue;
        }
        notes.extend(file.notes.iter().map(|n| format!("{name}: {n}")));
        match file.tempo_map {
            Some(map) if empty && tempo.is_none() => tempo = Some(map),
            Some(_) => kept_tempo = true,
            None => {}
        }
        for track in file.tracks {
            let node = match first.take() {
                Some(node) => node,
                None => {
                    let (command, input) =
                        add_track::with_input(scratch.graph(), || session.new_node_id());
                    if command.clone().apply(&mut scratch).is_err() {
                        notes.push(format!("Couldn't add a track for {name}"));
                        continue;
                    }
                    commands.push(command);
                    let group = scratch.graph().node(input).and_then(|n| n.parent);
                    if let (Some(group), false) = (group, track.name.is_empty()) {
                        commands.push(Command::SetConfig {
                            node: group,
                            key: PORT_NAME.into(),
                            value: Some(Value::Text(track.name.clone())),
                        });
                    }
                    input
                }
            };
            commands.push(Command::AddClip {
                id: next,
                clip: Clip {
                    node,
                    start: at,
                    content: ClipContent::Midi(track.clip),
                },
            });
            next.0 += 1;
            added += 1;
        }
    }
    if let Some(map) = tempo {
        commands.push(Command::SetTempoMap(map));
    }
    if kept_tempo {
        notes.push("the project's tempo and meter were kept".into());
    }
    if added > 0 {
        notes.push("the new tracks have no instrument yet".into());
    }
    let notice = (!notes.is_empty()).then(|| notes.join("; "));
    let command = match commands.len() {
        0 => None,
        1 => commands.pop(),
        _ => Some(Command::Batch(commands)),
    };
    Import { command, notice }
}

/// Which MIDI clips an export writes.
pub enum Scope {
    /// Every MIDI clip, a file track per Noodle track.
    Arrangement,
    /// One clip.
    Clip(ClipId),
    /// All the MIDI clips on one track.
    Track(NodeId),
}

/// The bytes of a file for `scope`, and what it could not carry. `None` when
/// there is nothing to write.
pub fn export(session: &Session, scope: &Scope) -> Option<(Vec<u8>, Vec<String>)> {
    let project = session.project();
    let tracks = noodle_io::project_tracks(project, |id, clip| match scope {
        Scope::Arrangement => true,
        Scope::Clip(want) => id == *want,
        Scope::Track(node) => clip.node == *node,
    });
    if tracks.is_empty() {
        return None;
    }
    let (file, notes) = noodle_io::export_smf(&tracks, project.tempo_map());
    Some((noodle_io::write_smf(&file), notes))
}
