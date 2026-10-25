//! Samples for the Sampler, kept by the hash of their contents (ADR 0004).
//!
//! A [`SampleStore`] maps a hash to decoded audio held in memory. When it has
//! a directory, a sample it doesn't hold is read from `<hash>.wav` there, and
//! [`SampleStore::save`] writes one. The Sampler's `sample` setting is the hash,
//! so a project, a preset and a saved group all name the same audio, and the
//! node plays the same on every machine that has the file.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use noodle_core::CacheKey;
use noodle_io::{Audio, DecodeError, WavError, decode_file, read_wav, write_wav};

/// Decoded audio, as two channels (a mono file is the same in both).
#[derive(Debug, PartialEq)]
pub struct Sample {
    pub left: Box<[f32]>,
    pub right: Box<[f32]>,
    pub sample_rate: u32,
}

impl Sample {
    /// Converts file audio: mono is duplicated, stereo kept, and anything
    /// wider keeps its first two channels.
    pub fn from_audio(audio: &Audio) -> Self {
        let channels = audio.channels.max(1);
        let frames = audio.samples.len() / channels;
        let channel = |c: usize| -> Box<[f32]> {
            (0..frames)
                .map(|f| audio.samples[f * channels + c.min(channels - 1)])
                .collect()
        };
        Self {
            left: channel(0),
            right: channel(1),
            sample_rate: audio.sample_rate,
        }
    }

    pub fn frames(&self) -> usize {
        self.left.len()
    }

    /// True when both channels are the same.
    pub fn is_mono(&self) -> bool {
        self.left == self.right
    }

    /// The file form: interleaved stereo, or mono if both channels match.
    fn to_audio(&self) -> Audio {
        if self.is_mono() {
            return Audio {
                samples: self.left.to_vec(),
                channels: 1,
                sample_rate: self.sample_rate,
            };
        }
        let samples = self
            .left
            .iter()
            .zip(&self.right)
            .flat_map(|(&l, &r)| [l, r])
            .collect();
        Audio {
            samples,
            channels: 2,
            sample_rate: self.sample_rate,
        }
    }

    /// The hash of the audio: its channel count, rate and samples, not the
    /// file it came from, so the same sound in another format is one sample.
    pub fn hash(&self) -> String {
        let audio = self.to_audio();
        let mut bytes = Vec::with_capacity(8 + audio.samples.len() * 4);
        bytes.extend_from_slice(&(audio.channels as u32).to_le_bytes());
        bytes.extend_from_slice(&audio.sample_rate.to_le_bytes());
        for x in &audio.samples {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        CacheKey::of_reader(&bytes[..])
            .expect("reading a slice can't fail")
            .to_hex()
    }
}

/// A reference to a sample, or to a window of one: what a sample wire carries.
/// As text it is the hash alone, or `hash:start:end` with the window as
/// fractions of the sample (see [`NodeType::sample_output`](noodle_engine::NodeType::sample_output)).
#[derive(Clone, Debug, PartialEq)]
pub struct SampleRef {
    pub hash: String,
    pub start: f32,
    pub end: f32,
}

impl SampleRef {
    /// The whole sample.
    pub fn whole(hash: impl Into<String>) -> Self {
        Self {
            hash: hash.into(),
            start: 0.0,
            end: 1.0,
        }
    }

    /// Reads the text form. A window that is missing or doesn't parse is the
    /// whole sample.
    pub fn parse(text: &str) -> Self {
        let mut parts = text.split(':');
        let hash = parts.next().unwrap_or_default().to_owned();
        let number = |p: Option<&str>| p.and_then(|p| p.parse::<f32>().ok());
        match (number(parts.next()), number(parts.next())) {
            (Some(start), Some(end)) if start.is_finite() && end.is_finite() => Self {
                hash,
                start: start.clamp(0.0, 1.0),
                end: end.clamp(0.0, 1.0),
            },
            _ => Self::whole(hash),
        }
    }

    pub fn is_whole(&self) -> bool {
        self.start <= 0.0 && self.end >= 1.0
    }

    pub fn to_text(&self) -> String {
        if self.is_whole() {
            self.hash.clone()
        } else {
            format!("{}:{}:{}", self.hash, self.start, self.end)
        }
    }
}

impl Sample {
    /// The part of the sample between two fractions of it, as a sample of its
    /// own. The whole sample is shared, not copied.
    pub fn window(self: &Arc<Self>, start: f32, end: f32) -> Arc<Sample> {
        let frames = self.frames();
        let a = ((start.clamp(0.0, 1.0) as f64) * frames as f64).round() as usize;
        let b = ((end.clamp(0.0, 1.0) as f64) * frames as f64).round() as usize;
        if a == 0 && b >= frames {
            return Arc::clone(self);
        }
        let b = b.max(a);
        Arc::new(Sample {
            left: self.left[a..b].into(),
            right: self.right[a..b].into(),
            sample_rate: self.sample_rate,
        })
    }
}

#[derive(Debug)]
pub enum SampleError {
    Decode(DecodeError),
    Wav(WavError),
    /// No directory is set to save to.
    NoDirectory,
    NotFound(String),
}

impl fmt::Display for SampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(e) => e.fmt(f),
            Self::Wav(e) => write!(f, "can't write the sample: {e:?}"),
            Self::NoDirectory => f.write_str("the sample store has no folder to save to"),
            Self::NotFound(hash) => write!(f, "no sample with hash {hash}"),
        }
    }
}

impl std::error::Error for SampleError {}

impl From<DecodeError> for SampleError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

impl From<WavError> for SampleError {
    fn from(e: WavError) -> Self {
        Self::Wav(e)
    }
}

#[derive(Default)]
struct Inner {
    dir: Option<PathBuf>,
    samples: HashMap<String, Arc<Sample>>,
    /// Hashes the folder did not have when last looked, so a view that asks
    /// every frame does not read the disk every frame. Forgotten when the
    /// folder changes or a sample comes in.
    missing: std::collections::HashSet<String>,
    /// The fundamental found for a hash, `None` if it has no pitch. The
    /// audio behind a hash never changes, so this is never stale.
    pitch: HashMap<String, Option<f32>>,
}

/// Samples by content hash. Clones share the store.
#[derive(Clone, Default)]
pub struct SampleStore {
    inner: Arc<Mutex<Inner>>,
}

impl SampleStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the folder that samples not in memory are read from and
    /// [`save`](Self::save) writes to, normally `samples` beside the project
    /// file.
    pub fn set_dir(&self, dir: impl Into<PathBuf>) {
        let mut inner = self.inner.lock().unwrap();
        inner.dir = Some(dir.into());
        inner.missing.clear();
    }

    /// Like [`set_dir`](Self::set_dir) but `None` means there is no folder
    /// yet (a project that has not been saved), so samples stay in memory.
    pub fn set_dir_option(&self, dir: Option<PathBuf>) {
        let mut inner = self.inner.lock().unwrap();
        inner.dir = dir;
        inner.missing.clear();
    }

    pub fn dir(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().dir.clone()
    }

    /// Adds audio and returns its hash.
    pub fn insert(&self, audio: &Audio) -> String {
        self.insert_sample(Sample::from_audio(audio))
    }

    pub fn insert_sample(&self, sample: Sample) -> String {
        let hash = sample.hash();
        let mut inner = self.inner.lock().unwrap();
        inner.missing.remove(&hash);
        inner
            .samples
            .entry(hash.clone())
            .or_insert_with(|| Arc::new(sample));
        hash
    }

    /// Decodes a file (any format the app reads) into the store and returns
    /// its hash.
    pub fn import(&self, path: &Path) -> Result<String, SampleError> {
        Ok(self.insert(&decode_file(path)?))
    }

    /// The sample with this hash, from memory or else the folder.
    pub fn get(&self, hash: &str) -> Option<Arc<Sample>> {
        let dir = {
            let inner = self.inner.lock().unwrap();
            if let Some(sample) = inner.samples.get(hash) {
                return Some(sample.clone());
            }
            if inner.missing.contains(hash) {
                return None;
            }
            inner.dir.clone()?
        };
        // Only a hash that looks like one becomes a path.
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let found = read_wav(&dir.join(format!("{hash}.wav")))
            .ok()
            .map(|audio| Arc::new(Sample::from_audio(&audio)))
            // The file's name is a claim; the audio is what counts.
            .filter(|sample| sample.hash() == hash);
        let Some(sample) = found else {
            self.inner.lock().unwrap().missing.insert(hash.to_owned());
            return None;
        };
        self.inner
            .lock()
            .unwrap()
            .samples
            .insert(hash.to_owned(), sample.clone());
        Some(sample)
    }

    /// The key number (fractional, 69 is A 440) of the fundamental of the part
    /// of a sample `reference` names, from its first moments (see
    /// [`crate::fundamental()`]), or `None` if the sample isn't there or has no
    /// clear pitch. Remembered by reference.
    pub fn pitch(&self, reference: &SampleRef) -> Option<f32> {
        let cache_key = reference.to_text();
        if let Some(&found) = self.inner.lock().unwrap().pitch.get(&cache_key) {
            return found;
        }
        let sample = self
            .get(&reference.hash)?
            .window(reference.start, reference.end);
        let found =
            crate::fundamental::fundamental(&sample.left, &sample.right, sample.sample_rate)
                .and_then(crate::fundamental::key_of);
        self.inner.lock().unwrap().pitch.insert(cache_key, found);
        found
    }

    /// Writes the sample to the folder as `<hash>.wav` (kept if already
    /// there).
    pub fn save(&self, hash: &str) -> Result<PathBuf, SampleError> {
        let dir = self.dir().ok_or(SampleError::NoDirectory)?;
        let sample = self
            .get(hash)
            .ok_or_else(|| SampleError::NotFound(hash.to_owned()))?;
        let path = dir.join(format!("{hash}.wav"));
        if !path.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| SampleError::Decode(e.into()))?;
            let audio = sample.to_audio();
            write_wav(&path, &audio.samples, audio.channels, audio.sample_rate)?;
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(samples: Vec<f32>, channels: usize) -> Audio {
        Audio {
            samples,
            channels,
            sample_rate: 44_100,
        }
    }

    #[test]
    fn the_same_audio_has_one_hash_and_different_audio_another() {
        let store = SampleStore::new();
        let a = store.insert(&audio(vec![0.1, 0.2, 0.3], 1));
        assert_eq!(a, store.insert(&audio(vec![0.1, 0.2, 0.3], 1)));
        assert_ne!(a, store.insert(&audio(vec![0.1, 0.2, 0.4], 1)));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn a_reference_round_trips_through_its_text() {
        let whole = SampleRef::whole("abc");
        assert_eq!(whole.to_text(), "abc");
        assert_eq!(SampleRef::parse("abc"), whole);
        let part = SampleRef {
            hash: "abc".into(),
            start: 0.25,
            end: 0.75,
        };
        assert_eq!(SampleRef::parse(&part.to_text()), part);
        assert_eq!(SampleRef::parse("abc:x:y"), whole);
        assert_eq!(SampleRef::parse(""), SampleRef::whole(""));
    }

    #[test]
    fn a_window_is_the_part_between_the_fractions() {
        let sample = Arc::new(Sample::from_audio(&audio(
            (0..100).map(|i| i as f32).collect(),
            1,
        )));
        let part = sample.window(0.25, 0.5);
        assert_eq!(part.frames(), 25);
        assert_eq!(part.left[0], 25.0);
        assert!(Arc::ptr_eq(&sample, &sample.window(0.0, 1.0)));
        assert_eq!(sample.window(0.6, 0.2).frames(), 0);
    }

    #[test]
    fn the_pitch_of_a_hash_is_found_once() {
        let store = SampleStore::new();
        let tone: Vec<f32> = (0..44_100)
            .map(|i| 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / 44_100.0).sin())
            .collect();
        let hash = store.insert(&audio(tone, 1));
        assert!((store.pitch(&SampleRef::whole(&hash)).unwrap() - 69.0).abs() < 0.1);
        assert!(store.inner.lock().unwrap().pitch.contains_key(&hash));
        // A window of the same sample is its own entry.
        assert!(
            store
                .pitch(&SampleRef {
                    hash: hash.clone(),
                    start: 0.0,
                    end: 0.5
                })
                .is_some()
        );
        let silence = store.insert(&audio(vec![0.0; 44_100], 1));
        assert_eq!(store.pitch(&SampleRef::whole(&silence)), None);
        assert_eq!(store.pitch(&SampleRef::whole("missing")), None);
    }

    #[test]
    fn mono_is_duplicated_and_stereo_kept() {
        let mono = Sample::from_audio(&audio(vec![0.5, 0.25], 1));
        assert_eq!(&*mono.right, &[0.5, 0.25]);
        let stereo = Sample::from_audio(&audio(vec![0.5, -0.5, 0.25, -0.25], 2));
        assert_eq!(&*stereo.left, &[0.5, 0.25]);
        assert_eq!(&*stereo.right, &[-0.5, -0.25]);
    }

    #[test]
    fn saved_samples_load_into_a_fresh_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = SampleStore::new();
        store.set_dir(dir.path());
        let hash = store.insert(&audio(vec![0.5, -0.5, 0.25, -0.125], 2));
        store.save(&hash).unwrap();

        let fresh = SampleStore::new();
        assert!(fresh.get(&hash).is_none(), "no folder, nothing in memory");
        fresh.set_dir(dir.path());
        let loaded = fresh.get(&hash).unwrap();
        assert_eq!(&*loaded.left, &[0.5, 0.25]);
        assert_eq!(loaded.hash(), hash);
    }

    #[test]
    fn a_file_that_does_not_match_its_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fake = "a".repeat(64);
        write_wav(
            &dir.path().join(format!("{fake}.wav")),
            &[0.1, 0.2],
            1,
            44_100,
        )
        .unwrap();
        let store = SampleStore::new();
        store.set_dir(dir.path());
        assert!(store.get(&fake).is_none());
        assert!(store.get("../escape").is_none());
    }
}
