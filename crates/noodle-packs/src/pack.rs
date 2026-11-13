use serde::{Deserialize, Serialize};

use crate::{Error, FORMAT_VERSION, Licence, Manifest, ManifestSample, check_format};

/// A pack definition: the RON file reviewed in git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackDef {
    /// The version of this file format ([`FORMAT_VERSION`]).
    pub format: u32,
    /// The pack's stable name in a source: lowercase, digits and hyphens.
    pub id: String,
    pub name: String,
    /// The pack's own revision, bumped by its author when samples change.
    pub version: u32,
    #[serde(default)]
    pub description: String,
    /// The default author of samples that name none.
    #[serde(default)]
    pub author: String,
    /// The default licence of samples that name none.
    #[serde(default)]
    pub licence: Licence,
    pub samples: Vec<SampleDef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleDef {
    /// Hash of the decoded audio (`Sample::hash()`): 64 lowercase hex digits.
    pub hash: String,
    pub name: String,
    /// The audio file, relative to the pack's directory, when the definition
    /// is checked or published from disk. Not part of the manifest.
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub licence: Option<Licence>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The attribution line that travels with the sample.
    #[serde(default)]
    pub credit: Option<String>,
}

impl PackDef {
    pub fn from_ron(text: &str) -> Result<Self, Error> {
        let pack: PackDef = ron::from_str(text)?;
        check_format(pack.format)?;
        Ok(pack)
    }

    pub fn to_ron(&self) -> Result<String, Error> {
        Ok(ron::ser::to_string_pretty(
            self,
            ron::ser::PrettyConfig::default(),
        )?)
    }

    /// The author of `sample`: its own, else the pack's.
    pub fn author_of<'a>(&'a self, sample: &'a SampleDef) -> &'a str {
        sample.author.as_deref().unwrap_or(&self.author)
    }

    /// The licence of `sample`: its own, else the pack's.
    pub fn licence_of<'a>(&'a self, sample: &'a SampleDef) -> &'a Licence {
        sample.licence.as_ref().unwrap_or(&self.licence)
    }

    /// The manifest to publish for this pack: every sample with its author and
    /// licence resolved, so a reader never needs the pack's defaults.
    pub fn manifest(&self) -> Manifest {
        Manifest {
            format: FORMAT_VERSION,
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version,
            description: self.description.clone(),
            samples: self
                .samples
                .iter()
                .map(|s| ManifestSample {
                    hash: s.hash.clone(),
                    name: s.name.clone(),
                    author: self.author_of(s).to_string(),
                    licence: self.licence_of(s).clone(),
                    tags: s.tags.clone(),
                    credit: s.credit.clone(),
                    bytes: None,
                })
                .collect(),
        }
    }
}
