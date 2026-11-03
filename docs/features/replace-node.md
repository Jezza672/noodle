# Replace node and missing plugins (#357, S18)

- `Command::SetNodeType` changes a node's type in place.
- `session/replace.rs` plans a Replace as one `Command::Batch`: wires, lanes
  and stored values follow by port key, by position, or (plugin to plugin) by
  parameter name, rescaled by slider position. The right-click menu "Replace
  with..." opens the add-node search for it.
- A plugin type that is not installed compiles as a pass-through
  (`noodle-engine/src/missing.rs`) instead of dropping the node and silencing
  everything downstream.
- The S18 route (`stories/routes/s18_plugins.rs`) swaps one reverb for another
  keeping its automation, and back.

Not built: a "detached" list on the node for values with no match (undo
restores them instead), and saving a plugin's parameter names with the node,
which would let a stand-in be replaced by name.
