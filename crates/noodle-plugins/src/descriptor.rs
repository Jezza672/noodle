//! What a plugin file offers, as the scanner reports it. The node types are
//! built from these, so a project can be opened and its graph compiled
//! without loading the plugins' code.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One plugin in a file. A CLAP file can hold several.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    /// The plugin's own ID, e.g. `com.u-he.diva`.
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// Whether it lists the `instrument` feature.
    pub instrument: bool,
    pub audio_inputs: Vec<AudioPort>,
    pub audio_outputs: Vec<AudioPort>,
    /// How many note input and output ports it has.
    pub note_inputs: u32,
    pub note_outputs: u32,
    /// The parameters a user can set: hidden and read-only ones are left out.
    pub params: Vec<ParamDesc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioPort {
    pub name: String,
    pub channels: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamDesc {
    /// Stable, chosen by the plugin; the port key is derived from it.
    pub id: u32,
    pub name: String,
    /// Where the plugin groups it, e.g. `Filter` for `Filter/Cutoff`.
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub stepped: bool,
}

/// What scanning one file found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginFile {
    pub path: PathBuf,
    /// The file's size and modification time (seconds since the epoch) when
    /// it was scanned, to tell when a scan is out of date.
    pub len: u64,
    pub modified: u64,
    pub plugins: Vec<Descriptor>,
    /// Why scanning failed (the file crashed, hung or isn't a CLAP plugin).
    /// A failed file isn't scanned again until it changes.
    #[serde(default)]
    pub error: Option<String>,
}

impl PluginFile {
    /// Whether the file on disk is still the one this scan saw.
    pub fn is_current(&self) -> bool {
        file_stamp(&self.path).is_some_and(|stamp| stamp == (self.len, self.modified))
    }
}

/// A plugin's size and modification time in whole seconds. For a macOS
/// bundle (a folder), the total size and the newest modification time of
/// everything inside: replacing the binary in place doesn't touch the
/// folder's own time.
pub fn file_stamp(path: &std::path::Path) -> Option<(u64, u64)> {
    fn stamp(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
        let modified = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some((meta.len(), modified))
    }
    fn walk(dir: &std::path::Path, depth: usize, total: &mut (u64, u64)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if let Some((len, modified)) = stamp(&meta) {
                total.0 += len;
                total.1 = total.1.max(modified);
            }
            if meta.is_dir() && depth < 6 {
                walk(&entry.path(), depth + 1, total);
            }
        }
    }
    let meta = std::fs::metadata(path).ok()?;
    let mut total = stamp(&meta)?;
    if meta.is_dir() {
        walk(path, 0, &mut total);
    }
    Some(total)
}

impl Descriptor {
    /// The node type ID for this plugin.
    pub fn node_id(&self) -> String {
        format!("{NODE_PREFIX}{}", self.id)
    }
}

/// Node type IDs of plugins start with this, then the plugin's own ID.
pub const NODE_PREFIX: &str = "clap.";

/// The port key of the `index`th audio input (`in`, `in2`, ...).
pub fn input_key(index: usize) -> String {
    numbered("in", index)
}

pub fn output_key(index: usize) -> String {
    numbered("out", index)
}

pub fn note_input_key(index: usize) -> String {
    numbered("notes", index)
}

pub fn note_output_key(index: usize) -> String {
    numbered("notes_out", index)
}

fn numbered(base: &str, index: usize) -> String {
    if index == 0 {
        base.to_owned()
    } else {
        format!("{base}{}", index + 1)
    }
}

/// The port key of a parameter.
pub fn param_key(id: u32) -> String {
    format!("p{id}")
}
