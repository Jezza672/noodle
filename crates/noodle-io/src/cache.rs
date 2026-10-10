//! The on-disk render cache: audio stored under a content-addressed
//! [`CacheKey`].
//!
//! An entry is a small header followed by raw little-endian `f32` samples, so
//! any range can be read back by seeking (offline nodes such as Reverse need
//! that), and the file length alone says whether it was finished. Entries are
//! written to a `.partial` file and renamed into place on
//! [`CacheWriter::commit`], so a reader never sees half a render, and a
//! render that is cancelled, fails or is dropped leaves nothing behind.
//! Keys name their contents, so an entry is never overwritten with something
//! different and a stale one is never served; the only way to lose one is
//! [`CacheStore::evict_to`] or deleting the folder.
//!
//! Nothing here is for the audio thread: every call does file I/O.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use noodle_core::CacheKey;

const MAGIC: [u8; 4] = *b"NRC1";
const HEADER_BYTES: u64 = 32;
const EXTENSION: &str = "nrc";
/// A `.partial` file this old can't belong to a render still running.
const STALE_PARTIAL: Duration = Duration::from_secs(60 * 60);
static PARTIALS: AtomicU64 = AtomicU64::new(0);

/// What a cached render holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheInfo {
    pub channels: usize,
    pub sample_rate: u32,
    pub frames: u64,
}

impl CacheInfo {
    fn data_bytes(&self) -> Option<u64> {
        self.frames
            .checked_mul(self.channels as u64)?
            .checked_mul(4)
    }

    fn header(&self) -> [u8; HEADER_BYTES as usize] {
        let mut bytes = [0; HEADER_BYTES as usize];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[4..8].copy_from_slice(&(self.channels as u32).to_le_bytes());
        bytes[8..12].copy_from_slice(&self.sample_rate.to_le_bytes());
        bytes[12..20].copy_from_slice(&self.frames.to_le_bytes());
        bytes
    }

    fn parse(bytes: &[u8; HEADER_BYTES as usize]) -> Option<Self> {
        if bytes[0..4] != MAGIC {
            return None;
        }
        let channels = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        if channels == 0 {
            return None;
        }
        Some(Self {
            channels,
            sample_rate: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            frames: u64::from_le_bytes(bytes[12..20].try_into().unwrap()),
        })
    }
}

/// A folder of cached renders.
#[derive(Clone, Debug)]
pub struct CacheStore {
    dir: PathBuf,
}

impl CacheStore {
    /// Opens (creating if need be) the store in `dir`, and clears out
    /// `.partial` files that an earlier crash left behind.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let store = Self { dir };
        let now = SystemTime::now();
        for entry in store.files()? {
            if entry.partial
                && now.duration_since(entry.modified).unwrap_or_default() > STALE_PARTIAL
            {
                let _ = fs::remove_file(&entry.path);
            }
        }
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: &CacheKey) -> PathBuf {
        self.dir.join(format!("{}.{EXTENSION}", key.to_hex()))
    }

    /// Whether a complete, valid entry exists for `key`.
    pub fn contains(&self, key: &CacheKey) -> bool {
        self.get(key).is_some()
    }

    /// Opens the entry for `key`. A damaged entry (truncated, or with a bad
    /// header) is deleted and reported as missing, so the caller just
    /// renders it again. Marks the entry as recently used.
    pub fn get(&self, key: &CacheKey) -> Option<CachedAudio> {
        let path = self.path(key);
        let audio = CachedAudio::open(&path);
        match &audio {
            Ok(_) => {
                // Best effort: only used to pick what to evict.
                if let Ok(file) = File::options().write(true).open(&path) {
                    let _ = file.set_modified(SystemTime::now());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                let _ = fs::remove_file(&path);
            }
        }
        audio.ok()
    }

    /// Starts writing the entry for `key`. Nothing is visible until
    /// [`CacheWriter::commit`].
    pub fn writer(
        &self,
        key: &CacheKey,
        channels: usize,
        sample_rate: u32,
    ) -> io::Result<CacheWriter> {
        if channels == 0 || channels > u32::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bad channel count",
            ));
        }
        let partial = self.dir.join(format!(
            "{}.{}.{}.partial",
            key.to_hex(),
            std::process::id(),
            PARTIALS.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = BufWriter::new(File::create(&partial)?);
        let info = CacheInfo {
            channels,
            sample_rate,
            frames: 0,
        };
        // Placeholder header; the real frame count is written on commit.
        file.write_all(&info.header())?;
        Ok(CacheWriter {
            file: Some(file),
            partial,
            target: self.path(key),
            info,
            scratch: Vec::new(),
        })
    }

    pub fn remove(&self, key: &CacheKey) -> io::Result<()> {
        match fs::remove_file(self.path(key)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// The bytes used by finished entries.
    pub fn total_bytes(&self) -> io::Result<u64> {
        Ok(self
            .files()?
            .iter()
            .filter(|f| !f.partial)
            .map(|f| f.bytes)
            .sum())
    }

    /// Deletes the least recently used entries until the store holds at
    /// most `max_bytes`. Returns how many bytes were freed.
    pub fn evict_to(&self, max_bytes: u64) -> io::Result<u64> {
        let mut entries: Vec<_> = self.files()?.into_iter().filter(|f| !f.partial).collect();
        let mut total: u64 = entries.iter().map(|f| f.bytes).sum();
        entries.sort_by_key(|f| f.modified);
        let mut freed = 0;
        for entry in entries {
            if total <= max_bytes {
                break;
            }
            if fs::remove_file(&entry.path).is_ok() {
                total -= entry.bytes;
                freed += entry.bytes;
            }
        }
        Ok(freed)
    }

    fn files(&self) -> io::Result<Vec<StoreFile>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            let partial = match path.extension().and_then(|e| e.to_str()) {
                Some("partial") => true,
                Some(EXTENSION) => false,
                _ => continue,
            };
            let Ok(meta) = entry.metadata() else { continue };
            files.push(StoreFile {
                path,
                partial,
                bytes: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
        Ok(files)
    }
}

struct StoreFile {
    path: PathBuf,
    partial: bool,
    bytes: u64,
    modified: SystemTime,
}

/// Writes one entry. Dropping it without calling [`commit`](Self::commit)
/// deletes the partial file.
pub struct CacheWriter {
    file: Option<BufWriter<File>>,
    partial: PathBuf,
    target: PathBuf,
    info: CacheInfo,
    scratch: Vec<u8>,
}

impl CacheWriter {
    /// Appends interleaved samples, a whole number of frames.
    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        if !samples.len().is_multiple_of(self.info.channels) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "samples are not a whole number of frames",
            ));
        }
        self.scratch.clear();
        self.scratch
            .extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        self.file
            .as_mut()
            .expect("writer used after commit")
            .write_all(&self.scratch)?;
        self.info.frames += (samples.len() / self.info.channels) as u64;
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.info.frames
    }

    /// Finishes the entry and makes it visible to readers.
    pub fn commit(mut self) -> io::Result<CacheInfo> {
        let mut file = self.file.take().expect("writer used after commit");
        file.flush()?;
        let mut file = file.into_inner().map_err(|e| e.into_error())?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&self.info.header())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.partial, &self.target)?;
        Ok(self.info)
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.partial);
        }
    }
}

/// A finished entry, readable at any position.
#[derive(Debug)]
pub struct CachedAudio {
    file: File,
    info: CacheInfo,
}

impl CachedAudio {
    fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
        let mut header = [0; HEADER_BYTES as usize];
        file.read_exact(&mut header)
            .map_err(|_| bad("truncated header"))?;
        let info = CacheInfo::parse(&header).ok_or_else(|| bad("bad header"))?;
        let expected = info.data_bytes().and_then(|d| d.checked_add(HEADER_BYTES));
        if expected != Some(file.metadata()?.len()) {
            return Err(bad("wrong length"));
        }
        Ok(Self { file, info })
    }

    pub fn info(&self) -> CacheInfo {
        self.info
    }

    /// Reads interleaved frames starting at frame `start` into `out` (a
    /// whole number of frames). Returns how many frames were read, fewer than
    /// `out` holds only at the end of the entry.
    pub fn read_frames(&mut self, start: u64, out: &mut [f32]) -> io::Result<usize> {
        let channels = self.info.channels;
        let wanted = (out.len() / channels) as u64;
        let frames = wanted.min(self.info.frames.saturating_sub(start)) as usize;
        if frames == 0 {
            return Ok(0);
        }
        self.file
            .seek(SeekFrom::Start(HEADER_BYTES + start * channels as u64 * 4))?;
        let mut bytes = vec![0u8; frames * channels * 4];
        self.file.read_exact(&mut bytes)?;
        let (raw, _) = bytes.as_chunks::<4>();
        for (sample, raw) in out.iter_mut().zip(raw) {
            *sample = f32::from_le_bytes(*raw);
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::KeyBuilder;

    fn key(n: u64) -> CacheKey {
        KeyBuilder::new("test").u64(n).finish()
    }

    fn store() -> (tempfile::TempDir, CacheStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::open(dir.path()).unwrap();
        (dir, store)
    }

    fn put(store: &CacheStore, key: &CacheKey, samples: &[f32]) {
        let mut w = store.writer(key, 2, 48_000).unwrap();
        w.write(samples).unwrap();
        w.commit().unwrap();
    }

    #[test]
    fn what_is_written_reads_back_from_any_position() {
        let (_dir, store) = store();
        let samples: Vec<f32> = (0..20).map(|i| i as f32).collect();
        let mut w = store.writer(&key(1), 2, 44_100).unwrap();
        w.write(&samples[..6]).unwrap();
        w.write(&samples[6..]).unwrap();
        let info = w.commit().unwrap();
        assert_eq!(
            info,
            CacheInfo {
                channels: 2,
                sample_rate: 44_100,
                frames: 10
            }
        );

        let mut audio = store.get(&key(1)).unwrap();
        assert_eq!(audio.info(), info);
        let mut out = [0.0; 6];
        assert_eq!(audio.read_frames(4, &mut out).unwrap(), 3);
        assert_eq!(out, [8.0, 9.0, 10.0, 11.0, 12.0, 13.0]);
        // Reading backwards works as well as forwards.
        assert_eq!(audio.read_frames(0, &mut out).unwrap(), 3);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        // Past the end is a short read.
        assert_eq!(audio.read_frames(9, &mut out).unwrap(), 1);
        assert_eq!(&out[..2], &[18.0, 19.0]);
        assert_eq!(audio.read_frames(10, &mut out).unwrap(), 0);
    }

    #[test]
    fn an_uncommitted_write_is_invisible_and_cleaned_up() {
        let (dir, store) = store();
        let mut w = store.writer(&key(1), 2, 48_000).unwrap();
        w.write(&[0.0; 4]).unwrap();
        assert!(!store.contains(&key(1)));
        drop(w);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_missing_key_is_a_miss() {
        let (_dir, store) = store();
        assert!(store.get(&key(1)).is_none());
    }

    #[test]
    fn a_damaged_entry_is_deleted_and_reported_missing() {
        let (dir, store) = store();
        put(&store, &key(1), &[1.0; 8]);
        let path = dir.path().join(format!("{}.nrc", key(1).to_hex()));
        let len = fs::metadata(&path).unwrap().len();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(len - 4)
            .unwrap();
        assert!(store.get(&key(1)).is_none());
        assert!(!path.exists());

        fs::write(&path, b"not an entry at all, not even close").unwrap();
        assert!(store.get(&key(1)).is_none());
        assert!(!path.exists());
    }

    #[test]
    fn partial_frames_are_rejected() {
        let (_dir, store) = store();
        let mut w = store.writer(&key(1), 2, 48_000).unwrap();
        assert!(w.write(&[0.0; 3]).is_err());
    }

    #[test]
    fn eviction_drops_the_least_recently_used_first() {
        let (_dir, store) = store();
        for n in 1..=3 {
            put(&store, &key(n), &[0.0; 100]);
            // Distinct modification times.
            std::thread::sleep(Duration::from_millis(20));
        }
        let each = store.total_bytes().unwrap() / 3;
        // Using the oldest makes it the newest.
        assert!(store.get(&key(1)).is_some());
        let freed = store.evict_to(each * 2).unwrap();
        assert_eq!(freed, each);
        assert!(store.contains(&key(1)));
        assert!(!store.contains(&key(2)));
        assert!(store.contains(&key(3)));
        assert_eq!(store.evict_to(0).unwrap(), each * 2);
        assert_eq!(store.total_bytes().unwrap(), 0);
    }

    #[test]
    fn open_clears_old_partials_but_not_recent_ones() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("a.1.0.partial");
        let recent = dir.path().join("b.1.0.partial");
        fs::write(&old, b"x").unwrap();
        fs::write(&recent, b"x").unwrap();
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - 2 * STALE_PARTIAL)
            .unwrap();
        CacheStore::open(dir.path()).unwrap();
        assert!(!old.exists());
        assert!(recent.exists());
    }
}
