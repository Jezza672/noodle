//! Adding, changing and removing the sources the user added, and the cache
//! they download into.
//!
//! The Sources page (`page.rs`) calls these, and so do tests. Whatever is
//! kept (the list, and the secrets) is saved as it changes.

use std::path::PathBuf;

use noodle_network::Auth;
use noodle_packs::Licence;

use super::Sounds;
use super::catalogue::SourceId;
use super::page::Cleanable;
use super::source::{Source, clean_url};
use super::vault::Kept;

/// The credentials a new source is added with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewLogin {
    None,
    Bearer(String),
    Basic { user: String, password: String },
}

impl Sounds {
    fn next_id(&self) -> SourceId {
        let mut n = 1;
        loop {
            let id = format!("source-{n}");
            if self.source(&id).is_none() {
                return id;
            }
            n += 1;
        }
    }

    /// Saves the list the user added, which is every source but the built-in.
    fn persist(&self) {
        let specs: Vec<_> = self
            .sources
            .iter()
            .filter(|s| !s.is_built_in())
            .map(Source::spec)
            .collect();
        crate::prefs::remember_sources(&specs);
    }

    fn adopt(&mut self, source: Source) -> SourceId {
        let id = source.id.clone();
        self.reach.insert(id.clone(), super::Reach::Asking);
        self.sources.push(source);
        self.persist();
        self.refresh();
        id
    }

    /// Adds a self-hosted source: a static site at `url`, with `login` if it
    /// asks for one. Says why when the address or the login won't do.
    pub fn add_remote(
        &mut self,
        name: &str,
        url: &str,
        login: NewLogin,
    ) -> Result<SourceId, String> {
        let url = clean_url(url, login != NewLogin::None)?;
        if self.sources.iter().any(|s| s.url == url) {
            return Err("That source is already added".into());
        }
        let auth = match login {
            NewLogin::None => None,
            NewLogin::Bearer(token) if token.trim().is_empty() => {
                return Err("Enter the token the source asks for".into());
            }
            NewLogin::Bearer(token) => Some(Auth::Bearer(token.trim().to_owned())),
            NewLogin::Basic { user, password } => {
                if user.trim().is_empty() {
                    return Err("Enter the user name the source asks for".into());
                }
                Some(Auth::Basic {
                    user: user.trim().to_owned(),
                    password,
                })
            }
        };
        let name = match name.trim() {
            "" => url
                .split("://")
                .nth(1)
                .and_then(|rest| rest.split('/').next())
                .unwrap_or("Source")
                .to_owned(),
            name => name.to_owned(),
        };
        let id = self.next_id();
        let source = Source::remote(id, name, url, auth);
        let kept = source
            .secret()
            .map(|secret| self.vault.keep(&source.id, secret));
        let name = source.name.clone();
        let id = self.adopt(source);
        self.note = Some(match kept {
            Some(Kept::PrefsFile) => format!(
                "Added {name}. Its login is kept in the preferences file, because there is no \
                 keychain Noodle can use here."
            ),
            Some(Kept::Keychain) => {
                format!("Added {name}. Its login is kept in the system keychain.")
            }
            None => format!("Added {name}."),
        });
        Ok(id)
    }

    /// Adds the folder at `path` as a source, read in place.
    pub fn add_folder(&mut self, path: PathBuf) -> Result<SourceId, String> {
        if !path.is_dir() {
            return Err("That isn't a folder".into());
        }
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if self
            .sources
            .iter()
            .any(|s| s.folder.as_ref().is_some_and(|f| f.path == path))
        {
            return Err("That folder is already a source".into());
        }
        let name = path
            .file_name()
            .map_or_else(|| "Folder".into(), |n| n.to_string_lossy().into_owned());
        let id = self.next_id();
        self.note = Some(format!(
            "Added {name}. Its sounds are read where they are: nothing is copied or uploaded."
        ));
        Ok(self.adopt(Source::local(id, name, path, Licence::Unknown)))
    }

    /// Adds a site the wizard exported to `dir` as a source, read from disk.
    pub fn add_site(&mut self, dir: &std::path::Path, name: &str) -> Result<SourceId, String> {
        if !dir.join("index.json").is_file() {
            return Err("That folder has no index.json".into());
        }
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
        let url = super::source::file_url(&dir);
        if self.sources.iter().any(|s| s.url == url) {
            return Err("That source is already added".into());
        }
        let name = match name.trim() {
            "" => "My pack",
            name => name,
        };
        let id = self.next_id();
        self.note = Some(format!(
            "Added {name}. It is read from {}, so move it to a host when you share it.",
            dir.display()
        ));
        Ok(self.adopt(Source::remote(id, name, url, None)))
    }

    /// Sets the licence every sound in a folder source is listed under.
    pub fn set_folder_licence(&mut self, id: &str, licence: Licence) {
        let Some(folder) = self
            .sources
            .iter_mut()
            .find(|s| s.id == id)
            .and_then(|s| s.folder.as_mut())
        else {
            return;
        };
        if folder.licence == licence {
            return;
        }
        folder.licence = licence.clone();
        for entry in self.entries.iter_mut().filter(|e| e.source == id) {
            entry.licence = licence.clone();
        }
        self.persist();
    }

    /// Takes a source the user added away: from the list, the preferences and
    /// the keychain. Sounds already downloaded stay until Clean up.
    pub fn remove_source(&mut self, id: &str) {
        let Some(at) = self
            .sources
            .iter()
            .position(|s| s.id == id && !s.is_built_in())
        else {
            return;
        };
        let source = self.sources.remove(at);
        self.vault.forget(id);
        self.reach.remove(id);
        self.entries.retain(|e| e.source != id);
        if self.filter.source.as_deref() == Some(id) {
            self.filter.source = None;
        }
        if let Some(file) = self.catalogue_file(id) {
            let _ = std::fs::remove_file(file);
        }
        self.persist();
        self.scan_cache();
        self.note = Some(format!("Removed {}.", source.name));
    }

    /// How many sounds a source lists, how many of them are on this computer,
    /// and how much room those take in the cache. A folder's sounds are not in
    /// the cache, so they take none.
    pub fn usage(&self, id: &str) -> Usage {
        let mut usage = Usage::default();
        for entry in self.entries.iter().filter(|e| e.source == id) {
            usage.sounds += 1;
            if let Some(bytes) = self.sizes.get(&entry.key) {
                usage.downloaded += 1;
                usage.bytes += bytes;
            }
        }
        usage
    }

    /// What Clean up would remove: what is in the cache that no source lists
    /// (sounds of a source that was removed, and downloads that never
    /// finished). Worked out when asked and kept until something changes it.
    pub fn cleanable(&mut self) -> Cleanable {
        if let Some(found) = self.cleanable {
            return found;
        }
        let listed: std::collections::HashSet<&str> =
            self.entries.iter().map(|e| e.key.as_str()).collect();
        let mut found = Cleanable::default();
        if let Some(cache) = &self.cache
            && let Ok(read) = std::fs::read_dir(cache.dir())
        {
            for file in read.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                let Ok(meta) = file.metadata() else {
                    continue;
                };
                if meta.is_file() && !listed.contains(name.as_str()) {
                    found.files += 1;
                    found.bytes += meta.len();
                }
            }
        }
        self.cleanable = Some(found);
        found
    }

    /// Deletes what [`cleanable`](Self::cleanable) found, and says how much.
    pub fn clean_up(&mut self) {
        let listed: std::collections::HashSet<String> =
            self.entries.iter().map(|e| e.key.clone()).collect();
        let (mut files, mut bytes) = (0, 0);
        if let Some(cache) = &self.cache
            && let Ok(read) = std::fs::read_dir(cache.dir())
        {
            for file in read.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                let Ok(meta) = file.metadata() else {
                    continue;
                };
                if meta.is_file()
                    && !listed.contains(&name)
                    && std::fs::remove_file(file.path()).is_ok()
                {
                    files += 1;
                    bytes += meta.len();
                }
            }
        }
        self.cleanable = None;
        self.note = Some(format!(
            "Cleaned up {files} file{}, {}.",
            if files == 1 { "" } else { "s" },
            page::size(bytes)
        ));
    }

    /// Whether the panel has asked for the Sources page since this was last
    /// called. The app opens Settings on it.
    pub fn take_wants_sources(&mut self) -> bool {
        std::mem::take(&mut self.wants_sources)
    }
}

use super::page;

/// What one source takes on this computer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub sounds: usize,
    pub downloaded: usize,
    pub bytes: u64,
}
