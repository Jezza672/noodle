//! Edits to the project. Every change goes through a [`Command`], and applying
//! a command returns its inverse, which is what undo and redo are built on.

use std::fmt;

use crate::{Connection, Endpoint, Frame, FrameId, Node, NodeId, Position, Project, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    AddNode {
        id: NodeId,
        node: Node,
    },
    /// Also removes the node's connections, and everything inside it if it's
    /// a group.
    RemoveNode {
        id: NodeId,
    },
    /// Replaces whatever was already connected to the input.
    Connect(Connection),
    Disconnect {
        input: Endpoint,
    },
    /// `None` puts the parameter back to its default.
    SetParam {
        node: NodeId,
        key: String,
        value: Option<f32>,
    },
    /// `None` puts the setting back to its default.
    SetConfig {
        node: NodeId,
        key: String,
        value: Option<Value>,
    },
    MoveNode {
        node: NodeId,
        position: Position,
    },
    /// Moves a node into a group, or `None` out to the top level. The node
    /// can't be wired to nodes outside its new group.
    SetParent {
        node: NodeId,
        parent: Option<NodeId>,
    },
    AddFrame {
        id: FrameId,
        frame: Frame,
    },
    /// Leaves the nodes inside the frame where they are.
    RemoveFrame {
        id: FrameId,
    },
    /// Replaces the frame's label, position and size.
    SetFrame {
        id: FrameId,
        frame: Frame,
    },
    /// Applied in order, as one step. If any command fails, none take effect.
    Batch(Vec<Command>),
}

impl Command {
    /// Applies the command and returns its inverse. On error, the project is
    /// left unchanged.
    pub fn apply(self, project: &mut Project) -> Result<Command, EditError> {
        if let Command::Batch(commands) = self {
            let mut inverses = Vec::with_capacity(commands.len());
            for command in commands {
                match command.apply(project) {
                    Ok(inverse) => inverses.push(inverse),
                    Err(error) => {
                        for inverse in inverses.into_iter().rev() {
                            inverse
                                .apply(project)
                                .expect("undoing part of a failed batch can't fail");
                        }
                        return Err(error);
                    }
                }
            }
            inverses.reverse();
            return Ok(Command::Batch(inverses));
        }

        match self {
            Command::AddFrame { id, frame } => {
                project.insert_frame(id, frame)?;
                return Ok(Command::RemoveFrame { id });
            }
            Command::RemoveFrame { id } => {
                let frame = project.remove_frame(id)?;
                return Ok(Command::AddFrame { id, frame });
            }
            Command::SetFrame { id, frame } => {
                let old = std::mem::replace(project.frame_mut(id)?, frame);
                return Ok(Command::SetFrame { id, frame: old });
            }
            _ => {}
        }

        let graph = project.graph_mut();
        Ok(match self {
            Command::AddNode { id, node } => {
                graph.insert_node(id, node)?;
                Command::RemoveNode { id }
            }
            Command::RemoveNode { id } => {
                // Contents first, so a parent is always added back before
                // the nodes inside it.
                let mut ids = graph.descendants(id);
                ids.reverse();
                ids.push(id);
                let mut nodes = Vec::new();
                let mut connections = Vec::new();
                for id in ids {
                    let (node, wires) = graph.remove_node(id)?;
                    nodes.push(Command::AddNode { id, node });
                    connections.extend(wires.into_iter().map(Command::Connect));
                }
                nodes.reverse();
                nodes.extend(connections);
                Command::Batch(nodes)
            }
            Command::Connect(connection) => {
                let input = connection.to.clone();
                match graph.connect(connection)? {
                    Some(from) => Command::Connect(Connection { from, to: input }),
                    None => Command::Disconnect { input },
                }
            }
            Command::Disconnect { input } => {
                let from = graph.disconnect(&input)?;
                Command::Connect(Connection { from, to: input })
            }
            Command::SetParam { node, key, value } => {
                let params = &mut graph.node_mut(node)?.params;
                let old = match value {
                    Some(value) => params.insert(key.clone(), value),
                    None => params.remove(&key),
                };
                Command::SetParam {
                    node,
                    key,
                    value: old,
                }
            }
            Command::SetConfig { node, key, value } => {
                let config = &mut graph.node_mut(node)?.config;
                let old = match value {
                    Some(value) => config.set(key.clone(), value),
                    None => config.remove(&key),
                };
                Command::SetConfig {
                    node,
                    key,
                    value: old,
                }
            }
            Command::MoveNode { node, position } => {
                let old = std::mem::replace(&mut graph.node_mut(node)?.position, position);
                Command::MoveNode {
                    node,
                    position: old,
                }
            }
            Command::SetParent { node, parent } => {
                let old = graph.set_parent(node, parent)?;
                Command::SetParent { node, parent: old }
            }
            Command::Batch(_)
            | Command::AddFrame { .. }
            | Command::RemoveFrame { .. }
            | Command::SetFrame { .. } => unreachable!("handled above"),
        })
    }
}

/// What a command that only sets a value sets, so later inverses of the same
/// thing in a group can be dropped.
#[derive(PartialEq)]
enum Target<'a> {
    Position(NodeId),
    Frame(FrameId),
    Param(NodeId, &'a str),
}

fn target(command: &Command) -> Option<Target<'_>> {
    match command {
        Command::MoveNode { node, .. } => Some(Target::Position(*node)),
        Command::SetFrame { id, .. } => Some(Target::Frame(*id)),
        Command::SetParam { node, key, .. } => Some(Target::Param(*node, key)),
        _ => None,
    }
}

/// Adds an inverse to an open group, flattening batches and dropping inverses
/// that an earlier one in the group already undoes. Undo applies a group in
/// reverse, so the earliest inverse of a value runs last and wins.
fn push_coalesced(group: &mut Vec<Command>, inverse: Command) {
    match inverse {
        // A batch's inverse is already in undo order; the group is reversed
        // when it ends, so push its parts in application order.
        Command::Batch(parts) => {
            for part in parts.into_iter().rev() {
                push_coalesced(group, part);
            }
        }
        inverse => {
            let seen = target(&inverse).is_some_and(|t| {
                group
                    .iter()
                    .any(|earlier| target(earlier).as_ref() == Some(&t))
            });
            if !seen {
                group.push(inverse);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditError {
    NoSuchNode(NodeId),
    NodeExists(NodeId),
    NotConnected(Endpoint),
    /// Only possible in a project file, since connecting an input that's
    /// already connected replaces the old connection.
    ConnectedTwice(Endpoint),
    NoSuchFrame(FrameId),
    FrameExists(FrameId),
    /// A node's parent, or a wire's ends, aren't in the same group.
    DifferentGroups(Connection),
    NotAGroup(NodeId),
    NothingToGroup,
    /// A node being grouped has a different parent from the first.
    NotSiblings(NodeId),
    GroupInsideItself(NodeId),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchNode(id) => write!(f, "there's no node {id}"),
            Self::NodeExists(id) => write!(f, "there's already a node {id}"),
            Self::NotConnected(input) => write!(f, "nothing is connected to {input}"),
            Self::ConnectedTwice(input) => write!(f, "{input} has more than one connection"),
            Self::NoSuchFrame(id) => write!(f, "there's no {id}"),
            Self::FrameExists(id) => write!(f, "there's already a {id}"),
            Self::DifferentGroups(c) => write!(
                f,
                "{} and {} aren't in the same group, so they can't be wired together",
                c.from, c.to
            ),
            Self::NothingToGroup => write!(f, "there's nothing selected to group"),
            Self::NotSiblings(id) => write!(f, "{id} isn't in the same group as the rest"),
            Self::NotAGroup(id) => write!(f, "{id} isn't a group"),
            Self::GroupInsideItself(id) => write!(f, "{id} can't be put inside itself"),
        }
    }
}

impl std::error::Error for EditError {}

/// The undo and redo stacks.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Command>,
    redo: Vec<Command>,
    /// Inverses collected while a group is open, in the order they were made.
    group: Option<Vec<Command>>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, project: &mut Project, command: Command) -> Result<(), EditError> {
        let inverse = command.apply(project)?;
        self.redo.clear();
        match &mut self.group {
            Some(group) => push_coalesced(group, inverse),
            None => self.undo.push(inverse),
        }
        Ok(())
    }

    /// Until [`end_group`](Self::end_group), everything applied becomes one
    /// undo step, e.g. all the moves made during one drag.
    ///
    /// A group keeps only the first inverse of each move, frame change or
    /// parameter change to the same thing, since that one restores the value
    /// from before the group. So a long drag stays one small undo step rather
    /// than one inverse per frame.
    pub fn begin_group(&mut self) {
        self.group.get_or_insert_with(Vec::new);
    }

    pub fn end_group(&mut self) {
        if let Some(mut inverses) = self.group.take()
            && !inverses.is_empty()
        {
            inverses.reverse();
            self.undo.push(Command::Batch(inverses));
        }
    }

    /// Returns false if there was nothing to undo.
    pub fn undo(&mut self, project: &mut Project) -> Result<bool, EditError> {
        self.end_group();
        Self::step(project, &mut self.undo, &mut self.redo)
    }

    /// Returns false if there was nothing to redo.
    pub fn redo(&mut self, project: &mut Project) -> Result<bool, EditError> {
        self.end_group();
        Self::step(project, &mut self.redo, &mut self.undo)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.group.as_ref().is_some_and(|g| !g.is_empty())
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn step(
        project: &mut Project,
        from: &mut Vec<Command>,
        to: &mut Vec<Command>,
    ) -> Result<bool, EditError> {
        let Some(command) = from.pop() else {
            return Ok(false);
        };
        match command.clone().apply(project) {
            Ok(inverse) => {
                to.push(inverse);
                Ok(true)
            }
            Err(error) => {
                from.push(command);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    fn add(history: &mut History, project: &mut Project, node: Node) -> NodeId {
        let id = project.new_node_id();
        history
            .apply(project, Command::AddNode { id, node })
            .unwrap();
        id
    }

    fn frame(label: &str) -> Frame {
        Frame {
            label: label.into(),
            position: Position { x: -10.0, y: 5.0 },
            width: 300.0,
            height: 200.0,
        }
    }

    #[test]
    fn frame_ids_are_not_reused_and_clash_cleanly() {
        let (mut p, mut h) = (Project::new(), History::new());
        let id = p.new_frame_id();
        h.apply(
            &mut p,
            Command::AddFrame {
                id,
                frame: frame("A"),
            },
        )
        .unwrap();
        assert_eq!(
            h.apply(
                &mut p,
                Command::AddFrame {
                    id,
                    frame: frame("B")
                }
            ),
            Err(EditError::FrameExists(id))
        );
        h.apply(&mut p, Command::RemoveFrame { id }).unwrap();
        assert_ne!(p.new_frame_id(), id);
        assert_eq!(
            h.apply(&mut p, Command::RemoveFrame { id }),
            Err(EditError::NoSuchFrame(id))
        );
    }

    fn connect(from: NodeId, from_port: &str, to: NodeId, to_port: &str) -> Command {
        Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        })
    }

    #[test]
    fn connecting_replaces_the_old_connection_and_undo_restores_it() {
        let (mut p, mut h) = (Project::new(), History::new());
        let a = add(&mut h, &mut p, Node::new("noodle.osc.sine"));
        let b = add(&mut h, &mut p, Node::new("noodle.osc.sine"));
        let mix = add(&mut h, &mut p, Node::new("noodle.util.mix"));
        let input = Endpoint::new(mix, "in1");

        h.apply(&mut p, connect(a, "out", mix, "in1")).unwrap();
        h.apply(&mut p, connect(b, "out", mix, "in1")).unwrap();
        assert_eq!(p.graph().source(&input), Some(&Endpoint::new(b, "out")));

        h.undo(&mut p).unwrap();
        assert_eq!(p.graph().source(&input), Some(&Endpoint::new(a, "out")));
    }

    #[test]
    fn removing_a_node_removes_its_connections_and_undo_restores_them() {
        let (mut p, mut h) = (Project::new(), History::new());
        let a = add(&mut h, &mut p, Node::new("noodle.osc.sine"));
        let gain = add(&mut h, &mut p, Node::new("noodle.util.gain"));
        let b = add(&mut h, &mut p, Node::new("noodle.util.gain"));
        h.apply(&mut p, connect(a, "out", gain, "in")).unwrap();
        h.apply(&mut p, connect(gain, "out", b, "in")).unwrap();
        let before = p.clone();

        h.apply(&mut p, Command::RemoveNode { id: gain }).unwrap();
        assert!(p.graph().node(gain).is_none());
        assert_eq!(p.graph().connections().count(), 0);

        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn undo_and_redo_step_through_every_state() {
        let (mut p, mut h) = (Project::new(), History::new());
        let mut states = vec![p.clone()];
        let a = add(&mut h, &mut p, Node::new("noodle.osc.sine"));
        states.push(p.clone());
        let mix = add(&mut h, &mut p, Node::new("noodle.util.mix"));
        states.push(p.clone());

        let commands = [
            connect(a, "out", mix, "in2"),
            Command::SetParam {
                node: a,
                key: "frequency".into(),
                value: Some(220.0),
            },
            Command::SetConfig {
                node: mix,
                key: "inputs".into(),
                value: Some(Value::Int(4)),
            },
            Command::MoveNode {
                node: mix,
                position: Position { x: 10.0, y: 20.0 },
            },
            Command::SetParam {
                node: a,
                key: "frequency".into(),
                value: None,
            },
            Command::Disconnect {
                input: Endpoint::new(mix, "in2"),
            },
            Command::AddFrame {
                id: FrameId(1),
                frame: frame("Synth"),
            },
            Command::SetFrame {
                id: FrameId(1),
                frame: frame("Voice"),
            },
            Command::RemoveFrame { id: FrameId(1) },
            Command::RemoveNode { id: a },
        ];
        for command in commands {
            h.apply(&mut p, command).unwrap();
            states.push(p.clone());
        }

        for state in states.iter().rev().skip(1) {
            assert!(h.undo(&mut p).unwrap());
            assert_eq!(&p, state);
        }
        assert!(!h.undo(&mut p).unwrap());

        for state in states.iter().skip(1) {
            assert!(h.redo(&mut p).unwrap());
            assert_eq!(&p, state);
        }
        assert!(!h.redo(&mut p).unwrap());
    }

    #[test]
    fn a_failed_batch_changes_nothing() {
        let (mut p, mut h) = (Project::new(), History::new());
        let id = p.new_node_id();
        let missing = NodeId(999);
        let batch = Command::Batch(vec![
            Command::AddNode {
                id,
                node: Node::new("noodle.osc.sine"),
            },
            connect(id, "out", missing, "in"),
        ]);
        assert_eq!(h.apply(&mut p, batch), Err(EditError::NoSuchNode(missing)));
        assert_eq!(p, Project::new());
        assert!(!h.can_undo());
    }

    #[test]
    fn a_group_is_one_undo_step() {
        let (mut p, mut h) = (Project::new(), History::new());
        let id = add(&mut h, &mut p, Node::new("noodle.osc.sine"));
        let before = p.clone();

        h.begin_group();
        for x in 1..=3 {
            let position = Position {
                x: x as f32,
                y: 0.0,
            };
            h.apply(&mut p, Command::MoveNode { node: id, position })
                .unwrap();
        }
        h.end_group();

        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap();
        assert_eq!(p.graph().node(id).unwrap().position.x, 3.0);
    }

    #[test]
    fn a_long_drag_keeps_one_inverse_per_thing() {
        let (mut p, mut h) = (Project::new(), History::new());
        let a = add(&mut h, &mut p, Node::new("noodle.osc.sine"));
        let b = add(&mut h, &mut p, Node::new("noodle.osc.sine").at(5.0, 5.0));
        let frame = p.new_frame_id();
        h.apply(
            &mut p,
            Command::AddFrame {
                id: frame,
                frame: super::tests::frame("F"),
            },
        )
        .unwrap();
        h.apply(&mut p, connect(a, "out", b, "frequency")).unwrap();
        let before = p.clone();

        h.begin_group();
        for x in 1..=100 {
            let position = Position {
                x: x as f32,
                y: 0.0,
            };
            let mut moved = super::tests::frame("F");
            moved.position = position;
            let batch = Command::Batch(vec![
                Command::MoveNode { node: a, position },
                Command::MoveNode { node: b, position },
                Command::SetFrame {
                    id: frame,
                    frame: moved,
                },
                Command::SetParam {
                    node: a,
                    key: "frequency".into(),
                    value: Some(x as f32),
                },
            ]);
            h.apply(&mut p, batch).unwrap();
        }
        // Something that isn't a value change is kept, and its batch inverse
        // (re-add, then reconnect) stays in order.
        h.apply(&mut p, Command::RemoveNode { id: b }).unwrap();
        h.end_group();

        let Some(Command::Batch(step)) = h.undo.last() else {
            panic!("one undo step");
        };
        assert_eq!(step.len(), 6, "{step:?}");
        let after = p.clone();
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap();
        assert_eq!(p, after);
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let (mut p, mut h) = (Project::new(), History::new());
        let id = add(&mut h, &mut p, Node::new("noodle.util.mix"));
        h.undo(&mut p).unwrap();
        assert!(h.can_redo());
        add(
            &mut h,
            &mut p,
            Node::new("noodle.util.mix").with_config(Config::new()),
        );
        assert!(!h.can_redo());
        assert_ne!(p.graph().nodes().next().unwrap().0, id, "IDs aren't reused");
    }
}
