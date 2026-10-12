//! A small helper for building node graphs through the normal commands.
//!
//! Every node and wire goes in through `History::apply` as a `Command`, the
//! same as the editor, so what the songs make is what a user could make by
//! hand. Nodes land in a grid, one column per step of the signal flow, so a
//! synth opened in the editor reads left to right.

use noodle_core::{
    Command, Config, Connection, Endpoint, History, Node, NodeId, Project, Value,
    group::{GROUP, GROUP_INPUT, GROUP_OUTPUT, PORT_NAME},
};

/// Where a patch is being built: the project, and the group (if any) new
/// nodes go inside.
pub struct Patch<'a> {
    pub project: &'a mut Project,
    pub history: &'a mut History,
    pub parent: Option<NodeId>,
}

/// A node to add: its type and settings.
pub struct Spec {
    kind: &'static str,
    params: Vec<(&'static str, f32)>,
    config: Vec<(&'static str, Value)>,
}

pub fn spec(kind: &'static str) -> Spec {
    Spec {
        kind,
        params: Vec::new(),
        config: Vec::new(),
    }
}

impl Spec {
    pub fn param(mut self, key: &'static str, value: f32) -> Self {
        self.params.push((key, value));
        self
    }

    pub fn config(mut self, key: &'static str, value: Value) -> Self {
        self.config.push((key, value));
        self
    }
}

/// One end of a wire.
pub type Port = (NodeId, &'static str);

const COLUMN: f32 = 220.0;
const ROW: f32 = 130.0;

impl<'a> Patch<'a> {
    pub fn new(project: &'a mut Project, history: &'a mut History) -> Self {
        Self {
            project,
            history,
            parent: None,
        }
    }

    fn apply(&mut self, command: Command) {
        self.history
            .apply(self.project, command)
            .expect("the songs build valid graphs");
    }

    /// Adds a node at grid cell (`column`, `row`) of the current group.
    pub fn add(&mut self, node: Spec, column: f32, row: f32) -> NodeId {
        let id = self.project.new_node_id();
        let mut built = Node::new(node.kind).at(column * COLUMN, row * ROW);
        for (key, value) in node.params {
            built = built.with_param(key, value);
        }
        let mut config = Config::new();
        for (key, value) in node.config {
            config.set(key, value);
        }
        built = built.with_config(config);
        built.parent = self.parent;
        self.apply(Command::AddNode { id, node: built });
        id
    }

    pub fn connect(&mut self, from: Port, to: Port) {
        self.apply(Command::Connect(Connection {
            from: Endpoint::new(from.0, from.1),
            to: Endpoint::new(to.0, to.1),
        }));
    }

    /// Adds a group named `name` in the current group and goes inside it.
    /// Returns the group; [`Patch::leave`] comes back out.
    pub fn enter(&mut self, name: &str, column: f32, row: f32) -> NodeId {
        let group = self.add(
            spec(GROUP).config(PORT_NAME, Value::Text(name.into())),
            column,
            row,
        );
        // A group's name is its `name` config, as the editor's rename sets it.
        self.parent = Some(group);
        group
    }

    pub fn leave(&mut self, group: NodeId) {
        self.parent = self
            .project
            .graph()
            .node(group)
            .and_then(|node| node.parent);
    }

    /// A group input named `name`; wire from its `out` port.
    pub fn input(&mut self, name: &str, row: f32) -> NodeId {
        self.add(
            spec(GROUP_INPUT).config(PORT_NAME, Value::Text(name.into())),
            0.0,
            row,
        )
    }

    /// A group output named `name`; wire into its `in` port.
    pub fn output(&mut self, name: &str, column: f32, row: f32) -> NodeId {
        self.add(
            spec(GROUP_OUTPUT).config(PORT_NAME, Value::Text(name.into())),
            column,
            row,
        )
    }
}
