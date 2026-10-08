//! Where everything in the graph is drawn, in graph coordinates: each node's
//! box and ports, each wire's ends, and each frame. Rebuilt every frame from
//! the project and the registry, so it never goes stale.

use std::collections::BTreeMap;

use egui::{Pos2, Rect, Vec2};
use noodle_core::group::{self, GROUP, GROUP_INPUT, GROUP_OUTPUT};
use noodle_core::{Connection, Endpoint, FrameId, Graph, Node, NodeId, Project};
use noodle_engine::{InputKind, ParamInfo, Registry};
use noodle_nodes::REROUTE_ID;

pub const NODE_WIDTH: f32 = 160.0;
pub const HEADER_HEIGHT: f32 = 24.0;
pub const ROW_HEIGHT: f32 = 22.0;
/// Space below the last row.
pub const PADDING: f32 = 6.0;
pub const REROUTE_SIZE: Vec2 = Vec2::new(32.0, 16.0);
pub const FRAME_HEADER_HEIGHT: f32 = 26.0;
/// The square in a frame's bottom-right corner that resizes it.
pub const FRAME_HANDLE: f32 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Input,
    Output,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PortKind {
    Audio,
    Param(ParamInfo),
    Event,
    /// A port the node doesn't have, but that a wire in the project refers
    /// to, e.g. on a node whose type is missing. Drawn so the wire has
    /// somewhere to go.
    Unknown,
}

impl PortKind {
    /// Whether an output of kind `self` can feed an input of kind `input`.
    pub fn feeds(&self, input: &PortKind) -> bool {
        matches!(
            (self, input),
            (PortKind::Audio, PortKind::Audio | PortKind::Param(_))
                | (PortKind::Event, PortKind::Event)
        )
    }
}

#[derive(Clone, Debug)]
pub struct PortGeom {
    pub key: String,
    pub name: String,
    pub side: Side,
    pub kind: PortKind,
    /// Where the socket is drawn and wires attach.
    pub socket: Pos2,
    /// The port's row of the node, for dropping a wire anywhere on it.
    pub row: Rect,
}

#[derive(Clone, Debug)]
pub struct NodeGeom {
    pub id: NodeId,
    pub title: String,
    pub category: String,
    pub rect: Rect,
    pub ports: Vec<PortGeom>,
    pub reroute: bool,
    /// Space for custom drawing below the ports; see [`super::body`].
    pub body: Option<Rect>,
    /// Why the node's ports couldn't be worked out, if they couldn't.
    pub error: Option<String>,
}

impl NodeGeom {
    pub fn header(&self) -> Rect {
        Rect::from_min_size(self.rect.min, Vec2::new(self.rect.width(), HEADER_HEIGHT))
    }

    pub fn port(&self, side: Side, key: &str) -> Option<&PortGeom> {
        self.ports.iter().find(|p| p.side == side && p.key == key)
    }
}

#[derive(Clone, Debug)]
pub struct WireGeom {
    pub connection: Connection,
    pub from: Pos2,
    pub to: Pos2,
    pub event: bool,
}

#[derive(Clone, Debug)]
pub struct FrameGeom {
    pub id: FrameId,
    pub label: String,
    pub rect: Rect,
}

impl FrameGeom {
    pub fn header(&self) -> Rect {
        Rect::from_min_size(
            self.rect.min,
            Vec2::new(
                self.rect.width(),
                FRAME_HEADER_HEIGHT.min(self.rect.height()),
            ),
        )
    }

    pub fn handle(&self) -> Rect {
        Rect::from_min_max(self.rect.max - Vec2::splat(FRAME_HANDLE), self.rect.max)
    }
}

/// Everything the editor draws, in graph coordinates.
pub struct Scene {
    /// In node ID order.
    pub nodes: Vec<NodeGeom>,
    pub wires: Vec<WireGeom>,
    pub frames: Vec<FrameGeom>,
    index: BTreeMap<NodeId, usize>,
}

impl Scene {
    /// The scene for one level of the project: the top level for `None`, or
    /// the inside of a group.
    pub fn build(project: &Project, registry: &Registry, level: Option<NodeId>) -> Self {
        let graph = project.graph();
        let connections: Vec<Connection> = graph
            .connections()
            .filter(|c| graph.node(c.from.node).is_some_and(|n| n.parent == level))
            .collect();

        let mut nodes = Vec::new();
        let mut index = BTreeMap::new();
        for (id, node) in graph.children(level) {
            let mut ports = Vec::new();
            let mut error = None;
            let (title, category) = if let Some((title, category, structural)) =
                structure(graph, id, node)
            {
                ports = structural;
                (title, category)
            } else {
                match registry.get(&node.type_id) {
                    Some(node_type) => {
                        let info = node_type.info();
                        match node_type.layout(&node.config) {
                            Ok(layout) => {
                                let port = |side, key: &str, name: &str, kind| PortGeom {
                                    key: key.to_owned(),
                                    name: name.to_owned(),
                                    side,
                                    kind,
                                    socket: Pos2::ZERO,
                                    row: Rect::NOTHING,
                                };
                                for p in &layout.outputs {
                                    ports.push(port(
                                        Side::Output,
                                        &p.key,
                                        &p.name,
                                        PortKind::Audio,
                                    ));
                                }
                                for p in &layout.event_outputs {
                                    ports.push(port(
                                        Side::Output,
                                        &p.key,
                                        &p.name,
                                        PortKind::Event,
                                    ));
                                }
                                for p in &layout.inputs {
                                    let kind = match &p.kind {
                                        InputKind::Audio => PortKind::Audio,
                                        InputKind::Param(info) => PortKind::Param(info.clone()),
                                    };
                                    ports.push(port(Side::Input, &p.key, &p.name, kind));
                                }
                                for p in &layout.event_inputs {
                                    ports.push(port(Side::Input, &p.key, &p.name, PortKind::Event));
                                }
                            }
                            Err(e) => error = Some(e.to_string()),
                        }
                        (info.name.to_owned(), info.category.to_owned())
                    }
                    None => {
                        error = Some(format!("unknown node type `{}`", node.type_id));
                        (node.type_id.clone(), String::new())
                    }
                }
            };

            // Ports that wires refer to but the node doesn't have.
            for connection in &connections {
                for (endpoint, side) in [
                    (&connection.from, Side::Output),
                    (&connection.to, Side::Input),
                ] {
                    if endpoint.node == id
                        && !ports
                            .iter()
                            .any(|p| p.side == side && p.key == endpoint.port)
                    {
                        ports.push(PortGeom {
                            key: endpoint.port.clone(),
                            name: endpoint.port.clone(),
                            side,
                            kind: PortKind::Unknown,
                            socket: Pos2::ZERO,
                            row: Rect::NOTHING,
                        });
                    }
                }
            }
            // Outputs above inputs, as in Blender, keeping each side's order.
            ports.sort_by_key(|p| p.side == Side::Input);

            let origin = Pos2::new(node.position.x, node.position.y);
            let reroute = node.type_id == REROUTE_ID && error.is_none();
            let mut body = None;
            let rect = if reroute {
                place_reroute(origin, &mut ports);
                Rect::from_min_size(origin, REROUTE_SIZE)
            } else {
                let rect = place_rows(origin, &mut ports);
                let height = super::body::height(&node.type_id);
                if height > 0.0 {
                    let top = rect.bottom() - PADDING;
                    body = Some(Rect::from_min_size(
                        Pos2::new(rect.left(), top),
                        Vec2::new(NODE_WIDTH, height),
                    ));
                    rect.with_max_y(rect.max.y + height)
                } else {
                    rect
                }
            };

            index.insert(id, nodes.len());
            nodes.push(NodeGeom {
                id,
                title,
                category,
                rect,
                ports,
                reroute,
                body,
                error,
            });
        }

        let mut scene = Self {
            nodes,
            wires: Vec::new(),
            // Frames belong to the top level for now.
            frames: project
                .frames()
                .filter(|_| level.is_none())
                .map(|(id, frame)| FrameGeom {
                    id,
                    label: frame.label.clone(),
                    rect: Rect::from_min_size(
                        Pos2::new(frame.position.x, frame.position.y),
                        Vec2::new(frame.width, frame.height),
                    ),
                })
                .collect(),
            index,
        };
        scene.wires = connections
            .into_iter()
            .filter_map(|connection| {
                let from = scene.port(&connection.from, Side::Output)?;
                let to = scene.port(&connection.to, Side::Input)?;
                Some(WireGeom {
                    event: from.kind == PortKind::Event,
                    from: from.socket,
                    to: to.socket,
                    connection,
                })
            })
            .collect();
        scene
    }

    pub fn node(&self, id: NodeId) -> Option<&NodeGeom> {
        self.index.get(&id).map(|&i| &self.nodes[i])
    }

    pub fn port(&self, endpoint: &Endpoint, side: Side) -> Option<&PortGeom> {
        self.node(endpoint.node)?.port(side, &endpoint.port)
    }

    /// The smallest rectangle around every node and frame.
    pub fn bounds(&self) -> Option<Rect> {
        self.nodes
            .iter()
            .map(|n| n.rect)
            .chain(self.frames.iter().map(|f| f.rect))
            .reduce(Rect::union)
    }
}

/// A group's name, from its `name` config, or just "Group".
pub fn group_title(graph: &Graph, id: NodeId) -> String {
    match graph.node(id).and_then(|n| n.config.get(group::PORT_NAME)) {
        Some(noodle_core::Value::Text(name)) if !name.is_empty() => name.clone(),
        _ => "Group".to_owned(),
    }
}

/// What a group node, or a boundary node inside one, looks like. These have
/// no entry in the registry, because the engine flattens them away.
fn structure(graph: &Graph, id: NodeId, node: &Node) -> Option<(String, String, Vec<PortGeom>)> {
    let port = |side, name: &str| PortGeom {
        key: name.to_owned(),
        name: name.to_owned(),
        side,
        kind: PortKind::Audio,
        socket: Pos2::ZERO,
        row: Rect::NOTHING,
    };
    let category = "Group".to_owned();
    match node.type_id.as_str() {
        GROUP => {
            let ports = graph.group_ports(id);
            let title = group_title(graph, id);
            let mut all: Vec<PortGeom> = ports
                .outputs
                .iter()
                .map(|p| port(Side::Output, &p.name))
                .collect();
            all.extend(ports.inputs.iter().map(|p| port(Side::Input, &p.name)));
            Some((title, category, all))
        }
        GROUP_INPUT => {
            let name = group::port_name(id, node);
            let mut input = port(Side::Output, &name);
            input.key = group::INPUT_PORT.to_owned();
            Some(("Group Input".to_owned(), category, vec![input]))
        }
        GROUP_OUTPUT => {
            let name = group::port_name(id, node);
            let mut output = port(Side::Input, &name);
            output.key = group::OUTPUT_PORT.to_owned();
            Some(("Group Output".to_owned(), category, vec![output]))
        }
        _ => None,
    }
}

/// Lays out a normal node: a header, then one row per port. Returns the
/// node's rectangle.
fn place_rows(origin: Pos2, ports: &mut [PortGeom]) -> Rect {
    for (i, port) in ports.iter_mut().enumerate() {
        let top = origin.y + HEADER_HEIGHT + i as f32 * ROW_HEIGHT;
        port.row = Rect::from_min_size(Pos2::new(origin.x, top), Vec2::new(NODE_WIDTH, ROW_HEIGHT));
        let x = match port.side {
            Side::Input => origin.x,
            Side::Output => origin.x + NODE_WIDTH,
        };
        port.socket = Pos2::new(x, top + ROW_HEIGHT / 2.0);
    }
    let height = HEADER_HEIGHT + ports.len() as f32 * ROW_HEIGHT + PADDING;
    Rect::from_min_size(origin, Vec2::new(NODE_WIDTH, height))
}

/// A reroute is a small pill with its input on the left and output on the
/// right.
fn place_reroute(origin: Pos2, ports: &mut [PortGeom]) {
    let rect = Rect::from_min_size(origin, REROUTE_SIZE);
    for port in ports {
        let (x, half) = match port.side {
            Side::Input => (rect.left(), rect.left()..=rect.center().x),
            Side::Output => (rect.right(), rect.center().x..=rect.right()),
        };
        port.socket = Pos2::new(x, rect.center().y);
        port.row = Rect::from_x_y_ranges(half, rect.y_range());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{Command, Frame, History, Node, Position};

    fn registry() -> Registry {
        crate::session::Nodes::all().registry
    }

    fn add(project: &mut Project, history: &mut History, node: Node) -> NodeId {
        let id = project.new_node_id();
        history
            .apply(project, Command::AddNode { id, node })
            .unwrap();
        id
    }

    #[test]
    fn ports_come_from_the_layout_with_outputs_first() {
        let (mut project, mut history) = (Project::new(), History::new());
        let gain = add(
            &mut project,
            &mut history,
            Node::new("noodle.util.gain").at(100.0, 50.0),
        );
        let scene = Scene::build(&project, &registry(), None);
        let node = scene.node(gain).unwrap();
        assert_eq!(node.title, "Gain");
        let keys: Vec<_> = node
            .ports
            .iter()
            .map(|p| (p.side, p.key.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                (Side::Output, "out"),
                (Side::Input, "in"),
                (Side::Input, "gain")
            ]
        );
        assert!(matches!(node.ports[2].kind, PortKind::Param(_)));
        assert_eq!(node.rect.min, Pos2::new(100.0, 50.0));
        assert_eq!(
            node.rect.height(),
            HEADER_HEIGHT + 3.0 * ROW_HEIGHT + PADDING
        );
        // Outputs on the right edge, inputs on the left, each in its own row.
        let out = node.port(Side::Output, "out").unwrap();
        assert_eq!(out.socket.x, node.rect.right());
        let input = node.port(Side::Input, "in").unwrap();
        assert_eq!(input.socket.x, node.rect.left());
        assert!(input.socket.y > out.socket.y);
        assert!(input.row.contains(input.socket));
    }

    #[test]
    fn config_changes_the_ports() {
        let (mut project, mut history) = (Project::new(), History::new());
        let config = noodle_core::Config::new().with("inputs", noodle_core::Value::Int(5));
        let mix = add(
            &mut project,
            &mut history,
            Node::new("noodle.util.mix").with_config(config),
        );
        let scene = Scene::build(&project, &registry(), None);
        let inputs = scene
            .node(mix)
            .unwrap()
            .ports
            .iter()
            .filter(|p| p.side == Side::Input)
            .count();
        assert_eq!(inputs, 5);
    }

    #[test]
    fn wires_join_sockets_and_missing_ports_still_get_one() {
        let (mut project, mut history) = (Project::new(), History::new());
        let sine = add(&mut project, &mut history, Node::new("noodle.osc.sine"));
        let missing = add(
            &mut project,
            &mut history,
            Node::new("no.such.type").at(300.0, 0.0),
        );
        for (from, to) in [("out", "in"), ("nope", "frequency")] {
            let (from_node, to_node) = if from == "out" {
                (sine, missing)
            } else {
                (missing, sine)
            };
            history
                .apply(
                    &mut project,
                    Command::Connect(Connection {
                        from: Endpoint::new(from_node, from),
                        to: Endpoint::new(to_node, to),
                    }),
                )
                .unwrap();
        }
        let scene = Scene::build(&project, &registry(), None);
        assert_eq!(scene.wires.len(), 2);
        let node = scene.node(missing).unwrap();
        assert!(node.error.as_ref().unwrap().contains("no.such.type"));
        assert_eq!(
            node.port(Side::Input, "in").unwrap().kind,
            PortKind::Unknown
        );
        assert_eq!(
            node.port(Side::Output, "nope").unwrap().kind,
            PortKind::Unknown
        );
        let wire = scene
            .wires
            .iter()
            .find(|w| w.connection.from.node == sine)
            .unwrap();
        assert_eq!(
            wire.from,
            scene
                .node(sine)
                .unwrap()
                .port(Side::Output, "out")
                .unwrap()
                .socket
        );
    }

    #[test]
    fn reroutes_are_small_with_a_socket_each_side() {
        let (mut project, mut history) = (Project::new(), History::new());
        let id = add(
            &mut project,
            &mut history,
            Node::new(REROUTE_ID).at(10.0, 20.0),
        );
        let scene = Scene::build(&project, &registry(), None);
        let node = scene.node(id).unwrap();
        assert!(node.reroute);
        assert_eq!(node.rect.size(), REROUTE_SIZE);
        assert_eq!(node.port(Side::Input, "in").unwrap().socket.x, 10.0);
        assert_eq!(
            node.port(Side::Output, "out").unwrap().socket.x,
            10.0 + REROUTE_SIZE.x
        );
    }

    #[test]
    fn frames_and_bounds() {
        let (mut project, mut history) = (Project::new(), History::new());
        add(
            &mut project,
            &mut history,
            Node::new("noodle.osc.sine").at(0.0, 0.0),
        );
        let id = project.new_frame_id();
        let frame = Frame {
            label: "Synth".into(),
            position: Position { x: -50.0, y: -60.0 },
            width: 500.0,
            height: 10.0,
        };
        history
            .apply(&mut project, Command::AddFrame { id, frame })
            .unwrap();
        let scene = Scene::build(&project, &registry(), None);
        assert_eq!(scene.frames[0].label, "Synth");
        // A frame shorter than its header doesn't get a header taller than itself.
        assert_eq!(scene.frames[0].header().height(), 10.0);
        let bounds = scene.bounds().unwrap();
        assert_eq!(bounds.min, Pos2::new(-50.0, -60.0));
        assert_eq!(bounds.max.x, 450.0);
        assert!(bounds.max.y > 0.0);
    }

    #[test]
    fn which_ports_can_connect() {
        let param = PortKind::Param(ParamInfo::new(0.0, 1.0, 0.5));
        assert!(PortKind::Audio.feeds(&PortKind::Audio));
        assert!(PortKind::Audio.feeds(&param));
        assert!(PortKind::Event.feeds(&PortKind::Event));
        assert!(!PortKind::Audio.feeds(&PortKind::Event));
        assert!(!PortKind::Event.feeds(&param));
        assert!(!PortKind::Unknown.feeds(&PortKind::Audio));
        assert!(!PortKind::Audio.feeds(&PortKind::Unknown));
    }
}
