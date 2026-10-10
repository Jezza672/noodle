//! Noticing that an audio file a clip plays was replaced on disk.
//!
//! Looks at each file's size and modification time now and then, which costs
//! a `stat` per file and needs no platform file-watching service. Waveforms
//! already do the same on their own (see `timeline::sources`); this is for
//! what plays: the clip streams, and the keys the cache of renders is made
//! with.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// How often the files are looked at.
pub const INTERVAL: Duration = Duration::from_secs(1);

type Stamp = Option<(u64, SystemTime)>;

#[derive(Default)]
pub struct FileWatch {
    seen: HashMap<PathBuf, Stamp>,
    checked: Option<Instant>,
}

fn stamp(path: &std::path::Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

impl FileWatch {
    /// Looks at `files` if it has been a while since the last look. Returns
    /// the ones that are not as they were last time: changed, gone, or back.
    /// A file seen for the first time is only remembered.
    pub fn changed(
        &mut self,
        now: Instant,
        files: impl IntoIterator<Item = PathBuf>,
    ) -> Vec<PathBuf> {
        if self
            .checked
            .is_some_and(|at| now.duration_since(at) < INTERVAL)
        {
            return Vec::new();
        }
        self.checked = Some(now);
        let files: Vec<PathBuf> = files.into_iter().collect();
        // Forget files the project no longer plays.
        self.seen.retain(|path, _| files.contains(path));
        let mut changed = Vec::new();
        for path in files {
            let now = stamp(&path);
            match self.seen.insert(path.clone(), now) {
                Some(before) if before != now => changed.push(path),
                _ => {}
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_file_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        std::fs::write(&path, b"one").unwrap();
        let mut watch = FileWatch::default();
        let mut now = Instant::now();
        let look = |watch: &mut FileWatch, now: Instant| watch.changed(now, [path.clone()]);
        assert!(look(&mut watch, now).is_empty(), "first sight");
        now += INTERVAL;
        assert!(look(&mut watch, now).is_empty(), "unchanged");
        // Another size is a change whatever the clock says.
        std::fs::write(&path, b"three").unwrap();
        // Looking again too soon says nothing.
        assert!(look(&mut watch, now + INTERVAL / 2).is_empty());
        now += INTERVAL;
        assert_eq!(look(&mut watch, now), std::slice::from_ref(&path));
        now += INTERVAL;
        assert!(look(&mut watch, now).is_empty(), "reported once");
        // Gone, then back.
        std::fs::remove_file(&path).unwrap();
        now += INTERVAL;
        assert_eq!(look(&mut watch, now), std::slice::from_ref(&path));
        std::fs::write(&path, b"back").unwrap();
        now += INTERVAL;
        assert_eq!(look(&mut watch, now), std::slice::from_ref(&path));
    }
}
