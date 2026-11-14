//! The Sounds panel: a content browser for what sources offer (ADR 0012).
//!
//! It lists the sounds of every source, searchable and filterable, and a click
//! auditions one through a player that is not the project's graph. A sound is
//! dragged onto a Sampler or a drum pad (which takes its hash), onto the
//! canvas (a Sample node) or onto the timeline (a clip carrying its credit).
//!
//! Nothing here touches the audio thread. Lists and sounds are fetched on
//! worker jobs through `noodle-network`; a sound that is not on this computer
//! is downloaded first and used when it arrives, and `SampleStore` is only
//! ever given a file that is already here.

pub mod apply;
pub mod catalogue;
mod folder;
mod manage;
mod page;
mod panel;
pub mod source;
mod vault;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Context, Id};
use noodle_core::{NodeId, Tick};
use noodle_engine::Job;
use noodle_network::Cache;
use noodle_nodes::Sample;

use crate::carry::SoundKey;
use crate::session::Session;
use catalogue::{Entry, Filter, SourceId};
use source::{Net, Source};
use vault::Vault;

/// A download bigger than this asks first.
pub const CONFIRM_BYTES: u64 = 8 * 1024 * 1024;

/// What a sound is for, once it is on this computer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Then {
    /// Play it, to try it.
    Audition,
    /// Make it the sample of this Sampler (or Sample node).
    Sampler(NodeId),
    /// Add a Sample node holding it at this point of the canvas, in `group`.
    Canvas {
        at: egui::Pos2,
        group: Option<NodeId>,
    },
    /// Copy it into the project and lay it on the timeline: on `track`, or on a
    /// new one when there is none.
    Timeline { track: Option<NodeId>, at: Tick },
}

/// Something a view asks of the panel. Views that only have `&Session` and
/// the egui context leave it in the context, and the app takes it each frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ask {
    /// Show the panel.
    Open,
    /// Get the sound with this key, then do `then` with it.
    Use(SoundKey, Then),
}

fn mailbox() -> Id {
    Id::new("sounds mailbox")
}

/// Leaves `ask` for the app to take.
pub fn ask(ctx: &Context, ask: Ask) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<Ask>>(mailbox()).push(ask));
    ctx.request_repaint();
}

/// Takes what views have asked since the last call.
pub fn take_asks(ctx: &Context) -> Vec<Ask> {
    ctx.data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<Vec<Ask>>(mailbox())))
}

/// A sound that has arrived, or failed to.
pub struct Done {
    pub entry: Entry,
    pub then: Then,
    pub result: Result<PathBuf, String>,
}

/// What a fetch job brings back.
struct Fetched {
    path: PathBuf,
    /// Decoded, for an audition (done on the worker, not the UI thread).
    sample: Option<Arc<Sample>>,
}

struct Fetch {
    entry: Entry,
    then: Then,
    job: Job<Result<Fetched, String>>,
}

/// Whether a source answered the last time it was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    Asking,
    Reachable,
    Unreachable(String),
}

/// The sound playing in the preview player.
struct Playing {
    name: String,
    preview: Option<noodle_io::Preview>,
    /// When it ends, for a sound played on the virtual output, which plays
    /// nothing to ask.
    until: Instant,
}

type Catalogues = Vec<(SourceId, Result<Vec<Entry>, String>)>;

pub struct Sounds {
    open: bool,
    net: Net,
    cache: Option<Cache>,
    /// Where each source's last catalogue is kept, so a source that can't be
    /// reached still lists what it had.
    catalogues: Option<PathBuf>,
    sources: Vec<Source>,
    reach: std::collections::HashMap<SourceId, Reach>,
    entries: Vec<Entry>,
    /// The hashes of the sounds on this computer.
    cached: HashSet<String>,
    refreshing: Option<Job<Catalogues>>,
    asked: bool,
    filter: Filter,
    fetching: Vec<Fetch>,
    /// A download waiting for the user to agree to its size.
    confirm: Option<(Entry, Then)>,
    playing: Option<Playing>,
    message: Option<String>,
    /// What has arrived and is not for the panel to use.
    arrived: Vec<Done>,
    /// Where the tokens and passwords of sources are kept.
    vault: Box<dyn Vault>,
    /// The size of each downloaded sound, by hash.
    sizes: std::collections::HashMap<String, u64>,
    /// What Clean up would remove, until something changes it.
    cleanable: Option<page::Cleanable>,
    /// A source was added while the lists were being fetched: ask again.
    again: bool,
    /// The panel asked for the Sources page; the app opens it.
    wants_sources: bool,
    /// The Add a source form on the Sources page.
    form: page::Form,
    /// What the Sources page last did, for the user to read.
    note: Option<String>,
}

impl Default for Sounds {
    fn default() -> Self {
        let dirs = directories::ProjectDirs::from("", "", "Noodle");
        Self::with(
            Net::http(),
            dirs.map(|d| d.data_dir().join("sounds")),
            Self::saved_sources(&vault::System),
        )
    }
}

impl Sounds {
    /// The built-in source and the ones the user added, with their secrets.
    fn saved_sources(vault: &dyn Vault) -> Vec<Source> {
        let mut sources = vec![Source::built_in()];
        for spec in crate::prefs::sources() {
            let secret = vault.recall(&spec.id);
            sources.push(Source::from_spec(spec, secret));
        }
        sources
    }

    /// A panel over `net`, keeping sounds and catalogues in `dir`.
    pub fn with(net: Net, dir: Option<PathBuf>, sources: Vec<Source>) -> Self {
        let cache = dir.as_ref().map(|d| Cache::new(d.join("objects")));
        let catalogues = dir.map(|d| d.join("catalogues"));
        let mut sounds = Self {
            open: false,
            net,
            cache,
            catalogues,
            reach: sources
                .iter()
                .map(|s| (s.id.clone(), Reach::Asking))
                .collect(),
            sources,
            entries: Vec::new(),
            cached: HashSet::new(),
            refreshing: None,
            asked: false,
            filter: Filter::default(),
            fetching: Vec::new(),
            confirm: None,
            playing: None,
            message: None,
            arrived: Vec::new(),
            vault: Box::new(vault::System),
            sizes: Default::default(),
            cleanable: None,
            again: false,
            wants_sources: false,
            form: Default::default(),
            note: None,
        };
        sounds.load_saved();
        sounds
    }

    /// Keeps secrets somewhere else, for tests.
    #[cfg(test)]
    fn with_vault(mut self, vault: impl Vault + 'static) -> Self {
        self.vault = Box::new(vault);
        self
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether a worker is still fetching a list or a sound.
    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.refreshing.is_some() || !self.fetching.is_empty()
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    /// What the panel lists, before the search.
    #[cfg(test)]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    #[cfg(test)]
    pub fn reach(&self, source: &str) -> Option<&Reach> {
        self.reach.get(source)
    }

    /// Whether the sound is on this computer.
    pub fn is_cached(&self, key: &str) -> bool {
        self.cached.contains(key)
    }

    /// The entry a carried sound is.
    pub fn entry(&self, key: SoundKey) -> Option<&Entry> {
        let hash = key.hash();
        self.entries.iter().find(|e| e.key == hash)
    }

    /// The name of the sound playing, if one is.
    #[cfg(test)]
    pub fn playing(&self) -> Option<&str> {
        self.playing.as_ref().map(|p| p.name.as_str())
    }

    pub fn stop(&mut self) {
        self.playing = None;
    }

    /// Asks for the sound a carried `key` names.
    pub fn use_key(&mut self, key: SoundKey, then: Then) {
        match self.entry(key).cloned() {
            Some(entry) => self.request(entry, then),
            None => self.message = Some("That sound isn't in any source".into()),
        }
    }

    fn source(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id)
    }

    fn catalogue_file(&self, source: &str) -> Option<PathBuf> {
        let name: String = source
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        self.catalogues
            .as_ref()
            .map(|d| d.join(format!("{name}.json")))
    }

    /// Reads the catalogues kept from last time, and which sounds are here.
    fn load_saved(&mut self) {
        for source in self.sources.clone() {
            let saved = self
                .catalogue_file(&source.id)
                .and_then(|f| std::fs::read_to_string(f).ok())
                .and_then(|text| serde_json::from_str::<Vec<Entry>>(&text).ok());
            self.entries.extend(saved.unwrap_or_default());
        }
        self.scan_cache();
    }

    /// Works out which sounds are here: downloaded into the cache, or in a
    /// folder source that was reachable the last time it was scanned.
    fn scan_cache(&mut self) {
        self.cleanable = None;
        self.cached.clear();
        self.sizes.clear();
        for entry in &self.entries {
            let in_folder = entry.file.is_some()
                && matches!(self.reach.get(&entry.source), Some(Reach::Reachable));
            if in_folder {
                self.cached.insert(entry.key.clone());
            } else if let Some(cache) = &self.cache
                && let Ok(meta) = std::fs::metadata(cache.path(&entry.key))
            {
                self.cached.insert(entry.key.clone());
                self.sizes.insert(entry.key.clone(), meta.len());
            }
        }
    }

    /// Asks every source for its catalogue, on a worker.
    pub fn refresh(&mut self) {
        if self.refreshing.is_some() {
            self.again = true;
            return;
        }
        self.asked = true;
        for reach in self.reach.values_mut() {
            *reach = Reach::Asking;
        }
        let net = self.net.clone();
        // A folder's last scan, so a file that hasn't changed isn't decoded again.
        let work: Vec<(Source, Vec<Entry>)> = self
            .sources
            .iter()
            .map(|s| {
                let known = if s.folder.is_some() {
                    self.entries
                        .iter()
                        .filter(|e| e.source == s.id)
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };
                (s.clone(), known)
            })
            .collect();
        self.refreshing = Some(Job::spawn(move |_| {
            work.iter()
                .map(|(s, known)| (s.id.clone(), source::fetch_catalogue(&net, s, known)))
                .collect()
        }));
    }

    /// Asks for the sound `entry` and says what to do with it. A sound that is
    /// not here and is big asks first.
    pub fn request(&mut self, entry: Entry, then: Then) {
        let in_folder = self
            .source(&entry.source)
            .is_some_and(|s| s.folder.is_some());
        if self.cache.is_none() && !in_folder {
            self.message = Some("There is nowhere to keep sounds on this computer".into());
            return;
        }
        let big = entry.bytes.is_some_and(|b| b > CONFIRM_BYTES);
        if big && !self.is_cached(&entry.key) {
            self.confirm = Some((entry, then));
            return;
        }
        self.start(entry, then);
    }

    fn start(&mut self, entry: Entry, then: Then) {
        let Some(source) = self.source(&entry.source).cloned() else {
            self.message = Some("That sound's source isn't set up".into());
            return;
        };
        let cache = self.cache.clone();
        let net = self.net.clone();
        let hash = entry.key.clone();
        let local = entry.clone();
        let audition = then == Then::Audition;
        let job = Job::spawn(move |progress| {
            let path = if source.folder.is_some() {
                // Read where it is: nothing is copied.
                source::open_local(&local)?
            } else {
                let cache = cache.ok_or("There is nowhere to keep sounds on this computer")?;
                source::download(&net, &cache, &source, &hash, &mut |f| {
                    progress.report(f).is_ok()
                })?
            };
            let sample = if audition {
                let audio = noodle_io::decode_file(&path).map_err(|e| e.to_string())?;
                Some(Arc::new(Sample::from_audio(&audio)))
            } else {
                None
            };
            Ok(Fetched { path, sample })
        });
        self.fetching.push(Fetch { entry, then, job });
    }

    /// Takes in what has arrived, refreshes, and plays. Call it every frame;
    /// what is returned is for the app to use.
    pub fn poll(&mut self, session: &Session) -> Vec<Done> {
        if let Some(job) = &mut self.refreshing
            && let Some(done) = job.poll()
        {
            self.refreshing = None;
            match done {
                Ok(catalogues) => self.take_catalogues(catalogues),
                Err(panicked) => self.message = Some(panicked.to_string()),
            }
            if std::mem::take(&mut self.again) {
                self.refresh();
            }
        }
        let mut i = 0;
        while i < self.fetching.len() {
            let Some(done) = self.fetching[i].job.poll() else {
                i += 1;
                continue;
            };
            let Fetch { entry, then, .. } = self.fetching.remove(i);
            let result = done.map_err(|p| p.to_string()).and_then(|r| r);
            match result {
                Ok(fetched) => {
                    self.cached.insert(entry.key.clone());
                    if let Ok(meta) = std::fs::metadata(&fetched.path)
                        && self
                            .source(&entry.source)
                            .is_some_and(|s| s.folder.is_none())
                    {
                        self.sizes.insert(entry.key.clone(), meta.len());
                        self.cleanable = None;
                    }
                    if then == Then::Audition {
                        self.audition(session, &entry, fetched.sample.as_deref());
                    } else {
                        self.arrived.push(Done {
                            entry,
                            then,
                            result: Ok(fetched.path),
                        });
                    }
                }
                Err(why) if then == Then::Audition => {
                    self.message = Some(format!("Couldn't get {}: {why}", entry.name));
                }
                Err(why) => self.arrived.push(Done {
                    entry,
                    then,
                    result: Err(why),
                }),
            }
        }
        if self.playing.as_ref().is_some_and(|p| {
            p.preview
                .as_ref()
                .map_or(Instant::now() >= p.until, |v| v.finished())
        }) {
            self.playing = None;
        }
        std::mem::take(&mut self.arrived)
    }

    fn audition(&mut self, session: &Session, entry: &Entry, sample: Option<&Sample>) {
        let Some(sample) = sample else {
            return;
        };
        // One at a time: a new one replaces the one playing.
        self.playing = None;
        let preview = match session.play_preview(sample) {
            Ok(preview) => preview,
            Err(why) => {
                self.message = Some(format!("Can't play {}: {why}", entry.name));
                return;
            }
        };
        let seconds = sample.frames() as f32 / sample.sample_rate.max(1) as f32;
        self.message = None;
        self.playing = Some(Playing {
            name: entry.name.clone(),
            preview,
            until: Instant::now() + Duration::from_secs_f32(seconds.min(30.0)),
        });
    }

    fn take_catalogues(&mut self, catalogues: Catalogues) {
        let mut entries = Vec::new();
        for (id, result) in catalogues {
            // A source removed while it was being asked is not listed again.
            if self.source(&id).is_none() {
                continue;
            }
            match result {
                Ok(found) => {
                    if let Some(file) = self.catalogue_file(&id) {
                        if let Some(dir) = file.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        if let Ok(text) = serde_json::to_string(&found) {
                            let _ = std::fs::write(file, text);
                        }
                    }
                    self.reach.insert(id, Reach::Reachable);
                    entries.extend(found);
                }
                Err(why) => {
                    // What it had last time stays listed, greyed.
                    entries.extend(self.entries.iter().filter(|e| e.source == id).cloned());
                    self.reach.insert(id, Reach::Unreachable(why));
                }
            }
        }
        self.entries = entries;
        self.scan_cache();
    }

    /// Shows the panel, if it is open, and answers what the views asked.
    pub fn show(&mut self, ctx: &Context) {
        for ask in take_asks(ctx) {
            match ask {
                Ask::Open => self.open = true,
                Ask::Use(key, then) => self.use_key(key, then),
            }
        }
        if self.open && !self.asked {
            self.refresh();
        }
        let busy = self.refreshing.is_some() || !self.fetching.is_empty() || self.playing.is_some();
        if busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.open {
            panel::show(self, ctx);
        }
    }
}

#[cfg(test)]
mod tests;
