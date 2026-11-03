//! Filling a Sample Set from files and a folder, and rescanning the folder.
//!
//! A rescan reads the folder and works out what it would change
//! ([`noodle_nodes::rescan`]); nothing is applied until the user has seen the
//! diff and agreed.

use std::path::{Path, PathBuf};

use noodle_core::{Command, NodeId, Value};
use noodle_nodes::{
    Rescan, SAMPLE_KEY, SAMPLE_SET_ID, SET_FOLDER_KEY, SET_NAMES_KEY, folder_of, middle_c_of,
    rescan, rows_of, rows_to_config,
};

use super::{Edit, Session};

/// The audio files a folder is scanned for.
const EXTENSIONS: [&str; 7] = ["wav", "flac", "mp3", "ogg", "aif", "aiff", "m4a"];

/// A rescan waiting for the user to look at it.
pub struct PendingRescan {
    pub node: NodeId,
    pub plan: Rescan,
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

impl Session {
    /// The files in `folder` (not its subfolders), by name.
    fn audio_in(folder: &Path) -> Result<Vec<PathBuf>, String> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
            .map_err(|e| format!("Couldn't read {}: {e}", folder.display()))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && is_audio(p))
            .collect();
        files.sort();
        Ok(files)
    }

    /// Imports `paths` into the sample store, as `(file name, hash)`. A file
    /// that won't decode is an error for the whole import: a set with a hole
    /// in it would be worse than none.
    fn import_named(&self, paths: &[PathBuf]) -> Result<Vec<(String, String)>, String> {
        paths
            .iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.samples
                    .import(path)
                    .map(|hash| (name.clone(), hash))
                    .map_err(|e| format!("Couldn't read {name}: {e}"))
            })
            .collect()
    }

    fn set_node(&self, node: NodeId) -> Result<&noodle_core::Node, String> {
        self.project
            .graph()
            .node(node)
            .filter(|n| n.type_id == SAMPLE_SET_ID)
            .ok_or_else(|| "That is not a Sample Set.".to_owned())
    }

    fn set_rows_commands(node: NodeId, rows: &[noodle_nodes::Row]) -> Vec<Command> {
        let (sample, names) = rows_to_config(rows);
        [(SAMPLE_KEY, sample), (SET_NAMES_KEY, names)]
            .into_iter()
            .map(|(key, text)| Command::SetConfig {
                node,
                key: key.to_owned(),
                value: Some(Value::Text(text)),
            })
            .collect()
    }

    /// The edit that adds recordings to a Sample Set: a file already in the
    /// set (by name) keeps its row unless its audio is new, and the rest are
    /// read from their names. One undo step.
    pub fn sample_set_add(&self, node: NodeId, paths: &[PathBuf]) -> Result<Vec<Edit>, String> {
        let set = self.set_node(node)?;
        let old = rows_of(&set.config);
        let mut found: Vec<(String, String)> = old
            .iter()
            .map(|r| (r.name.clone(), r.entry.reference.hash.clone()))
            .collect();
        for (name, hash) in self.import_named(paths)? {
            match found.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = hash,
                None => found.push((name, hash)),
            }
        }
        let plan = rescan(&old, &found, middle_c_of(&set.config));
        Ok(vec![Edit::Apply(Command::Batch(Self::set_rows_commands(
            node, &plan.rows,
        )))])
    }

    /// The edit that fills a Sample Set from every audio file in `folder`
    /// and remembers the folder for [`Self::sample_set_rescan`]. One undo
    /// step.
    pub fn sample_set_load_folder(&self, node: NodeId, folder: &Path) -> Result<Vec<Edit>, String> {
        let set = self.set_node(node)?;
        let found = self.import_named(&Self::audio_in(folder)?)?;
        let plan = rescan(&rows_of(&set.config), &found, middle_c_of(&set.config));
        let mut commands = Self::set_rows_commands(node, &plan.rows);
        commands.push(Command::SetConfig {
            node,
            key: SET_FOLDER_KEY.to_owned(),
            value: Some(Value::Text(folder.display().to_string())),
        });
        Ok(vec![Edit::Apply(Command::Batch(commands))])
    }

    /// Reads the folder a Sample Set came from and works out what a rescan
    /// would change. Nothing changes until [`Self::apply_rescan`]; the diff is
    /// in [`Self::pending_rescan`]. Returns whether there is anything to
    /// look at.
    pub fn sample_set_rescan(&self, node: NodeId) -> Result<bool, String> {
        let set = self.set_node(node)?;
        let folder = folder_of(&set.config);
        if folder.is_empty() {
            return Err("This set has no folder to rescan. Load a folder first.".into());
        }
        let found = self.import_named(&Self::audio_in(Path::new(&folder))?)?;
        let plan = rescan(&rows_of(&set.config), &found, middle_c_of(&set.config));
        if plan.is_empty() {
            return Ok(false);
        }
        *self.rescan.borrow_mut() = Some(PendingRescan { node, plan });
        Ok(true)
    }

    /// The rescan waiting for the user's answer, if there is one.
    pub fn pending_rescan(&self) -> std::cell::Ref<'_, Option<PendingRescan>> {
        self.rescan.borrow()
    }

    /// The user accepted the diff: apply it as one undo step.
    pub fn apply_rescan(&mut self) {
        let Some(PendingRescan { node, plan }) = self.rescan.take() else {
            return;
        };
        if self.set_node(node).is_ok() {
            let commands = Self::set_rows_commands(node, &plan.rows);
            self.edit([Edit::Apply(Command::Batch(commands))]);
        }
    }

    /// The user declined: nothing changes.
    pub fn cancel_rescan(&self) {
        *self.rescan.borrow_mut() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Nodes;
    use noodle_core::Node;
    use noodle_io::write_wav;

    /// A tone of `hz`, half a second long.
    fn tone(path: &Path, hz: f32) {
        let samples: Vec<f32> = (0..24_000)
            .map(|i| 0.5 * (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin())
            .collect();
        write_wav(path, &samples, 1, 48_000).unwrap();
    }

    fn hz(note: u8) -> f32 {
        440.0 * 2f32.powf((f32::from(note) - 69.0) / 12.0)
    }

    fn rig(dir: &Path) -> (Session, NodeId) {
        let mut session = Session::new(Nodes::all());
        let id = session.new_node_id();
        session.edit([Edit::Apply(Command::AddNode {
            id,
            node: Node::new(SAMPLE_SET_ID),
        })]);
        let _ = dir;
        (session, id)
    }

    fn rows(session: &Session, id: NodeId) -> Vec<noodle_nodes::Row> {
        rows_of(&session.project().graph().node(id).unwrap().config)
    }

    #[test]
    fn a_folder_becomes_a_set_read_from_the_file_names_in_one_undo() {
        let dir = tempfile::tempdir().unwrap();
        for (name, note) in [
            ("Piano_C4_mf.wav", 60),
            ("Piano_Eb4_mf.wav", 63),
            ("Piano_G4_mf.wav", 67),
        ] {
            tone(&dir.path().join(name), hz(note));
        }
        std::fs::write(dir.path().join("notes.txt"), "not audio").unwrap();
        let (mut session, id) = rig(dir.path());
        let edits = session.sample_set_load_folder(id, dir.path()).unwrap();
        session.edit(edits);
        let roots: Vec<_> = rows(&session, id).iter().map(|r| r.entry.root).collect();
        assert_eq!(roots, [Some(60), Some(63), Some(67)]);
        assert_eq!(
            folder_of(&session.project().graph().node(id).unwrap().config),
            dir.path().display().to_string()
        );
        session.undo();
        assert!(rows(&session, id).is_empty(), "one undo takes the lot back");
    }

    #[test]
    fn a_rescan_shows_the_diff_and_changes_nothing_until_applied() {
        let dir = tempfile::tempdir().unwrap();
        for (name, note) in [("C4.wav", 60), ("D4.wav", 62), ("E4.wav", 64)] {
            tone(&dir.path().join(name), hz(note));
        }
        let (mut session, id) = rig(dir.path());
        let edits = session.sample_set_load_folder(id, dir.path()).unwrap();
        session.edit(edits);
        assert_eq!(session.sample_set_rescan(id), Ok(false), "nothing to do");

        // A recording replaced, one removed, one renamed, two added.
        tone(&dir.path().join("D4.wav"), hz(62) * 1.01);
        std::fs::remove_file(dir.path().join("E4.wav")).unwrap();
        tone(&dir.path().join("F4.wav"), hz(65));
        tone(&dir.path().join("G4.wav"), hz(67));
        std::fs::rename(dir.path().join("C4.wav"), dir.path().join("60.wav")).unwrap();

        let before = session.project().clone();
        assert_eq!(session.sample_set_rescan(id), Ok(true));
        {
            let pending = session.pending_rescan();
            let plan = &pending.as_ref().unwrap().plan;
            assert_eq!(plan.added, ["F4.wav", "G4.wav"]);
            assert_eq!(plan.replaced, ["D4.wav"]);
            assert_eq!(plan.removed, ["E4.wav"]);
            assert_eq!(plan.renamed, [("C4.wav".to_owned(), "60.wav".to_owned())]);
        }
        assert_eq!(session.project(), &before, "looking changes nothing");

        session.cancel_rescan();
        assert!(session.pending_rescan().is_none());
        assert_eq!(session.project(), &before, "cancel changes nothing");

        assert_eq!(session.sample_set_rescan(id), Ok(true));
        session.apply_rescan();
        let names: Vec<_> = rows(&session, id).into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["60.wav", "D4.wav", "F4.wav", "G4.wav"]);
        session.undo();
        assert_eq!(session.project(), &before, "one undo step");
    }

    #[test]
    fn adding_files_keeps_the_rows_already_there_with_their_edits() {
        let dir = tempfile::tempdir().unwrap();
        tone(&dir.path().join("C4.wav"), hz(60));
        tone(&dir.path().join("E4.wav"), hz(64));
        let (mut session, id) = rig(dir.path());
        let edits = session
            .sample_set_add(id, &[dir.path().join("C4.wav")])
            .unwrap();
        session.edit(edits);
        // A hand edit to the first row.
        let mut edited = rows(&session, id);
        edited[0].entry.root = Some(50);
        let commands = Session::set_rows_commands(id, &edited);
        session.edit([Edit::Apply(Command::Batch(commands))]);

        let edits = session
            .sample_set_add(id, &[dir.path().join("E4.wav")])
            .unwrap();
        session.edit(edits);
        let roots: Vec<_> = rows(&session, id).iter().map(|r| r.entry.root).collect();
        assert_eq!(roots, [Some(50), Some(64)]);
    }

    #[test]
    fn a_file_that_will_not_decode_is_an_error_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        tone(&dir.path().join("C4.wav"), hz(60));
        std::fs::write(dir.path().join("D4.wav"), "not a wav").unwrap();
        let (session, id) = rig(dir.path());
        let error = session.sample_set_load_folder(id, dir.path()).unwrap_err();
        assert!(error.contains("D4.wav"), "{error}");
    }
}
