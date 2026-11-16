//! Library entries in a pack: group templates and presets (ADR 0012, extended).
//!
//! An entry is a small RON file, so unlike audio it lives in git beside the
//! pack definition and is published as `items/<sha256>.ron`. Its name is the
//! SHA-256 of its bytes, so it is as immutable as a sample and a download is
//! checked the same way.

use std::collections::BTreeSet;
use std::fs;

use noodle_core::library::{Item, Kind};
use noodle_nodes::sample_hashes;
use noodle_packs::{ItemKind, is_hash};
use sha2::{Digest, Sha256};

use crate::{FoundPack, PublishError, io};

/// The SHA-256 of `bytes`: 64 lowercase hex digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The pack kind of a library item.
pub fn kind_of(item: &Item) -> ItemKind {
    match item.kind() {
        Kind::Template => ItemKind::Template,
        Kind::NodePreset => ItemKind::NodePreset,
        Kind::GroupPreset => ItemKind::GroupPreset,
    }
}

/// The samples an entry names, by hash, each once and in order.
pub fn item_samples(item: &Item) -> Vec<String> {
    let found: BTreeSet<String> = item
        .config_texts()
        .iter()
        .flat_map(|text| sample_hashes(text))
        .filter(|h| is_hash(h))
        .collect();
    found.into_iter().collect()
}

/// Reads an entry's RON and checks it is what `kind` says and holds nothing
/// that would not load. A published entry carries no slug and no origin: the
/// store it is installed into gives the first, and the install sets the
/// second.
pub fn parse_item(bytes: &[u8], kind: ItemKind) -> Result<Item, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())?;
    let item = Item::from_ron(text).map_err(|e| format!("not a library entry: {e}"))?;
    if kind_of(&item) != kind {
        return Err(format!(
            "it is a {:?}, not a {kind:?}",
            kind_of(&item)
        ));
    }
    if let Item::Template(t) = &item {
        t.validate().map_err(|e| e.to_string())?;
        if t.hidden {
            return Err("a hidden template is not shareable".into());
        }
    }
    Ok(item)
}

/// The RON a pack publishes for `item`: without the slug or the origin, so
/// the same entry always hashes the same.
pub fn item_file(item: &Item) -> String {
    let mut item = item.clone();
    item.set_slug(String::new());
    item.set_origin(None);
    item.to_ron()
}

/// Reads and checks the file of item `index` of `pack`: it hashes to the
/// definition's hash, parses as the kind it names, and every sample it uses
/// is one it declares in `needs`. Returns the bytes.
pub(crate) fn load_item(pack: &FoundPack, index: usize) -> Result<Vec<u8>, PublishError> {
    let def = &pack.def.items[index];
    let path = pack.dir.join("pack.ron");
    let rel = def.file.as_deref().ok_or_else(|| PublishError::NoFile {
        path: path.clone(),
        name: def.name.clone(),
    })?;
    let file = pack.dir.join(rel);
    let bytes = fs::read(&file).map_err(io(&file))?;
    let actual = sha256_hex(&bytes);
    if actual != def.hash {
        return Err(PublishError::HashMismatch {
            path: file,
            name: def.name.clone(),
            expected: def.hash.clone(),
            actual,
        });
    }
    let item = parse_item(&bytes, def.kind).map_err(|message| PublishError::BadItem {
        path: file.clone(),
        name: def.name.clone(),
        message,
    })?;
    let undeclared: Vec<String> = item_samples(&item)
        .into_iter()
        .filter(|h| !def.needs.contains(h))
        .collect();
    if !undeclared.is_empty() {
        return Err(PublishError::BadItem {
            path: file,
            name: def.name.clone(),
            message: format!("uses samples it does not list in `needs`: {}", undeclared.join(", ")),
        });
    }
    Ok(bytes)
}
