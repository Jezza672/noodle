//! What the arrangement knows about the audio files its clips point at: each
//! file's rate and length from its header, straight away, and its waveform
//! from a couple of background workers when that's done.
//!
//! A file is looked at again every couple of seconds: one that couldn't be
//! read is tried again, one whose waveform failed gets another go, and one
//! whose modification time changed (a re-export) is read afresh.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use noodle_io::{Decoder, Peaks};

use super::clips::Source;

/// How long to leave a file alone before looking at it again, so a broken
/// path doesn't cost a read per frame.
const RETRY: Duration = Duration::from_secs(2);
/// How many waveforms are worked out at once, however many files a project
/// has.
const WORKERS: usize = 2;

/// A file that could be read.
#[derive(Clone)]
pub struct Loaded {
    pub source: Source,
    /// `None` while the waveform is still being worked out.
    pub peaks: Option<Arc<Peaks>>,
}

enum Waveform {
    Working,
    Ready,
    /// When the attempt failed, so it's tried again after a while.
    Failed(Instant),
}

struct Found {
    loaded: Loaded,
    /// When the file was last changed, as of when it was read.
    modified: Option<SystemTime>,
    /// When it was last looked at.
    checked: Instant,
    waveform: Waveform,
}

enum Entry {
    Found(Found),
    Missing(Instant),
}

struct Job {
    path: PathBuf,
    modified: Option<SystemTime>,
    ctx: egui::Context,
}

type Done = (PathBuf, Option<SystemTime>, Option<Arc<Peaks>>);

pub struct Sources {
    entries: HashMap<PathBuf, Entry>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    retry: Duration,
}

impl Default for Sources {
    fn default() -> Self {
        let (jobs, queue) = channel::<Job>();
        let (sender, done) = channel::<Done>();
        let queue = Arc::new(Mutex::new(queue));
        for _ in 0..WORKERS {
            let (queue, sender) = (queue.clone(), sender.clone());
            std::thread::spawn(move || {
                // Ends when the `Sources` is dropped.
                while let Ok(job) = queue.lock().unwrap().recv() {
                    let peaks = Peaks::from_file(&job.path).ok().map(Arc::new);
                    let _ = sender.send((job.path, job.modified, peaks));
                    job.ctx.request_repaint();
                }
            });
        }
        Self {
            entries: HashMap::new(),
            jobs,
            done,
            retry: RETRY,
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

impl Sources {
    #[cfg(test)]
    pub fn retry_at_once(&mut self) {
        self.retry = Duration::ZERO;
    }

    /// The file `name`, which is relative to `directory` (the folder of the
    /// project file; the working directory if it has none). `None` if it
    /// can't be read.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        directory: Option<&Path>,
        name: &str,
    ) -> Option<Loaded> {
        self.collect();
        let path = directory.unwrap_or(Path::new("")).join(name);
        match self.entries.get_mut(&path) {
            Some(Entry::Found(found)) => {
                if found.checked.elapsed() < self.retry {
                    return Some(found.loaded.clone());
                }
                found.checked = Instant::now();
                ctx.request_repaint_after(self.retry);
                if modified(&path) == found.modified {
                    let again = matches!(found.waveform, Waveform::Failed(at) if at.elapsed() >= self.retry);
                    if again {
                        found.waveform = Waveform::Working;
                        let job = Job {
                            path: path.clone(),
                            modified: found.modified,
                            ctx: ctx.clone(),
                        };
                        let _ = self.jobs.send(job);
                    }
                    return Some(found.loaded.clone());
                }
                // Changed since it was read: start over.
                self.entries.remove(&path);
            }
            Some(Entry::Missing(since)) if since.elapsed() < self.retry => return None,
            _ => {}
        }
        let Ok(decoder) = Decoder::open(&path) else {
            self.entries.insert(path, Entry::Missing(Instant::now()));
            // Look again later even if nothing else repaints.
            ctx.request_repaint_after(self.retry);
            return None;
        };
        let info = decoder.info();
        let loaded = Loaded {
            source: Source {
                sample_rate: info.sample_rate,
                frames: info.frames,
            },
            peaks: None,
        };
        let modified = modified(&path);
        let job = Job {
            path: path.clone(),
            modified,
            ctx: ctx.clone(),
        };
        let _ = self.jobs.send(job);
        self.entries.insert(
            path,
            Entry::Found(Found {
                loaded: loaded.clone(),
                modified,
                checked: Instant::now(),
                waveform: Waveform::Working,
            }),
        );
        ctx.request_repaint_after(self.retry);
        Some(loaded)
    }

    /// Takes in the waveforms the workers have finished.
    fn collect(&mut self) {
        for (path, modified, peaks) in self.done.try_iter() {
            let Some(Entry::Found(found)) = self.entries.get_mut(&path) else {
                continue;
            };
            // A result for a file that has changed since is out of date.
            if found.modified != modified {
                continue;
            }
            match peaks {
                Some(peaks) => {
                    found.loaded.source.frames.get_or_insert(peaks.frames());
                    found.loaded.peaks = Some(peaks);
                    found.waveform = Waveform::Ready;
                }
                None => found.waveform = Waveform::Failed(Instant::now()),
            }
        }
    }
}
