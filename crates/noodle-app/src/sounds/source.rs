//! Sources: where sounds come from, and reading one.
//!
//! A source is a static site (ADR 0012): `index.json`, a manifest per pack and
//! an immutable `objects/<hash>.flac` per sample. Everything here goes through
//! a [`Net`], so tests serve a fake site and the app serves HTTP.

use std::io::Read;
use std::sync::Arc;

use noodle_io::decode_file;
use noodle_network::{Auth, Cache, Downloader, Error, Request, Response, Transport};
use noodle_nodes::Sample;
use noodle_packs::{Index, Manifest};

use super::catalogue::{Entry, SourceId, entries_of};

/// The id of the source Noodle ships with.
pub const BUILT_IN_ID: &str = "built-in";

/// Where the built-in source is served from, until the first publish says
/// otherwise. `NOODLE_SOUNDS_URL` overrides it.
pub const BUILT_IN_URL: &str = "https://sounds.noodledaw.com";

/// The most an index or a manifest may be. A source is not trusted.
const MAX_TEXT: u64 = 16 * 1024 * 1024;

/// A static site of packs.
#[derive(Clone)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    pub url: String,
    pub auth: Option<Auth>,
}

impl Source {
    /// The source Noodle ships with.
    pub fn built_in() -> Self {
        Self {
            id: BUILT_IN_ID.into(),
            name: "Noodle sounds".into(),
            url: std::env::var("NOODLE_SOUNDS_URL").unwrap_or_else(|_| BUILT_IN_URL.into()),
            auth: None,
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

/// How bytes reach the app. Clones share the transport.
#[derive(Clone)]
pub struct Net(Arc<dyn Transport + Send + Sync>);

impl Net {
    pub fn new(transport: impl Transport + Send + Sync + 'static) -> Self {
        Self(Arc::new(transport))
    }

    pub fn http() -> Self {
        Self::new(noodle_network::Http)
    }
}

impl Transport for Net {
    fn open(&self, request: &Request) -> Result<Response, Error> {
        self.0.open(request)
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
pub fn fetch_catalogue(net: &Net, source: &Source) -> Result<Vec<Entry>, String> {
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
fn audio_hash(path: &std::path::Path) -> Result<String, String> {
    let audio = decode_file(path).map_err(|e| e.to_string())?;
    Ok(Sample::from_audio(&audio).hash())
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
        super::Source {
            id: "fake".into(),
            name: "Fake".into(),
            url: BASE.into(),
            auth: None,
        }
    }
}
