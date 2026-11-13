use serde::{Deserialize, Serialize};

use crate::{Error, Licence, check_format};

/// The per-pack manifest a source serves. JSON, so any static host and any
/// language can read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub id: String,
    pub name: String,
    pub version: u32,
    #[serde(default)]
    pub description: String,
    pub samples: Vec<ManifestSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestSample {
    pub hash: String,
    pub name: String,
    pub author: String,
    pub licence: Licence,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub credit: Option<String>,
    /// Size of the uploaded object, filled in by the publisher.
    #[serde(default)]
    pub bytes: Option<u64>,
}

/// The source's `index.json`: the mutable list of packs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub format: u32,
    pub packs: Vec<IndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub name: String,
    pub version: u32,
    #[serde(default)]
    pub description: String,
    /// Where the manifest is, relative to the index's URL.
    pub manifest: String,
    pub samples: u32,
}

impl Manifest {
    pub fn from_json(text: &str) -> Result<Self, Error> {
        let m: Manifest = serde_json::from_str(text)?;
        check_format(m.format)?;
        Ok(m)
    }

    pub fn to_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// This pack's entry in the index, naming `manifest_path` as its manifest.
    pub fn index_entry(&self, manifest_path: &str) -> IndexEntry {
        IndexEntry {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version,
            description: self.description.clone(),
            manifest: manifest_path.to_string(),
            samples: self.samples.len() as u32,
        }
    }
}

impl Index {
    pub fn new(packs: Vec<IndexEntry>) -> Self {
        Index {
            format: crate::FORMAT_VERSION,
            packs,
        }
    }

    pub fn from_json(text: &str) -> Result<Self, Error> {
        let i: Index = serde_json::from_str(text)?;
        check_format(i.format)?;
        Ok(i)
    }

    pub fn to_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}
