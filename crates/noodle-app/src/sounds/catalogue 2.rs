//! What the Sounds panel lists, and how it is searched and filtered.
//!
//! An [`Entry`] is one item of content from a source. The panel is a general
//! content browser (ADR 0012), so nothing here assumes the item is a sample:
//! [`Kind`] says what it is, and Nodes, Groups and Presets can be added to it
//! as tabs later. Everything here is pure, so the search is tested without a
//! window.

use std::collections::HashSet;

use noodle_packs::{Licence, Manifest, is_hash};
use serde::{Deserialize, Serialize};

use super::folder::LocalFile;

/// The name a source is known by in a saved catalogue and in filters.
pub type SourceId = String;

/// What kind of content an entry is. Only sounds so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Sound,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Sound => "Sounds",
        }
    }
}

/// One item a source offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    /// What identifies it within its kind. For a sound, the hash of its
    /// decoded audio, which is also the name of its object in every source.
    pub key: String,
    pub name: String,
    pub pack: String,
    pub author: String,
    pub licence: Licence,
    pub tags: Vec<String>,
    /// The attribution line that travels with it.
    pub credit: Option<String>,
    /// The size of its object, if the source says.
    pub bytes: Option<u64>,
    pub source: SourceId,
    /// Where its audio is, for a sound in a folder source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<LocalFile>,
}

impl Entry {
    /// The attribution to put on a clip made from this entry: the author's
    /// own line, or one made from the author and licence when the licence
    /// asks for credit and the author gave none. `None` when nothing is owed
    /// and nothing was given.
    pub fn credit_line(&self) -> Option<String> {
        if let Some(credit) = self.credit.as_deref().filter(|c| !c.trim().is_empty()) {
            return Some(credit.to_owned());
        }
        self.licence.requires_credit().then(|| {
            let by = if self.author.is_empty() {
                String::new()
            } else {
                format!(" by {}", self.author)
            };
            format!("{}{by} ({})", self.name, self.licence.label())
        })
    }
}

/// The entries of a source's pack. A sample whose hash is not 64 hex digits is
/// left out: the hash becomes a file name, so it must be nothing else.
pub fn entries_of(source: &str, manifest: &Manifest) -> Vec<Entry> {
    manifest
        .samples
        .iter()
        .filter(|s| is_hash(&s.hash))
        .map(|s| Entry {
            kind: Kind::Sound,
            key: s.hash.clone(),
            name: s.name.clone(),
            pack: manifest.name.clone(),
            author: s.author.clone(),
            licence: s.licence.clone(),
            tags: s.tags.clone(),
            credit: s.credit.clone(),
            bytes: s.bytes,
            source: source.to_owned(),
            file: None,
        })
        .collect()
}

/// What the search box and the two filters ask for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub query: String,
    /// Only this source.
    pub source: Option<SourceId>,
    /// Only this licence, by its label.
    pub licence: Option<String>,
}

impl Filter {
    /// Whether `entry` passes: every word of the query is in its name, tags,
    /// pack or author, and it is from the chosen source and under the chosen
    /// licence.
    pub fn accepts(&self, entry: &Entry) -> bool {
        if self.source.as_ref().is_some_and(|s| *s != entry.source) {
            return false;
        }
        if self
            .licence
            .as_ref()
            .is_some_and(|l| *l != entry.licence.label())
        {
            return false;
        }
        let haystack = format!(
            "{} {} {} {}",
            entry.name,
            entry.tags.join(" "),
            entry.pack,
            entry.author
        )
        .to_lowercase();
        self.query
            .split_whitespace()
            .all(|word| haystack.contains(&word.to_lowercase()))
    }
}

/// The entries `filter` accepts, those already on this computer first, then
/// by name.
pub fn list<'a>(entries: &'a [Entry], filter: &Filter, cached: &HashSet<String>) -> Vec<&'a Entry> {
    let mut found: Vec<&Entry> = entries.iter().filter(|e| filter.accepts(e)).collect();
    found.sort_by_cached_key(|e| (!cached.contains(&e.key), e.name.to_lowercase()));
    found
}

/// The licences in `entries`, by label, each once, in order.
pub fn licences(entries: &[Entry]) -> Vec<String> {
    let mut labels: Vec<String> = entries.iter().map(|e| e.licence.label()).collect();
    labels.sort();
    labels.dedup();
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    pub(crate) fn entry(n: u8, name: &str, pack: &str, source: &str) -> Entry {
        Entry {
            kind: Kind::Sound,
            key: hash(n),
            name: name.into(),
            pack: pack.into(),
            author: "Ada".into(),
            licence: Licence::Default,
            tags: vec!["drum".into()],
            credit: None,
            bytes: Some(1000),
            source: source.into(),
            file: None,
        }
    }

    #[test]
    fn the_search_covers_name_tags_pack_and_author() {
        let mut kick = entry(1, "Deep Kick", "Boom Pack", "a");
        kick.author = "Grace Hopper".into();
        let all = [kick];
        for (query, want) in [
            ("kick", 1),
            ("DRUM", 1),
            ("boom", 1),
            ("hopper", 1),
            ("kick boom", 1),
            ("kick snare", 0),
            ("", 1),
        ] {
            let filter = Filter {
                query: query.into(),
                ..Filter::default()
            };
            assert_eq!(list(&all, &filter, &HashSet::new()).len(), want, "{query}");
        }
    }

    #[test]
    fn the_filters_narrow_by_source_and_licence() {
        let mut cc = entry(2, "Hat", "P", "b");
        cc.licence = Licence::Spdx("CC-BY-4.0".into());
        let all = [entry(1, "Kick", "P", "a"), cc];
        let by_source = Filter {
            source: Some("b".into()),
            ..Filter::default()
        };
        assert_eq!(list(&all, &by_source, &HashSet::new())[0].name, "Hat");
        let by_licence = Filter {
            licence: Some("CC-BY-4.0".into()),
            ..Filter::default()
        };
        assert_eq!(list(&all, &by_licence, &HashSet::new()).len(), 1);
        assert_eq!(licences(&all), ["CC-BY-4.0", "Default"]);
    }

    #[test]
    fn sounds_already_downloaded_come_first() {
        let all = [
            entry(1, "Apple", "P", "a"),
            entry(2, "Zebra", "P", "a"),
            entry(3, "Mango", "P", "a"),
        ];
        let cached: HashSet<String> = [hash(2)].into();
        let names: Vec<_> = list(&all, &Filter::default(), &cached)
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, ["Zebra", "Apple", "Mango"]);
    }

    #[test]
    fn credit_is_the_authors_line_or_one_made_when_the_licence_asks() {
        let mut e = entry(1, "Kick", "P", "a");
        assert_eq!(e.credit_line(), None, "the default licence owes nothing");
        e.licence = Licence::Spdx("CC-BY-4.0".into());
        assert_eq!(e.credit_line().unwrap(), "Kick by Ada (CC-BY-4.0)");
        e.credit = Some("Kick by Ada Lovelace, freesound".into());
        assert_eq!(e.credit_line().unwrap(), "Kick by Ada Lovelace, freesound");
    }

    #[test]
    fn a_sample_whose_hash_is_not_a_hash_is_not_listed() {
        let manifest = Manifest {
            format: noodle_packs::FORMAT_VERSION,
            id: "p".into(),
            name: "P".into(),
            version: 1,
            description: String::new(),
            samples: ["../../etc/passwd", &hash(1)]
                .iter()
                .map(|h| noodle_packs::ManifestSample {
                    hash: (*h).into(),
                    name: "x".into(),
                    author: "a".into(),
                    licence: Licence::Default,
                    tags: vec![],
                    credit: None,
                    bytes: None,
                })
                .collect(),
        };
        let found = entries_of("s", &manifest);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, hash(1));
    }
}
