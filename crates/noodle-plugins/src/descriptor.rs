//! What a plugin file offers, as the scanner reports it. The node types are
//! built from these, so a project can be opened and its graph compiled
//! without loading the plugins' code.

use std::path::PathBuf;

use noodle_engine::Config;
use serde::{Deserialize, Serialize};

/// The plugin standard a file follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginFormat {
    #[default]
    Clap,
    Vst3,
    /// An Audio Unit (macOS only). Listed as a virtual file, see `au/`.
    Au,
}

impl PluginFormat {
    /// The word for it in node IDs and the plugin browser.
    pub fn name(self) -> &'static str {
        match self {
            PluginFormat::Clap => "CLAP",
            PluginFormat::Vst3 => "VST3",
            PluginFormat::Au => "AU",
        }
    }

    /// The prefix of node type IDs of this format's plugins.
    pub fn node_prefix(self) -> &'static str {
        match self {
            PluginFormat::Clap => "clap.",
            PluginFormat::Vst3 => "vst3.",
            PluginFormat::Au => "au.",
        }
    }

    /// The format a plugin file is in, from its extension.
    pub fn of_path(path: &std::path::Path) -> Self {
        if crate::au::is_component_path(path) {
            PluginFormat::Au
        } else if path.extension().is_some_and(|ext| ext == "vst3") {
            PluginFormat::Vst3
        } else {
            PluginFormat::Clap
        }
    }
}

/// One plugin in a file. A CLAP file can hold several, and so can a VST3
/// bundle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    /// Absent in scans saved before VST3 was supported, which are all CLAP.
    #[serde(default)]
    pub format: PluginFormat,
    /// The plugin's own ID, e.g. `com.u-he.diva` for CLAP; for VST3 the class
    /// ID as 32 hex digits.
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
    /// Why scanning failed (the file crashed, hung or isn't a plugin).
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

/// A plugin's size and modification time in whole seconds. (For an Audio
/// Unit, zero and its version number.) For a macOS
/// bundle (a folder), the total size and the newest modification time of
/// everything inside: replacing the binary in place doesn't touch the
/// folder's own time.
pub fn file_stamp(path: &std::path::Path) -> Option<(u64, u64)> {
    // An Audio Unit is no file: its stamp is its version.
    if crate::au::is_component_path(path) {
        return crate::au::stamp(path);
    }
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
        format!("{}{}", self.format.node_prefix(), self.id)
    }
}

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

/// A plugin with at most this many parameters shows all of them on a new
/// node; one with more shows none, and the user opts in to the ones they
/// want (a synth can have hundreds, which makes a node too tall to use).
/// See [`initial_params`].
pub const DEFAULT_EXPOSED_LIMIT: usize = 4;

/// The `params` setting a newly added node of this plugin starts with, if it
/// needs one. A node without the setting shows every parameter, which is what
/// projects saved before the setting existed rely on.
pub fn initial_params(descriptor: &Descriptor) -> Option<String> {
    (descriptor.params.len() > DEFAULT_EXPOSED_LIMIT).then(|| params_text(&[]))
}

/// The ids of the parameters chosen in `config`, if the user chose any.
/// The setting is a comma-separated list of ids; `none` means an empty choice
/// (an empty setting means no choice yet).
pub fn chosen_params(config: &Config) -> Option<Vec<u32>> {
    let Some(noodle_engine::Value::Text(text)) = config.get(crate::PARAMS_KEY) else {
        return None;
    };
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(
        text.split(',')
            .filter_map(|id| id.trim().parse().ok())
            .collect(),
    )
}

/// The parameters that get a port on the node, in the plugin's order: the
/// chosen ones, or all of them if nothing was chosen.
pub fn exposed_params<'a>(descriptor: &'a Descriptor, config: &Config) -> Vec<&'a ParamDesc> {
    match chosen_params(config) {
        Some(ids) => descriptor
            .params
            .iter()
            .filter(|p| ids.contains(&p.id))
            .collect(),
        None => descriptor.params.iter().collect(),
    }
}

/// The setting's text for a choice of parameter ids.
pub fn params_text(ids: &[u32]) -> String {
    if ids.is_empty() {
        "none".to_owned()
    } else {
        ids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(format: PluginFormat, params: usize) -> Descriptor {
        Descriptor {
            format,
            id: "test".into(),
            name: "Test".into(),
            vendor: String::new(),
            instrument: false,
            audio_inputs: Vec::new(),
            audio_outputs: Vec::new(),
            note_inputs: 0,
            note_outputs: 0,
            params: (0..params as u32)
                .map(|id| ParamDesc {
                    id,
                    name: format!("p{id}"),
                    module: String::new(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    stepped: false,
                })
                .collect(),
        }
    }

    #[test]
    fn parameters_are_opt_in_from_five_up_for_both_formats() {
        for format in [PluginFormat::Clap, PluginFormat::Vst3] {
            for count in [0, 1, 4] {
                assert_eq!(initial_params(&descriptor(format, count)), None);
            }
            for count in [5, 6, 300] {
                let d = descriptor(format, count);
                let initial = initial_params(&d).expect("starts with a choice");
                let config =
                    Config::new().with(crate::PARAMS_KEY, noodle_engine::Value::Text(initial));
                assert!(exposed_params(&d, &config).is_empty());
            }
        }
    }
}
