//! What the arrangement knows about the audio files its clips point at: each
//! file's rate and length from its header, straight away, and its waveform
//! from a background thread when that's done.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use noodle_io::{Decoder, Peaks};

use super::clips::Source;

/// How long a file that couldn't be read is left alone before the next try,
/// so relinking or re-exporting it shows up without a restart and a broken
/// path doesn't cost a read per frame.
const RETRY: Duration = Duration::from_secs(2);

/// A file that could be read.
#[derive(Clone)]
pub struct Loaded {
    pub source: Source,
    /// `None` while the waveform is still being worked out.
    pub peaks: Option<Arc<Peaks>>,
}

enum Entry {
    Found(Loaded),
    Missing(Instant),
}

pub struct Sources {
    entries: HashMap<PathBuf, Entry>,
    done: Receiver<(PathBuf, Option<Arc<Peaks>>)>,
    sender: Sender<(PathBuf, Option<Arc<Peaks>>)>,
    retry: Duration,
}

impl Default for Sources {
    fn default() -> Self {
        let (sender, done) = channel();
        Self {
            entries: HashMap::new(),
            done,
            sender,
            retry: RETRY,
        }
    }
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
        for (path, peaks) in self.done.try_iter() {
            if let (Some(Entry::Found(loaded)), Some(peaks)) = (self.entries.get_mut(&path), peaks)
            {
                loaded.source.frames.get_or_insert(peaks.frames());
                loaded.peaks = Some(peaks);
            }
        }
        let path = directory.unwrap_or(Path::new("")).join(name);
        match self.entries.get(&path) {
            Some(Entry::Found(loaded)) => return Some(loaded.clone()),
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
        self.entries
            .insert(path.clone(), Entry::Found(loaded.clone()));
        let (sender, ctx) = (self.sender.clone(), ctx.clone());
        std::thread::spawn(move || {
            let peaks = Peaks::from_file(&path).ok().map(Arc::new);
            let _ = sender.send((path, peaks));
            ctx.request_repaint();
        });
        Some(loaded)
    }
}
