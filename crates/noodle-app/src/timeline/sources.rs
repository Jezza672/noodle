//! What the arrangement knows about the audio files its clips point at: each
//! file's rate and length from its header, straight away, and its waveform
//! from a couple of background workers when that's done.
//!
//! Nothing here polls. [`noodle_io::AudioFiles`] watches the files and wakes
//! the window when one changes: a file that couldn't be read is tried again
//! when its stamp (size and modification time) differs, and so is one whose
//! waveform failed, and one that changed under a loaded waveform (a
//! re-export) is read afresh.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

pub use noodle_io::Analysis;
use noodle_io::{ANALYSIS_SECONDS, AudioFiles, Decoder, FileStamp, Peaks, PeaksBuilder};

use super::clips::Source;

/// How many waveforms are worked out at once, however many files a project
/// has.
const WORKERS: usize = 2;

/// A file that could be read.
#[derive(Clone)]
pub struct Loaded {
    /// Where the file is, for reading samples out of it.
    pub path: PathBuf,
    pub source: Source,
    /// `None` while the waveform is still being worked out.
    pub peaks: Option<Arc<Peaks>>,
    /// The transients and tempo found along with the waveform.
    pub analysis: Option<Arc<Analysis>>,
    /// The audio itself, when it is in memory (a Sampler's sample) and `path`
    /// names nothing.
    pub memory: Option<Arc<noodle_nodes::Sample>>,
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
    /// The attempt failed. It is tried again if the file changes.
    Failed,
}

struct Found {
    loaded: Loaded,
    /// The file's stamp as of when it was read.
    stamp: Option<FileStamp>,
    waveform: Waveform,
}

enum Entry {
    Found(Found),
    /// Couldn't be opened when the file had this stamp.
    Missing(Option<FileStamp>),
}

struct Job {
    path: PathBuf,
    stamp: Option<FileStamp>,
    ctx: egui::Context,
    /// Told once the worker has reported and asked for its repaint.
    #[cfg(test)]
    done: Sender<()>,
}

type Done = (
    PathBuf,
    Option<FileStamp>,
    Option<(Arc<Peaks>, Arc<Analysis>)>,
);

pub struct Sources {
    entries: HashMap<PathBuf, Entry>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    files: &'static AudioFiles,
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
                    let _ = sender.send((job.path, job.stamp, peaks));
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
            files: AudioFiles::global(),
        }
    }
}

impl Sources {
    /// Has the files looked at every few milliseconds, so a test need not
    /// wait a second to see a change.
    #[cfg(test)]
    pub fn watch_quickly(&mut self) {
        let files = AudioFiles::new(std::time::Duration::from_millis(5), || {});
        self.files = Box::leak(Box::new(files));
    }

    /// Queues a waveform job for `path`.
    ///
    /// Under test the call waits for the worker to report, so the repaint it
    /// asks for always lands inside the frame that queued the job and never in
    /// the middle of a later `Harness::run`, whose step count it would
    /// otherwise change from run to run. A worker that dies drops its
    /// acknowledgement, so the wait ends instead of hanging the test.
    fn submit(jobs: &Sender<Job>, path: PathBuf, stamp: Option<FileStamp>, ctx: &egui::Context) {
        #[cfg(test)]
        let (done, acked) = channel();
        let job = Job {
            path,
            stamp,
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
        let stamp = self.files.stamp(&path);
        match self.entries.get(&path) {
            Some(Entry::Found(found)) if found.stamp == stamp => {
                return Some(found.loaded.clone());
            }
            // Same file as the failed attempt: it would fail again. If it
            // changes the watcher wakes the window.
            Some(Entry::Missing(at)) if *at == stamp => return None,
            // Changed since it was read: start over.
            _ => {
                self.entries.remove(&path);
            }
        }
        let Ok(decoder) = Decoder::open(&path) else {
            self.entries.insert(path, Entry::Missing(stamp));
            return None;
        };
        let info = decoder.info();
        let loaded = Loaded {
            path: path.clone(),
            source: Source {
                sample_rate: info.sample_rate,
                frames: info.frames,
            },
            peaks: None,
            analysis: None,
            memory: None,
        };
        Self::submit(&self.jobs, path.clone(), stamp, ctx);
        self.entries.insert(
            path,
            Entry::Found(Found {
                loaded: loaded.clone(),
                stamp,
                waveform: Waveform::Working,
            }),
        );
        Some(loaded)
    }

    /// Takes in the waveforms the workers have finished.
    fn collect(&mut self) {
        for (path, stamp, peaks) in self.done.try_iter() {
            let Some(Entry::Found(found)) = self.entries.get_mut(&path) else {
                continue;
            };
            // A result for a file that has changed since is out of date.
            if found.stamp != stamp {
                continue;
            }
            match peaks {
                Some((peaks, analysis)) => {
                    found.loaded.source.frames.get_or_insert(peaks.frames());
                    found.loaded.peaks = Some(peaks);
                    found.loaded.analysis = Some(analysis);
                    found.waveform = Waveform::Ready;
                }
                None => found.waveform = Waveform::Failed,
            }
        }
    }
}
