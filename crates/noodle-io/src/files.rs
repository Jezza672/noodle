//! What is on disk for the audio files a project plays: one answer, in one
//! place, to "has this file changed?".
//!
//! Waveforms, the clip schedule and the session's change notices used to
//! each `stat` the files on their own, with different ideas of what counts as
//! a change. Now a [`FileStamp`] (size and modification time) is the one rule,
//! and a single thread looks at the files that were asked about, once a
//! second, and wakes the UI when one differs. Everyone else compares stamps
//! with [`AudioFiles::stamp`], which reads what the thread last saw and does
//! no I/O for a file it already knows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime};

/// How often the known files are looked at.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// How many files are watched at once. Past it the ones asked about longest
/// ago are dropped; a window with nothing repainting asks about none, so
/// time alone must not drop a file the project still plays.
const MAX_WATCHED: usize = 4096;

/// A file's size and modification time. Two stamps are the same file as far
/// as anything cached from it is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

fn look(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

struct Known {
    stamp: Option<FileStamp>,
    asked: Instant,
}

#[derive(Default)]
struct Shared {
    known: Mutex<HashMap<PathBuf, Known>>,
    /// Wakes the thread when the first file is added, since it sleeps while
    /// there are none.
    added: Condvar,
    /// How many times a caller had to look at the disk itself.
    first_looks: AtomicUsize,
}

/// The watched audio files. See the module docs.
pub struct AudioFiles {
    shared: Arc<Shared>,
}

impl AudioFiles {
    /// A watcher that looks every `interval` and calls `on_change` when a
    /// file differs from what it saw last.
    pub fn new(interval: Duration, on_change: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        let theirs = Arc::downgrade(&shared);
        let _ = std::thread::Builder::new()
            .name("noodle-audio-files".into())
            .spawn(move || watch(theirs, interval, on_change));
        Self { shared }
    }

    /// The process's watcher, which wakes the UI (see `noodle_engine::wake`).
    pub fn global() -> &'static AudioFiles {
        static FILES: OnceLock<AudioFiles> = OnceLock::new();
        FILES.get_or_init(|| AudioFiles::new(INTERVAL, noodle_engine::wake::wake))
    }

    /// The stamp of `path` as the watcher last saw it, `None` if the file
    /// can't be read. The first time a path is asked about it is looked at
    /// right away and watched from then on; after that this does no I/O, so it
    /// is cheap enough to call on every edit and every frame.
    pub fn stamp(&self, path: &Path) -> Option<FileStamp> {
        let mut known = self.shared.known.lock().expect("files lock");
        if let Some(entry) = known.get_mut(path) {
            entry.asked = Instant::now();
            return entry.stamp;
        }
        self.shared.first_looks.fetch_add(1, Ordering::Relaxed);
        let stamp = look(path);
        known.insert(
            path.to_path_buf(),
            Known {
                stamp,
                asked: Instant::now(),
            },
        );
        self.shared.added.notify_one();
        stamp
    }

    /// How many times [`stamp`](Self::stamp) had to look at the disk itself,
    /// for tests of what stays off it.
    pub fn first_looks(&self) -> usize {
        self.shared.first_looks.load(Ordering::Relaxed)
    }
}

fn watch(shared: Weak<Shared>, interval: Duration, on_change: impl Fn()) {
    loop {
        let Some(strong) = shared.upgrade() else {
            return;
        };
        let paths: Vec<PathBuf> = {
            let mut known = strong.known.lock().expect("files lock");
            if known.len() > MAX_WATCHED {
                let mut asked: Vec<Instant> = known.values().map(|e| e.asked).collect();
                asked.sort();
                let cutoff = asked[known.len() - MAX_WATCHED];
                known.retain(|_, entry| entry.asked >= cutoff);
            }
            if known.is_empty() {
                // Nothing to look at: sleep until there is, and let go of
                // the shared state now and then so a dropped watcher ends.
                let _ = strong
                    .added
                    .wait_timeout(known, Duration::from_secs(5))
                    .expect("files lock");
                continue;
            }
            known.keys().cloned().collect()
        };
        let seen: Vec<(PathBuf, Option<FileStamp>)> = paths
            .into_iter()
            .map(|path| {
                let stamp = look(&path);
                (path, stamp)
            })
            .collect();
        let mut changed = false;
        {
            let mut known = strong.known.lock().expect("files lock");
            for (path, stamp) in seen {
                if let Some(entry) = known.get_mut(&path)
                    && entry.stamp != stamp
                {
                    entry.stamp = stamp;
                    changed = true;
                }
            }
        }
        drop(strong);
        if changed {
            on_change();
        }
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn a_replacement_with_the_same_mtime_but_another_length_is_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        std::fs::write(&path, b"one").unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();

        let woke = Arc::new(AtomicBool::new(false));
        let flag = woke.clone();
        let files = AudioFiles::new(Duration::from_millis(10), move || {
            flag.store(true, Ordering::SeqCst);
        });
        let first = files.stamp(&path).unwrap();
        assert_eq!(first.len, 3);

        // Replaced by a longer file that has the old modification time, as
        // `cp -p` or a coarse filesystem clock would leave it.
        std::fs::write(&path, b"three").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before)
            .unwrap();

        let start = Instant::now();
        while !woke.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(woke.load(Ordering::SeqCst), "the change was not noticed");
        let now = files.stamp(&path).unwrap();
        assert_eq!(now.len, 5);
        assert_eq!(now.modified, first.modified, "only the length differs");
        assert_ne!(now, first);
    }

    #[test]
    fn known_files_cost_no_disk_access_and_a_missing_file_turning_up_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("late.wav");
        let woke = Arc::new(AtomicBool::new(false));
        let flag = woke.clone();
        let files = AudioFiles::new(Duration::from_millis(10), move || {
            flag.store(true, Ordering::SeqCst);
        });
        assert_eq!(files.stamp(&path), None);
        for _ in 0..100 {
            files.stamp(&path);
        }
        assert_eq!(files.first_looks(), 1, "a hundred asks, one look");

        std::fs::write(&path, b"x").unwrap();
        let start = Instant::now();
        while !woke.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(files.stamp(&path).map(|s| s.len), Some(1));
    }
}
