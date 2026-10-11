//! The plugins a program knows about, and how they join a node registry.

use std::collections::HashSet;
use std::sync::Arc;

use noodle_engine::{NodeType, Registry};

use crate::descriptor::PluginFile;
use crate::node::{PluginNodeType, Shared};
use crate::scan::{ScanCache, ScanCommand, find_plugin_files, standard_dirs};

/// A set of scanned plugins, ready to be registered as node types.
#[derive(Clone, Default)]
pub struct Catalog {
    types: Vec<Arc<PluginNodeType>>,
}

/// A function that adds the catalog's node types to a registry; the type of
/// `RenderRequest::extend_registry` in `noodle-nodes`.
pub type ExtendRegistry = Arc<dyn Fn(&mut Registry) + Send + Sync>;

impl Catalog {
    /// Builds a node type for every plugin in `files`. If two files hold the
    /// same plugin ID, the first wins.
    pub fn new(files: &[PluginFile]) -> Self {
        let mut seen = HashSet::new();
        let mut types = Vec::new();
        for file in files.iter().filter(|f| f.error.is_none()) {
            for plugin in &file.plugins {
                if seen.insert(plugin.node_id()) {
                    types.push(Arc::new(PluginNodeType::new(
                        file.path.clone(),
                        plugin.clone(),
                    )));
                }
            }
        }
        Self { types }
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PluginNodeType> {
        self.types.iter().map(|t| &**t)
    }

    /// The plugin whose node type is `node_type_id` (`clap.<plugin id>`).
    pub fn get(&self, node_type_id: &str) -> Option<&PluginNodeType> {
        self.iter().find(|t| t.info().id == node_type_id)
    }

    /// Adds a node type for each plugin, replacing any with the same ID.
    pub fn register(&self, registry: &mut Registry) {
        self.register_as(registry, false);
    }

    fn register_as(&self, registry: &mut Registry, patient: bool) {
        for node_type in &self.types {
            registry.replace(Shared(Arc::clone(node_type), patient));
        }
    }

    /// A function for `RenderRequest::extend_registry`, so freezes, exports
    /// and background renders see the same plugins as live playback.
    pub fn extender(&self) -> ExtendRegistry {
        let catalog = self.clone();
        // Renders aren't against the clock: a plugin in a host process may
        // take as long as it likes per block.
        Arc::new(move |registry| catalog.register_as(registry, true))
    }
}

/// What a look at the machine's plugins found.
pub struct Discovery {
    pub catalog: Catalog,
    /// Every file looked at, including ones that failed to scan.
    pub files: Vec<PluginFile>,
    /// How many files were scanned afresh (the rest came from the cache).
    pub scanned: usize,
}

/// Looks in `dirs` for plugin files and scans the new or changed ones with
/// `command`, keeping results in the cache file at `cache_path`.
pub fn discover_in(
    dirs: &[std::path::PathBuf],
    command: &ScanCommand,
    cache_path: Option<&std::path::Path>,
) -> Discovery {
    discover_in_with(dirs, command, cache_path, false)
}

/// [`discover_in`], optionally ignoring what the cache holds so every file is
/// scanned again (a plugin that failed to scan, or was fixed in place).
pub fn discover_in_with(
    dirs: &[std::path::PathBuf],
    command: &ScanCommand,
    cache_path: Option<&std::path::Path>,
    everything: bool,
) -> Discovery {
    let mut cache = if everything {
        ScanCache::default()
    } else {
        cache_path.map(ScanCache::load).unwrap_or_default()
    };
    let found = find_plugin_files(dirs);
    let scanned = cache.refresh(command, &found);
    if let (Some(path), true) = (cache_path, scanned > 0 || cache.files.is_empty()) {
        // A cache that can't be saved only means scanning again next time.
        let _ = cache.save(path);
    }
    Discovery {
        catalog: Catalog::new(&cache.files),
        files: cache.files,
        scanned,
    }
}

/// [`discover_in`] over the standard folders, with the scanner being this
/// program (see [`run_scan_child_if_requested`](crate::run_scan_child_if_requested))
/// and the cache in the user's cache folder. Without a way to start the
/// scanner, nothing new is scanned and the cache is all there is.
pub fn discover() -> Discovery {
    discover_with(false)
}

/// [`discover`], optionally scanning every file again.
pub fn discover_with(everything: bool) -> Discovery {
    let cache_path = ScanCache::default_path();
    match ScanCommand::current_exe() {
        Ok(command) => discover_in_with(
            &standard_dirs(),
            &command,
            cache_path.as_deref(),
            everything,
        ),
        Err(_) => {
            let cache = cache_path
                .as_deref()
                .map(ScanCache::load)
                .unwrap_or_default();
            Discovery {
                catalog: Catalog::new(&cache.files),
                files: cache.files,
                scanned: 0,
            }
        }
    }
}
