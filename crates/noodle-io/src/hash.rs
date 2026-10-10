//! Content hashes of files, for cache keys.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use noodle_core::CacheKey;

/// A file's size, modification time and the hash taken then.
type Seen = (u64, Option<SystemTime>, CacheKey);

/// What [`FileHasher::peek`] can say without reading the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peek {
    /// The hash taken when the file last looked like this.
    Known(CacheKey),
    /// Not hashed yet, or changed since: [`FileHasher::hash`] would read it.
    Unknown,
    /// It can't be read (missing, or reading failed the last time it looked
    /// like this).
    Unreadable,
}

/// Hashes files by content, and remembers the answer while a file's size and
/// modification time stay the same, so keying a project doesn't read every
/// clip file again after each edit. A file replaced by one of the same size
/// within the clock's resolution of the old modification time would be
/// missed, which is the usual trade-off of a stat-based check.
#[derive(Debug, Default)]
pub struct FileHasher {
    seen: Mutex<HashMap<PathBuf, Seen>>,
    /// Files that couldn't be read, with the size and time they had then, so
    /// a file that won't open isn't retried until it changes.
    failed: Mutex<HashMap<PathBuf, (u64, Option<SystemTime>)>>,
}

impl FileHasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// What is known of the file without reading it: only its size and
    /// modification time are looked at, so this is cheap enough for a UI
    /// thread. Hash the files that come back `Unknown` somewhere else (see
    /// [`hash`](Self::hash)).
    pub fn peek(&self, path: &Path) -> Peek {
        let Ok(meta) = std::fs::metadata(path) else {
            return Peek::Unreadable;
        };
        let stamp = (meta.len(), meta.modified().ok());
        if let Some(&(len, modified, key)) = self.seen.lock().expect("hasher lock").get(path)
            && (len, modified) == stamp
            && modified.is_some()
        {
            return Peek::Known(key);
        }
        if self.failed.lock().expect("hasher lock").get(path) == Some(&stamp) {
            return Peek::Unreadable;
        }
        Peek::Unknown
    }

    /// The hash of the file's contents. Reads the whole file unless it is
    /// remembered, so keep it off a UI thread.
    pub fn hash(&self, path: &Path) -> io::Result<CacheKey> {
        let meta = std::fs::metadata(path)?;
        let stamp = (meta.len(), meta.modified().ok());
        if let Some(&(len, modified, key)) = self.seen.lock().expect("hasher lock").get(path)
            && (len, modified) == stamp
            && modified.is_some()
        {
            return Ok(key);
        }
        let hashed = File::open(path).and_then(CacheKey::of_reader);
        let key = match hashed {
            Ok(key) => key,
            Err(error) => {
                self.failed
                    .lock()
                    .expect("hasher lock")
                    .insert(path.to_path_buf(), stamp);
                return Err(error);
            }
        };
        self.failed.lock().expect("hasher lock").remove(path);
        self.seen
            .lock()
            .expect("hasher lock")
            .insert(path.to_path_buf(), (stamp.0, stamp.1, key));
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_same_key_and_a_change_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, b"hello").unwrap();
        std::fs::write(&b, b"hello").unwrap();
        let hasher = FileHasher::new();
        let first = hasher.hash(&a).unwrap();
        assert_eq!(first, hasher.hash(&b).unwrap());
        assert_eq!(first, hasher.hash(&a).unwrap());
        // Same length, new contents, at the same path.
        std::fs::write(&a, b"jello").unwrap();
        let file = File::options().write(true).open(&a).unwrap();
        file.set_modified(SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        assert_ne!(first, hasher.hash(&a).unwrap());
        assert!(hasher.hash(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn peeking_never_reads_and_sees_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        std::fs::write(&path, b"hello").unwrap();
        let hasher = FileHasher::new();
        // Not hashed yet: unknown, and still unknown after looking.
        assert_eq!(hasher.peek(&path), Peek::Unknown);
        assert_eq!(hasher.peek(&path), Peek::Unknown);
        let key = hasher.hash(&path).unwrap();
        assert_eq!(hasher.peek(&path), Peek::Known(key));
        // A file that changed is unknown again until it is hashed.
        std::fs::write(&path, b"jellyfish").unwrap();
        assert_eq!(hasher.peek(&path), Peek::Unknown);
        assert_eq!(hasher.peek(&dir.path().join("missing")), Peek::Unreadable);
    }
}
