//! A folder on disk as a source (ADR 0012): scanned, hashed and read in place.
//!
//! Nothing is copied or uploaded. Every audio file under the folder becomes an
//! [`Entry`] named by the hash of its decoded audio, so it can be dropped on a
//! Sampler like any other sound. The name is the file's, the tags are the
//! folders it sits in, and the licence is the folder's (unknown until the user
//! sets one). A rescan skips decoding a file whose size and modified time are
//! what they were, so reopening a big folder is quick.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use super::catalogue::{Entry, Kind};
use super::source::{Folder, Source, audio_hash};

/// What the scanner reads as audio.
pub const EXTENSIONS: &[&str] = &["wav", "flac", "mp3", "ogg", "m4a", "aac"];

/// The most files one folder source lists, and how deep it looks. A folder
/// that is far bigger than a sample library is probably not one.
const MAX_FILES: usize = 20_000;
const MAX_DEPTH: usize = 12;

/// Where an entry's audio is, for a folder source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFile {
    pub path: PathBuf,
    /// Seconds since the epoch when it was last changed, with its size (the
    /// entry's `bytes`) what the rescan checks before decoding it again.
    pub modified: u64,
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Every audio file under `root`, in a fixed order. Symbolic links and hidden
/// files and folders are left alone, so a link can't lead the scan out of the
/// folder or round in a loop.
fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_owned(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<_> = read.flatten().collect();
        children.sort_by_key(|c| c.file_name());
        for child in children {
            if child.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(kind) = child.file_type() else {
                continue;
            };
            let path = child.path();
            if kind.is_dir() && depth < MAX_DEPTH {
                stack.push((path, depth + 1));
            } else if kind.is_file() && is_audio(&path) {
                found.push(path);
            }
        }
    }
    found.sort();
    found.truncate(MAX_FILES);
    found
}

/// The words a path's folders say about its sound: each folder between the
/// source's folder and the file, in lower case.
fn tags_of(root: &Path, path: &Path) -> Vec<String> {
    let Ok(relative) = path.strip_prefix(root) else {
        return Vec::new();
    };
    let mut tags: Vec<String> = relative
        .parent()
        .into_iter()
        .flat_map(|p| p.components())
        .map(|c| c.as_os_str().to_string_lossy().trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    tags.dedup();
    tags
}

fn stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((meta.len(), modified))
}

/// The sounds in `local`'s folder. `known` is what the last scan found, whose
/// hashes are reused for a file that has not changed. A folder that isn't
/// there is the source being unreachable; a file that isn't audio is skipped.
pub fn scan(source: &Source, local: &Folder, known: &[Entry]) -> Result<Vec<Entry>, String> {
    if !local.path.is_dir() {
        return Err("the folder isn't there".into());
    }
    let before: HashMap<&Path, &Entry> = known
        .iter()
        .filter_map(|e| Some((e.file.as_ref()?.path.as_path(), e)))
        .collect();
    let pack = local
        .path
        .file_name()
        .map_or_else(|| source.name.clone(), |n| n.to_string_lossy().into_owned());
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for path in files_under(&local.path) {
        let Some((bytes, modified)) = stamp(&path) else {
            continue;
        };
        let reused = before.get(path.as_path()).filter(|e| {
            e.bytes == Some(bytes) && e.file.as_ref().is_some_and(|f| f.modified == modified)
        });
        let key = match reused {
            Some(e) => e.key.clone(),
            None => match audio_hash(&path) {
                Ok(hash) => hash,
                Err(_) => continue,
            },
        };
        // The same audio twice is one sound.
        if !seen.insert(key.clone()) {
            continue;
        }
        entries.push(Entry {
            kind: Kind::Sound,
            key,
            name: path
                .file_stem()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            pack: pack.clone(),
            author: String::new(),
            licence: local.licence.clone(),
            tags: tags_of(&local.path, &path),
            credit: None,
            bytes: Some(bytes),
            source: source.id.clone(),
            file: Some(LocalFile { path, modified }),
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sounds::source::fake;
    use noodle_packs::Licence;

    fn source(dir: &Path) -> (Source, Folder) {
        let source = Source::local("f", "Mine", dir.to_owned(), Licence::Unknown);
        let folder = source.folder.clone().unwrap();
        (source, folder)
    }

    #[test]
    fn a_folder_is_scanned_hashed_and_tagged_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let drums = dir.path().join("Drums").join("Kicks");
        std::fs::create_dir_all(&drums).unwrap();
        let (bytes, hash) = fake::tone(scratch.path(), 110.0);
        std::fs::write(drums.join("Deep.flac"), &bytes).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not audio").unwrap();
        std::fs::write(dir.path().join("bad.wav"), "not audio either").unwrap();
        let (source, folder) = source(dir.path());
        let found = scan(&source, &folder, &[]).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        let sound = &found[0];
        assert_eq!(sound.key, hash);
        assert_eq!(sound.name, "Deep");
        assert_eq!(sound.tags, ["drums", "kicks"]);
        assert_eq!(sound.licence, Licence::Unknown);
        assert_eq!(sound.file.as_ref().unwrap().path, drums.join("Deep.flac"));
    }

    #[test]
    fn a_rescan_decodes_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let (bytes, hash) = fake::tone(scratch.path(), 110.0);
        let path = dir.path().join("a.flac");
        std::fs::write(&path, &bytes).unwrap();
        let (source, folder) = source(dir.path());
        let first = scan(&source, &folder, &[]).unwrap();
        // Poison the remembered hash: a file whose size and time match is not
        // decoded again, so the poison is reused.
        let mut poisoned = first.clone();
        poisoned[0].key = "0".repeat(64);
        let again = scan(&source, &folder, &poisoned).unwrap();
        assert_eq!(again[0].key, "0".repeat(64));
        // A changed file is.
        let (other, other_hash) = fake::tone(scratch.path(), 330.0);
        std::fs::write(&path, other).unwrap();
        let changed = scan(&source, &folder, &poisoned).unwrap();
        assert_eq!(changed[0].key, other_hash);
        assert_ne!(changed[0].key, hash);
    }

    #[test]
    fn the_same_audio_twice_is_one_sound_and_links_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (bytes, _) = fake::tone(scratch.path(), 110.0);
        std::fs::write(dir.path().join("a.flac"), &bytes).unwrap();
        std::fs::write(dir.path().join("copy.flac"), &bytes).unwrap();
        let (out_bytes, _) = fake::tone(scratch.path(), 440.0);
        std::fs::write(outside.path().join("secret.flac"), out_bytes).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let (source, folder) = source(dir.path());
        let found = scan(&source, &folder, &[]).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn a_missing_folder_is_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");
        let (source, folder) = source(&gone);
        assert!(scan(&source, &folder, &[]).is_err());
    }
}
