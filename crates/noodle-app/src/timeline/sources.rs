//! What the arrangement knows about the audio files its clips point at.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::clips::Source;

/// Looks each file up once. A file that can't be read is remembered as
/// missing, so a broken path costs one failed read, not one per frame.
#[derive(Default)]
pub struct Sources {
    known: HashMap<PathBuf, Option<Source>>,
}

impl Sources {
    /// The file `name`, which is relative to `directory` (the folder of the
    /// project file; the working directory if it has none).
    pub fn get(&mut self, directory: Option<&Path>, name: &str) -> Option<Source> {
        let path = directory.unwrap_or(Path::new("")).join(name);
        *self.known.entry(path.clone()).or_insert_with(|| {
            let audio = noodle_io::read_wav(&path).ok()?;
            Some(Source {
                sample_rate: audio.sample_rate,
                frames: (audio.samples.len() / audio.channels.max(1)) as u64,
            })
        })
    }
}
