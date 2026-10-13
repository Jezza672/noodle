//! The library: group templates and presets, saved for reuse.
//!
//! - A [`GroupTemplate`] is a group with its contents, saved so it can be
//!   added again. Using one puts an *unlinked copy* in the project (the copy
//!   remembers its source in [`Node::template`] and nothing more), so editing
//!   or upgrading a template never changes a project.
//! - A [`NodePreset`] is one node's parameter values and config, applicable
//!   to a node of the same type and port layout.
//! - A [`GroupPreset`] is the values of every node inside a group, applicable
//!   to a group with the same [topology](topology).
//!
//! Applying a preset sets stored values only, as one undo step; it never
//! touches wires, automation lanes, mute or solo.
//!
//! Entries live in a [`Library`] with two stores: the system one is embedded
//! in the app and read-only, the user one is a folder of `.ron` files, one
//! per entry, which can be shared by copying them.
//!
//! Port layouts come from the node types, which this crate doesn't know, so
//! the functions that compare them take a [`LayoutOf`] that gives a node's
//! layout as a string (equal strings mean equal ports).
//!
//! A track is a *hidden* template ([`track_template`]): the library doesn't
//! list it and the Add menu doesn't offer it. Only
//! [`create_track`](crate::group::create_track) instantiates it.

mod presets;
mod store;
mod template;
pub mod topology;

use serde::{Deserialize, Serialize};

use crate::Node;

pub use presets::{GroupPreset, NodePreset, PresetError, PresetValues};
pub use store::{Library, SYSTEM, Stored, USER};
pub use template::{GroupTemplate, TRACK, TemplateError, track_template};

/// A node's port layout as a string: equal strings mean the same ports.
/// `None` for a node whose type is unknown.
pub type LayoutOf<'a> = &'a dyn Fn(&Node) -> Option<String>;

/// What a library file holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Item {
    Template(GroupTemplate),
    NodePreset(NodePreset),
    GroupPreset(GroupPreset),
}

/// The three kinds of entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Template,
    NodePreset,
    GroupPreset,
}

impl Kind {
    /// The kind's part of an entry ID and file name.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Template => "template",
            Kind::NodePreset => "node",
            Kind::GroupPreset => "group",
        }
    }
}

impl Item {
    pub fn kind(&self) -> Kind {
        match self {
            Item::Template(_) => Kind::Template,
            Item::NodePreset(_) => Kind::NodePreset,
            Item::GroupPreset(_) => Kind::GroupPreset,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Item::Template(t) => &t.name,
            Item::NodePreset(p) => &p.name,
            Item::GroupPreset(p) => &p.name,
        }
    }

    pub(crate) fn slug(&self) -> &str {
        match self {
            Item::Template(t) => &t.slug,
            Item::NodePreset(p) => &p.slug,
            Item::GroupPreset(p) => &p.slug,
        }
    }

    pub(crate) fn set_slug(&mut self, slug: String) {
        match self {
            Item::Template(t) => t.slug = slug,
            Item::NodePreset(p) => p.slug = slug,
            Item::GroupPreset(p) => p.slug = slug,
        }
    }

    /// Whether the library lists this entry.
    pub fn hidden(&self) -> bool {
        matches!(self, Item::Template(t) if t.hidden)
    }

    /// RON, as a library file holds it.
    pub fn to_ron(&self) -> String {
        let pretty = ron::ser::PrettyConfig::default().depth_limit(4);
        ron::ser::to_string_pretty(self, pretty).expect("library entries always serialize")
    }

    pub fn from_ron(text: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(text)
    }
}

/// A system library file: a list of entries, as RON.
pub fn pack_to_ron(items: &[Item]) -> String {
    let pretty = ron::ser::PrettyConfig::default().depth_limit(4);
    ron::ser::to_string_pretty(items, pretty).expect("library entries always serialize")
}

/// A file name and entry ID friendly form of `name`: lower case letters and
/// digits, with single dashes between words.
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "untitled".into()
    } else {
        slug
    }
}

/// 64-bit FNV-1a. A fixed, specified hash, because topology keys are saved
/// in preset files and must not change between releases.
pub(crate) fn fnv(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &byte in *part {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // A separator, so ("ab", "c") and ("a", "bc") differ.
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests;
