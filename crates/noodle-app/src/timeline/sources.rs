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

use noodle_io::{
    Decoder, Peaks, PeaksBuilder, TempoEstimate, detect_onsets, estimate_grid, estimate_tempo,
};

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
    /// The transients and tempo found along with the waveform.
    pub analysis: Option<Arc<Analysis>>,
}

/// How much of a file is listened to for transients: a long recording is
/// analysed from its start only, to keep the work and memory bounded.
const ANALYSIS_SECONDS: f64 = 120.0;

/// What the clip editor learns from a file's audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    /// Where transients start, in seconds from the start of the file.
    pub onsets: Vec<f64>,
    /// The tempo they sit on the beat of, if there's a clear one.
    pub tempo: Option<f64>,
    /// The tempo and where the beat falls, even when it is only a guess (see
    /// [`TempoEstimate::confidence`]). `None` if there are too few
    /// transients, or none agree on a tempo.
    pub grid: Option<TempoEstimate>,
    /// Whether the file is long enough, with transients close enough, that
    /// its beats are worth keeping whole when it is stretched.
    pub percussive: bool,
}

impl Analysis {
    fn of(mono: &[f32], rate: u32) -> Self {
        let onsets: Vec<f64> = detect_onsets(mono, rate)
            .into_iter()
            .map(|frame| frame as f64 / f64::from(rate))
            .collect();
        let seconds = mono.len() as f64 / f64::from(rate);
        Self {
            tempo: estimate_tempo(&onsets),
            grid: estimate_grid(&onsets),
            percussive: seconds > 0.0 && onsets.len() as f64 / seconds >= 1.0,
            onsets,
        }
    }
}

/// Reads the whole file once for its waveform, and the start of it for its
/// transients.
fn analyse(path: &Path) -> Option<(Arc<Peaks>, Arc<Analysis>)> {
    let mut decoder = Decoder::open(path).ok()?;
    let info = decoder.info();
    let mut builder = PeaksBuilder::new(info.channels, info.sample_rate);
    let limit = (f64::from(info.sample_rate) * ANALYSIS_SECONDS) as usize;
    let mut mono: Vec<f32> = Vec::new();
    let mut chunk = Vec::new();
    while decoder.read_chunk(&mut chunk).ok()?.is_some() {
        builder.push(&chunk);
        if mono.len() < limit {
            let room = limit - mono.len();
            mono.extend(
                chunk
                    .chunks_exact(info.channels.max(1))
                    .take(room)
                    .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32),
            );
        }
    }
    let analysis = Analysis::of(&mono, info.sample_rate);
    Some((Arc::new(builder.finish()), Arc::new(analysis)))
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
    /// Told once the worker has reported and asked for its repaint.
    #[cfg(test)]
    done: Sender<()>,
}

type Done = (
    PathBuf,
    Option<SystemTime>,
    Option<(Arc<Peaks>, Arc<Analysis>)>,
);

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
                loop {
                    // Take the job first, so the lock isn't held while it runs.
                    let job = queue.lock().unwrap().recv();
                    let Ok(job) = job else { break };
                    let peaks = analyse(&job.path);
                    let _ = sender.send((job.path, job.modified, peaks));
                    job.ctx.request_repaint();
                    #[cfg(test)]
                    let _ = job.done.send(());
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

    /// Queues a waveform job for `path`.
    ///
    /// Under test the call waits for the worker to report, so the repaint it
    /// asks for always lands inside the frame that queued the job and never in
    /// the middle of a later `Harness::run`, whose step count it would
    /// otherwise change from run to run. A worker that dies drops its
    /// acknowledgement, so the wait ends instead of hanging the test.
    fn submit(
        jobs: &Sender<Job>,
        path: PathBuf,
        modified: Option<SystemTime>,
        ctx: &egui::Context,
    ) {
        #[cfg(test)]
        let (done, acked) = channel();
        let job = Job {
            path,
            modified,
            ctx: ctx.clone(),
            #[cfg(test)]
            done,
        };
        let _ = jobs.send(job);
        #[cfg(test)]
        let _ = acked.recv();
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
                        Self::submit(&self.jobs, path.clone(), found.modified, ctx);
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
            analysis: None,
        };
        let modified = modified(&path);
        Self::submit(&self.jobs, path.clone(), modified, ctx);
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
                Some((peaks, analysis)) => {
                    found.loaded.source.frames.get_or_insert(peaks.frames());
                    found.loaded.peaks = Some(peaks);
                    found.loaded.analysis = Some(analysis);
                    found.waveform = Waveform::Ready;
                }
                None => found.waveform = Waveform::Failed(Instant::now()),
            }
        }
    }
}
