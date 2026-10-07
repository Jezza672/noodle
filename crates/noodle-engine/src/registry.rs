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

    pub fn get(&self, id: &str) -> Option<&Arc<dyn NodeType>> {
        self.types.get(id)
    }

    /// All types, ordered by ID.
    pub fn iter(&self) -> impl Iterator<Item = &dyn NodeType> {
        self.types.values().map(|node_type| &**node_type)
    }
}
