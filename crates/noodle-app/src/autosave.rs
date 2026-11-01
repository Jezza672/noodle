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
//! history. Two copies of Noodle running at once share one autosave, and the
//! later write wins.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::session::write_atomically;

/// How long an unsaved change waits before it is written out.
pub const INTERVAL: Duration = Duration::from_secs(30);

const PROJECT: &str = "autosave.ron";
/// The project file the copy came from, so its relative clip paths still
/// resolve. Empty for an untitled project.
const ORIGIN: &str = "autosave.origin";

/// Where autosaves live on this platform, e.g. `~/.local/share/noodle/autosave`
/// on Linux.
pub fn default_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Noodle").map(|dirs| dirs.data_dir().join("autosave"))
}

/// An autosave left behind by a run that did not end cleanly.
#[derive(Clone, Debug)]
pub struct Recovery {
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
pub struct Autosave {
    dir: PathBuf,
    /// The history position last written, or `None` when no copy is on disk.
    written: Option<u64>,
    last_write: Instant,
}

impl Autosave {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            written: None,
            last_write: Instant::now(),
        }
    }

    /// When the next write is due, for tests to tick at.
    #[cfg(test)]
    pub fn next_due(&self) -> Instant {
        self.last_write + INTERVAL
    }

    /// The autosave a crashed run left, if any. A copy that can't be read is
    /// no use and is removed.
    pub fn pending(&self) -> Option<Recovery> {
        let file = self.dir.join(PROJECT);
        let text = fs::read_to_string(&file).ok()?;
        let origin = fs::read_to_string(self.dir.join(ORIGIN))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let written = fs::metadata(&file).and_then(|m| m.modified()).ok();
        Some(Recovery {
            text,
            origin,
            written,
        })
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
            .and_then(|()| write_atomically(&self.dir.join(ORIGIN), &origin))
            .and_then(|()| write_atomically(&self.dir.join(PROJECT), &text));
        match result {
            Ok(()) => self.written = Some(position),
            Err(error) => eprintln!("noodle: couldn't autosave: {error}"),
        }
        // A failed write is not retried every frame.
        self.last_write = now;
    }

    /// Removes the copy, if there is one.
    pub fn clear(&mut self) {
        if self.written.take().is_some() {
            self.remove();
        }
    }

    /// Removes the copy whether this run wrote it or not: a recovery that was
    /// answered, or a clean exit.
    pub fn remove(&mut self) {
        self.written = None;
        let _ = fs::remove_file(self.dir.join(PROJECT));
        let _ = fs::remove_file(self.dir.join(ORIGIN));
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

    #[test]
    fn a_dirty_project_is_written_and_a_clean_one_removes_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = Autosave::new(dir.path().to_owned());
        assert!(a.pending().is_none());
        due(&mut a, true, 1, "(project)", Some("/music/song.ron"));
        let found = Autosave::new(dir.path().to_owned()).pending().unwrap();
        assert_eq!(found.text, "(project)");
        assert_eq!(found.origin, Some(PathBuf::from("/music/song.ron")));
        assert_eq!(found.name(), "song");
        due(&mut a, false, 1, "", None);
        assert!(a.pending().is_none(), "saving removes the copy");
    }

    #[test]
    fn it_waits_out_the_interval_and_skips_unchanged_work() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = Autosave::new(dir.path().to_owned());
        let start = a.last_write;
        a.tick(start + INTERVAL / 2, true, 1, || ("early".into(), None));
        assert!(a.pending().is_none(), "too soon");
        due(&mut a, true, 1, "first", None);
        // Same position: nothing to write, so the closure must not run.
        let now = a.last_write + INTERVAL;
        a.tick(now, true, 1, || panic!("unchanged project was serialised"));
        let now = a.last_write + INTERVAL;
        a.tick(now, true, 2, || ("second".into(), None));
        assert_eq!(a.pending().unwrap().text, "second");
        assert_eq!(a.pending().unwrap().origin, None);
    }

    #[test]
    fn a_copy_from_another_run_is_removed_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        due(
            &mut Autosave::new(dir.path().to_owned()),
            true,
            1,
            "x",
            None,
        );
        let mut next = Autosave::new(dir.path().to_owned());
        // A clean project this run must not delete the crash copy before the
        // user has answered.
        due(&mut next, false, 0, "", None);
        assert!(next.pending().is_some());
        next.remove();
        assert!(next.pending().is_none());
    }
}
