//! Where the downloaded weights are kept, and downloading them.
//!
//! Files are named by their SHA-256 in `noodle-download`'s cache, so the same
//! weights are fetched once. A model counts as downloaded when every file of
//! it is in the cache; removing it deletes those files.

use std::fmt;
use std::path::{Path, PathBuf};

use noodle_download::{Cache, Consent, Downloader, Request, Transport};

use crate::drum::{DrumModelManifest, ModelFile};

#[derive(Debug)]
pub enum StoreError {
    /// The manifest has no address or checksum for this file yet, so nothing
    /// can be downloaded (or checked) for it.
    NotPublished(String),
    Download(noodle_download::Error),
    Disk(std::io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotPublished(file) => write!(
                f,
                "{file} has no download address or checksum yet, so it can't be downloaded"
            ),
            StoreError::Download(e) => write!(f, "{e}"),
            StoreError::Disk(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<noodle_download::Error> for StoreError {
    fn from(e: noodle_download::Error) -> Self {
        StoreError::Download(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Disk(e)
    }
}

#[derive(Clone, Debug)]
pub struct DrumModelStore {
    cache: Cache,
}

impl DrumModelStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            cache: Cache::new(dir),
        }
    }

    /// The folder the weights are kept in, for the settings tab to show.
    pub fn dir(&self) -> &Path {
        self.cache.dir()
    }

    /// Where a file is (or will be) kept. `None` when it has no checksum yet.
    pub fn path(&self, file: &ModelFile) -> Option<PathBuf> {
        file.sha256.map(|h| self.cache.path(h))
    }

    /// The files of `model` in the order its runtime opens them, if all are here.
    pub fn files(&self, model: &DrumModelManifest) -> Option<Vec<PathBuf>> {
        model
            .files
            .iter()
            .map(|f| self.path(f).filter(|p| p.is_file()))
            .collect()
    }

    pub fn is_downloaded(&self, model: &DrumModelManifest) -> bool {
        self.files(model).is_some()
    }

    /// Bytes on disk for `model`.
    pub fn size_on_disk(&self, model: &DrumModelManifest) -> u64 {
        model
            .files
            .iter()
            .filter_map(|f| self.path(f))
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum()
    }

    /// Downloads whatever of `model` is missing. `consent` is asked once per
    /// file fetched (the app shows the licence there); `progress` is 0 to 1
    /// across the whole model and answers whether to go on.
    pub fn download<T: Transport>(
        &self,
        model: &DrumModelManifest,
        downloader: &Downloader<T>,
        consent: Option<Consent<'_>>,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<(), StoreError> {
        let count = model.files.len().max(1) as f32;
        for (i, file) in model.files.iter().enumerate() {
            let (Some(url), Some(sha256)) = (file.url, file.sha256) else {
                return Err(StoreError::NotPublished(file.name.to_owned()));
            };
            let request = Request::new(url).with_sha256(sha256);
            let base = i as f32;
            self.cache
                .get(downloader, sha256, &request, consent, &mut |f| {
                    progress((base + f) / count)
                })?;
        }
        progress(1.0);
        Ok(())
    }

    /// [`download`](Self::download) over the real network, for the app. The
    /// consent was the user's own choice in the Models tab or the first-use
    /// popup, which show the licence, so nothing is asked again.
    pub fn fetch(
        &self,
        model: &DrumModelManifest,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<(), StoreError> {
        self.download(model, &Downloader::default(), None, progress)
    }

    /// Deletes the weights of `model`. Nothing else shares them, since the
    /// names are checksums of this model's own files.
    pub fn remove(&self, model: &DrumModelManifest) -> Result<(), StoreError> {
        for path in model.files.iter().filter_map(|f| self.path(f)) {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Cursor;

    use noodle_download::{Error, Response};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::drum::{Chunking, DrumPart, Layout, MaskOutput};

    struct Fake {
        body: Vec<u8>,
        opened: Cell<u32>,
    }

    impl Transport for &Fake {
        fn open(&self, _: &Request) -> Result<Response, Error> {
            self.opened.set(self.opened.get() + 1);
            Ok(Response {
                length: Some(self.body.len() as u64),
                body: Box::new(Cursor::new(self.body.clone())),
            })
        }
    }

    fn hex(bytes: &[u8]) -> &'static str {
        let s: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Box::leak(s.into_boxed_str())
    }

    fn model(sha: Option<&'static str>) -> &'static DrumModelManifest {
        let file = ModelFile {
            part: Some(DrumPart::Kick),
            name: "kick.onnx",
            url: Some("http://example/kick.onnx"),
            sha256: sha,
            size: Some(10),
        };
        Box::leak(Box::new(DrumModelManifest {
            id: "t",
            name: "T",
            version: "1",
            parts: &[DrumPart::Kick],
            lacks: &[],
            licence: "MIT",
            non_commercial: false,
            credit: "x",
            source_url: "x",
            sample_rate: 44_100,
            chunking: Chunking {
                segment_samples: 1024,
                stft_size: 64,
                hop: 16,
            },
            layout: Layout::PerPartSpectrogram(MaskOutput::Mask),
            files: Box::leak(Box::new([file])),
            provenance: "",
        }))
    }

    #[test]
    fn a_download_is_kept_checked_and_removable() {
        let dir = tempfile::tempdir().unwrap();
        let store = DrumModelStore::new(dir.path());
        let fake = Fake {
            body: vec![7u8; 10_000],
            opened: Cell::new(0),
        };
        let m = model(Some(hex(&fake.body)));
        assert!(!store.is_downloaded(m));
        let asked = Cell::new(0);
        let consent = |_: &Request| {
            asked.set(asked.get() + 1);
            true
        };
        let mut last = 0.0;
        store
            .download(m, &Downloader::new(&fake), Some(&consent), &mut |f| {
                last = f;
                true
            })
            .unwrap();
        assert_eq!(last, 1.0);
        assert!(store.is_downloaded(m));
        assert_eq!(store.size_on_disk(m), 10_000);
        assert_eq!(asked.get(), 1);
        // Again: a hit asks nothing and touches no network.
        store
            .download(m, &Downloader::new(&fake), Some(&consent), &mut |_| true)
            .unwrap();
        assert_eq!((asked.get(), fake.opened.get()), (1, 1));
        store.remove(m).unwrap();
        assert!(!store.is_downloaded(m));
        store.remove(m).unwrap();
    }

    #[test]
    fn a_wrong_file_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = DrumModelStore::new(dir.path());
        let fake = Fake {
            body: vec![1u8; 100],
            opened: Cell::new(0),
        };
        let m = model(Some(hex(b"something else")));
        let result = store.download(m, &Downloader::new(&fake), None, &mut |_| true);
        assert!(matches!(
            result,
            Err(StoreError::Download(Error::WrongChecksum))
        ));
        assert!(!store.is_downloaded(m));
    }

    #[test]
    fn declining_or_cancelling_downloads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = DrumModelStore::new(dir.path());
        let fake = Fake {
            body: vec![1u8; 100],
            opened: Cell::new(0),
        };
        let m = model(Some(hex(&fake.body)));
        let no = |_: &Request| false;
        let declined = store.download(m, &Downloader::new(&fake), Some(&no), &mut |_| true);
        assert!(matches!(
            declined,
            Err(StoreError::Download(Error::Declined))
        ));
        let cancelled = store.download(m, &Downloader::new(&fake), None, &mut |_| false);
        assert!(matches!(
            cancelled,
            Err(StoreError::Download(Error::Cancelled))
        ));
        assert!(!store.is_downloaded(m));
        assert_eq!(fake.opened.get(), 1);
    }

    #[test]
    fn a_model_without_a_checksum_cannot_be_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let store = DrumModelStore::new(dir.path());
        let fake = Fake {
            body: vec![],
            opened: Cell::new(0),
        };
        let m = model(None);
        let result = store.download(m, &Downloader::new(&fake), None, &mut |_| true);
        assert!(matches!(result, Err(StoreError::NotPublished(_))));
        assert_eq!(fake.opened.get(), 0);
        assert!(!store.is_downloaded(m));
        assert!(!m.is_published());
    }
}
