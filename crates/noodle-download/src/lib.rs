//! The shared downloader (ADR 0006, ADR 0012): the one place Noodle fetches
//! files from the network.
//!
//! A download asks consent first (when a hook is set), streams to a `.partial`
//! file with progress and cancel, checks the SHA-256 if one is known, and only
//! then renames into place. A failed or cancelled download leaves nothing
//! behind: failure is clean, not resumable.
//!
//! Nothing reachable from `Processor::process` may use this, and the CLI and
//! offline renders never call it.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Credentials sent with a request, for self-hosted sources.
#[derive(Clone, PartialEq, Eq)]
pub enum Auth {
    Bearer(String),
    Basic { user: String, password: String },
}

impl Auth {
    /// The `Authorization` header value.
    pub fn header(&self) -> String {
        match self {
            Auth::Bearer(token) => format!("Bearer {token}"),
            Auth::Basic { user, password } => {
                format!("Basic {}", base64(format!("{user}:{password}").as_bytes()))
            }
        }
    }
}

// Secrets stay out of logs and error output.
impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Auth::Bearer(_) => write!(f, "Auth::Bearer(..)"),
            Auth::Basic { user, .. } => write!(f, "Auth::Basic({user}, ..)"),
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// One thing to fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    /// The expected SHA-256, as hex. Without it the file is kept unchecked.
    pub sha256: Option<String>,
    pub auth: Option<Auth>,
}

impl Request {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            sha256: None,
            auth: None,
        }
    }

    pub fn with_sha256(mut self, sha256: impl AsRef<str>) -> Self {
        self.sha256 = Some(sha256.as_ref().trim().to_ascii_lowercase());
        self
    }

    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }
}

/// Why a download failed.
#[derive(Debug)]
pub enum Error {
    /// The consent hook said no. Nothing was fetched.
    Declined,
    Cancelled,
    Network(String),
    Disk(std::io::Error),
    /// The file arrived but is not the one expected.
    WrongChecksum,
    /// A cache lookup needs a key.
    NoChecksum,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Declined => write!(f, "the download was not allowed"),
            Error::Cancelled => write!(f, "cancelled"),
            Error::Network(why) => write!(f, "the download failed: {why}"),
            Error::Disk(why) => write!(f, "couldn't save the download: {why}"),
            Error::WrongChecksum => write!(f, "the downloaded file is not the one expected"),
            Error::NoChecksum => write!(f, "a cached download needs a key"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Disk(error)
    }
}

/// An open response: how long it says it is, and its bytes.
pub struct Response {
    pub length: Option<u64>,
    pub body: Box<dyn Read + Send>,
}

/// Where the bytes come from. Tests use a fake; the app uses [`Http`].
pub trait Transport {
    fn open(&self, request: &Request) -> Result<Response, Error>;
}

/// HTTP(S) through `ureq`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Http;

impl Transport for Http {
    fn open(&self, request: &Request) -> Result<Response, Error> {
        let mut get = ureq::get(&request.url);
        if let Some(auth) = &request.auth {
            get = get.header("Authorization", auth.header());
        }
        let response = get.call().map_err(|e| Error::Network(e.to_string()))?;
        let length = response.body().content_length();
        Ok(Response {
            length,
            body: Box::new(response.into_body().into_reader()),
        })
    }
}

/// Asked before anything is fetched; `false` declines.
pub type Consent<'a> = &'a dyn Fn(&Request) -> bool;

/// Told how far along a download is (0 to 1; it stays 0 when the size is
/// unknown); answers whether to go on.
pub type Progress<'a> = &'a mut dyn FnMut(f32) -> bool;

/// A check run on the finished `.partial` file, after the byte checksum. An
/// error discards the download. Pack samples use it to decode the file and
/// hash the audio (their names are the hash of decoded audio, not of bytes).
pub type Verify = Box<dyn Fn(&Path) -> Result<(), Error> + Send>;

/// Downloads over a transport.
pub struct Downloader<T = Http> {
    transport: T,
    verify: Option<Verify>,
}

impl Default for Downloader<Http> {
    fn default() -> Self {
        Self::new(Http)
    }
}

impl<T: Transport> Downloader<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            verify: None,
        }
    }

    /// Whether a verify hook is set.
    pub fn has_verify(&self) -> bool {
        self.verify.is_some()
    }

    /// Adds a check on the whole file, run after the byte checksum (if any).
    pub fn with_verify(
        mut self,
        verify: impl Fn(&Path) -> Result<(), Error> + Send + 'static,
    ) -> Self {
        self.verify = Some(Box::new(verify));
        self
    }

    /// Fetches `request` to `dest`. The file appears under its name only once
    /// it has arrived whole and matches the checksum, if there is one.
    /// `consent`, when given, is asked first.
    pub fn fetch(
        &self,
        request: &Request,
        dest: &Path,
        consent: Option<Consent<'_>>,
        progress: Progress<'_>,
    ) -> Result<(), Error> {
        if consent.is_some_and(|ask| !ask(request)) {
            return Err(Error::Declined);
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut name = dest.as_os_str().to_owned();
        name.push(".partial");
        let partial = PathBuf::from(name);
        let streamed = self.stream(request, &partial, progress).and_then(|()| {
            self.verify
                .as_ref()
                .map_or(Ok(()), |verify| verify(&partial))
        });
        match streamed {
            Ok(()) => std::fs::rename(&partial, dest).map_err(|e| {
                let _ = std::fs::remove_file(&partial);
                Error::Disk(e)
            }),
            Err(error) => {
                let _ = std::fs::remove_file(&partial);
                Err(error)
            }
        }
    }

    fn stream(
        &self,
        request: &Request,
        partial: &Path,
        progress: Progress<'_>,
    ) -> Result<(), Error> {
        let mut response = self.transport.open(request)?;
        let mut file = std::fs::File::create(partial)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut done = 0u64;
        loop {
            let n = response
                .body
                .read(&mut buffer)
                .map_err(|e| Error::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            file.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
            done += n as u64;
            let fraction = response
                .length
                .map_or(0.0, |t| (done as f32 / t.max(1) as f32).min(1.0));
            if !progress(fraction) {
                return Err(Error::Cancelled);
            }
        }
        file.flush()?;
        if response.length.is_some_and(|t| t != done) {
            return Err(Error::Network("the file was cut short".into()));
        }
        if let Some(want) = &request.sha256 {
            let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if &got != want {
                return Err(Error::WrongChecksum);
            }
        }
        progress(1.0);
        Ok(())
    }
}

/// A folder of downloads named by their SHA-256, so the same content is
/// fetched once however many sources name it.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where content with this SHA-256 is kept.
    pub fn path(&self, sha256: &str) -> PathBuf {
        self.dir.join(sha256.trim().to_ascii_lowercase())
    }

    pub fn contains(&self, sha256: &str) -> bool {
        self.path(sha256).is_file()
    }

    /// The cached file for `key`, fetching it first if it is not here. The key
    /// is the content's identity: the byte SHA-256 for model files, the hash of
    /// the decoded audio for pack samples (checked by the downloader's
    /// [`with_verify`](Downloader::with_verify)). A hit asks no consent and
    /// touches no network.
    ///
    /// The key must be earned: either `request.sha256` equals the key, or the
    /// downloader has a verify hook (which is then responsible for checking
    /// the content against the key). Otherwise nothing unchecked is stored
    /// under a content-addressed name, and this returns `NoChecksum`.
    pub fn get<T: Transport>(
        &self,
        downloader: &Downloader<T>,
        key: &str,
        request: &Request,
        consent: Option<Consent<'_>>,
        progress: Progress<'_>,
    ) -> Result<PathBuf, Error> {
        let key = key.trim().to_ascii_lowercase();
        let earned = request.sha256.as_deref() == Some(key.as_str()) || downloader.has_verify();
        if key.is_empty() || !earned {
            return Err(Error::NoChecksum);
        }
        let path = self.path(&key);
        if path.is_file() {
            return Ok(path);
        }
        downloader.fetch(request, &path, consent, progress)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Cursor;

    use super::*;

    /// A fake transport: no network.
    struct Fake {
        body: Vec<u8>,
        claimed: Option<u64>,
        seen_auth: Cell<Option<Auth>>,
        opened: Cell<u32>,
    }

    impl Fake {
        fn new(body: Vec<u8>) -> Self {
            let claimed = Some(body.len() as u64);
            Self {
                body,
                claimed,
                seen_auth: Cell::new(None),
                opened: Cell::new(0),
            }
        }
    }

    impl Transport for &Fake {
        fn open(&self, request: &Request) -> Result<Response, Error> {
            self.opened.set(self.opened.get() + 1);
            self.seen_auth.set(request.auth.clone());
            Ok(Response {
                length: self.claimed,
                body: Box::new(Cursor::new(self.body.clone())),
            })
        }
    }

    fn sha256_of(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|i| i as u8).collect()
    }

    #[test]
    fn a_download_lands_under_its_name_with_progress() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new(body());
        let request = Request::new("http://x/y").with_sha256(sha256_of(&fake.body));
        let dest = dir.path().join("sub").join("file.bin");
        let mut seen = Vec::new();
        Downloader::new(&fake)
            .fetch(&request, &dest, None, &mut |f| {
                seen.push(f);
                true
            })
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), fake.body);
        assert_eq!(seen.last(), Some(&1.0));
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
        assert!(!dir.path().join("sub/file.bin.partial").exists());
    }

    #[test]
    fn a_corrupted_payload_fails_the_hash_check_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake::new(body());
        let request = Request::new("http://x/y").with_sha256(sha256_of(&fake.body));
        fake.body[1234] ^= 0xff;
        let dest = dir.path().join("file.bin");
        let error = Downloader::new(&fake)
            .fetch(&request, &dest, None, &mut |_| true)
            .unwrap_err();
        assert!(matches!(error, Error::WrongChecksum), "{error}");
        assert!(!dest.exists());
        assert!(!dir.path().join("file.bin.partial").exists());
    }

    #[test]
    fn cancelling_and_a_short_file_leave_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake::new(vec![1u8; 500_000]);
        let dest = dir.path().join("file.bin");
        let request = Request::new("http://x/y");
        let error = Downloader::new(&fake)
            .fetch(&request, &dest, None, &mut |_| false)
            .unwrap_err();
        assert!(matches!(error, Error::Cancelled), "{error}");
        fake.claimed = Some(900_000);
        let error = Downloader::new(&fake)
            .fetch(&request, &dest, None, &mut |_| true)
            .unwrap_err();
        assert!(matches!(error, Error::Network(_)), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn declined_consent_fetches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new(vec![1; 10]);
        let error = Downloader::new(&fake)
            .fetch(
                &Request::new("http://x/y"),
                &dir.path().join("f"),
                Some(&|_| false),
                &mut |_| true,
            )
            .unwrap_err();
        assert!(matches!(error, Error::Declined));
        assert_eq!(fake.opened.get(), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_failing_verify_discards_the_download() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new(body());
        let dest = dir.path().join("f");
        let request = Request::new("http://x/y");
        let rejecting = Downloader::new(&fake).with_verify(|_| Err(Error::WrongChecksum));
        let error = rejecting
            .fetch(&request, &dest, None, &mut |_| true)
            .unwrap_err();
        assert!(matches!(error, Error::WrongChecksum));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // It sees the finished file, so it can decode and hash the content.
        let accepting = Downloader::new(&fake).with_verify(|p| {
            assert_eq!(std::fs::metadata(p).unwrap().len(), 200_000);
            Ok(())
        });
        accepting
            .fetch(&request, &dest, None, &mut |_| true)
            .unwrap();
        assert!(dest.is_file());
    }

    #[test]
    fn credentials_reach_the_transport() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new(vec![1; 10]);
        let auth = Auth::Bearer("tok".into());
        let request = Request::new("http://x/y").with_auth(auth.clone());
        Downloader::new(&fake)
            .fetch(&request, &dir.path().join("f"), None, &mut |_| true)
            .unwrap();
        assert_eq!(fake.seen_auth.take(), Some(auth));
    }

    #[test]
    fn auth_headers() {
        assert_eq!(Auth::Bearer("abc".into()).header(), "Bearer abc");
        let basic = Auth::Basic {
            user: "Aladdin".into(),
            password: "open sesame".into(),
        };
        // RFC 7617's example.
        assert_eq!(basic.header(), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert!(!format!("{:?}", Auth::Bearer("secret".into())).contains("secret"));
    }

    #[test]
    fn the_cache_is_keyed_by_content_and_a_hit_skips_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("cache"));
        let fake = Fake::new(body());
        let hash = sha256_of(&fake.body);
        let downloader = Downloader::new(&fake);
        let request = Request::new("http://x/y").with_sha256(&hash);
        let path = cache
            .get(&downloader, &hash, &request, None, &mut |_| true)
            .unwrap();
        assert_eq!(path, cache.path(&hash));
        assert!(cache.contains(&hash));
        // Another address for the same content is a hit: no consent, no fetch.
        let again = Request::new("http://other/z").with_sha256(&hash);
        let hit = cache
            .get(&downloader, &hash, &again, Some(&|_| false), &mut |_| true)
            .unwrap();
        assert_eq!(hit, path);
        assert_eq!(fake.opened.get(), 1);
        // A key the download is not checked against is refused, and nothing is stored.
        let other_key = "ab".repeat(32);
        let unkeyed = Request::new("http://x/y");
        assert!(matches!(
            cache.get(&downloader, &other_key, &request, None, &mut |_| true),
            Err(Error::NoChecksum)
        ));
        assert!(!cache.contains(&other_key));
        // With a verify hook the caller vouches for the key.
        let hooked = Downloader::new(&fake).with_verify(|_| Ok(()));
        assert!(
            cache
                .get(&hooked, &other_key, &unkeyed, None, &mut |_| true)
                .is_ok()
        );
        assert!(matches!(
            cache.get(&downloader, "", &unkeyed, None, &mut |_| true),
            Err(Error::NoChecksum)
        ));
    }
}
