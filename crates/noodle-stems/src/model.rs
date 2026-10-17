//! The model weights and ONNX Runtime: where they live and how they are
//! downloaded.
//!
//! Neither is bundled. The app asks the user, then downloads them once into
//! the user data folder (ADR 0006). Both come from fixed addresses and are
//! checked against a SHA-256 before they are kept.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ModelId;

/// Where the HT-Demucs weights are downloaded from: Hugging Face
/// `adowu/htdemucs-onnx` (MIT, matching Demucs), pinned to one commit. A single
/// ONNX file with the STFT and its inverse inside the graph, one input `mix`
/// `[1, 2, 343980]` and one output `stems` `[1, 4, 2, 343980]` (drums, bass,
/// other, vocals): the signature `onnx.rs` runs.
const HTDEMUCS_URL: &str = "https://huggingface.co/adowu/htdemucs-onnx/resolve/\
    e6b688a9a9f977e2efaba79d0f18b9b0cfc606ea/htdemucs.onnx";
const HTDEMUCS_SHA256: &str = "68d0bf16428ef66e692cdff8a9ccf28f1ef3f69440d57e58605a4cc55fcc5e74";

/// Set to download the HT-Demucs ONNX weights from somewhere else.
pub const URL_VARIABLE: &str = "NOODLE_STEM_MODEL_URL";
/// Set to the SHA-256 of the file at that address (as hex), to check it. Without
/// an address of its own, the built-in address keeps its built-in checksum.
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
    /// HT-Demucs, from the built-in address. `NOODLE_STEM_MODEL_URL` (and
    /// `NOODLE_STEM_MODEL_SHA256` to check it) points at another export instead.
    pub fn htdemucs() -> Self {
        Self::htdemucs_with(
            std::env::var(URL_VARIABLE).ok(),
            std::env::var(SHA256_VARIABLE).ok(),
        )
    }

    fn htdemucs_with(url: Option<String>, sha256: Option<String>) -> Self {
        let url = url.filter(|u| !u.is_empty());
        let sha256 = sha256
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty());
        let (url, sha256) = match url {
            Some(url) => (url, sha256),
            None => (HTDEMUCS_URL.to_owned(), Some(HTDEMUCS_SHA256.to_owned())),
        };
        Self {
            id: "htdemucs",
            version: "1",
            file: "htdemucs.onnx",
            url: Some(url),
            sha256,
        }
    }

    pub fn model_id(&self) -> ModelId {
        ModelId {
            id: self.id.into(),
            version: self.version.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Archive {
    TarGz,
    Zip,
}

/// Where to get ONNX Runtime for this platform: Microsoft's own release
/// archive (MIT), the one library in it, and the archive's SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSpec {
    pub url: String,
    pub sha256: &'static str,
    archive: Archive,
    /// The library's path inside the archive.
    member: &'static str,
    /// The file name it is kept under in the store (what `OnnxSeparator` looks for).
    pub file: &'static str,
}

const RUNTIME_VERSION: &str = "1.22.0";

impl RuntimeSpec {
    /// The runtime for this platform, if Microsoft publishes one for it.
    pub fn current() -> Option<Self> {
        Self::for_target(std::env::consts::OS, std::env::consts::ARCH)
    }

    pub fn for_target(os: &str, arch: &str) -> Option<Self> {
        let (name, archive, member, file, sha256) = match (os, arch) {
            ("linux", "x86_64") => (
                "linux-x64",
                Archive::TarGz,
                "lib/libonnxruntime.so.1.22.0",
                "libonnxruntime.so",
                "8344d55f93d5bc5021ce342db50f62079daf39aaafb5d311a451846228be49b3",
            ),
            ("linux", "aarch64") => (
                "linux-aarch64",
                Archive::TarGz,
                "lib/libonnxruntime.so.1.22.0",
                "libonnxruntime.so",
                "bb76395092d150b52c7092dc6b8f2fe4d80f0f3bf0416d2f269193e347e24702",
            ),
            ("macos", "aarch64") => (
                "osx-arm64",
                Archive::TarGz,
                "lib/libonnxruntime.1.22.0.dylib",
                "libonnxruntime.dylib",
                "cab6dcbd77e7ec775390e7b73a8939d45fec3379b017c7cb74f5b204c1a1cc07",
            ),
            ("windows", "x86_64") => (
                "win-x64",
                Archive::Zip,
                "lib/onnxruntime.dll",
                "onnxruntime.dll",
                "174c616efc0271194488642a72f1a514e01487da4dfe84c49296d66e40ebe0da",
            ),
            _ => return None,
        };
        let v = RUNTIME_VERSION;
        let ext = if archive == Archive::Zip {
            "zip"
        } else {
            "tgz"
        };
        Some(Self {
            url: format!(
                "https://github.com/microsoft/onnxruntime/releases/download/v{v}/onnxruntime-{name}-{v}.{ext}"
            ),
            sha256,
            archive,
            member,
            file,
        })
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

    /// Where the runtime library is kept: in a folder named for its version,
    /// so a newer pinned version is downloaded rather than shadowed by an
    /// older one that is already here.
    pub fn runtime_path(&self, spec: &RuntimeSpec) -> PathBuf {
        self.dir
            .join(format!("onnxruntime-{RUNTIME_VERSION}"))
            .join(spec.file)
    }

    /// Whether the runtime library is here already.
    pub fn has_runtime(&self, spec: &RuntimeSpec) -> bool {
        self.runtime_path(spec).is_file()
    }

    /// Downloads ONNX Runtime's archive, checks it, and keeps the library from
    /// it under `spec.file`. Progress and cancelling are as for
    /// [`download`](Self::download).
    pub fn download_runtime(
        &self,
        spec: &RuntimeSpec,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<PathBuf, DownloadError> {
        let target = self.runtime_path(spec);
        let folder = target.parent().unwrap_or(&self.dir);
        std::fs::create_dir_all(folder)?;
        let archive = folder.join("onnxruntime-archive.partial");
        let partial = folder.join(format!("{}.partial", spec.file));
        let result = fetch(&spec.url, &archive, Some(spec.sha256), progress)
            .and_then(|()| extract(spec, &archive, &partial));
        let _ = std::fs::remove_file(&archive);
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

/// Copies the library out of the archive to `out`.
fn extract(spec: &RuntimeSpec, archive: &Path, out: &Path) -> Result<(), DownloadError> {
    let missing = || DownloadError::Network(format!("{} is not in the archive", spec.member));
    let bad = |e: &dyn fmt::Display| DownloadError::Network(format!("bad archive: {e}"));
    let file = std::fs::File::open(archive)?;
    // The member sits in a folder named after the release, so match its tail.
    let is_member = |path: &Path| path.ends_with(spec.member);
    let mut out_file = std::fs::File::create(out)?;
    match spec.archive {
        Archive::TarGz => {
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
            for entry in tar.entries().map_err(|e| bad(&e))? {
                let mut entry = entry.map_err(|e| bad(&e))?;
                let path = entry.path().map_err(|e| bad(&e))?.into_owned();
                if entry.header().entry_type().is_file() && is_member(&path) {
                    std::io::copy(&mut entry, &mut out_file)?;
                    return Ok(());
                }
            }
            Err(missing())
        }
        Archive::Zip => {
            let mut zip = zip::ZipArchive::new(file).map_err(|e| bad(&e))?;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i).map_err(|e| bad(&e))?;
                if entry.is_file() && entry.name().is_ok_and(|n| is_member(Path::new(n.as_ref()))) {
                    std::io::copy(&mut entry, &mut out_file)?;
                    return Ok(());
                }
            }
            Err(missing())
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

    fn runtime_spec(
        archive: Archive,
        member: &'static str,
        url: String,
        bytes: &[u8],
    ) -> RuntimeSpec {
        RuntimeSpec {
            url,
            sha256: Box::leak(sha256_of(bytes).into_boxed_str()),
            archive,
            member,
            file: "libtest.so",
        }
    }

    #[test]
    fn the_runtime_is_taken_out_of_a_tar_archive() {
        let library = vec![9u8; 5000];
        let mut tar_bytes = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_bytes);
            for (path, data) in [
                ("rel/lib/other.so", &[1u8; 10][..]),
                ("rel/lib/real.so.1", &library[..]),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                tar.append_data(&mut header, path, data).unwrap();
            }
            tar.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            enc.write_all(&tar_bytes).unwrap();
            enc.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let spec = runtime_spec(
            Archive::TarGz,
            "lib/real.so.1",
            serve(gz.clone(), gz.len()),
            &gz,
        );
        assert!(!store.has_runtime(&spec));
        let path = store.download_runtime(&spec, &mut |_| true).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), library);
        assert!(store.has_runtime(&spec));
        let folder = store.runtime_path(&spec);
        let left: Vec<_> = std::fs::read_dir(folder.parent().unwrap())
            .unwrap()
            .collect();
        assert_eq!(left.len(), 1, "only the library is kept: {left:?}");
    }

    #[test]
    fn the_runtime_is_taken_out_of_a_zip_archive() {
        let library = vec![7u8; 3000];
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("rel/lib/real.dll", options).unwrap();
            zip.write_all(&library).unwrap();
            zip.finish().unwrap();
        }
        let bytes = buf.into_inner();
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let spec = runtime_spec(
            Archive::Zip,
            "lib/real.dll",
            serve(bytes.clone(), bytes.len()),
            &bytes,
        );
        let path = store.download_runtime(&spec, &mut |_| true).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), library);
    }

    #[test]
    fn a_runtime_archive_without_the_library_or_with_a_wrong_checksum_leaves_nothing() {
        let bytes = vec![3u8; 2000];
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let mut spec = runtime_spec(
            Archive::Zip,
            "lib/x.dll",
            serve(bytes.clone(), bytes.len()),
            &bytes,
        );
        // Right checksum, but not an archive.
        assert!(store.download_runtime(&spec, &mut |_| true).is_err());
        spec.url = serve(bytes.clone(), bytes.len());
        spec.sha256 = "00".repeat(32).leak();
        let error = store.download_runtime(&spec, &mut |_| true).unwrap_err();
        assert!(matches!(error, DownloadError::WrongChecksum), "{error}");
        assert!(!store.has_runtime(&spec));
        let folder = store.runtime_path(&spec);
        assert_eq!(
            std::fs::read_dir(folder.parent().unwrap()).unwrap().count(),
            0
        );
    }

    #[test]
    fn every_supported_platform_has_a_pinned_runtime() {
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "aarch64"),
            ("windows", "x86_64"),
        ] {
            let spec = RuntimeSpec::for_target(os, arch).unwrap();
            assert!(
                spec.url
                    .starts_with("https://github.com/microsoft/onnxruntime/")
            );
            assert_eq!(spec.sha256.len(), 64);
        }
        assert!(RuntimeSpec::for_target("macos", "x86_64").is_none());
    }

    #[test]
    fn the_built_in_address_is_pinned_and_checked_unless_another_is_given() {
        let built_in = ModelSpec::htdemucs_with(None, None);
        assert!(
            built_in
                .url
                .as_deref()
                .unwrap()
                .contains("/resolve/e6b688a9")
        );
        assert_eq!(built_in.sha256.as_deref(), Some(HTDEMUCS_SHA256));
        // Another address does not inherit the checksum of the built-in file.
        let other = ModelSpec::htdemucs_with(Some("http://x/y.onnx".into()), None);
        assert_eq!(other.url.as_deref(), Some("http://x/y.onnx"));
        assert_eq!(other.sha256, None);
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
