//! Mix versions and articulations: two small lists the project keeps, and the
//! commands that keep nodes in step with them.
//!
//! **Versions.** The project holds the version list (names, order, and the
//! active one). Nothing else stores alternate values: a Version Value node has
//! one slot per version and outputs the active version's, so the graph shows
//! what differs. The node's slot count is its `versions` config, which these
//! commands keep equal to the list's length, so adding a version is one undo
//! step that also adds a slot (holding the value the active version has) to
//! every Version Value node.
//!
//! **Articulations.** The project holds a list of named articulations. A MIDI
//! note refers to one by ID; an Articulation Map node turns the ID into the
//! instrument's own way of asking (a keyswitch, a controller, a program).

use serde::{Deserialize, Serialize};

use crate::{Command, EditError, Node, NodeId, Project, Value};

/// The Version Value node type.
pub const VERSION_VALUE_ID: &str = "noodle.mod.version_value";
/// The Version Value node's config key for how many slots (versions) it has.
pub const VERSION_COUNT_KEY: &str = "versions";
/// The most versions a project can have.
pub const MAX_VERSIONS: usize = 16;

/// The parameter key of the slot for version `index` (from 0).
pub fn version_slot_key(index: usize) -> String {
    format!("v{}", index + 1)
}

/// The Articulation Map node type.
pub const ARTICULATION_MAP_ID: &str = "noodle.event.articulation_map";
/// The Articulation Map's config key for its table.
pub const ARTICULATION_MAP_KEY: &str = "map";
/// The most articulations a project can have.
pub const MAX_ARTICULATIONS: usize = 64;

/// A named way of playing, for notes to refer to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Articulation {
    /// Stays with the articulation through renames. Never 0, which means "no
    /// articulation", the instrument's default.
    pub id: u16,
    pub name: String,
    /// The tint the piano roll gives notes that have it.
    pub colour: [u8; 3],
}

/// Colours a new articulation is given in turn.
pub const ARTICULATION_COLOURS: [[u8; 3]; 6] = [
    [0x4C, 0x9F, 0xD8],
    [0xE0, 0x8A, 0x3C],
    [0x6B, 0xBF, 0x6A],
    [0xC9, 0x5F, 0xA6],
    [0xD6, 0xC2, 0x4A],
    [0x8E, 0x7C, 0xD8],
];

impl Project {
    /// The names of the mix versions, in order. Empty until the first "Vary
    /// per version".
    pub fn versions(&self) -> &[String] {
        &self.versions
    }

    /// The version that plays: an index into [`versions`](Self::versions).
    pub fn active_version(&self) -> usize {
        self.active_version
    }

    pub(crate) fn replace_versions(
        &mut self,
        names: Vec<String>,
        active: usize,
    ) -> Result<(Vec<String>, usize), EditError> {
        if names.len() > MAX_VERSIONS {
            return Err(EditError::Invalid(format!(
                "a project has at most {MAX_VERSIONS} versions"
            )));
        }
        if active >= names.len().max(1) {
            return Err(EditError::Invalid(format!(
                "version {active} is not in the list"
            )));
        }
        let was = (
            std::mem::replace(&mut self.versions, names),
            std::mem::replace(&mut self.active_version, active),
        );
        Ok(was)
    }

    /// The articulations notes can use.
    pub fn articulations(&self) -> &[Articulation] {
        &self.articulations
    }

    pub fn articulation(&self, id: u16) -> Option<&Articulation> {
        self.articulations.iter().find(|a| a.id == id)
    }

    /// An ID no articulation has.
    pub fn new_articulation_id(&self) -> u16 {
        self.articulations.iter().map(|a| a.id).max().unwrap_or(0) + 1
    }

    pub(crate) fn replace_articulations(
        &mut self,
        list: Vec<Articulation>,
    ) -> Result<Vec<Articulation>, EditError> {
        if list.len() > MAX_ARTICULATIONS {
            return Err(EditError::Invalid(format!(
                "a project has at most {MAX_ARTICULATIONS} articulations"
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        if list.iter().any(|a| a.id == 0 || !seen.insert(a.id)) {
            return Err(EditError::Invalid(
                "articulation IDs must be unique and not 0".into(),
            ));
        }
        Ok(std::mem::replace(&mut self.articulations, list))
    }

    /// The Version Value nodes, with the number of slots each has.
    fn version_nodes(&self) -> Vec<(NodeId, usize)> {
        self.graph()
            .nodes()
            .filter(|(_, node)| node.type_id == VERSION_VALUE_ID)
            .map(|(id, node)| (id, slots(node)))
            .collect()
    }

    /// The command that adds a version called `name` after the last, and a
    /// slot to every Version Value node, holding the value the active version
    /// has there. The new version is not made active.
    pub fn add_version_command(&self, name: &str) -> Result<Command, EditError> {
        if self.versions.len() >= MAX_VERSIONS {
            return Err(EditError::Invalid(format!(
                "a project has at most {MAX_VERSIONS} versions"
            )));
        }
        let mut names = self.versions.clone();
        names.push(name.to_owned());
        let mut commands = vec![Command::SetVersions {
            names,
            active: self.active_version,
        }];
        for (id, count) in self.version_nodes() {
            let node = self.graph().node(id).expect("just listed");
            if count >= MAX_VERSIONS {
                continue;
            }
            let held = slot_value(node, self.active_version.min(count.saturating_sub(1)));
            commands.push(set_count(id, count + 1));
            commands.push(set_slot(id, count, held));
        }
        Ok(Command::Batch(commands))
    }

    /// The command that removes version `index` and its slot from every
    /// Version Value node. The last version can't be removed.
    pub fn remove_version_command(&self, index: usize) -> Result<Command, EditError> {
        if index >= self.versions.len() || self.versions.len() < 2 {
            return Err(EditError::Invalid("that version can't be removed".into()));
        }
        let mut names = self.versions.clone();
        names.remove(index);
        let active = if self.active_version > index || self.active_version >= names.len() {
            self.active_version
                .saturating_sub(1)
                .min(names.len().saturating_sub(1))
        } else {
            self.active_version
        };
        let mut commands = vec![Command::SetVersions { names, active }];
        for (id, count) in self.version_nodes() {
            if index >= count {
                continue;
            }
            let node = self.graph().node(id).expect("just listed");
            let values: Vec<f32> = (0..count).map(|i| slot_value(node, i)).collect();
            for (i, value) in values.iter().enumerate().skip(index + 1) {
                commands.push(set_slot(id, i - 1, *value));
            }
            commands.push(Command::SetParam {
                node: id,
                key: version_slot_key(count - 1),
                value: None,
            });
            commands.push(set_count(id, count.saturating_sub(1).max(1)));
        }
        Ok(Command::Batch(commands))
    }

    /// The command that moves version `from` to place `to`, moving the slots
    /// of every Version Value node with it.
    pub fn move_version_command(&self, from: usize, to: usize) -> Result<Command, EditError> {
        let len = self.versions.len();
        if from >= len || to >= len {
            return Err(EditError::Invalid("that version is not in the list".into()));
        }
        let order = |count: usize| -> Vec<usize> {
            let mut order: Vec<usize> = (0..count).collect();
            if from < count && to < count {
                let moved = order.remove(from);
                order.insert(to, moved);
            }
            order
        };
        let mut names = self.versions.clone();
        let name = names.remove(from);
        names.insert(to, name);
        let active = {
            let mut indexes: Vec<usize> = (0..len).collect();
            let moved = indexes.remove(from);
            indexes.insert(to, moved);
            indexes
                .iter()
                .position(|&i| i == self.active_version)
                .unwrap_or(0)
        };
        let mut commands = vec![Command::SetVersions { names, active }];
        for (id, count) in self.version_nodes() {
            let node = self.graph().node(id).expect("just listed");
            let values: Vec<f32> = (0..count).map(|i| slot_value(node, i)).collect();
            for (new, &old) in order(count).iter().enumerate() {
                commands.push(set_slot(id, new, values[old]));
            }
        }
        Ok(Command::Batch(commands))
    }

    /// The command that makes `index` the version that plays.
    pub fn select_version_command(&self, index: usize) -> Result<Command, EditError> {
        if index >= self.versions.len() {
            return Err(EditError::Invalid("that version is not in the list".into()));
        }
        Ok(Command::SetVersions {
            names: self.versions.clone(),
            active: index,
        })
    }

    /// The command that renames version `index`.
    pub fn rename_version_command(&self, index: usize, name: &str) -> Result<Command, EditError> {
        if index >= self.versions.len() {
            return Err(EditError::Invalid("that version is not in the list".into()));
        }
        let mut names = self.versions.clone();
        names[index] = name.to_owned();
        Ok(Command::SetVersions {
            names,
            active: self.active_version,
        })
    }

    /// The project as it plays with version `index` active, for a pass of a
    /// batch export. Not an edit: the copy is never put in a history.
    pub fn with_active_version(&self, index: usize) -> Project {
        let mut copy = self.clone();
        if index < copy.versions.len() {
            copy.active_version = index;
        }
        copy
    }

    /// The command that renames articulation `id`. Notes and maps refer to
    /// it by ID, so they follow.
    pub fn rename_articulation_command(&self, id: u16, name: &str) -> Result<Command, EditError> {
        let mut list = self.articulations.clone();
        list.iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| EditError::Invalid("that articulation is gone".into()))?
            .name = name.to_owned();
        Ok(Command::SetArticulations(list))
    }

    /// The command that adds an articulation called `name` and gives it a
    /// colour.
    pub fn add_articulation_command(&self, name: &str) -> Result<Command, EditError> {
        let mut list = self.articulations.clone();
        list.push(Articulation {
            id: self.new_articulation_id(),
            name: name.to_owned(),
            colour: ARTICULATION_COLOURS[list.len() % ARTICULATION_COLOURS.len()],
        });
        Ok(Command::SetArticulations(list))
    }
}

/// How many slots a Version Value node has.
pub fn slots(node: &Node) -> usize {
    match node.config.get(VERSION_COUNT_KEY) {
        Some(Value::Int(n)) => usize::try_from(*n).unwrap_or(1).clamp(1, MAX_VERSIONS),
        _ => 2,
    }
}

fn slot_value(node: &Node, index: usize) -> f32 {
    node.params
        .get(&version_slot_key(index))
        .copied()
        .unwrap_or(0.0)
}

fn set_count(node: NodeId, count: usize) -> Command {
    Command::SetConfig {
        node,
        key: VERSION_COUNT_KEY.to_owned(),
        value: Some(Value::Int(count as i64)),
    }
}

fn set_slot(node: NodeId, index: usize, value: f32) -> Command {
    Command::SetParam {
        node,
        key: version_slot_key(index),
        value: Some(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, History};

    fn with_node(values: &[f32]) -> (Project, NodeId) {
        let mut project = Project::new();
        let id = project.new_node_id();
        let mut node = Node::new(VERSION_VALUE_ID)
            .with_config(Config::new().with(VERSION_COUNT_KEY, Value::Int(values.len() as i64)));
        for (i, v) in values.iter().enumerate() {
            node = node.with_param(version_slot_key(i), *v);
        }
        Command::AddNode { id, node }.apply(&mut project).unwrap();
        Command::SetVersions {
            names: (0..values.len()).map(|i| format!("V{i}")).collect(),
            active: 0,
        }
        .apply(&mut project)
        .unwrap();
        (project, id)
    }

    fn values(project: &Project, id: NodeId) -> Vec<f32> {
        let node = project.graph().node(id).unwrap();
        (0..slots(node)).map(|i| slot_value(node, i)).collect()
    }

    #[test]
    fn adding_a_version_gives_every_node_a_slot_holding_the_active_value() {
        let (mut project, id) = with_node(&[0.2, 0.9]);
        let command = project.add_version_command("TV").unwrap();
        let mut history = History::new();
        history.apply(&mut project, command).unwrap();
        assert_eq!(project.versions(), ["V0", "V1", "TV"]);
        assert_eq!(values(&project, id), [0.2, 0.9, 0.2]);
        assert!(history.undo(&mut project).unwrap());
        assert_eq!(values(&project, id), [0.2, 0.9]);
        assert_eq!(project.versions().len(), 2);
    }

    #[test]
    fn removing_a_version_shifts_the_slots_after_it() {
        let (mut project, id) = with_node(&[0.1, 0.2, 0.3]);
        let command = project.remove_version_command(1).unwrap();
        command.apply(&mut project).unwrap();
        assert_eq!(project.versions(), ["V0", "V2"]);
        assert_eq!(values(&project, id), [0.1, 0.3]);
    }

    #[test]
    fn moving_a_version_moves_its_slots_and_the_active_index() {
        let (mut project, id) = with_node(&[0.1, 0.2, 0.3]);
        let command = project.move_version_command(0, 2).unwrap();
        command.apply(&mut project).unwrap();
        assert_eq!(project.versions(), ["V1", "V2", "V0"]);
        assert_eq!(values(&project, id), [0.2, 0.3, 0.1]);
        assert_eq!(project.active_version(), 2);
    }

    #[test]
    fn the_last_version_stays() {
        let (project, _) = with_node(&[0.5]);
        assert!(project.remove_version_command(0).is_err());
    }

    #[test]
    fn the_lists_are_saved_and_loaded() {
        let (mut project, _) = with_node(&[0.1, 0.2]);
        Command::SetVersions {
            names: vec!["Main".into(), "Vocal up".into()],
            active: 1,
        }
        .apply(&mut project)
        .unwrap();
        let add = project.add_articulation_command("Staccato").unwrap();
        add.apply(&mut project).unwrap();
        let loaded = Project::from_ron(&project.to_ron()).unwrap();
        assert_eq!(loaded.versions(), ["Main", "Vocal up"]);
        assert_eq!(loaded.active_version(), 1);
        assert_eq!(loaded.articulations()[0].name, "Staccato");
        assert_eq!(loaded, project);
    }

    #[test]
    fn articulation_ids_are_unique_and_nonzero() {
        let mut project = Project::new();
        let bad = Command::SetArticulations(vec![Articulation {
            id: 0,
            name: "x".into(),
            colour: [0; 3],
        }]);
        assert!(bad.apply(&mut project).is_err());
    }
}
