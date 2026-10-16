//! The model weights: where they live and how they are downloaded.
//!
//! The weights are not bundled. The app asks the user, then downloads them
//! once into the user data folder (ADR 0006).

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ModelId;

/// Set to where the HT-Demucs ONNX weights can be downloaded from.
pub const URL_VARIABLE: &str = "NOODLE_STEM_MODEL_URL";
/// Set to the SHA-256 of the file at that address (as hex), to check it.
pub const SHA256_VARIABLE: &str = "NOODLE_STEM_MODEL_SHA256";

/// One model's weights: what it is called, the file it is kept in and where
/// to get it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: &'static str,
    pub version: &'static str,
    /// The file name in the store.
    pub file: &'static str,
    /// Where to download it from. `None` when no address is configured.
    pub url: Option<String>,
    /// The expected SHA-256, as lowercase hex.
    pub sha256: Option<String>,
}

impl ModelSpec {
    /// HT-Demucs. The address comes from the environment (`NOODLE_STEM_MODEL_URL`,
    /// and `NOODLE_STEM_MODEL_SHA256` to check it) until the licence of the
    /// weights has been settled and an address can be built in.
    pub fn htdemucs() -> Self {
        Self {
            id: "htdemucs",
            version: "1",
            file: "htdemucs.onnx",
            url: std::env::var(URL_VARIABLE).ok().filter(|u| !u.is_empty()),
            sha256: std::env::var(SHA256_VARIABLE)
                .ok()
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty()),
        }
    }

    pub fn model_id(&self) -> ModelId {
        ModelId {
            id: self.id.into(),
            version: self.version.into(),
        }
    }
}

/// Why the weights could not be downloaded.
#[derive(Debug)]
pub enum DownloadError {
    /// No address is configured.
    NoAddress,
    Cancelled,
    Network(String),
    Disk(std::io::Error),
    /// The file arrived but is not the one expected.
    WrongChecksum,
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::NoAddress => write!(
                f,
                "no download address is set for the stem model (set {URL_VARIABLE})"
            ),
            DownloadError::Cancelled => write!(f, "cancelled"),
            DownloadError::Network(why) => write!(f, "the download failed: {why}"),
            DownloadError::Disk(why) => write!(f, "couldn't save the model: {why}"),
            DownloadError::WrongChecksum => {
                write!(f, "the downloaded model is not the file that was expected")
            }
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<std::io::Error> for DownloadError {
    fn from(error: std::io::Error) -> Self {
        DownloadError::Disk(error)
    }
}

/// A folder of model files.
#[derive(Clone, Debug)]
pub struct ModelStore {
    dir: PathBuf,
}

impl ModelStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, spec: &ModelSpec) -> PathBuf {
        self.dir.join(spec.file)
    }

    /// Whether the weights are here already.
    pub fn is_present(&self, spec: &ModelSpec) -> bool {
        self.path(spec).is_file()
    }

    /// Downloads the weights and returns where they are. `progress` is told how
    /// far along it is (0 to 1; it stays 0 when the server doesn't say how big
    /// the file is) and answers whether to go on. The file appears under its
    /// name only once it has arrived whole and, if a checksum is known,
    /// matches it.
    pub fn download(
        &self,
        spec: &ModelSpec,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<PathBuf, DownloadError> {
        let url = spec.url.as_deref().ok_or(DownloadError::NoAddress)?;
        std::fs::create_dir_all(&self.dir)?;
        let target = self.path(spec);
        let partial = target.with_extension("partial");
        let result = fetch(url, &partial, spec.sha256.as_deref(), progress);
        match result {
            Ok(()) => {
                std::fs::rename(&partial, &target)?;
                Ok(target)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&partial);
                Err(error)
            }
        }
    }
}

fn fetch(
    url: &str,
    partial: &Path,
    sha256: Option<&str>,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<(), DownloadError> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| DownloadError::Network(e.to_string()))?;
    let total = response.body().content_length();
    let mut reader = response.into_body().into_reader();
    let mut file = std::fs::File::create(partial)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| DownloadError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
        done += n as u64;
        let fraction = total.map_or(0.0, |t| (done as f32 / t.max(1) as f32).min(1.0));
        if !progress(fraction) {
            return Err(DownloadError::Cancelled);
        }
    }
    file.flush()?;
    if total.is_some_and(|t| t != done) {
        return Err(DownloadError::Network("the file was cut short".into()));
    }
    if let Some(want) = sha256 {
        let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != want {
            return Err(DownloadError::WrongChecksum);
        }
    }
    progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// Serves `body` once at the address it returns, saying it is `claimed`
    /// bytes long.
    fn serve(body: Vec<u8>, claimed: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {claimed}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
        format!("http://{address}/model.onnx")
    }

    fn spec(url: Option<String>, sha256: Option<String>) -> ModelSpec {
        ModelSpec {
            id: "test",
            version: "1",
            file: "test.onnx",
            url,
            sha256,
        }
    }

    fn sha256_of(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn a_download_lands_under_its_name_with_progress() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().join("models"));
        let body: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let spec = spec(
            Some(serve(body.clone(), body.len())),
            Some(sha256_of(&body)),
        );
        assert!(!store.is_present(&spec));
        let mut seen = Vec::new();
        let path = store
            .download(&spec, &mut |f| {
                seen.push(f);
                true
            })
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(store.is_present(&spec));
        assert_eq!(seen.last(), Some(&1.0));
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
        assert!(!path.with_extension("partial").exists());
    }

    #[test]
    fn a_wrong_checksum_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = vec![7u8; 1000];
        let spec = spec(Some(serve(body.clone(), body.len())), Some("00".repeat(32)));
        let error = store.download(&spec, &mut |_| true).unwrap_err();
        assert!(matches!(error, DownloadError::WrongChecksum), "{error}");
        assert!(!store.is_present(&spec));
        assert!(!store.path(&spec).with_extension("partial").exists());
    }

    #[test]
    fn a_cancelled_download_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = vec![1u8; 500_000];
        let spec = spec(Some(serve(body.clone(), body.len())), None);
        let error = store.download(&spec, &mut |_| false).unwrap_err();
        assert!(matches!(error, DownloadError::Cancelled), "{error}");
        assert!(!store.is_present(&spec));
        assert!(!store.path(&spec).with_extension("partial").exists());
    }

    #[test]
    fn a_file_cut_short_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let spec = spec(Some(serve(vec![1u8; 100], 5_000)), None);
        assert!(store.download(&spec, &mut |_| true).is_err());
        assert!(!store.is_present(&spec));
    }

    #[test]
    fn without_an_address_there_is_nothing_to_download() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let error = store
            .download(&spec(None, None), &mut |_| true)
            .unwrap_err();
        assert!(matches!(error, DownloadError::NoAddress));
    }
}
