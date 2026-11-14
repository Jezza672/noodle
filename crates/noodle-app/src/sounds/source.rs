//! Sources: where sounds come from, and reading one.
//!
//! A source is a static site (ADR 0012): `index.json`, a manifest per pack and
//! an immutable `objects/<hash>.flac` per sample. Everything here goes through
//! a [`Net`], so tests serve a fake site and the app serves HTTP.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use noodle_io::decode_file;
use noodle_network::{Auth, Cache, Downloader, Error, Request, Response, Transport};
use noodle_nodes::Sample;
use noodle_packs::{Index, Licence, Manifest};
use serde::{Deserialize, Serialize};

use super::catalogue::{Entry, SourceId, entries_of};
use super::folder;

/// The id of the source Noodle ships with.
pub const BUILT_IN_ID: &str = "built-in";

/// Where the built-in source is served from, until the first publish says
/// otherwise. `NOODLE_SOUNDS_URL` overrides it.
pub const BUILT_IN_URL: &str = "https://sounds.noodledaw.com";

/// The most an index or a manifest may be. A source is not trusted.
const MAX_TEXT: u64 = 16 * 1024 * 1024;

/// A place sounds come from: a static site of packs, or a folder on disk.
#[derive(Clone)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    /// Where the site is. Empty for a folder.
    pub url: String,
    pub auth: Option<Auth>,
    /// Set when the source is a folder, which is read in place.
    pub folder: Option<Folder>,
}

/// A folder on this computer, used as a source. Nothing in it is copied or
/// uploaded: the files are scanned, hashed and played from where they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    /// What its sounds are licensed under, [`Licence::Unknown`] until set.
    pub licence: Licence,
}

/// How a source's credentials are sent. The secret itself (the token or the
/// password) is not here: it is in the keychain, or the preferences file where
/// there is none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Login {
    None,
    Bearer,
    Basic(String),
}

/// A source the user added, as it is kept in the preferences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpec {
    pub id: SourceId,
    pub name: String,
    pub place: Place,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Place {
    Url { url: String, login: Login },
    Folder { path: PathBuf, licence: Licence },
}

impl Source {
    /// The source Noodle ships with.
    pub fn built_in() -> Self {
        Self::remote(
            BUILT_IN_ID,
            "Noodle sounds",
            std::env::var("NOODLE_SOUNDS_URL").unwrap_or_else(|_| BUILT_IN_URL.into()),
            None,
        )
    }

    /// A static site at `url`.
    pub fn remote(
        id: impl Into<SourceId>,
        name: impl Into<String>,
        url: impl Into<String>,
        auth: Option<Auth>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            url: url.into(),
            auth,
            folder: None,
        }
    }

    /// The folder at `path`.
    pub fn local(
        id: impl Into<SourceId>,
        name: impl Into<String>,
        path: PathBuf,
        licence: Licence,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            url: String::new(),
            auth: None,
            folder: Some(Folder { path, licence }),
        }
    }

    /// Whether Noodle ships it. The rest are the user's, and can be removed.
    pub fn is_built_in(&self) -> bool {
        self.id == BUILT_IN_ID
    }

    /// Whether the user pointed Noodle at a host it knows nothing about.
    pub fn is_self_hosted(&self) -> bool {
        !self.is_built_in() && self.folder.is_none()
    }

    /// What is kept of it in the preferences. The secret is not.
    pub fn spec(&self) -> SourceSpec {
        let place = match &self.folder {
            Some(folder) => Place::Folder {
                path: folder.path.clone(),
                licence: folder.licence.clone(),
            },
            None => Place::Url {
                url: self.url.clone(),
                login: match &self.auth {
                    None => Login::None,
                    Some(Auth::Bearer(_)) => Login::Bearer,
                    Some(Auth::Basic { user, .. }) => Login::Basic(user.clone()),
                },
            },
        };
        SourceSpec {
            id: self.id.clone(),
            name: self.name.clone(),
            place,
        }
    }

    /// The source a spec describes, with its `secret` (the token or password)
    /// if it has credentials.
    pub fn from_spec(spec: SourceSpec, secret: Option<String>) -> Self {
        match spec.place {
            Place::Folder { path, licence } => Self::local(spec.id, spec.name, path, licence),
            Place::Url { url, login } => {
                let auth = match (login, secret) {
                    (Login::Bearer, Some(token)) => Some(Auth::Bearer(token)),
                    (Login::Basic(user), Some(password)) => Some(Auth::Basic { user, password }),
                    _ => None,
                };
                Self::remote(spec.id, spec.name, url, auth)
            }
        }
    }

    /// The token or password, which is what goes in the keychain.
    pub fn secret(&self) -> Option<&str> {
        match self.auth.as_ref()? {
            Auth::Bearer(token) => Some(token),
            Auth::Basic { password, .. } => Some(password),
        }
    }

    /// `relative`, resolved against the source's address.
    pub fn join(&self, relative: &str) -> String {
        format!(
            "{}/{}",
            self.url.trim_end_matches('/'),
            relative.trim_start_matches('/')
        )
    }

    /// Where the audio of the sound with this hash is.
    pub fn object_url(&self, hash: &str) -> String {
        self.join(&format!("objects/{hash}.flac"))
    }

    fn request(&self, url: String) -> Request {
        let request = Request::new(url);
        match &self.auth {
            Some(auth) => request.with_auth(auth.clone()),
            None => request,
        }
    }
}

/// The address of a source's site, tidied: the scheme checked, a pasted
/// `index.json` and trailing slash dropped. A login is only ever sent over
/// https (or to this computer), and never goes in the address, where it would
/// be saved in plain text and shown.
pub fn clean_url(raw: &str, with_login: bool) -> Result<String, String> {
    let raw = raw.trim();
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or("The address should start with https://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err("The address should start with https://".into());
    }
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        return Err("There is no host in that address".into());
    }
    if authority.contains('@') {
        return Err("Enter the login in its own fields, not in the address".into());
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().map(|h| format!("[{h}]")),
        None => authority.split(':').next().map(str::to_owned),
    }
    .unwrap_or_default()
    .to_ascii_lowercase();
    let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]");
    if scheme == "http" && with_login && !local {
        return Err("A login is only sent over https. Use an https address".into());
    }
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix("/index.json").unwrap_or(rest);
    Ok(format!("{scheme}://{}", rest.trim_end_matches('/')))
}

/// How bytes reach the app. Clones share the transport.
#[derive(Clone)]
pub struct Net(Arc<dyn Transport + Send + Sync>);

impl Net {
    pub fn new(transport: impl Transport + Send + Sync + 'static) -> Self {
        Self(Arc::new(transport))
    }

    /// HTTP, and the `file://` sites the wizard exports to this computer.
    pub fn http() -> Self {
        Self::new(Hosted)
    }
}

impl Transport for Net {
    fn open(&self, request: &Request) -> Result<Response, Error> {
        self.0.open(request)
    }
}

/// The address of a site on this computer, which is how a pack the wizard
/// exported is added as a source.
pub fn file_url(dir: &std::path::Path) -> String {
    format!("file://{}", dir.to_string_lossy().replace('\\', "/"))
}

/// Http for a site on the internet, the disk for a `file://` one.
struct Hosted;

impl Transport for Hosted {
    fn open(&self, request: &Request) -> Result<Response, Error> {
        let Some(path) = request.url.strip_prefix("file://") else {
            return noodle_network::Http.open(request);
        };
        let file = std::fs::File::open(path).map_err(|e| Error::Network(e.to_string()))?;
        let length = file.metadata().ok().map(|m| m.len());
        Ok(Response {
            length,
            body: Box::new(file),
        })
    }
}

fn read_text(net: &Net, request: &Request) -> Result<String, String> {
    let response = net.open(request).map_err(|e| e.to_string())?;
    let mut text = String::new();
    response
        .body
        .take(MAX_TEXT + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_TEXT {
        return Err("the file is too big to be an index".into());
    }
    Ok(text)
}

/// The catalogue of `source`: every pack the index lists. A pack whose
/// manifest can't be read is left out and doesn't hide the others; an index
/// that can't be read is the source being unreachable.
pub fn fetch_catalogue(net: &Net, source: &Source, known: &[Entry]) -> Result<Vec<Entry>, String> {
    if let Some(local) = &source.folder {
        return folder::scan(source, local, known);
    }
    let index_url = source.join("index.json");
    let index = Index::from_json(&read_text(net, &source.request(index_url))?)
        .map_err(|e| format!("the index isn't one Noodle can read: {e}"))?;
    let mut entries = Vec::new();
    for pack in &index.packs {
        let url = source.join(&pack.manifest);
        let Ok(text) = read_text(net, &source.request(url)) else {
            continue;
        };
        if let Ok(manifest) = Manifest::from_json(&text) {
            entries.extend(entries_of(&source.id, &manifest));
        }
    }
    Ok(entries)
}

/// The hash of the audio in the file at `path`, which is what names a sound.
pub(super) fn audio_hash(path: &std::path::Path) -> Result<String, String> {
    let audio = decode_file(path).map_err(|e| e.to_string())?;
    Ok(Sample::from_audio(&audio).hash())
}

/// The file of a sound in a folder source, which is played where it is.
/// Checked first: a file that has changed since the scan is not the sound that
/// was listed, and the sound's hash is what the project keeps.
pub fn open_local(entry: &Entry) -> Result<std::path::PathBuf, String> {
    let file = entry
        .file
        .as_ref()
        .ok_or("that sound has no file on this computer")?;
    if !file.path.is_file() {
        return Err("the file isn't there any more. Refresh the source".into());
    }
    if audio_hash(&file.path)? != entry.key {
        return Err("the file has changed since it was scanned. Refresh the source".into());
    }
    Ok(file.path.clone())
}

/// Brings the sound `hash` into `cache` from `source` and returns where it
/// is, checking that the audio is what its name says. A file that isn't audio,
/// or is other audio, is discarded.
pub fn download(
    net: &Net,
    cache: &Cache,
    source: &Source,
    hash: &str,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<std::path::PathBuf, String> {
    if !noodle_packs::is_hash(hash) {
        return Err("that isn't a sound's hash".into());
    }
    let want = hash.to_owned();
    let downloader = Downloader::new(net.clone()).with_verify(move |path| {
        let got = audio_hash(path).map_err(|why| Error::Network(format!("not audio: {why}")))?;
        if got == want {
            Ok(())
        } else {
            Err(Error::WrongChecksum)
        }
    });
    let request = source.request(source.object_url(hash));
    cache
        .get(&downloader, hash, &request, None, progress)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) mod fake {
    //! A source served from memory, for tests.

    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    use noodle_io::{ExportFormat, ExportWriter, decode_file};
    use noodle_network::{Error, Request, Response, Transport};
    use noodle_nodes::Sample;
    use noodle_packs::{FORMAT_VERSION, Index, Licence, Manifest, ManifestSample};

    /// A fake host: the bodies it serves by URL, and every URL asked for.
    #[derive(Clone, Default)]
    pub struct Site {
        pub files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        pub asked: Arc<Mutex<Vec<String>>>,
        pub offline: Arc<std::sync::atomic::AtomicBool>,
        /// The `Authorization` header every request must carry, if set.
        pub require: Arc<Mutex<Option<String>>>,
    }

    impl Site {
        pub fn put(&self, url: &str, body: impl Into<Vec<u8>>) {
            self.files.lock().unwrap().insert(url.into(), body.into());
        }

        pub fn asked_for(&self, part: &str) -> usize {
            self.asked
                .lock()
                .unwrap()
                .iter()
                .filter(|u| u.contains(part))
                .count()
        }
    }

    impl Transport for Site {
        fn open(&self, request: &Request) -> Result<Response, Error> {
            self.asked.lock().unwrap().push(request.url.clone());
            if self.offline.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Error::Network("no connection".into()));
            }
            let want = self.require.lock().unwrap().clone();
            if want.is_some_and(|w| request.auth.as_ref().map(|a| a.header()) != Some(w)) {
                return Err(Error::Network("401".into()));
            }
            match self.files.lock().unwrap().get(&request.url) {
                Some(body) => Ok(Response {
                    length: Some(body.len() as u64),
                    body: Box::new(Cursor::new(body.clone())),
                }),
                None => Err(Error::Network("404".into())),
            }
        }
    }

    pub const BASE: &str = "https://fake.test";

    /// A short FLAC of a tone, and the hash of what it decodes to.
    pub fn tone(dir: &std::path::Path, hz: f32) -> (Vec<u8>, String) {
        let path = dir.join(format!("{hz}.flac"));
        let mut writer =
            ExportWriter::create(&path, ExportFormat::Flac24, 1, 48_000, Some(4800)).unwrap();
        let samples: Vec<f32> = (0..4800)
            .map(|i| 0.4 * (i as f32 * hz * std::f32::consts::TAU / 48_000.0).sin())
            .collect();
        writer.write(&samples).unwrap();
        writer.finish().unwrap();
        let hash = Sample::from_audio(&decode_file(&path).unwrap()).hash();
        (std::fs::read(&path).unwrap(), hash)
    }

    /// A site with one pack of `tones`, each named `Tone <hz>`.
    pub fn site(dir: &std::path::Path, tones: &[f32]) -> (Site, Vec<String>) {
        let site = Site::default();
        let mut hashes = Vec::new();
        let mut samples = Vec::new();
        for &hz in tones {
            let (bytes, hash) = tone(dir, hz);
            site.put(&format!("{BASE}/objects/{hash}.flac"), bytes.clone());
            samples.push(ManifestSample {
                hash: hash.clone(),
                name: format!("Tone {hz}"),
                author: "Ada".into(),
                licence: if hz > 400.0 {
                    Licence::Spdx("CC-BY-4.0".into())
                } else {
                    Licence::Default
                },
                tags: vec!["tone".into()],
                credit: None,
                bytes: Some(bytes.len() as u64),
            });
            hashes.push(hash);
        }
        let manifest = Manifest {
            format: FORMAT_VERSION,
            id: "tones".into(),
            name: "Tones".into(),
            version: 1,
            description: String::new(),
            samples,
        };
        let index = Index::new(vec![manifest.index_entry("packs/tones/manifest.json")]);
        site.put(&format!("{BASE}/index.json"), index.to_json().unwrap());
        site.put(
            &format!("{BASE}/packs/tones/manifest.json"),
            manifest.to_json().unwrap(),
        );
        (site, hashes)
    }

    pub fn source() -> super::Source {
        super::Source::remote("fake", "Fake", BASE, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_tidied_and_a_login_is_kept_off_plain_http() {
        let ok = |raw, login| clean_url(raw, login);
        assert_eq!(
            ok(" https://Packs.example.com/sounds/index.json ", false).unwrap(),
            "https://Packs.example.com/sounds"
        );
        assert_eq!(ok("https://x.test/", true).unwrap(), "https://x.test");
        assert_eq!(
            ok("https://x.test/p/?a=1#b", false).unwrap(),
            "https://x.test/p"
        );
        assert!(ok("x.test", false).is_err(), "no scheme");
        assert!(ok("ftp://x.test", false).is_err());
        assert!(ok("https://", false).is_err());
        assert!(
            ok("https://me:pw@x.test", false).is_err(),
            "login in the address"
        );
        assert!(
            ok("http://x.test", true).is_err(),
            "a login over plain http"
        );
        assert!(ok("http://x.test", false).is_ok());
        assert!(ok("http://localhost:8080", true).is_ok());
        assert!(ok("http://127.0.0.1:9", true).is_ok());
        assert!(ok("http://[::1]:9", true).is_ok());
    }
}
