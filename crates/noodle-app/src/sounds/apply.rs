//! Using a sound that has arrived: the part that edits the project.

use noodle_core::{Command, Config, Node, Value};

use super::{Done, Then};
use crate::session::{Edit, Session};
use crate::timeline::{add_track, import};

/// Does what the sound was asked for, or says why it can't.
pub fn apply(session: &mut Session, done: Done) {
    let Done {
        entry,
        then,
        result,
    } = done;
    let path = match result {
        Ok(path) => path,
        Err(why) => return session.notify(format!("Couldn't get {}: {why}", entry.name)),
    };
    match then {
        Then::Audition => {}
        Then::Sampler(node) => {
            // Read from the file that was just downloaded, here and not on
            // the audio thread; the hash is the sound's, which the download
            // checked.
            let hash = match session.samples().import(&path) {
                Ok(hash) => hash,
                Err(e) => return session.notify(format!("Couldn't read {}: {e}", entry.name)),
            };
            let is_sampler = session
                .project()
                .graph()
                .node(node)
                .is_some_and(|n| n.type_id == noodle_nodes::SAMPLER_ID);
            let command = if is_sampler {
                crate::sample_drop::choose(session, node, &hash)
            } else {
                Command::SetConfig {
                    node,
                    key: noodle_nodes::SAMPLE_KEY.to_owned(),
                    value: Some(Value::Text(hash)),
                }
            };
            session.edit([Edit::Apply(command)]);
        }
        Then::Canvas { at, group } => {
            let hash = match session.samples().import(&path) {
                Ok(hash) => hash,
                Err(e) => return session.notify(format!("Couldn't read {}: {e}", entry.name)),
            };
            let id = session.new_node_id();
            let mut node = Node::new(noodle_nodes::SAMPLE_INPUT_ID)
                .with_config(Config::new().with(noodle_nodes::SAMPLE_KEY, Value::Text(hash)))
                .at(at.x, at.y);
            node.parent = group;
            session.edit([Edit::Apply(Command::AddNode { id, node })]);
        }
        Then::Timeline { track, at } => {
            let Some(dir) = session.directory().map(std::path::Path::to_path_buf) else {
                return session.notify(
                    "Save the project first: a sound dropped on the timeline is copied into its folder"
                        .into(),
                );
            };
            let copy = match copy_into(&dir, &path, &entry.name, &entry.key) {
                Ok(copy) => copy,
                Err(e) => return session.notify(format!("Couldn't copy {}: {e}", entry.name)),
            };
            let credit = entry.credit_line();
            let (new_track, track) = match track {
                Some(track) => (None, track),
                None => {
                    let (create, track) =
                        add_track::with_input(session.project().graph(), || session.new_node_id());
                    (Some(create), track)
                }
            };
            let added = import::import_credited(session, &copy, track, at, credit);
            let command = match (new_track, added.command) {
                (Some(create), Some(clip)) => Some(Command::Batch(vec![create, clip])),
                (None, clip) => clip,
                (Some(_), None) => None,
            };
            if let Some(command) = command {
                session.clear_message();
                session.edit([Edit::Apply(command)]);
            }
            if let Some(notice) = added.notice {
                session.notify(notice);
            }
        }
    }
}

/// Copies the downloaded `file` into `<dir>/sounds/`, named for the sound and
/// the start of its hash so two sounds with one name don't meet. A copy that is
/// already there is kept.
fn copy_into(
    dir: &std::path::Path,
    file: &std::path::Path,
    name: &str,
    hash: &str,
) -> std::io::Result<std::path::PathBuf> {
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let folder = dir.join("sounds");
    std::fs::create_dir_all(&folder)?;
    let dest = folder.join(format!("{stem}-{}.flac", &hash[..8.min(hash.len())]));
    if !dest.is_file() {
        std::fs::copy(file, &dest)?;
    }
    Ok(dest)
}
