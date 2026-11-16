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

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use noodle_core::CacheKey;

const MAGIC: [u8; 4] = *b"NRC1";
const HEADER_BYTES: u64 = 32;
const EXTENSION: &str = "nrc";
/// A `.partial` file this old can't belong to a render still running.
const STALE_PARTIAL: Duration = Duration::from_secs(60 * 60);
static PARTIALS: AtomicU64 = AtomicU64::new(0);
/// Frames between the points where a writer makes what it has written
/// readable (see [`Growth`]): about 85 ms at 48 kHz.
const PUBLISH_FRAMES: u64 = 4096;

/// How far an entry that is still being written has got, shared between the
/// writer and whoever reads it as it grows. Only a writer in this process is
/// seen: a render is started by the app that plays it.
#[derive(Debug)]
pub struct Growth {
    path: PathBuf,
    channels: usize,
    sample_rate: u32,
    /// Frames on disk, as of the writer's last publish.
    frames: AtomicU64,
    /// The entry was committed: `frames` is its length and it won't change.
    finished: AtomicBool,
    /// The writer was dropped without committing: nothing more will come.
    abandoned: AtomicBool,
}

impl Growth {
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frames that can be read now.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Acquire)
    }

    /// Whether the entry is complete.
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Whether the render stopped without finishing the entry.
    pub fn abandoned(&self) -> bool {
        self.abandoned.load(Ordering::Acquire)
    }
}

type Growing = Arc<Mutex<HashMap<CacheKey, Arc<Growth>>>>;

/// What a [`CacheStore`] holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUsage {
    /// Finished entries.
    pub entries: usize,
    pub bytes: u64,
}

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
    /// The entries being written now, by key.
    growing: Growing,
}

impl CacheStore {
    /// Opens (creating if need be) the store in `dir`, and clears out
    /// `.partial` files that an earlier crash left behind.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let store = Self {
            dir,
            growing: Growing::default(),
        };
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
        self.open_entry(key, false).is_some()
    }

    /// Opens the entry for `key`. A damaged entry (truncated, or with a bad
    /// header) is deleted and reported as missing, so the caller just
    /// renders it again. Marks the entry as recently used.
    pub fn get(&self, key: &CacheKey) -> Option<CachedAudio> {
        self.open_entry(key, true)
    }

    /// The entry for `key` as far as it has been written, if a render in
    /// this process is writing it now. It reads like [`get`](Self::get), but
    /// [`CachedAudio::available`] grows as the render goes on, and the entry
    /// is whole once [`Growth::finished`].
    pub fn get_growing(&self, key: &CacheKey) -> Option<CachedAudio> {
        let growth = self.growth(key)?;
        let file = File::open(&growth.path).ok()?;
        Some(CachedAudio {
            file,
            info: CacheInfo {
                channels: growth.channels,
                sample_rate: growth.sample_rate,
                frames: growth.frames(),
            },
            growth: Some(growth),
        })
    }

    /// How far the render writing `key` has got, if one is.
    pub fn growth(&self, key: &CacheKey) -> Option<Arc<Growth>> {
        self.growing.lock().expect("growing lock").get(key).cloned()
    }

    fn open_entry(&self, key: &CacheKey, touch: bool) -> Option<CachedAudio> {
        let path = self.path(key);
        match CachedAudio::open(&path) {
            Ok(audio) => {
                if touch {
                    // Best effort: only used to pick what to evict.
                    if let Ok(file) = File::options().write(true).open(&path) {
                        let _ = file.set_modified(SystemTime::now());
                    }
                }
                Some(audio)
            }
            // Only an entry that is really damaged is deleted. Any other
            // error (permissions, too many open files) says nothing about
            // the entry, so it is just a miss.
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                let _ = fs::remove_file(&path);
                None
            }
            Err(_) => None,
        }
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
        // The header is on disk before anyone can look for the entry.
        file.flush()?;
        let growth = Arc::new(Growth {
            path: partial.clone(),
            channels,
            sample_rate,
            frames: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
        });
        self.growing
            .lock()
            .expect("growing lock")
            .insert(*key, Arc::clone(&growth));
        Ok(CacheWriter {
            file: Some(file),
            committed: false,
            partial,
            target: self.path(key),
            info,
            scratch: Vec::new(),
            key: *key,
            growing: Arc::clone(&self.growing),
            growth,
            published: 0,
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

    /// How much the store holds: finished entries and their bytes.
    pub fn usage(&self) -> io::Result<CacheUsage> {
        let files = self.files()?;
        let finished = files.iter().filter(|f| !f.partial);
        Ok(CacheUsage {
            entries: finished.clone().count(),
            bytes: finished.map(|f| f.bytes).sum(),
        })
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

/// Writes one entry. Dropping it without a successful
/// [`commit`](Self::commit) deletes the partial file.
pub struct CacheWriter {
    file: Option<BufWriter<File>>,
    committed: bool,
    partial: PathBuf,
    target: PathBuf,
    info: CacheInfo,
    scratch: Vec<u8>,
    key: CacheKey,
    growing: Growing,
    growth: Arc<Growth>,
    /// Frames made readable so far.
    published: u64,
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
        if self.info.frames - self.published >= PUBLISH_FRAMES {
            self.publish()?;
        }
        Ok(())
    }

    /// Makes what has been written readable through
    /// [`CacheStore::get_growing`]. [`write`](Self::write) does it every few
    /// thousand frames by itself.
    pub fn publish(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .expect("writer used after commit")
            .flush()?;
        self.published = self.info.frames;
        self.growth.frames.store(self.published, Ordering::Release);
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
        self.committed = true;
        // Readers hold the file open, which the rename leaves alone.
        self.growth
            .frames
            .store(self.info.frames, Ordering::Release);
        self.growth.finished.store(true, Ordering::Release);
        Ok(self.info)
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        {
            let mut growing = self.growing.lock().expect("growing lock");
            // A render of the same key may have replaced this one.
            if growing
                .get(&self.key)
                .is_some_and(|g| Arc::ptr_eq(g, &self.growth))
            {
                growing.remove(&self.key);
            }
        }
        if !self.committed {
            self.growth.abandoned.store(true, Ordering::Release);
        }
        // Also after a commit that failed part way, e.g. on a full disk.
        if !self.committed {
            self.file = None;
            let _ = fs::remove_file(&self.partial);
        }
    }
}

/// A finished entry, readable at any position.
#[derive(Debug)]
pub struct CachedAudio {
    file: File,
    info: CacheInfo,
    /// Set for an entry opened while it is being written.
    growth: Option<Arc<Growth>>,
}

impl CachedAudio {
    fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
        let mut header = [0; HEADER_BYTES as usize];
        file.read_exact(&mut header).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                bad("truncated header")
            } else {
                e
            }
        })?;
        let info = CacheInfo::parse(&header).ok_or_else(|| bad("bad header"))?;
        let expected = info.data_bytes().and_then(|d| d.checked_add(HEADER_BYTES));
        if expected != Some(file.metadata()?.len()) {
            return Err(bad("wrong length"));
        }
        Ok(Self {
            file,
            info,
            growth: None,
        })
    }

    pub fn info(&self) -> CacheInfo {
        self.info
    }

    /// Frames that can be read now: the whole entry, or for one opened while
    /// it was being written (see [`CacheStore::get_growing`]) as many as the
    /// render has published.
    pub fn available(&self) -> u64 {
        self.growth
            .as_ref()
            .map_or(self.info.frames, |growth| growth.frames())
    }

    /// Whether more may still arrive: the entry is being written and its
    /// render is still going.
    pub fn growing(&self) -> bool {
        self.growth
            .as_ref()
            .is_some_and(|growth| !growth.finished() && !growth.abandoned())
    }

    /// Reads interleaved frames starting at frame `start` into `out` (a
    /// whole number of frames). Returns how many frames were read, fewer than
    /// `out` holds only at the end of the entry.
    pub fn read_frames(&mut self, start: u64, out: &mut [f32]) -> io::Result<usize> {
        let channels = self.info.channels;
        let wanted = (out.len() / channels) as u64;
        let frames = wanted.min(self.available().saturating_sub(start)) as usize;
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
    fn contains_does_not_count_as_a_use() {
        let (dir, store) = store();
        put(&store, &key(1), &[0.0; 4]);
        let path = dir.path().join(format!("{}.nrc", key(1).to_hex()));
        let old = SystemTime::now() - Duration::from_secs(1000);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(store.contains(&key(1)));
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), old);
        assert!(store.get(&key(1)).is_some());
        assert!(fs::metadata(&path).unwrap().modified().unwrap() > old);
    }

    #[test]
    fn an_unreadable_entry_is_kept() {
        // A directory where the entry should be: opening it is an I/O error
        // that isn't damage, so it must not be deleted.
        let (dir, store) = store();
        let path = dir.path().join(format!("{}.nrc", key(1).to_hex()));
        fs::create_dir(&path).unwrap();
        assert!(store.get(&key(1)).is_none());
        assert!(path.exists());
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
    fn an_entry_can_be_read_while_it_is_written_and_stays_readable_after_the_commit() {
        let (_dir, store) = store();
        let mut w = store.writer(&key(1), 1, 48_000).unwrap();
        // Nothing is published yet, and the entry is not whole.
        let mut audio = store.get_growing(&key(1)).unwrap();
        assert_eq!(audio.available(), 0);
        assert!(audio.growing());
        assert!(!store.contains(&key(1)));

        let samples: Vec<f32> = (0..10_000).map(|i| i as f32).collect();
        w.write(&samples[..3000]).unwrap();
        // Less than a publish step: written, not yet readable.
        assert_eq!(audio.available(), 0);
        w.write(&samples[3000..6000]).unwrap();
        assert_eq!(audio.available(), 6000);
        let mut out = [0.0; 4];
        assert_eq!(audio.read_frames(5998, &mut out).unwrap(), 2);
        assert_eq!(out[..2], [5998.0, 5999.0]);
        // Past what is published is a short read, never a stale value.
        assert_eq!(audio.read_frames(6000, &mut out).unwrap(), 0);

        w.write(&samples[6000..]).unwrap();
        w.publish().unwrap();
        assert_eq!(audio.available(), 10_000);
        w.commit().unwrap();
        // The reader opened before the rename still reads, and knows it is whole.
        assert!(!audio.growing());
        assert_eq!(audio.available(), 10_000);
        assert_eq!(audio.read_frames(9998, &mut out).unwrap(), 2);
        assert_eq!(out[..2], [9998.0, 9999.0]);
        // Once committed there is nothing growing to find.
        assert!(store.get_growing(&key(1)).is_none());
        assert!(store.contains(&key(1)));
    }

    #[test]
    fn a_dropped_writer_is_seen_as_abandoned() {
        let (_dir, store) = store();
        let mut w = store.writer(&key(1), 1, 48_000).unwrap();
        w.write(&[0.0; 8192]).unwrap();
        let growth = store.growth(&key(1)).unwrap();
        assert!(!growth.abandoned());
        drop(w);
        assert!(growth.abandoned() && !growth.finished());
        assert!(store.growth(&key(1)).is_none());
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
