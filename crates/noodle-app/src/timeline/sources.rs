//! What the arrangement knows about the audio files its clips point at: each
//! file's rate and length from its header, straight away, and its waveform
//! from a couple of background workers when that's done.
//!
//! A file is looked at again every couple of seconds: one that couldn't be
//! read is tried again, one whose waveform failed gets another go, and one
//! whose modification time changed (a re-export) is read afresh.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// Jobs sent and not yet reported, repaint included.
    in_flight: Arc<AtomicUsize>,
}

impl Default for Sources {
    fn default() -> Self {
        let (jobs, queue) = channel::<Job>();
        let (sender, done) = channel::<Done>();
        let queue = Arc::new(Mutex::new(queue));
        let in_flight = Arc::new(AtomicUsize::new(0));
        for _ in 0..WORKERS {
            let (queue, sender) = (queue.clone(), sender.clone());
            let in_flight = in_flight.clone();
            std::thread::spawn(move || {
                // Ends when the `Sources` is dropped.
                loop {
                    // Take the job first, so the lock isn't held while it runs.
                    let job = queue.lock().unwrap().recv();
                    let Ok(job) = job else { break };
                    let peaks = Peaks::from_file(&job.path).ok().map(Arc::new);
                    let _ = sender.send((job.path, job.modified, peaks));
                    job.ctx.request_repaint();
                    in_flight.fetch_sub(1, Ordering::Release);
                }
            });
        }
        Self {
            entries: HashMap::new(),
            jobs,
            done,
            retry: RETRY,
            in_flight,
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

    /// Queues a waveform job.
    ///
    /// Under test the call waits for the workers to report, so the repaint a
    /// worker asks for always lands inside the frame that queued the job and
    /// never in the middle of a later `Harness::run`, whose step count it
    /// would otherwise change from run to run.
    fn submit(jobs: &Sender<Job>, in_flight: &AtomicUsize, job: Job) {
        in_flight.fetch_add(1, Ordering::Acquire);
        if jobs.send(job).is_err() {
            in_flight.fetch_sub(1, Ordering::Release);
        }
        #[cfg(test)]
        while in_flight.load(Ordering::Acquire) > 0 {
            std::thread::yield_now();
        }
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
                        Self::submit(&self.jobs, &self.in_flight, job);
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
        Self::submit(&self.jobs, &self.in_flight, job);
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
