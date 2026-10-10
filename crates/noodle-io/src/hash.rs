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

/// Hashes files by content, and remembers the answer while a file's size and
/// modification time stay the same, so keying a project doesn't read every
/// clip file again after each edit. A file replaced by one of the same size
/// within the clock's resolution of the old modification time would be
/// missed, which is the usual trade-off of a stat-based check.
#[derive(Debug, Default)]
pub struct FileHasher {
    seen: Mutex<HashMap<PathBuf, Seen>>,
}

impl FileHasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// The hash of the file's contents.
    pub fn hash(&self, path: &Path) -> io::Result<CacheKey> {
        let meta = std::fs::metadata(path)?;
        let stamp = (meta.len(), meta.modified().ok());
        if let Some(&(len, modified, key)) = self.seen.lock().expect("hasher lock").get(path)
            && (len, modified) == stamp
            && modified.is_some()
        {
            return Ok(key);
        }
        let key = CacheKey::of_reader(File::open(path)?)?;
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
}
