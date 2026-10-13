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

    /// A node's port layout as a string, for comparing: equal strings mean
    /// the node has the same ports with the same keys and kinds. `None` if
    /// the type is unknown or its config is invalid. Presets use this to
    /// tell which nodes and groups they fit.
    pub fn layout_signature(&self, node: &noodle_core::Node) -> Option<String> {
        use crate::InputKind;
        let layout = self.get(&node.type_id)?.layout(&node.config).ok()?;
        let mut out = String::new();
        for input in &layout.inputs {
            let kind = match input.kind {
                InputKind::Audio => 'a',
                InputKind::Param(_) => 'p',
            };
            out.push_str(&format!("i{kind}:{};", input.key));
        }
        for port in &layout.outputs {
            out.push_str(&format!("o:{};", port.key));
        }
        for port in &layout.event_inputs {
            out.push_str(&format!("ei:{};", port.key));
        }
        for port in &layout.event_outputs {
            out.push_str(&format!("eo:{};", port.key));
        }
        Some(out)
    }

    /// All types, ordered by ID.
    pub fn iter(&self) -> impl Iterator<Item = &dyn NodeType> {
        self.types.values().map(|node_type| &**node_type)
    }
}
