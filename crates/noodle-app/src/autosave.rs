//! Autosave and crash recovery.
//!
//! While a project has unsaved changes, a copy is written to the app's data
//! directory every [`INTERVAL`]. The copy goes away when the project is
//! saved, discarded or the app exits cleanly, so a copy found at startup
//! means the last run ended without that: a crash or a power cut. The app
//! then offers to recover it.
//!
//! The copy is the project file only. Samples held in memory and not yet
//! saved beside a project file are not in it, and neither is the undo
//! history. Copies of Noodle running at once each keep their own autosave.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::session::write_atomically;

/// How long an unsaved change waits before it is written out.
pub const INTERVAL: Duration = Duration::from_secs(30);

const PROJECT: &str = "ron";
/// The project file the copy came from, so its relative clip paths still
/// resolve. Empty for an untitled project.
const ORIGIN: &str = "origin";
const LOCK: &str = "lock";

/// Where autosaves live on this platform, e.g. `~/.local/share/noodle/autosave`
/// on Linux.
pub fn default_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Noodle").map(|dirs| dirs.data_dir().join("autosave"))
}

/// An autosave left behind by a run that did not end cleanly.
#[derive(Clone, Debug)]
pub struct Recovery {
    /// Which run wrote it: the files' common name, without extension.
    stem: String,
    pub text: String,
    pub origin: Option<PathBuf>,
    pub written: Option<SystemTime>,
}

impl Recovery {
    /// What to call the project in the question.
    pub fn name(&self) -> String {
        self.origin
            .as_deref()
            .and_then(Path::file_stem)
            .map_or_else(|| "Untitled".into(), |s| s.to_string_lossy().into_owned())
    }

    /// How long ago it was written, in words.
    pub fn age(&self) -> Option<String> {
        let seconds = SystemTime::now()
            .duration_since(self.written?)
            .ok()?
            .as_secs();
        Some(match seconds {
            0..=89 => "a minute ago".into(),
            90..=5399 => format!("{} minutes ago", (seconds + 30) / 60),
            5400..=129_599 => format!("{} hours ago", (seconds + 1800) / 3600),
            _ => format!("{} days ago", seconds / 86_400),
        })
    }
}

/// Writes autosaves into one directory.
///
/// Each run writes files of its own, named by its process ID, and holds an
/// OS lock on one of them while it lives. The lock goes with the process, so
/// a copy whose lock is free belongs to a run that is gone, and one whose
/// lock is held belongs to another Noodle still running, which is left alone.
pub struct Autosave {
    dir: PathBuf,
    stem: String,
    /// The held lock, once this run has written anything.
    lock: Option<fs::File>,
    /// The history position last written, or `None` when no copy is on disk.
    written: Option<u64>,
    last_write: Instant,
}

impl Autosave {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            stem: format!("autosave-{}", std::process::id()),
            lock: None,
            written: None,
            last_write: Instant::now(),
        }
    }

    /// An autosave as the run with process ID `id` would write it.
    #[cfg(test)]
    pub fn for_run(dir: PathBuf, id: u32) -> Self {
        let mut a = Self::new(dir);
        a.stem = format!("autosave-{id}");
        a
    }

    /// When the next write is due, for tests to tick at.
    #[cfg(test)]
    pub fn next_due(&self) -> Instant {
        self.last_write + INTERVAL
    }

    fn file(&self, stem: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{stem}.{extension}"))
    }

    /// The newest autosave a dead run left, if any.
    pub fn pending(&self) -> Option<Recovery> {
        let mut found: Vec<(SystemTime, String)> = fs::read_dir(&self.dir)
            .ok()?
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                if path.extension()? != PROJECT {
                    return None;
                }
                let stem = path.file_stem()?.to_str()?.to_owned();
                if stem == self.stem || !stem.starts_with("autosave-") {
                    return None;
                }
                let time = fs::metadata(&path).and_then(|m| m.modified()).ok()?;
                Some((time, stem))
            })
            .collect();
        found.sort();
        while let Some((written, stem)) = found.pop() {
            // A held lock is a run that is still going.
            let Ok(lock) = fs::File::create(self.file(&stem, LOCK)) else {
                continue;
            };
            if lock.try_lock().is_err() {
                continue;
            }
            let Ok(text) = fs::read_to_string(self.file(&stem, PROJECT)) else {
                continue;
            };
            let origin = fs::read_to_string(self.file(&stem, ORIGIN))
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
            return Some(Recovery {
                stem,
                text,
                origin,
                written: Some(written),
            });
        }
        None
    }

    /// Called every frame. Writes the project when it has changed and the
    /// last write is [`INTERVAL`] old, and removes the copy once the project
    /// is clean. `project` is only called when a write is due.
    pub fn tick(
        &mut self,
        now: Instant,
        dirty: bool,
        position: u64,
        project: impl FnOnce() -> (String, Option<PathBuf>),
    ) {
        if !dirty {
            self.clear();
            return;
        }
        if self.written == Some(position) || now.duration_since(self.last_write) < INTERVAL {
            return;
        }
        let (text, origin) = project();
        let origin = origin.map(|p| p.display().to_string()).unwrap_or_default();
        let result = fs::create_dir_all(&self.dir)
            .and_then(|()| self.hold_lock())
            .and_then(|()| write_atomically(&self.file(&self.stem, ORIGIN), &origin))
            .and_then(|()| write_atomically(&self.file(&self.stem, PROJECT), &text));
        match result {
            Ok(()) => self.written = Some(position),
            Err(error) => eprintln!("noodle: couldn't autosave: {error}"),
        }
        // A failed write is not retried every frame.
        self.last_write = now;
    }

    fn hold_lock(&mut self) -> std::io::Result<()> {
        if self.lock.is_none() {
            let lock = fs::File::create(self.file(&self.stem, LOCK))?;
            lock.try_lock().map_err(std::io::Error::other)?;
            self.lock = Some(lock);
        }
        Ok(())
    }

    /// Removes this run's copy, if there is one.
    pub fn clear(&mut self) {
        if self.written.take().is_some() {
            self.remove_own();
        }
    }

    /// Removes this run's files, for a clean exit.
    pub fn remove_own(&mut self) {
        self.written = None;
        self.lock = None;
        remove_files(&self.dir, &self.stem);
    }

    /// Removes a recovery that was answered.
    pub fn remove(&mut self, recovery: &Recovery) {
        remove_files(&self.dir, &recovery.stem);
    }

    /// Moves a copy that couldn't be recovered to `failed-autosave.ron`, so it
    /// can be looked at by hand and isn't offered again.
    pub fn set_aside(&mut self, recovery: &Recovery) {
        let _ = fs::rename(
            self.file(&recovery.stem, PROJECT),
            self.dir.join("failed-autosave.ron"),
        );
        remove_files(&self.dir, &recovery.stem);
    }
}

fn remove_files(dir: &Path, stem: &str) {
    for extension in [PROJECT, ORIGIN, LOCK] {
        let _ = fs::remove_file(dir.join(format!("{stem}.{extension}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(a: &mut Autosave, dirty: bool, position: u64, text: &str, origin: Option<&str>) {
        let now = a.last_write + INTERVAL;
        a.tick(now, dirty, position, || {
            (text.into(), origin.map(PathBuf::from))
        });
    }

    /// An autosave as a run with another process ID would have left it.
    fn other_run(dir: &Path, id: u32) -> Autosave {
        Autosave::for_run(dir.to_owned(), id)
    }

    #[test]
    fn a_dirty_project_is_written_and_a_clean_one_removes_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = other_run(dir.path(), 1);
        due(&mut a, true, 1, "(project)", Some("/music/song.ron"));
        // The run is over: its lock goes with it.
        a.lock = None;
        let found = Autosave::new(dir.path().to_owned()).pending().unwrap();
        assert_eq!(found.text, "(project)");
        assert_eq!(found.origin, Some(PathBuf::from("/music/song.ron")));
        assert_eq!(found.name(), "song");
        let mut a = other_run(dir.path(), 2);
        due(&mut a, true, 1, "x", None);
        due(&mut a, false, 1, "", None);
        assert!(
            !a.file(&a.stem, PROJECT).exists(),
            "saving removes the copy"
        );
    }

    #[test]
    fn it_waits_out_the_interval_and_skips_unchanged_work() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = other_run(dir.path(), 1);
        let start = a.last_write;
        a.tick(start + INTERVAL / 2, true, 1, || ("early".into(), None));
        assert!(!a.file(&a.stem, PROJECT).exists(), "too soon");
        due(&mut a, true, 1, "first", None);
        // Same position: nothing to write, so the closure must not run.
        let now = a.last_write + INTERVAL;
        a.tick(now, true, 1, || panic!("unchanged project was serialised"));
        let now = a.last_write + INTERVAL;
        a.tick(now, true, 2, || ("second".into(), None));
        a.lock = None;
        assert_eq!(
            Autosave::new(dir.path().to_owned()).pending().unwrap().text,
            "second"
        );
    }

    #[test]
    fn a_running_instances_copy_is_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let mut live = other_run(dir.path(), 1);
        due(&mut live, true, 1, "live", None);
        let mut second = Autosave::new(dir.path().to_owned());
        assert!(second.pending().is_none(), "its lock is held");
        // Quitting the second instance cleanly leaves the first one's copy.
        second.remove_own();
        assert!(live.file(&live.stem, PROJECT).exists());
        // Once the first is gone, its copy is offered.
        live.lock = None;
        assert!(Autosave::new(dir.path().to_owned()).pending().is_some());
    }

    #[test]
    fn a_copy_that_cant_be_recovered_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let mut dead = other_run(dir.path(), 1);
        due(&mut dead, true, 1, "broken", None);
        dead.lock = None;
        let mut next = Autosave::new(dir.path().to_owned());
        let found = next.pending().unwrap();
        next.set_aside(&found);
        assert!(next.pending().is_none());
        assert_eq!(
            fs::read_to_string(dir.path().join("failed-autosave.ron")).unwrap(),
            "broken"
        );
    }
}
