//! The two stores: system entries embedded in the app, user entries as one
//! `.ron` file each in a folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{GroupPreset, GroupTemplate, Item, Kind, NodePreset, slugify};

/// The store an entry ID starts with.
pub const SYSTEM: &str = "system";
pub const USER: &str = "user";

/// The embedded files. Each holds a list of entries: a template followed by
/// its presets.
const EMBEDDED: &[&str] = &[
    include_str!("../../library/system/track.ron"),
    include_str!("../../library/system/subtractive-voice.ron"),
    include_str!("../../library/system/mono-synth.ron"),
    include_str!("../../library/system/poly-synth.ron"),
    include_str!("../../library/system/pad.ron"),
    include_str!("../../library/system/kick.ron"),
    include_str!("../../library/system/snare.ron"),
    include_str!("../../library/system/hat.ron"),
    include_str!("../../library/system/clap.ron"),
    include_str!("../../library/system/nodes.ron"),
];

pub(super) fn parse_pack(text: &str) -> Result<Vec<Item>, ron::error::SpannedError> {
    ron::from_str(text)
}

/// One entry and where it came from.
#[derive(Clone, Debug)]
pub struct Stored {
    /// `store:kind:slug`, e.g. `system:template:kick`. Nodes remember it.
    pub id: String,
    pub item: Item,
    file: Option<PathBuf>,
}

impl Stored {
    pub fn is_system(&self) -> bool {
        self.id.starts_with(SYSTEM)
    }
}

/// Everything saved for reuse.
#[derive(Debug)]
pub struct Library {
    entries: Vec<Stored>,
    /// Where user entries are kept; `None` keeps them in memory only.
    dir: Option<PathBuf>,
    /// User files that couldn't be read, for the app to mention.
    pub problems: Vec<String>,
}

fn entry_id(store: &str, item: &Item) -> String {
    format!("{store}:{}:{}", item.kind().tag(), item.slug())
}

impl Library {
    /// The system entries and the user entries in `dir`, if there is one.
    pub fn open(dir: Option<PathBuf>) -> Self {
        let mut library = Self {
            entries: Vec::new(),
            dir,
            problems: Vec::new(),
        };
        for text in EMBEDDED {
            for mut item in parse_pack(text).expect("embedded library files are valid") {
                if item.slug().is_empty() {
                    item.set_slug(slugify(item.name()));
                }
                if let Item::Template(t) = &item {
                    t.validate().expect("embedded templates are valid");
                }
                library.entries.push(Stored {
                    id: entry_id(SYSTEM, &item),
                    item,
                    file: None,
                });
            }
        }
        library.load_user();
        library
    }

    /// System entries only, nothing kept on disk.
    pub fn in_memory() -> Self {
        Self::open(None)
    }

    fn load_user(&mut self) {
        let Some(dir) = self.dir.clone() else { return };
        let Ok(read) = fs::read_dir(&dir) else { return };
        let mut files: Vec<PathBuf> = read
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "ron"))
            .collect();
        files.sort();
        for file in files {
            match fs::read_to_string(&file) {
                Ok(text) => match Item::from_ron(&text) {
                    Ok(mut item) => {
                        if let Item::Template(t) = &item
                            && let Err(e) = t.validate()
                        {
                            self.problems.push(format!("{}: {e}", file.display()));
                            continue;
                        }
                        if item.slug().is_empty() {
                            item.set_slug(slugify(item.name()));
                        }
                        // A file copied in under another name can carry a
                        // slug already taken; give it its own, or it could
                        // never be reached.
                        let slug = self.unique_slug(item.kind(), item.slug());
                        item.set_slug(slug);
                        self.entries.push(Stored {
                            id: entry_id(USER, &item),
                            item,
                            file: Some(file),
                        });
                    }
                    Err(e) => self.problems.push(format!("{}: {e}", file.display())),
                },
                Err(e) => self.problems.push(format!("{}: {e}", file.display())),
            }
        }
    }

    /// Reads the user folder again, e.g. after files were copied in.
    pub fn reload(&mut self) {
        self.entries.retain(Stored::is_system);
        self.problems.clear();
        self.load_user();
    }

    pub fn get(&self, id: &str) -> Option<&Stored> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Every entry, hidden ones included, system before user.
    pub fn all(&self) -> impl Iterator<Item = &Stored> {
        self.entries.iter()
    }

    /// The entries the library lists: not hidden ones.
    pub fn listed(&self) -> impl Iterator<Item = &Stored> {
        self.entries.iter().filter(|e| !e.item.hidden())
    }

    /// Listed group templates, as (ID, template).
    pub fn templates(&self) -> impl Iterator<Item = (&str, &GroupTemplate)> {
        self.listed().filter_map(|e| match &e.item {
            Item::Template(t) => Some((e.id.as_str(), t)),
            _ => None,
        })
    }

    pub fn template(&self, id: &str) -> Option<&GroupTemplate> {
        match &self.get(id)?.item {
            Item::Template(t) => Some(t),
            _ => None,
        }
    }

    pub fn node_presets(&self) -> impl Iterator<Item = (&str, &NodePreset)> {
        self.entries.iter().filter_map(|e| match &e.item {
            Item::NodePreset(p) => Some((e.id.as_str(), p)),
            _ => None,
        })
    }

    pub fn group_presets(&self) -> impl Iterator<Item = (&str, &GroupPreset)> {
        self.entries.iter().filter_map(|e| match &e.item {
            Item::GroupPreset(p) => Some((e.id.as_str(), p)),
            _ => None,
        })
    }

    pub fn node_preset(&self, id: &str) -> Option<&NodePreset> {
        match &self.get(id)?.item {
            Item::NodePreset(p) => Some(p),
            _ => None,
        }
    }

    pub fn group_preset(&self, id: &str) -> Option<&GroupPreset> {
        match &self.get(id)?.item {
            Item::GroupPreset(p) => Some(p),
            _ => None,
        }
    }

    fn unique_slug(&self, kind: Kind, base: &str) -> String {
        let taken = |slug: &str| {
            self.entries
                .iter()
                .any(|e| e.id == format!("{USER}:{}:{slug}", kind.tag()))
        };
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|s| !taken(s))
            .expect("some number is free")
    }

    fn write(&self, item: &Item, file: &Path) -> io::Result<()> {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(file, item.to_ron())
    }

    /// Saves a new user entry under a name nothing else of its kind has, and
    /// returns its ID.
    pub fn save(&mut self, mut item: Item) -> io::Result<String> {
        let slug = self.unique_slug(item.kind(), &slugify(item.name()));
        item.set_slug(slug.clone());
        let file = self
            .dir
            .as_ref()
            .map(|dir| dir.join(format!("{}-{slug}.ron", item.kind().tag())));
        if let Some(file) = &file {
            self.write(&item, file)?;
        }
        let id = entry_id(USER, &item);
        self.entries.push(Stored {
            id: id.clone(),
            item,
            file,
        });
        Ok(id)
    }

    /// Replaces the user entry `id` with `item`, keeping its ID. System
    /// entries are read-only.
    pub fn overwrite(&mut self, id: &str, mut item: Item) -> io::Result<()> {
        let read_only = || io::Error::new(io::ErrorKind::PermissionDenied, "not a user entry");
        let at = self
            .entries
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(read_only)?;
        let stored = &self.entries[at];
        if stored.is_system() || stored.item.kind() != item.kind() {
            return Err(read_only());
        }
        item.set_slug(stored.item.slug().to_string());
        if let Some(file) = &stored.file {
            self.write(&item, file)?;
        }
        self.entries[at].item = item;
        Ok(())
    }

    /// Deletes a user entry and its file.
    pub fn remove(&mut self, id: &str) -> io::Result<()> {
        let at = self
            .entries
            .iter()
            .position(|e| e.id == id && !e.is_system())
            .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "not a user entry"))?;
        if let Some(file) = &self.entries[at].file {
            fs::remove_file(file)?;
        }
        self.entries.remove(at);
        Ok(())
    }
}
