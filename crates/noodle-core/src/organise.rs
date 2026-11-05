//! How the arrangement's tracks are organised for the eye: a palette colour
//! for each track, and folders that gather tracks and fold away.
//!
//! None of it reaches the engine. A track is named by its group, as in the
//! track order. The whole of it is replaced by one command
//! ([`Command::SetOrganisation`](crate::Command::SetOrganisation)), so
//! recolouring or regrouping many tracks is a single undo step.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::NodeId;

/// Names a folder. Never reused within a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FolderId(pub u64);

/// A folder of tracks. Its tracks stay in the track order; the folder only
/// gathers them under a header that can fold them out of sight.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    /// A palette slot (see the app's theme), or none for the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<u8>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
}

/// Colours and folders for the tracks of a project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Organisation {
    /// Palette slots, by track group. A slot, not an RGB value, so a change of
    /// colour scheme recolours every track at once.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub colours: BTreeMap<NodeId, u8>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub folders: BTreeMap<FolderId, Folder>,
    /// The folder each track (group) is in. A track is in at most one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub membership: BTreeMap<NodeId, FolderId>,
}

impl Organisation {
    pub fn is_empty(&self) -> bool {
        self.colours.is_empty() && self.folders.is_empty() && self.membership.is_empty()
    }

    /// The palette slot chosen for a track, if any.
    pub fn colour(&self, track: NodeId) -> Option<u8> {
        self.colours.get(&track).copied()
    }

    pub fn folder_of(&self, track: NodeId) -> Option<FolderId> {
        self.membership.get(&track).copied()
    }

    /// A free ID for a new folder.
    pub fn new_folder_id(&self) -> FolderId {
        FolderId(self.folders.keys().last().map_or(1, |id| id.0 + 1))
    }

    /// The colour a track shows: its own, else its folder's.
    pub fn shown_colour(&self, track: NodeId) -> Option<u8> {
        self.colour(track).or_else(|| {
            let folder = self.folders.get(&self.folder_of(track)?)?;
            folder.colour
        })
    }

    /// Sets (or with `None` clears) the colour of `tracks`.
    pub fn with_colour(mut self, tracks: &[NodeId], colour: Option<u8>) -> Self {
        for &track in tracks {
            match colour {
                Some(slot) => self.colours.insert(track, slot),
                None => self.colours.remove(&track),
            };
        }
        self
    }

    /// Puts `tracks` in `folder`, or takes them out of any folder. A folder
    /// left with no tracks goes.
    pub fn with_folder(mut self, tracks: &[NodeId], folder: Option<FolderId>) -> Self {
        for &track in tracks {
            match folder {
                Some(folder) => self.membership.insert(track, folder),
                None => self.membership.remove(&track),
            };
        }
        self.drop_empty_folders();
        self
    }

    /// Adds a folder holding `tracks`.
    pub fn with_new_folder(mut self, name: &str, tracks: &[NodeId]) -> Self {
        let id = self.new_folder_id();
        let folder = Folder {
            name: name.to_owned(),
            colour: None,
            folded: false,
        };
        self.folders.insert(id, folder);
        self.with_folder(tracks, Some(id))
    }

    pub fn with_folded(mut self, folder: FolderId, folded: bool) -> Self {
        if let Some(folder) = self.folders.get_mut(&folder) {
            folder.folded = folded;
        }
        self
    }

    pub fn with_all_folded(mut self, folded: bool) -> Self {
        for folder in self.folders.values_mut() {
            folder.folded = folded;
        }
        self
    }

    pub fn with_folder_name(mut self, folder: FolderId, name: &str) -> Self {
        if let Some(folder) = self.folders.get_mut(&folder) {
            folder.name = name.to_owned();
        }
        self
    }

    pub fn with_folder_colour(mut self, folder: FolderId, colour: Option<u8>) -> Self {
        if let Some(folder) = self.folders.get_mut(&folder) {
            folder.colour = colour;
        }
        self
    }

    /// Dissolves a folder; its tracks stay where they are.
    pub fn without_folder(mut self, folder: FolderId) -> Self {
        self.folders.remove(&folder);
        self.membership.retain(|_, f| *f != folder);
        self
    }

    fn drop_empty_folders(&mut self) {
        let used: std::collections::BTreeSet<FolderId> =
            self.membership.values().copied().collect();
        self.folders.retain(|id, _| used.contains(id));
    }

    /// Without the tracks `keep` rejects, and without folders left empty.
    pub(crate) fn retain_tracks(&mut self, keep: impl Fn(NodeId) -> bool) {
        self.colours.retain(|&t, _| keep(t));
        self.membership.retain(|&t, _| keep(t));
        self.drop_empty_folders();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_with_no_tracks_goes() {
        let (a, b) = (NodeId(1), NodeId(2));
        let org = Organisation::default().with_new_folder("Drums", &[a, b]);
        assert_eq!(org.folders.len(), 1);
        let folder = org.folder_of(a).unwrap();
        let org = org.with_folder(&[a], None);
        assert_eq!(org.folders.len(), 1);
        let org = org.with_folder(&[b], None);
        assert!(org.folders.is_empty(), "{folder:?}");
        assert!(org.is_empty());
    }

    #[test]
    fn a_track_shows_its_own_colour_before_its_folders() {
        let a = NodeId(1);
        let org = Organisation::default().with_new_folder("Drums", &[a]);
        let folder = org.folder_of(a).unwrap();
        let org = org.with_folder_colour(folder, Some(3));
        assert_eq!(org.shown_colour(a), Some(3));
        let org = org.with_colour(&[a], Some(5));
        assert_eq!(org.shown_colour(a), Some(5));
    }
}
