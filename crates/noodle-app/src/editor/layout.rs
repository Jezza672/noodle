//! Where everything in the graph is drawn, in graph coordinates: each node's
//! box and ports, each wire's ends, and each frame. Rebuilt every frame from
//! the project and the registry, so it never goes stale.

use std::collections::BTreeMap;

use egui::{Pos2, Rect, Vec2};
use noodle_core::group::{self, GROUP, GROUP_INPUT, GROUP_OUTPUT};
use noodle_core::spare;
use noodle_core::{Command, Connection, Endpoint, FrameId, Graph, LaneId, Node, NodeId, Project};
use noodle_engine::{InputKind, ParamInfo, Registry, Unit};
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

impl Side {
    pub fn other(self) -> Self {
        match self {
            Self::Input => Self::Output,
            Self::Output => Self::Input,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PortKind {
    Audio,
    Param(ParamInfo),
    Event,
    /// A group's port that nothing is wired to on either side yet: it takes
    /// a wire of any kind, and becomes that kind once one is connected.
    Any,
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
                | (PortKind::Any, PortKind::Audio | PortKind::Param(_))
                | (PortKind::Any, PortKind::Event | PortKind::Any)
                | (PortKind::Audio | PortKind::Event, PortKind::Any)
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
    /// Whether the socket sits on the title bar, unlabelled: a node's only
    /// input or only output has no need of a row.
    pub in_header: bool,
    /// A port that isn't stored yet: the spare on a mixer or a group, which
    /// becomes real when a wire is dropped on it. Drawn greyed.
    pub spare: bool,
    /// An output nothing feeds, such as a track's `midi` with no MIDI clips.
    /// Drawn greyed, but it can still be wired.
    pub idle: bool,
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
    /// Set for a wire that stands for an automation lane: it isn't stored as
    /// a connection, and cutting it removes the lane.
    pub lane: Option<LaneId>,
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

impl WireGeom {
    /// The command that removes this wire: a disconnect, or for a lane's
    /// wire, removing the lane.
    pub fn cut(&self) -> Command {
        match self.lane {
            Some(id) => Command::RemoveLane { id },
            None => Command::Disconnect {
                input: self.connection.to.clone(),
            },
        }
    }
}

/// The key of the port on a track's input node that stands for a lane.
fn lane_key(lane: LaneId) -> String {
    format!("lane{}", lane.0)
}

/// Whether `endpoint` is the port that stands for a lane on a track's input.
pub fn is_lane_port(graph: &Graph, endpoint: &Endpoint) -> bool {
    endpoint.port.starts_with("lane")
        && graph
            .node(endpoint.node)
            .is_some_and(|n| n.type_id == group::TRACK_INPUT)
}

/// What a lane's wire is called: its port's name and the control it drives,
/// e.g. "out gain lane".
fn lane_name(graph: &Graph, target: &Endpoint) -> String {
    let (control, id) = group::parse_control_key(&target.port).unwrap_or((group::GAIN, 0));
    let name = graph
        .node(target.node)
        .and_then(|n| n.boundary_ports().into_iter().find(|&(p, _)| p == id))
        .map_or_else(|| target.port.clone(), |(_, name)| name);
    format!("{name} {control} lane")
}

/// The lanes shown as wires from `node`, when it is a track's input: those
/// driving a boundary node of its group. A lane has no wire of its own in
/// the project, so the first track input in a group stands for all of them.
fn lane_wires(project: &Project, node: NodeId) -> Vec<(LaneId, Endpoint)> {
    let graph = project.graph();
    let Some(input) = graph.node(node).filter(|n| n.type_id == group::TRACK_INPUT) else {
        return Vec::new();
    };
    let first = graph
        .children(input.parent)
        .find(|(_, n)| n.type_id == group::TRACK_INPUT)
        .map(|(id, _)| id);
    if first != Some(node) {
        return Vec::new();
    }
    project
        .lanes()
        .filter(|(_, lane)| {
            // Solo has no port, and a real wire in the way hides the lane.
            matches!(
                group::parse_control_key(&lane.target.port),
                Some((group::GAIN | group::MUTE, _))
            ) && graph.source(&lane.target).is_none()
                && !lane.points.is_empty()
                && graph.node(lane.target.node).is_some_and(|n| {
                    n.parent == input.parent
                        && matches!(n.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT)
                })
        })
        .map(|(id, lane)| (id, lane.target.clone()))
        .collect()
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
                structure(project, registry, id, node)
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
                                    in_header: false,
                                    spare: false,
                                    idle: false,
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
                                // Note inputs first: they are the way into an
                                // instrument, and a plugin can have hundreds of
                                // parameters below them.
                                for p in &layout.event_inputs {
                                    ports.push(port(Side::Input, &p.key, &p.name, PortKind::Event));
                                }
                                for p in &layout.inputs {
                                    // A parameter the user (or the node type)
                                    // leaves off the node keeps its port while
                                    // a wire or a lane goes into it.
                                    let endpoint = Endpoint::new(id, p.key.to_string());
                                    // (A mixer's gains and mutes come and go with
                                    // its inputs; `add_spares` trims them.)
                                    if matches!(p.kind, InputKind::Param(_))
                                        && node.type_id != spare::MIXER
                                        && !layout.param_shown(&node.param_shown, &p.key)
                                        && graph.source(&endpoint).is_none()
                                        && project.lane_for(&endpoint).is_none()
                                    {
                                        continue;
                                    }
                                    let kind = match &p.kind {
                                        InputKind::Audio => PortKind::Audio,
                                        InputKind::Param(info) => PortKind::Param(info.clone()),
                                    };
                                    ports.push(port(Side::Input, &p.key, &p.name, kind));
                                }
                            }
                            Err(e) => error = Some(e.to_string()),
                        }
                        (info.name.to_owned(), info.category.to_owned())
                    }
                    None => {
                        error = Some(format!("unknown node type `{}`", node.type_id));
                        (friendly_type_name(&node.type_id), String::new())
                    }
                }
            };

            // A track's source node is named after its group.
            let title = match node.parent {
                Some(parent) if node.type_id == group::TRACK_INPUT => {
                    match graph
                        .node(parent)
                        .and_then(|n| n.config.get(group::PORT_NAME))
                    {
                        Some(noodle_core::Value::Text(name)) if !name.is_empty() => name.clone(),
                        _ => title,
                    }
                }
                _ => title,
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
                            in_header: false,
                            spare: false,
                            idle: false,
                        });
                    }
                }
            }
            add_spares(graph, id, node, &mut ports);
            if node.type_id == group::TRACK_INPUT {
                for (lane, target) in lane_wires(project, id) {
                    ports.push(PortGeom {
                        key: lane_key(lane),
                        name: lane_name(graph, &target),
                        side: Side::Output,
                        kind: PortKind::Audio,
                        socket: Pos2::ZERO,
                        row: Rect::NOTHING,
                        in_header: false,
                        spare: false,
                        idle: false,
                    });
                }
            }
            if node.type_id == group::TRACK_INPUT {
                // A track's outputs carry what its clips make.
                let audio = project.clips_on(id).any(|(_, c)| c.as_audio().is_some());
                let midi = project.clips_on(id).any(|(_, c)| c.as_midi().is_some());
                for port in &mut ports {
                    port.idle = match port.key.as_str() {
                        group::AUDIO => !audio,
                        group::MIDI => !midi,
                        _ => false,
                    };
                }
            }

            // The user's order first, then the rest in the node's own.
            let order = &node.port_order;
            let rank = |p: &PortGeom| order.iter().position(|key| *key == p.key);
            ports.sort_by_key(|p| rank(p).unwrap_or(usize::MAX));

            let origin = Pos2::new(node.position.x, node.position.y);
            let reroute = node.type_id == REROUTE_ID && error.is_none();
            let mut body = None;
            let rect = if reroute {
                place_reroute(origin, &mut ports);
                Rect::from_min_size(origin, REROUTE_SIZE)
            } else {
                let has_field = |p: &PortGeom| {
                    p.side == Side::Input
                        && matches!(p.kind, PortKind::Param(_))
                        && graph.source(&Endpoint::new(id, p.key.clone())).is_none()
                        && project
                            .lane_for(&Endpoint::new(id, p.key.clone()))
                            .is_none()
                };
                // A group's input or output node labels its port, so the name
                // is drawn where the port is and can be edited in place.
                let labelled = matches!(node.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT);
                let rect = place_columns(origin, &mut ports, has_field, labelled);
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
                    lane: None,
                    event: from.kind == PortKind::Event,
                    from: from.socket,
                    to: to.socket,
                    connection,
                })
            })
            .collect();
        // Lanes show as wires from their track's input node.
        for &id in scene.index.keys() {
            for (lane, target) in lane_wires(project, id) {
                let from = Endpoint::new(id, lane_key(lane));
                let (Some(a), Some(b)) = (
                    scene.port(&from, Side::Output),
                    scene.port(&target, Side::Input),
                ) else {
                    continue;
                };
                let (a, b) = (a.socket, b.socket);
                scene.wires.push(WireGeom {
                    lane: Some(lane),
                    event: false,
                    from: a,
                    to: b,
                    connection: Connection { from, to: target },
                });
            }
        }
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

/// Trims a mixer to its wired inputs, and gives mixers and groups a spare
/// port to wire the next signal to. See [`noodle_core::spare`].
fn add_spares(graph: &Graph, id: NodeId, node: &Node, ports: &mut Vec<PortGeom>) {
    let spare = |side, key: String| PortGeom {
        name: match spare::mixer_input_index(&key) {
            Some(i) => format!("In {i}"),
            None => key.clone(),
        },
        key,
        side,
        kind: if matches!(node.type_id.as_str(), GROUP | GROUP_INPUT | GROUP_OUTPUT) {
            PortKind::Any
        } else {
            PortKind::Audio
        },
        socket: Pos2::ZERO,
        row: Rect::NOTHING,
        in_header: false,
        spare: true,
        idle: false,
    };
    match node.type_id.as_str() {
        spare::MIXER if !ports.is_empty() => {
            let used = spare::mixer_used(graph, id);
            // An input's gain and mute go with it.
            ports.retain(|p| {
                p.side != Side::Input
                    || spare::mixer_input_index(&p.key)
                        .or_else(|| spare::mixer_param_index(&p.key))
                        .is_none_or(|i| i <= used)
            });
            if (used as i64) < spare::MIXER_MAX_INPUTS {
                ports.push(spare(Side::Input, spare::mixer_spare_key(graph, id)));
            }
            // Each input is followed by its own gain and mute.
            ports.sort_by_key(|p| {
                let index = spare::mixer_input_index(&p.key);
                let param = spare::mixer_param_index(&p.key);
                (
                    p.side == Side::Output,
                    index.or(param).unwrap_or(0),
                    // Gain before mute.
                    p.key.starts_with("mute"),
                    param.is_some(),
                )
            });
        }
        GROUP_INPUT | GROUP_OUTPUT => {
            let Some(group) = node.parent else { return };
            let (side, port_side) = if node.type_id == GROUP_INPUT {
                (spare::Side::Input, Side::Output)
            } else {
                (spare::Side::Output, Side::Input)
            };
            // The next port's key on this node, drawn under the ports.
            let next = node
                .boundary_ports()
                .iter()
                .map(|&(pid, _)| pid)
                .max()
                .map_or(1, |pid| pid + 1);
            let mut port = spare(port_side, group::port_key(next));
            port.name = spare::spare_group_name(graph, group, side);
            let at = ports
                .iter()
                .rposition(|p| p.side == port_side && group::parse_port_key(&p.key).is_some())
                .map_or(0, |i| i + 1);
            ports.insert(at, port);
        }
        GROUP => {
            let input = spare::spare_group_name(graph, id, spare::Side::Input);
            let output = spare::spare_group_name(graph, id, spare::Side::Output);
            ports.push(spare(Side::Input, input));
            ports.push(spare(Side::Output, output));
        }
        _ => {}
    }
}

/// A readable name for a node type the registry doesn't know, from the last
/// part of its ID: `noodle.effect.low_pass` is "Low pass".
pub fn friendly_type_name(type_id: &str) -> String {
    let last = type_id.rsplit('.').next().unwrap_or(type_id);
    capitalised(&last.replace(['_', '-'], " "))
}

/// `text` with its first letter in upper case.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A group's name, from its `name` config, or just "Group".
pub fn group_title(graph: &Graph, id: NodeId) -> String {
    match graph.node(id).and_then(|n| n.config.get(group::PORT_NAME)) {
        Some(noodle_core::Value::Text(name)) if !name.is_empty() => name.clone(),
        _ => "Group".to_owned(),
    }
}

/// Whether a boundary port shows its gain and mute ports: the group's
/// control port ([`Graph::control_port`]), which the mixer and track header
/// drive, or any port that has a value, a wire or a lane on either, so none
/// of them is left hidden while it still affects the sound.
fn show_controls(project: &Project, id: NodeId, node: &Node, port: u32) -> bool {
    let graph = project.graph();
    let control = node.parent.and_then(|group| graph.control_port(group));
    control.is_some_and(|c| (c.node, c.id) == (id, port))
        || node.has_gain_or_mute(port)
        || [group::GAIN, group::MUTE].iter().any(|control| {
            graph
                .source(&Endpoint::new(id, group::control_key(control, port)))
                .is_some()
        })
        || project.lanes().any(|(_, lane)| {
            lane.target.node == id
                && [group::GAIN, group::MUTE]
                    .iter()
                    .any(|control| lane.target.port == group::control_key(control, port))
        })
}

/// Works out what kind of signal a group's port carries from the wires on
/// either side of the boundary. The port is whatever the wires on one side
/// say it is, so the other side then only takes that kind; with no wire on
/// either side it is [`PortKind::Any`].
struct Kinds<'a> {
    project: &'a Project,
    registry: &'a Registry,
}

impl Kinds<'_> {
    /// How many groups deep to look through, so a chain of pass-through
    /// ports and a loop of them both end.
    const DEPTH: u8 = 6;

    /// The kind of port `id` of the boundary node `node` (and of the group's
    /// port of the same name).
    fn boundary(&self, node: NodeId, id: u32, depth: u8) -> PortKind {
        let graph = self.project.graph();
        let Some(boundary) = graph.node(node) else {
            return PortKind::Any;
        };
        let (Some(group), Some(name)) = (
            boundary.parent,
            boundary
                .boundary_ports()
                .into_iter()
                .find(|&(p, _)| p == id)
                .map(|(_, name)| name),
        ) else {
            return PortKind::Any;
        };
        if depth == 0 {
            return PortKind::Any;
        }
        let key = Endpoint::new(node, group::port_key(id));
        let outer = Endpoint::new(group, name);
        // Inside the group, then outside it: what a group input's wires go
        // to and what feeds it, and the other way round for an output.
        if boundary.type_id == GROUP_INPUT {
            self.first_known(&[self.sinks(&key, depth), self.source_of(&outer, depth)])
        } else {
            self.first_known(&[self.source_of(&key, depth), self.sinks(&outer, depth)])
        }
    }

    fn first_known(&self, kinds: &[PortKind]) -> PortKind {
        kinds
            .iter()
            .find(|kind| **kind != PortKind::Any)
            .cloned()
            .unwrap_or(PortKind::Any)
    }

    /// The kind of the output feeding `input`, or Any with nothing wired.
    fn source_of(&self, input: &Endpoint, depth: u8) -> PortKind {
        match self.project.graph().source(input) {
            Some(from) => self.output(from, depth - 1),
            None => PortKind::Any,
        }
    }

    /// The kind the inputs fed by output `from` want, or Any with none.
    fn sinks(&self, from: &Endpoint, depth: u8) -> PortKind {
        let kinds: Vec<PortKind> = self
            .project
            .graph()
            .connections()
            .filter(|c| c.from == *from)
            .map(|c| self.input(&c.to, depth - 1))
            .collect();
        self.first_known(&kinds)
    }

    /// The kind of the output port `at`: audio or events for a node, and for
    /// a group's boundary whatever it was worked out to be.
    fn output(&self, at: &Endpoint, depth: u8) -> PortKind {
        let graph = self.project.graph();
        let Some(node) = graph.node(at.node) else {
            return PortKind::Any;
        };
        match node.type_id.as_str() {
            GROUP => graph
                .group_ports(at.node)
                .output(&at.port)
                .map_or(PortKind::Any, |p| self.boundary(p.node, p.id, depth)),
            GROUP_INPUT => group::parse_port_key(&at.port)
                .map_or(PortKind::Any, |id| self.boundary(at.node, id, depth)),
            GROUP_OUTPUT => PortKind::Any,
            _ => {
                let Some(layout) = self
                    .registry
                    .get(&node.type_id)
                    .and_then(|t| t.layout(&node.config).ok())
                else {
                    return PortKind::Any;
                };
                if layout.event_outputs.iter().any(|p| p.key == at.port) {
                    PortKind::Event
                } else if layout.outputs.iter().any(|p| p.key == at.port) {
                    PortKind::Audio
                } else {
                    PortKind::Any
                }
            }
        }
    }

    /// The kind of the input port `at`.
    fn input(&self, at: &Endpoint, depth: u8) -> PortKind {
        let graph = self.project.graph();
        let Some(node) = graph.node(at.node) else {
            return PortKind::Any;
        };
        match node.type_id.as_str() {
            GROUP => graph
                .group_ports(at.node)
                .input(&at.port)
                .map_or(PortKind::Any, |p| self.boundary(p.node, p.id, depth)),
            GROUP_OUTPUT => group::parse_port_key(&at.port)
                .map_or(PortKind::Audio, |id| self.boundary(at.node, id, depth)),
            GROUP_INPUT => PortKind::Any,
            _ => {
                let Some(layout) = self
                    .registry
                    .get(&node.type_id)
                    .and_then(|t| t.layout(&node.config).ok())
                else {
                    return PortKind::Any;
                };
                if layout.event_inputs.iter().any(|p| p.key == at.port) {
                    PortKind::Event
                } else if layout.inputs.iter().any(|p| p.key == at.port) {
                    PortKind::Audio
                } else {
                    PortKind::Any
                }
            }
        }
    }
}

/// What a group node, or a boundary node inside one, looks like. These have
/// no entry in the registry, because the engine flattens them away.
fn structure(
    project: &Project,
    registry: &Registry,
    id: NodeId,
    node: &Node,
) -> Option<(String, String, Vec<PortGeom>)> {
    let graph = project.graph();
    let port = |side, name: &str| PortGeom {
        key: name.to_owned(),
        name: name.to_owned(),
        side,
        kind: PortKind::Audio,
        socket: Pos2::ZERO,
        row: Rect::NOTHING,
        in_header: false,
        spare: false,
        idle: false,
    };
    let category = "Group".to_owned();
    let kinds = Kinds { project, registry };
    // A boundary port's gain and mute are parameters, so they have ports and
    // can be wired like any other. Solo is read from the project, so it has
    // none. A group has one gain and mute, on its control port, so only that
    // port shows them, plus any port that has them in use.
    let controls = |pid: u32, name: &str, only: bool| {
        [
            (
                group::GAIN,
                "gain",
                ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels),
            ),
            (group::MUTE, "mute", ParamInfo::choice(["Off", "On"])),
        ]
        .map(|(control, what, info)| {
            let label = if only {
                what.to_owned()
            } else {
                format!("{name} {what}")
            };
            let mut port = port(Side::Input, &capitalised(&label));
            port.key = group::control_key(control, pid);
            port.kind = PortKind::Param(info);
            port
        })
    };
    match node.type_id.as_str() {
        GROUP => {
            let ports = graph.group_ports(id);
            let title = group_title(graph, id);
            let typed = |side, p: &group::GroupPort| {
                let mut port = port(side, &p.name);
                port.kind = kinds.boundary(p.node, p.id, Kinds::DEPTH);
                port
            };
            let mut all: Vec<PortGeom> = ports
                .outputs
                .iter()
                .map(|p| typed(Side::Output, p))
                .collect();
            all.extend(ports.inputs.iter().map(|p| typed(Side::Input, p)));
            Some((title, category, all))
        }
        GROUP_INPUT | GROUP_OUTPUT => {
            let is_input = node.type_id == GROUP_INPUT;
            // The group's input signals come out of its input node.
            let side = if is_input { Side::Output } else { Side::Input };
            let list = node.boundary_ports();
            let mut ports: Vec<PortGeom> = list
                .iter()
                .map(|(pid, name)| {
                    let mut port = port(side, name);
                    port.key = group::port_key(*pid);
                    port.kind = kinds.boundary(id, *pid, Kinds::DEPTH);
                    port
                })
                .collect();
            for (pid, name) in &list {
                if show_controls(project, id, node, *pid) {
                    ports.extend(controls(*pid, name, list.len() == 1));
                }
            }
            let title = if is_input {
                "Group Input"
            } else {
                "Group Output"
            };
            Some((title.to_owned(), category, ports))
        }
        _ => None,
    }
}

/// Lays out a normal node: a header, then rows with inputs down the left and
/// outputs down the right. The two sides fill rows independently, except that
/// an input with a field (`has_field`) takes its whole row. A side with only
/// one port puts its socket on the header instead, unless `labelled` keeps
/// it on a row so its name is drawn. Returns the node's rectangle.
fn place_columns(
    origin: Pos2,
    ports: &mut [PortGeom],
    has_field: impl Fn(&PortGeom) -> bool,
    labelled: bool,
) -> Rect {
    let count = |side| ports.iter().filter(|p| p.side == side).count();
    let lone = |side, p: &PortGeom| !labelled && count(side) == 1 && !has_field(p);
    // The row each port takes, or None for the header.
    let mut rows: Vec<Option<usize>> = vec![None; ports.len()];
    let mut next = 0;
    // Rows with room for an output beside the input.
    let mut open = Vec::new();
    for (i, port) in ports.iter().enumerate() {
        if port.side == Side::Input && !lone(Side::Input, port) {
            rows[i] = Some(next);
            if !has_field(port) {
                open.push(next);
            }
            next += 1;
        }
    }
    let mut open = open.into_iter();
    for (i, port) in ports.iter().enumerate() {
        if port.side == Side::Output && !lone(Side::Output, port) {
            let row = open.next().unwrap_or_else(|| {
                next += 1;
                next - 1
            });
            rows[i] = Some(row);
        }
    }
    for (port, row) in ports.iter_mut().zip(rows) {
        let x = match port.side {
            Side::Input => origin.x,
            Side::Output => origin.x + NODE_WIDTH,
        };
        let (top, height) = match row {
            Some(row) => (
                origin.y + HEADER_HEIGHT + row as f32 * ROW_HEIGHT,
                ROW_HEIGHT,
            ),
            None => (origin.y, HEADER_HEIGHT),
        };
        port.in_header = row.is_none();
        let full = Rect::from_min_size(Pos2::new(origin.x, top), Vec2::new(NODE_WIDTH, height));
        // A socket on the header only takes its own half of the title bar.
        port.row = if port.in_header {
            let half = NODE_WIDTH / 2.0;
            match port.side {
                Side::Input => full.with_max_x(origin.x + half),
                Side::Output => full.with_min_x(origin.x + half),
            }
        } else {
            full
        };
        port.socket = Pos2::new(x, top + height / 2.0);
    }
    let height = HEADER_HEIGHT + next as f32 * ROW_HEIGHT + PADDING;
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
        port.in_header = false;
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
    fn inputs_go_down_the_left_and_a_lone_output_sits_on_the_title_bar() {
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
                (Side::Input, "gain"),
            ]
        );
        assert!(matches!(node.ports[2].kind, PortKind::Param(_)));
        assert_eq!(node.rect.min, Pos2::new(100.0, 50.0));
        // The output is the only one, so it has no row of its own.
        assert_eq!(
            node.rect.height(),
            HEADER_HEIGHT
                + 2.0 * ROW_HEIGHT
                + PADDING
                + crate::editor::body::height("noodle.util.gain")
        );
        let out = node.port(Side::Output, "out").unwrap();
        assert!(out.in_header);
        assert_eq!(out.socket.x, node.rect.right());
        assert_eq!(out.socket.y, node.rect.top() + HEADER_HEIGHT / 2.0);
        // Two inputs, so both keep their labels and rows.
        let input = node.port(Side::Input, "in").unwrap();
        assert!(!input.in_header);
        assert_eq!(input.socket.x, node.rect.left());
        assert!(input.row.contains(input.socket));
    }

    /// A mixer with `wired` of its inputs fed by their own oscillators.
    fn wired_mixer(
        project: &mut Project,
        history: &mut History,
        config: i64,
        wired: usize,
    ) -> NodeId {
        let config = noodle_core::Config::new().with("inputs", noodle_core::Value::Int(config));
        let mix = add(
            project,
            history,
            Node::new("noodle.util.mix").with_config(config),
        );
        for i in 1..=wired {
            let src = add(project, history, Node::new("noodle.osc.sine"));
            let wire = Command::Connect(Connection {
                from: Endpoint::new(src, "out"),
                to: Endpoint::new(mix, format!("in{i}")),
            });
            history.apply(project, wire).unwrap();
        }
        mix
    }

    #[test]
    fn outputs_share_rows_with_inputs_that_have_no_field() {
        let (mut project, mut history) = (Project::new(), History::new());
        let mix = wired_mixer(&mut project, &mut history, 3, 3);
        let scene = Scene::build(&project, &registry(), None);
        let node = scene.node(mix).unwrap();
        // Three wired inputs, each with a gain and a mute, and the spare,
        // and a lone output on the title bar: ten rows.
        assert_eq!(node.ports.iter().filter(|p| !p.in_header).count(), 10);
        assert_eq!(
            node.rect.height(),
            HEADER_HEIGHT + 10.0 * ROW_HEIGHT + PADDING
        );
    }

    #[test]
    fn a_wire_on_a_mixers_gain_keeps_that_input_shown() {
        let (mut project, mut history) = (Project::new(), History::new());
        let mix = wired_mixer(&mut project, &mut history, 4, 1);
        let lfo = add(&mut project, &mut history, Node::new("noodle.osc.sine"));
        let wire = Command::Connect(Connection {
            from: Endpoint::new(lfo, "out"),
            to: Endpoint::new(mix, "gain3"),
        });
        history.apply(&mut project, wire).unwrap();
        let scene = Scene::build(&project, &registry(), None);
        let node = scene.node(mix).unwrap();
        assert!(node.port(Side::Input, "gain3").is_some());
        assert!(scene.wires.iter().any(|w| w.connection.to.port == "gain3"));
    }

    #[test]
    fn a_mixer_shows_its_wired_inputs_and_one_spare() {
        let (mut project, mut history) = (Project::new(), History::new());
        // Five inputs in the config, two wired.
        let mix = wired_mixer(&mut project, &mut history, 5, 2);
        let scene = Scene::build(&project, &registry(), None);
        let inputs: Vec<_> = scene
            .node(mix)
            .unwrap()
            .ports
            .iter()
            .filter(|p| p.side == Side::Input)
            .map(|p| (p.key.as_str(), p.spare))
            .collect();
        // Each wired input is followed by its own gain and mute.
        assert_eq!(
            inputs,
            [
                ("in1", false),
                ("gain1", false),
                ("mute1", false),
                ("in2", false),
                ("gain2", false),
                ("mute2", false),
                ("in3", true)
            ]
        );
        // Nothing wired: just the spare.
        let bare = wired_mixer(&mut project, &mut history, 5, 0);
        let scene = Scene::build(&project, &registry(), None);
        let ports = &scene.node(bare).unwrap().ports;
        let inputs: Vec<_> = ports.iter().filter(|p| p.side == Side::Input).collect();
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].spare);
        assert_eq!(inputs[0].key, "in1");
    }

    #[test]
    fn a_track_input_is_named_after_its_group() {
        let (mut project, mut history) = (Project::new(), History::new());
        let mut next = 0;
        let (group, create) = noodle_core::group::create_track(None, Position::default(), || {
            next += 1;
            NodeId(next)
        });
        history.apply(&mut project, create).unwrap();
        let input = |project: &Project| {
            let scene = Scene::build(project, &registry(), Some(group));
            let id = project
                .graph()
                .children(Some(group))
                .find(|(_, n)| n.type_id == noodle_core::group::TRACK_INPUT)
                .map(|(id, _)| id)
                .unwrap();
            scene.node(id).unwrap().title.clone()
        };
        assert_eq!(input(&project), "Track Input");
        let name = Command::SetConfig {
            node: group,
            key: "name".into(),
            value: Some(noodle_core::Value::Text("Drums".into())),
        };
        history.apply(&mut project, name).unwrap();
        assert_eq!(input(&project), "Drums");
    }

    #[test]
    fn a_track_inputs_outputs_are_idle_until_a_clip_feeds_them() {
        use noodle_core::{Clip, Tick};
        let (mut project, mut history) = (Project::new(), History::new());
        let input = add(
            &mut project,
            &mut history,
            Node::new(noodle_core::group::TRACK_INPUT),
        );
        let idle = |project: &Project| {
            let scene = Scene::build(project, &registry(), None);
            let node = scene.node(input).unwrap();
            let get = |key| node.port(Side::Output, key).unwrap().idle;
            (get("audio"), get("midi"))
        };
        assert_eq!(idle(&project), (true, true));
        let id = project.new_clip_id();
        let clip = Clip::audio(input, Tick(0), "a.wav", 100);
        history
            .apply(&mut project, Command::AddClip { id, clip })
            .unwrap();
        // There are no MIDI clips yet, so midi stays idle.
        assert_eq!(idle(&project), (false, true));
    }

    #[test]
    fn boundary_ports_have_gain_and_mute_ports() {
        let (mut project, mut history) = (Project::new(), History::new());
        let group = add(&mut project, &mut history, Node::new(GROUP));
        let output = add(
            &mut project,
            &mut history,
            Node::new(GROUP_OUTPUT)
                .with_config(group::ports_config(&[(1, "out".into())]))
                .in_group(group),
        );
        let scene = Scene::build(&project, &registry(), Some(group));
        let node = scene.node(output).unwrap();
        for control in [group::GAIN, group::MUTE] {
            let key = group::control_key(control, 1);
            let port = node.port(Side::Input, &key).unwrap();
            assert!(matches!(port.kind, PortKind::Param(_)), "{key}");
        }
        assert!(node.port(Side::Input, "solo.1").is_none());
        // Its signal port is keyed by ID, and named for the group's output.
        assert_eq!(node.port(Side::Input, "p1").unwrap().name, "out");
    }

    #[test]
    fn a_boundary_node_has_a_spare_port_for_the_next_wire() {
        let (mut project, mut history) = (Project::new(), History::new());
        let group = add(&mut project, &mut history, Node::new(GROUP));
        let input = add(
            &mut project,
            &mut history,
            Node::new(GROUP_INPUT)
                .with_config(group::ports_config(&[(1, "a".into()), (3, "b".into())]))
                .in_group(group),
        );
        let scene = Scene::build(&project, &registry(), Some(group));
        let node = scene.node(input).unwrap();
        let spare = node.port(Side::Output, "p4").unwrap();
        assert!(spare.spare);
        assert_eq!(spare.name, "in1", "named as the group's next free input");
        assert!(node.port(Side::Output, "p3").is_some_and(|p| !p.spare));
    }

    /// A group with one input "a" (port 1) and one output "b" (port 1), and
    /// its two boundary nodes.
    fn passthrough(project: &mut Project, history: &mut History) -> (NodeId, NodeId, NodeId) {
        let group = add(project, history, Node::new(GROUP));
        let input = add(
            project,
            history,
            Node::new(GROUP_INPUT)
                .with_config(group::ports_config(&[(1, "a".into())]))
                .in_group(group),
        );
        let output = add(
            project,
            history,
            Node::new(GROUP_OUTPUT)
                .with_config(group::ports_config(&[(1, "b".into())]))
                .in_group(group),
        );
        (group, input, output)
    }

    fn kind_of(scene: &Scene, node: NodeId, side: Side, key: &str) -> PortKind {
        scene
            .node(node)
            .unwrap()
            .port(side, key)
            .unwrap()
            .kind
            .clone()
    }

    #[test]
    fn a_group_port_with_nothing_wired_takes_any_kind() {
        let (mut project, mut history) = (Project::new(), History::new());
        let (group, input, output) = passthrough(&mut project, &mut history);
        let inside = Scene::build(&project, &registry(), Some(group));
        let outside = Scene::build(&project, &registry(), None);
        assert_eq!(kind_of(&inside, input, Side::Output, "p1"), PortKind::Any);
        assert_eq!(kind_of(&inside, output, Side::Input, "p1"), PortKind::Any);
        assert_eq!(kind_of(&outside, group, Side::Input, "a"), PortKind::Any);
        // The spare ports take anything too.
        assert!(
            inside
                .node(input)
                .unwrap()
                .ports
                .iter()
                .any(|p| p.spare && p.kind == PortKind::Any)
        );
        assert!(PortKind::Any.feeds(&PortKind::Event));
        assert!(PortKind::Any.feeds(&PortKind::Audio));
        assert!(PortKind::Event.feeds(&PortKind::Any));
        assert!(PortKind::Audio.feeds(&PortKind::Any));
        assert!(!PortKind::Event.feeds(&PortKind::Audio));
    }

    #[test]
    fn a_group_port_becomes_the_kind_of_the_wire_on_either_side() {
        let (mut project, mut history) = (Project::new(), History::new());
        let (group, input, output) = passthrough(&mut project, &mut history);
        // Events wired in from outside make the port's far side events.
        let keys = add(
            &mut project,
            &mut history,
            Node::new("noodle.event.midi_in"),
        );
        history
            .apply(
                &mut project,
                Command::Connect(Connection {
                    from: Endpoint::new(keys, "out"),
                    to: Endpoint::new(group, "a"),
                }),
            )
            .unwrap();
        let inside = Scene::build(&project, &registry(), Some(group));
        assert_eq!(kind_of(&inside, input, Side::Output, "p1"), PortKind::Event);
        // Audio wired inside, at the output, makes it audio outside.
        let osc = add(
            &mut project,
            &mut history,
            Node::new("noodle.osc.sine").in_group(group),
        );
        history
            .apply(
                &mut project,
                Command::Connect(Connection {
                    from: Endpoint::new(osc, "out"),
                    to: Endpoint::new(output, "p1"),
                }),
            )
            .unwrap();
        let outside = Scene::build(&project, &registry(), None);
        assert_eq!(kind_of(&outside, group, Side::Output, "b"), PortKind::Audio);
        assert_eq!(kind_of(&outside, group, Side::Input, "a"), PortKind::Event);
        // Taking the wire away frees the port again.
        history
            .apply(
                &mut project,
                Command::Disconnect {
                    input: Endpoint::new(group, "a"),
                },
            )
            .unwrap();
        let inside = Scene::build(&project, &registry(), Some(group));
        assert_eq!(kind_of(&inside, input, Side::Output, "p1"), PortKind::Any);
    }

    #[test]
    fn a_group_port_takes_the_kind_of_what_it_feeds_inside() {
        let (mut project, mut history) = (Project::new(), History::new());
        let (group, input, _) = passthrough(&mut project, &mut history);
        let arp = add(
            &mut project,
            &mut history,
            Node::new("noodle.event.arpeggiator").in_group(group),
        );
        history
            .apply(
                &mut project,
                Command::Connect(Connection {
                    from: Endpoint::new(input, "p1"),
                    to: Endpoint::new(arp, "in"),
                }),
            )
            .unwrap();
        let outside = Scene::build(&project, &registry(), None);
        assert_eq!(kind_of(&outside, group, Side::Input, "a"), PortKind::Event);
    }

    #[test]
    fn a_lane_on_a_boundary_shows_as_a_wire_from_the_track_input() {
        use noodle_core::group::{GAIN, TRACK_INPUT, create_track};
        use noodle_core::{AutomationLane, AutomationPoint, Curve, Tick};
        let (mut project, mut history) = (Project::new(), History::new());
        let mut next = 0;
        let (group, create) = create_track(None, Position::default(), || {
            next += 1;
            NodeId(next)
        });
        history.apply(&mut project, create).unwrap();
        let children = |project: &Project, kind: &str| {
            project
                .graph()
                .children(Some(group))
                .find(|(_, n)| n.type_id == kind)
                .map(|(id, _)| id)
                .unwrap()
        };
        let (input, output) = (
            children(&project, TRACK_INPUT),
            children(&project, GROUP_OUTPUT),
        );
        let target = Endpoint::new(output, group::control_key(GAIN, 1));
        let id = project.new_lane_id();
        let point = AutomationPoint {
            tick: Tick(0),
            value: -6.0,
            curve: Curve::Linear,
        };
        let lane = AutomationLane::new(target.clone(), vec![point]);
        history
            .apply(&mut project, Command::AddLane { id, lane })
            .unwrap();
        let scene = Scene::build(&project, &registry(), Some(group));
        let wire = scene.wires.iter().find(|w| w.lane == Some(id)).unwrap();
        assert_eq!(wire.connection.from.node, input);
        assert_eq!(wire.connection.to, target);
        assert_eq!(wire.cut(), Command::RemoveLane { id });

        // A real wire into the port takes over; the lane waits behind it.
        let mix = add(&mut project, &mut history, Node::new("mix").in_group(group));
        let connection = Connection {
            from: Endpoint::new(mix, "out"),
            to: target.clone(),
        };
        history
            .apply(&mut project, Command::Connect(connection))
            .unwrap();
        let scene = Scene::build(&project, &registry(), Some(group));
        assert!(scene.wires.iter().all(|w| w.lane.is_none()));
        assert!(scene.wires.iter().any(|w| w.connection.to == target));
    }

    #[test]
    fn a_groups_gain_and_mute_show_on_one_port() {
        use noodle_core::group::{GAIN, MUTE, control_key};
        let (mut project, mut history) = (Project::new(), History::new());
        let group = add(&mut project, &mut history, Node::new(GROUP));
        let input = add(
            &mut project,
            &mut history,
            Node::new(GROUP_INPUT)
                .with_config(group::ports_config(&[(1, "a".into()), (2, "b".into())]))
                .in_group(group),
        );
        let output = add(
            &mut project,
            &mut history,
            Node::new(GROUP_OUTPUT)
                .with_config(group::ports_config(&[(1, "out".into())]))
                .in_group(group),
        );
        let shows = |scene: &Scene, id, port| {
            let node = scene.node(id).unwrap();
            [GAIN, MUTE]
                .iter()
                .all(|c| node.ports.iter().any(|p| p.key == control_key(c, port)))
        };
        let scene = Scene::build(&project, &registry(), Some(group));
        // The group's control port is its first output, so only that shows them.
        assert!(shows(&scene, output, 1));
        assert!(!shows(&scene, input, 1) && !shows(&scene, input, 2));

        // A port with gain set shows its controls too, so the value that
        // affects the sound stays visible.
        let set = Command::SetParam {
            node: input,
            key: control_key(GAIN, 2),
            value: Some(-3.0),
        };
        history.apply(&mut project, set).unwrap();
        let scene = Scene::build(&project, &registry(), Some(group));
        assert!(shows(&scene, input, 2));
        assert!(!shows(&scene, input, 1));
        // With several ports on the node, the controls say whose they are.
        let node = scene.node(input).unwrap();
        assert_eq!(node.port(Side::Input, "gain.2").unwrap().name, "B gain");
    }

    #[test]
    fn a_group_input_labels_its_port_on_a_row() {
        let (mut project, mut history) = (Project::new(), History::new());
        let group = add(&mut project, &mut history, Node::new(GROUP));
        let input = add(
            &mut project,
            &mut history,
            Node::new(GROUP_INPUT)
                .with_config(group::ports_config(&[(1, "a".into())]))
                .in_group(group),
        );
        let scene = Scene::build(&project, &registry(), Some(group));
        let port = scene
            .node(input)
            .unwrap()
            .port(Side::Output, &group::port_key(1))
            .unwrap();
        assert!(!port.in_header, "the name is drawn on its row");
    }

    #[test]
    fn a_group_has_a_spare_input_and_output() {
        let (mut project, mut history) = (Project::new(), History::new());
        let group = add(&mut project, &mut history, Node::new(GROUP));
        let scene = Scene::build(&project, &registry(), None);
        let spares: Vec<_> = scene
            .node(group)
            .unwrap()
            .ports
            .iter()
            .filter(|p| p.spare)
            .map(|p| (p.side, p.key.as_str()))
            .collect();
        assert_eq!(spares, [(Side::Input, "in1"), (Side::Output, "out1")]);
    }

    #[test]
    fn several_outputs_fill_the_right_column_beside_the_inputs() {
        let mut ports = vec![
            test_port(Side::Input, "a"),
            test_port(Side::Input, "b"),
            test_port(Side::Output, "x"),
            test_port(Side::Output, "y"),
            test_port(Side::Output, "z"),
        ];
        let rect = place_columns(Pos2::ZERO, &mut ports, |_| false, false);
        // Three rows, because the outputs outgrow the inputs on their own.
        assert_eq!(rect.height(), HEADER_HEIGHT + 3.0 * ROW_HEIGHT + PADDING);
        assert_eq!(ports[0].socket.y, ports[2].socket.y);
        assert_eq!(ports[1].socket.y, ports[3].socket.y);
        assert!(ports[4].socket.y > ports[3].socket.y);
    }

    #[test]
    fn a_field_takes_its_row_alone() {
        let mut ports = vec![
            test_port(Side::Input, "freq"),
            test_port(Side::Input, "phase"),
            test_port(Side::Output, "x"),
            test_port(Side::Output, "y"),
        ];
        // The first input has a field, so the first output goes beside the
        // second input instead.
        let rect = place_columns(Pos2::ZERO, &mut ports, |p| p.key == "freq", false);
        assert_eq!(rect.height(), HEADER_HEIGHT + 3.0 * ROW_HEIGHT + PADDING);
        assert_eq!(ports[2].socket.y, ports[1].socket.y);
        assert!(ports[3].socket.y > ports[1].socket.y);
        assert!(ports[0].row.width() == NODE_WIDTH);
    }

    #[test]
    fn a_saved_port_order_overrides_the_nodes_own() {
        let (mut project, mut history) = (Project::new(), History::new());
        let id = project.new_node_id();
        let mut node = Node::new("noodle.util.gain");
        node.port_order = vec!["gain".into()];
        history
            .apply(&mut project, Command::AddNode { id, node })
            .unwrap();
        let scene = Scene::build(&project, &registry(), None);
        let keys: Vec<_> = scene
            .node(id)
            .unwrap()
            .ports
            .iter()
            .map(|p| p.key.as_str())
            .collect();
        // Named ports first, the rest in their own order. The columns are
        // separate, so only the order within a side shows.
        assert_eq!(keys, ["gain", "out", "in"]);
    }

    #[test]
    fn note_inputs_are_listed_before_the_other_inputs() {
        let (mut project, mut history) = (Project::new(), History::new());
        let id = add(&mut project, &mut history, Node::new("noodle.poly.voices"));
        let scene = Scene::build(&project, &registry(), None);
        let inputs: Vec<_> = scene
            .node(id)
            .unwrap()
            .ports
            .iter()
            .filter(|p| p.side == Side::Input)
            .map(|p| p.key.as_str())
            .collect();
        assert_eq!(inputs, ["in", "tail", "steal_fade", "busy"]);
    }

    fn test_port(side: Side, key: &str) -> PortGeom {
        PortGeom {
            key: key.into(),
            name: key.into(),
            side,
            kind: PortKind::Audio,
            socket: Pos2::ZERO,
            row: Rect::NOTHING,
            in_header: false,
            spare: false,
            idle: false,
        }
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
