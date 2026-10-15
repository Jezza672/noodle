use std::collections::BTreeMap;
use std::sync::Arc;

use crate::NodeType;

/// Every node type the engine knows about, by ID.
#[derive(Default)]
pub struct Registry {
    types: BTreeMap<&'static str, Arc<dyn NodeType>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Panics if a type with the same ID is already registered.
    pub fn register(&mut self, node_type: impl NodeType) {
        let id = node_type.info().id;
        let previous = self.types.insert(id, Arc::new(node_type));
        assert!(previous.is_none(), "node type {id} registered twice");
    }

    /// Registers a type, replacing one with the same ID if there is one.
    /// For a type that has a plain version in
    /// [`with_builtins`](Self::with_builtins) and a fuller one elsewhere.
    pub fn replace(&mut self, node_type: impl NodeType) {
        self.types.insert(node_type.info().id, Arc::new(node_type));
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn NodeType>> {
        self.types.get(id)
    }

    /// The keys of a node's ports as one string, sorted, so the order ports
    /// are declared in doesn't matter: `i:key` for an audio or parameter
    /// input, `o:key` for an output, `ei:` and `eo:` for events, separated by
    /// `;`. `None` if the type is unknown or its config is invalid. Presets
    /// use this to tell whether a node has the ports they set, by name.
    pub fn layout_signature(&self, node: &noodle_core::Node) -> Option<String> {
        let layout = self.get(&node.type_id)?.layout(&node.config).ok()?;
        let mut keys: Vec<String> = Vec::new();
        keys.extend(layout.inputs.iter().map(|p| format!("i:{}", p.key)));
        keys.extend(layout.outputs.iter().map(|p| format!("o:{}", p.key)));
        keys.extend(layout.event_inputs.iter().map(|p| format!("ei:{}", p.key)));
        keys.extend(layout.event_outputs.iter().map(|p| format!("eo:{}", p.key)));
        keys.sort();
        Some(keys.join(";"))
    }

    /// All types, ordered by ID.
    pub fn iter(&self) -> impl Iterator<Item = &dyn NodeType> {
        self.types.values().map(|node_type| &**node_type)
    }
}
