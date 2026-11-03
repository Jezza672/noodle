use std::collections::BTreeMap;
use std::sync::Arc;

use crate::NodeType;

/// Every node type the engine knows about, by ID.
#[derive(Default)]
pub struct Registry {
    types: BTreeMap<&'static str, Arc<dyn NodeType>>,
    /// Stand-ins for plugin types that are not installed, by ID; see
    /// [`provide_placeholders`](Self::provide_placeholders).
    placeholders: BTreeMap<String, Arc<dyn NodeType>>,
    generation: u64,
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
        self.generation += 1;
    }

    /// Registers a type, replacing one with the same ID if there is one.
    /// For a type that has a plain version in
    /// [`with_builtins`](Self::with_builtins) and a fuller one elsewhere.
    pub fn replace(&mut self, node_type: impl NodeType) {
        self.types.insert(node_type.info().id, Arc::new(node_type));
        self.generation += 1;
    }

    /// Changes whenever a type is registered or replaced, so what was derived
    /// from the registry (a node's layout) can be told to be stale.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn NodeType>> {
        self.types.get(id).or_else(|| self.placeholders.get(id))
    }

    /// Gives every plugin node type in `graph` that is not registered a
    /// pass-through stand-in (see `MissingPlugin`), so the editor can draw
    /// the node and the engine keeps the signal moving. Forgets stand-ins
    /// for types that are registered now or no longer used.
    pub fn provide_placeholders(&mut self, graph: &noodle_core::Graph) {
        let mut next: BTreeMap<String, Arc<dyn NodeType>> = BTreeMap::new();
        for (id, node) in graph.nodes() {
            if self.types.contains_key(node.type_id.as_str()) || next.contains_key(&node.type_id) {
                continue;
            }
            if let Some(stand_in) = crate::missing::placeholder(graph, id, node) {
                next.insert(node.type_id.clone(), Arc::new(stand_in));
            }
        }
        let same = next.len() == self.placeholders.len()
            && next.iter().all(|(k, v)| {
                self.placeholders.get(k).is_some_and(|old| {
                    old.layout(&noodle_core::Config::new()).ok()
                        == v.layout(&noodle_core::Config::new()).ok()
                })
            });
        if !same {
            self.placeholders = next;
            self.generation += 1;
        }
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
        keys.extend(
            layout
                .spectral_inputs
                .iter()
                .map(|p| format!("si:{}", p.key)),
        );
        keys.extend(
            layout
                .spectral_outputs
                .iter()
                .map(|p| format!("so:{}", p.key)),
        );
        keys.sort();
        Some(keys.join(";"))
    }

    /// All types, ordered by ID.
    pub fn iter(&self) -> impl Iterator<Item = &dyn NodeType> {
        self.types.values().map(|node_type| &**node_type)
    }
}
