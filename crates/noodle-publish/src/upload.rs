//! Uploading a built site to a [`Store`]: an S3-compatible bucket (R2), or a
//! directory for a dry run.
//!
//! Two steps, by two kinds of person. A contributor [`upload_objects`] from a
//! built site, so the audio is in the store before the PR. CI [`publish`]es
//! from the definitions alone, after checking each object is there and hashes
//! to its name. Objects are immutable: a put of one is conditional
//! (`If-None-Match: *`), so a name the store already has is left as it is.

use std::fs;
use std::path::{Path, PathBuf};

use noodle_packs::{Index, Manifest};

use crate::{INDEX, PublishError, manifest_path, object_path};

pub use noodle_network::s3::Put;

/// Where a site is published.
pub trait Store {
    /// The size of the object at `key`, or `None` if there is none.
    fn size(&self, key: &str) -> Result<Option<u64>, String>;
    /// The object at `key`, or `None` if there is none.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String>;
    /// Writes `body` at `key`. With `only_new`, leaves an existing key alone.
    fn put(
        &self,
        key: &str,
        body: &[u8],
        content_type: &str,
        cache_control: &str,
        only_new: bool,
    ) -> Result<Put, String>;
}

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";

/// What an upload did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Uploaded {
    pub objects_stored: usize,
    pub objects_present: usize,
}

fn fail(key: &str, message: String) -> PublishError {
    PublishError::Upload {
        key: key.to_owned(),
        message,
    }
}

fn manifests_of(site: &Path) -> Result<Vec<Manifest>, PublishError> {
    let index_path = site.join(INDEX);
    let text = fs::read_to_string(&index_path).map_err(crate::io(&index_path))?;
    let index = Index::from_json(&text).map_err(|source| PublishError::Pack {
        path: index_path.clone(),
        source: Box::new(source),
    })?;
    index
        .packs
        .iter()
        .map(|entry| {
            let path = site.join(&entry.manifest);
            let text = fs::read_to_string(&path).map_err(crate::io(&path))?;
            Manifest::from_json(&text).map_err(|source| PublishError::Pack {
                path: path.clone(),
                source: Box::new(source),
            })
        })
        .collect()
}

/// Uploads the objects of a built `site`, and only those: a contributor does
/// this before raising the PR, so the files are already in the store when CI
/// checks the definition. Each put is conditional, so an object the store
/// already has is left as it is. Verify the site first ([`crate::verify`]).
pub fn upload_objects(site: &Path, store: &dyn Store) -> Result<Uploaded, PublishError> {
    let mut done = Uploaded::default();
    for manifest in manifests_of(site)? {
        for s in &manifest.samples {
            let key = object_path(&s.hash);
            let file = site.join(&key);
            let body = fs::read(&file).map_err(crate::io(&file))?;
            match store
                .put(&key, &body, "audio/flac", IMMUTABLE, true)
                .map_err(|m| fail(&key, m))?
            {
                Put::Stored => done.objects_stored += 1,
                Put::Exists => done.objects_present += 1,
            }
        }
    }
    Ok(done)
}

/// What a publish did.
#[derive(Debug, PartialEq, Eq)]
pub struct Published {
    pub packs: usize,
    pub samples: usize,
}

/// Publishes the packs under `packs_dir` from definitions alone: the audio is
/// already in the store (see [`upload_objects`]). Every object a definition
/// names must exist and decode to the hash in its name; a manifest's `bytes`
/// is the size of the stored object. Manifests go up after the check of every
/// pack, `index.json` last, so a reader never sees a pack whose objects are
/// missing.
pub fn publish(packs_dir: &Path, store: &dyn Store) -> Result<Published, PublishError> {
    let packs = crate::check_defs(packs_dir)?;
    let mut manifests = Vec::new();
    let mut samples = 0;
    for pack in &packs {
        let mut manifest = pack.def.manifest();
        for s in &mut manifest.samples {
            let key = object_path(&s.hash);
            let body = store.get(&key).map_err(|m| fail(&key, m))?.ok_or_else(|| {
                PublishError::MissingObject {
                    pack: pack.def.id.clone(),
                    hash: s.hash.clone(),
                }
            })?;
            let actual = crate::bytes_hash(&body)?;
            if actual != s.hash {
                return Err(PublishError::HashMismatch {
                    path: PathBuf::from(&key),
                    name: s.name.clone(),
                    expected: s.hash.clone(),
                    actual,
                });
            }
            s.bytes = Some(body.len() as u64);
            samples += 1;
        }
        let key = manifest_path(&pack.def.id);
        manifests.push((key, manifest));
    }
    let mut entries = Vec::new();
    for (key, manifest) in &manifests {
        let json = manifest.to_json().map_err(|source| PublishError::Pack {
            path: PathBuf::from(key),
            source: Box::new(source),
        })?;
        store
            .put(key, json.as_bytes(), "application/json", REVALIDATE, false)
            .map_err(|m| fail(key, m))?;
        entries.push(manifest.index_entry(key));
    }
    let index = Index::new(entries)
        .to_json()
        .map_err(|source| PublishError::Pack {
            path: PathBuf::from(INDEX),
            source: Box::new(source),
        })?;
    store
        .put(
            INDEX,
            index.as_bytes(),
            "application/json",
            REVALIDATE,
            false,
        )
        .map_err(|m| fail(INDEX, m))?;
    Ok(Published {
        packs: packs.len(),
        samples,
    })
}

/// A directory standing in for a bucket, for dry runs and tests.
pub struct DirStore(pub PathBuf);

impl Store for DirStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        match fs::read(self.0.join(key)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn size(&self, key: &str) -> Result<Option<u64>, String> {
        match fs::metadata(self.0.join(key)) {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn put(
        &self,
        key: &str,
        body: &[u8],
        _content_type: &str,
        _cache_control: &str,
        only_new: bool,
    ) -> Result<Put, String> {
        let path = self.0.join(key);
        if only_new && path.exists() {
            return Ok(Put::Exists);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let partial = path.with_extension("partial");
        fs::write(&partial, body).map_err(|e| e.to_string())?;
        fs::rename(&partial, &path).map_err(|e| e.to_string())?;
        Ok(Put::Stored)
    }
}

/// An R2 bucket as a [`Store`].
pub struct R2(pub noodle_network::s3::Bucket);

impl Store for R2 {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.0.get(key)
    }

    fn size(&self, key: &str) -> Result<Option<u64>, String> {
        self.0.size(key)
    }

    fn put(
        &self,
        key: &str,
        body: &[u8],
        content_type: &str,
        cache_control: &str,
        only_new: bool,
    ) -> Result<Put, String> {
        self.0.put(key, body, content_type, cache_control, only_new)
    }
}
