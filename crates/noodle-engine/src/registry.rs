use std::collections::BTreeMap;
use std::sync::Arc;

use crate::{NodeType, Telemetry};

/// Every node type the engine knows about, by ID.
#[derive(Default)]
pub struct Registry {
    types: BTreeMap<&'static str, Arc<dyn NodeType>>,
    telemetry: Telemetry,
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

    /// The hub that nodes and plans built with this registry report to, for
    /// the UI to read. Nodes that report (meters, scopes) are given it when
    /// they're registered, and the engine opens a tap on every wired
    /// parameter in it.
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn NodeType>> {
        self.types.get(id)
    }

    /// All types, ordered by ID.
    pub fn iter(&self) -> impl Iterator<Item = &dyn NodeType> {
        self.types.values().map(|node_type| &**node_type)
    }
}
