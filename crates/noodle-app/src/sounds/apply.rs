//! Using a sound that has arrived: the part that edits the project.

use noodle_core::{Command, Config, Node, Value};

use super::{Done, Installed, Then};
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
            // Any node that plays a sample (Sampler, Grains, Wavetable) takes it
            // by `choose`; a Sample node has only the config.
            let plays = session
                .project()
                .graph()
                .node(node)
                .is_some_and(|n| n.type_id != noodle_nodes::SAMPLE_INPUT_ID);
            let command = if plays {
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
            let credit = entry.credit_line();
            let (new_track, track) = match track {
                Some(track) => (None, track),
                None => {
                    let (create, track) =
                        add_track::with_input(session.project().graph(), || session.new_node_id());
                    (Some(create), track)
                }
            };
            let added =
                import::import_stored(session, &path, &entry.name, &entry.key, track, at, credit);
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

/// Puts a library entry that has arrived into the user's library, with its
/// samples registered so the group or preset finds them by hash, and with its
/// origin kept so the credit and licence travel with it.
pub fn install(session: &mut Session, got: Installed) {
    let Installed { entry, result } = got;
    let fetched = match result {
        Ok(fetched) => fetched,
        Err(why) => return session.notify(format!("Couldn't install {}: {why}", entry.name)),
    };
    for (_, path) in &fetched.samples {
        if let Err(e) = session.samples().import(path) {
            return session.notify(format!("Couldn't read a sound of {}: {e}", entry.name));
        }
    }
    let origin = noodle_core::library::Origin {
        hash: entry.key.clone(),
        pack: entry.pack.clone(),
        author: entry.author.clone(),
        licence: entry.licence.label(),
        credit: entry.credit_line(),
    };
    session.install_library_item(fetched.item, origin);
}
