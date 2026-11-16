//! The library as the views use it: presets that fit a node or group, the
//! group templates, and saving to the user's store.

use noodle_core::library::{self, GroupPreset, GroupTemplate, Item, NodePreset};
use noodle_core::{Node, NodeId};

use super::{Edit, Session};

/// A save to the user's library (not a change to the project).
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryEdit {
    /// Saves the group as a template.
    Template { group: NodeId, name: String },
    /// Saves a node's values as a preset; `overwrite` is the user preset ID
    /// to replace instead of adding a new one.
    NodePreset {
        node: NodeId,
        name: String,
        overwrite: Option<String>,
    },
    /// Saves every setting inside a group as a preset.
    GroupPreset {
        group: NodeId,
        name: String,
        overwrite: Option<String>,
    },
}

/// What is applied to a node or group, for the Presets control.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetStatus {
    /// The entry ID.
    pub id: String,
    pub name: String,
    /// Edited since it was applied.
    pub modified: bool,
    /// A user preset, which Save can overwrite.
    pub user: bool,
}

impl Session {
    pub fn library(&self) -> &library::Library {
        &self.library
    }

    /// Replaces the library, e.g. with one that keeps user entries on disk.
    pub fn set_library(&mut self, library: library::Library) {
        if let Some(first) = library.problems.first() {
            let more = library.problems.len() - 1;
            self.message = Some(match more {
                0 => format!("Library: {first}"),
                n => format!("Library: {first} (and {n} more files couldn't be read)"),
            });
        }
        self.library = library;
    }

    /// The hashes of the entries in the user's library that came from a pack.
    pub fn installed_hashes(&self) -> std::collections::HashSet<String> {
        self.library
            .all()
            .filter(|e| !e.is_system())
            .filter_map(|e| e.item.origin().map(|o| o.hash.clone()))
            .collect()
    }

    /// Adds an entry that came from a pack to the user's library. Not an undo
    /// step, like any save to the library.
    pub fn install_library_item(&mut self, mut item: Item, origin: library::Origin) {
        let name = item.name().to_string();
        if self.library.installed(&origin.hash).is_some() {
            self.message = Some(format!("{name} is already in your library"));
            return;
        }
        item.set_origin(Some(origin));
        self.message = Some(match self.library.save(item) {
            Ok(_) => format!("Added {name} to your library"),
            Err(e) => format!("Couldn't save {name} to the library: {e}"),
        });
    }

    /// A node's port layout, as presets compare it.
    fn layout_of(&self) -> impl Fn(&Node) -> Option<String> + '_ {
        |node| self.registry.layout_signature(node)
    }

    /// The presets that fit a node (same type and ports) or group (same
    /// topology), with their IDs: system ones first.
    pub fn presets_for(&self, id: NodeId) -> Vec<(&str, &str)> {
        let graph = self.project.graph();
        let Some(node) = graph.node(id) else {
            return Vec::new();
        };
        let layout = self.layout_of();
        if node.type_id == noodle_core::group::GROUP {
            // Worked out once, not once per preset: this runs every frame.
            let order = noodle_core::library::topology::topology(graph, id);
            self.library
                .group_presets()
                .filter(|(_, p)| p.fits(graph, id, &order, &layout))
                .map(|(entry, p)| (entry, p.name.as_str()))
                .collect()
        } else {
            self.library
                .node_presets()
                .filter(|(_, p)| p.applies_to(node, &layout))
                .map(|(entry, p)| (entry, p.name.as_str()))
                .collect()
        }
    }

    /// The node presets made for node type `type_id`, with their IDs: system
    /// ones first. They fit a node of that type with its default ports.
    pub fn presets_for_type(&self, type_id: &str) -> Vec<(&str, &str)> {
        self.library
            .node_presets()
            .filter(|(_, p)| p.node_type == type_id)
            .map(|(entry, p)| (entry, p.name.as_str()))
            .collect()
    }

    /// The group presets made for the group that library template `entry`
    /// makes: those with its topology.
    pub fn presets_for_template(&self, entry: &str) -> Vec<(&str, &str)> {
        let Some(template) = self.library.template(entry) else {
            return Vec::new();
        };
        let graph = template.graph();
        let Some(root) = graph
            .nodes()
            .find(|(_, n)| n.parent.is_none())
            .map(|(id, _)| id)
        else {
            return Vec::new();
        };
        let key = noodle_core::library::topology::topology(&graph, root).key;
        self.library
            .group_presets()
            .filter(|(_, p)| p.topology == key)
            .map(|(entry, p)| (entry, p.name.as_str()))
            .collect()
    }

    /// The command that applies library preset `entry` to `id`, if it fits.
    pub fn apply_preset(&self, id: NodeId, entry: &str) -> Option<Edit> {
        let graph = self.project.graph();
        let layout = self.layout_of();
        let node = graph.node(id)?;
        let command = if node.type_id == noodle_core::group::GROUP {
            self.library
                .group_preset(entry)?
                .apply(graph, id, entry, &layout)
        } else {
            self.library
                .node_preset(entry)?
                .apply(graph, id, entry, &layout)
        };
        command.ok().map(Edit::Apply)
    }

    /// The command for [`Edit::MorphPresets`]. Says in the status bar what it
    /// wired and what it left out.
    pub(super) fn morph_presets(
        &mut self,
        group: NodeId,
        presets: &[String],
    ) -> Result<noodle_core::Command, String> {
        let found = presets
            .iter()
            .map(|id| self.library.group_preset(id).ok_or("a preset is gone"))
            .collect::<Result<Vec<_>, _>>()?;
        let ids = super::morph::Ids {
            morph: self.new_node_id(),
            frame: self.new_frame_id(),
            boundary: self.new_node_id(),
        };
        let morphed = super::morph::command(
            &self.project,
            &self.registry,
            group,
            (&presets[0], found[0]),
            &found[1..],
            &ids,
        )?;
        self.message = Some(morphed.notice());
        Ok(morphed.command)
    }

    /// The preset last applied to a node or group, and whether it has been
    /// edited since. `None` if none was applied or it's gone from the
    /// library.
    pub fn preset_status(&self, id: NodeId) -> Option<PresetStatus> {
        let graph = self.project.graph();
        let node = graph.node(id)?;
        let entry = node.preset.as_deref()?;
        let (name, modified) = if node.type_id == noodle_core::group::GROUP {
            let preset = self.library.group_preset(entry)?;
            (preset.name.clone(), !preset.matches(graph, id))
        } else {
            let preset = self.library.node_preset(entry)?;
            (preset.name.clone(), !preset.matches(node))
        };
        Some(PresetStatus {
            id: entry.to_string(),
            name,
            modified,
            user: entry.starts_with(library::USER),
        })
    }

    /// The library template a group was made from, if it is still there.
    fn source_template(&self, group: NodeId) -> Option<(&str, &GroupTemplate)> {
        let entry = self.project.graph().node(group)?.template.as_deref()?;
        let (id, _) = self.library.templates().find(|(id, _)| *id == entry)?;
        Some((id, self.library.template(id)?))
    }

    /// The command that replaces a group's contents with its template's
    /// current version. Says in the status bar what it removes.
    pub(super) fn update_from_template(
        &mut self,
        group: NodeId,
    ) -> Result<noodle_core::Command, String> {
        let (_, template) = self
            .source_template(group)
            .ok_or("its template is no longer in the library")?;
        let name = template.name.clone();
        let (command, lanes) = template
            .update_group(&self.project, group, || self.new_node_id())
            .map_err(|e| e.to_string())?;
        self.message = Some(match lanes {
            0 => format!("Updated from {name}"),
            1 => format!("Updated from {name} (one automation lane was removed)"),
            n => format!("Updated from {name} ({n} automation lanes were removed)"),
        });
        Ok(command)
    }

    pub(super) fn library_edit(&mut self, edit: LibraryEdit) {
        let result = self.try_library_edit(edit);
        match result {
            Ok(message) => self.message = Some(message),
            Err(error) => self.message = Some(format!("Couldn't save to the library: {error}")),
        }
    }

    fn try_library_edit(&mut self, edit: LibraryEdit) -> Result<String, String> {
        let (item, overwrite) = self.capture(edit)?;
        let name = item.name().to_string();
        match overwrite {
            Some(id) => self
                .library
                .overwrite(&id, item)
                .map_err(|e| e.to_string())?,
            None => {
                self.library.save(item).map_err(|e| e.to_string())?;
            }
        }
        Ok(format!("Saved {name} to the library"))
    }

    /// The entry a save edit describes, from the project as it is now, and
    /// the user entry it replaces.
    fn capture(&self, edit: LibraryEdit) -> Result<(Item, Option<String>), String> {
        let graph = self.project.graph();
        let layout = self.layout_of();
        Ok(match edit {
            LibraryEdit::Template { group, name } => {
                let template =
                    GroupTemplate::from_group(graph, group, name).map_err(|e| e.to_string())?;
                (Item::Template(template), None)
            }
            LibraryEdit::NodePreset {
                node,
                name,
                overwrite,
            } => {
                let node = graph.node(node).ok_or("the node is gone")?;
                layout(node).ok_or("this node has no preset layout")?;
                (Item::NodePreset(NodePreset::capture(node, name)), overwrite)
            }
            LibraryEdit::GroupPreset {
                group,
                name,
                overwrite,
            } => (
                Item::GroupPreset(GroupPreset::capture(graph, group, name)),
                overwrite,
            ),
        })
    }
}
