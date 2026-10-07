//! The node editor: the canvas in the middle of the window.
//!
//! It's a custom canvas rather than `egui-snarl`, so it can follow Blender's
//! interactions closely:
//!
//! | Input | Does |
//! |---|---|
//! | Middle-drag, trackpad scroll | Pan |
//! | Mouse wheel, pinch, Ctrl+scroll | Zoom around the pointer |
//! | Home | Fit everything in view |
//! | Click, Shift+click | Select, or toggle |
//! | Drag on empty space | Box select (Shift adds, Ctrl removes) |
//! | Drag a node or frame title | Move the selection |
//! | Drag from a socket | Connect; from a connected input, pick the wire up |
//! | Ctrl+right-drag | Cut the wires crossed |
//! | Shift+right-drag | Add a reroute on each wire crossed |
//! | Shift+A | Search for a node to add |
//! | X, Delete | Delete the selection |
//! | Shift+D | Duplicate the selection |
//! | A, Alt+A | Select all, select none |
//! | Ctrl+J | Put the selected nodes in a new frame |
//! | Double-click a frame title, F2 | Rename the frame |
//!
//! Each frame the editor rebuilds where everything goes from the project (see
//! [`layout`]), handles input against that, then draws it. It never changes
//! the project itself: it returns [`Edit`]s for the session to apply.

mod body;
mod draw;
mod layout;
mod search;
mod view;
mod wire;

use std::collections::{BTreeMap, BTreeSet};

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Sense, Vec2};
use noodle_core::{Command, Connection, Endpoint, Frame, FrameId, Node, NodeId, Position, Project};
use noodle_engine::{Diagnostic, Location, Registry};
use noodle_nodes::REROUTE_ID;

use crate::session::{Edit, Session};
use crate::theme;
use layout::{FRAME_HEADER_HEIGHT, PortKind, REROUTE_SIZE, Scene, Side};
use search::{Choice, Search};
use view::Transform;
pub use view::View;

/// How close to a socket, in screen points, counts as on it.
const SOCKET_REACH: f32 = 9.0;
/// How close to a wire, in screen points, counts as on it.
const WIRE_REACH: f32 = 6.0;
/// How far duplicates are placed from the originals.
const DUPLICATE_OFFSET: Vec2 = Vec2::new(30.0, 30.0);
/// Space around nodes when framing them.
const FRAME_MARGIN: f32 = 20.0;
const MIN_FRAME_SIZE: Vec2 = Vec2::new(80.0, 60.0);

/// What the editor remembers between frames.
pub struct EditorState {
    pub selected: BTreeSet<NodeId>,
    /// The node the properties panel shows. Always one of `selected`.
    pub active: Option<NodeId>,
    pub selected_frames: BTreeSet<FrameId>,
    pub view: View,
    /// Where the canvas was last drawn, on screen.
    canvas: Rect,
    gesture: Gesture,
    search: Option<Search>,
    rename: Option<Rename>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            selected: BTreeSet::new(),
            active: None,
            selected_frames: BTreeSet::new(),
            view: View::default(),
            canvas: Rect::NOTHING,
            gesture: Gesture::Idle,
            search: None,
            rename: None,
        }
    }
}

impl EditorState {
    /// Forgets nodes and frames that no longer exist, e.g. after an undo.
    pub fn retain_existing(&mut self, session: &Session) {
        self.retain_in(session.project());
    }

    fn retain_in(&mut self, project: &Project) {
        let graph = project.graph();
        self.selected.retain(|&id| graph.node(id).is_some());
        if self.active.is_some_and(|id| !self.selected.contains(&id)) {
            self.active = None;
        }
        self.selected_frames
            .retain(|&id| project.frame(id).is_some());
    }

    fn select_only(&mut self, nodes: impl IntoIterator<Item = NodeId>) {
        self.selected = nodes.into_iter().collect();
        self.active = self.selected.iter().next_back().copied();
        self.selected_frames.clear();
    }

    fn clear_selection(&mut self) {
        self.selected.clear();
        self.selected_frames.clear();
        self.active = None;
    }
}

/// A drag in progress.
#[derive(Default)]
enum Gesture {
    #[default]
    Idle,
    Pan,
    /// Moving nodes and frames. Positions are where each started.
    Move {
        start: Pos2,
        nodes: Vec<(NodeId, Pos2)>,
        frames: Vec<(FrameId, Frame)>,
        moved: bool,
    },
    /// Dragging a new wire from `anchor`, or one picked up off the input
    /// `detached`, whose other end stays at `anchor`.
    Link {
        anchor: Endpoint,
        side: Side,
        kind: PortKind,
        detached: Option<Endpoint>,
    },
    BoxSelect {
        start: Pos2,
        mode: BoxMode,
    },
    /// A stroke across wires: to cut them, or to put reroutes on them.
    Stroke {
        points: Vec<Pos2>,
        action: StrokeAction,
    },
    Resize {
        id: FrameId,
        start: Pos2,
        original: Frame,
        moved: bool,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum BoxMode {
    Replace,
    Add,
    Remove,
}

#[derive(Clone, Copy, PartialEq)]
enum StrokeAction {
    Cut,
    Reroute,
}

/// Renaming a frame in place.
struct Rename {
    id: FrameId,
    label: String,
    focused: bool,
}

/// What's under a point on the canvas, topmost first.
#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Port(Endpoint, Side),
    Node(NodeId),
    FrameHandle(FrameId),
    FrameHeader(FrameId),
    Nothing,
}

/// Draws the editor and returns the edits the user made.
pub fn show(ui: &mut egui::Ui, state: &mut EditorState, session: &Session) -> Vec<Edit> {
    let mut new_node_id = || session.new_node_id();
    show_project(
        ui,
        state,
        Inputs {
            project: session.project(),
            registry: session.registry(),
            diagnostics: session.diagnostics(),
            new_node_id: &mut new_node_id,
        },
    )
}

/// What the editor reads: the parts of a [`Session`] it uses.
struct Inputs<'a> {
    project: &'a Project,
    registry: &'a Registry,
    diagnostics: &'a [Diagnostic],
    new_node_id: &'a mut dyn FnMut() -> NodeId,
}

/// Problems from compiling, by where they belong.
#[derive(Default)]
struct Problems {
    nodes: BTreeMap<NodeId, Vec<String>>,
    wires: BTreeMap<Endpoint, Vec<String>>,
}

impl Problems {
    fn new(diagnostics: &[Diagnostic]) -> Self {
        let mut problems = Self::default();
        for diagnostic in diagnostics {
            let text = diagnostic.problem.to_string();
            match &diagnostic.location {
                Location::Node(id) => problems.nodes.entry(*id).or_default().push(text),
                Location::Wire(input) => {
                    problems.wires.entry(input.clone()).or_default().push(text)
                }
            }
        }
        problems
    }
}

/// One frame of the editor, with everything it needs worked out.
struct Frame_<'a> {
    project: &'a Project,
    scene: Scene,
    t: Transform,
    /// Indices into `scene.nodes`, bottom first. Selected nodes are on top.
    order: Vec<usize>,
    /// Indices into `scene.frames`, bottom first. Smaller frames are on top,
    /// so a frame inside another can still be grabbed.
    frame_order: Vec<usize>,
}

fn show_project(ui: &mut egui::Ui, state: &mut EditorState, mut inputs: Inputs<'_>) -> Vec<Edit> {
    let project = inputs.project;
    state.retain_in(project);
    let (canvas, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    state.canvas = canvas;
    let scene = Scene::build(project, inputs.registry);
    let problems = Problems::new(inputs.diagnostics);
    let mut edits = Vec::new();

    navigate(ui, state, canvas, &response, &scene);

    let mut order: Vec<usize> = (0..scene.nodes.len()).collect();
    order.sort_by_key(|&i| state.selected.contains(&scene.nodes[i].id));
    let mut frame_order: Vec<usize> = (0..scene.frames.len()).collect();
    frame_order.sort_by(|&a, &b| {
        let area = |i: usize| scene.frames[i].rect.area();
        area(b).total_cmp(&area(a))
    });
    let f = Frame_ {
        project,
        t: state.view.on(canvas),
        scene,
        order,
        frame_order,
    };

    pointer(ui, state, &f, &response, &mut inputs, &mut edits);
    if response.contains_pointer()
        && state.search.is_none()
        && state.rename.is_none()
        && !ui.ctx().egui_wants_keyboard_input()
    {
        keyboard(ui, state, &f, canvas, &mut inputs, &mut edits);
    }

    let painter = ui.painter_at(canvas);
    let pointer_pos = ui.input(|i| i.pointer.latest_pos());
    draw::canvas(&painter, canvas, &f.t);
    draw::frames(&painter, &f, state);
    let detached = match &state.gesture {
        Gesture::Link { detached, .. } => detached.as_ref(),
        _ => None,
    };
    draw::wires(&painter, &f, state, &problems, detached);
    draw::nodes(&painter, &f, state, &problems);
    if let Some(p) = pointer_pos {
        draw::gesture(&painter, &f, &state.gesture, p);
    }

    if f.scene.nodes.is_empty() && f.scene.frames.is_empty() {
        let hint = egui::Label::new(
            egui::RichText::new("No nodes yet. Shift+A adds one.").color(theme::editor::TEXT_WEAK),
        )
        .selectable(false);
        ui.put(
            Rect::from_center_size(canvas.center(), Vec2::new(300.0, 20.0)),
            hint,
        );
    }
    popups(ui, state, &f, &mut inputs, &mut edits);

    if response.hovered()
        && matches!(state.gesture, Gesture::Idle)
        && let Some(p) = pointer_pos
        && let Some(text) = hover_problem(&f, &problems, p)
    {
        response.on_hover_text_at_pointer(text);
    }
    edits
}

/// Panning and zooming.
fn navigate(
    ui: &egui::Ui,
    state: &mut EditorState,
    canvas: Rect,
    response: &egui::Response,
    scene: &Scene,
) {
    if response.contains_pointer() {
        let (events, zoom, pointer) = ui.input(|i| {
            (
                i.events.clone(),
                i.zoom_delta(),
                i.pointer.latest_pos().unwrap_or(canvas.center()),
            )
        });
        for event in events {
            if let Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } = event
            {
                if modifiers.command {
                    continue; // egui turns it into zoom_delta
                }
                match unit {
                    // A trackpad: scrolling pans.
                    MouseWheelUnit::Point => state.view.pan(delta),
                    // A mouse wheel: zooms, as in Blender.
                    MouseWheelUnit::Line | MouseWheelUnit::Page => {
                        state
                            .view
                            .zoom_around(canvas, pointer, 1.15_f32.powf(delta.y));
                    }
                }
            }
        }
        if zoom != 1.0 {
            state.view.zoom_around(canvas, pointer, zoom);
        }
    }
    if response.dragged_by(PointerButton::Middle) {
        state.view.pan(response.drag_delta());
    }
    if response.contains_pointer()
        && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Home))
        && let Some(bounds) = scene.bounds()
    {
        state.view.fit(canvas, bounds);
    }
}

fn hit(f: &Frame_<'_>, p: Pos2) -> Hit {
    for &i in f.order.iter().rev() {
        let node = &f.scene.nodes[i];
        for port in &node.ports {
            if f.t.to_screen(port.socket).distance(p) <= SOCKET_REACH {
                return Hit::Port(Endpoint::new(node.id, port.key.clone()), port.side);
            }
        }
    }
    let g = f.t.to_graph(p);
    for &i in f.order.iter().rev() {
        let node = &f.scene.nodes[i];
        if node.rect.contains(g) {
            return Hit::Node(node.id);
        }
    }
    for &i in f.frame_order.iter().rev() {
        let frame = &f.scene.frames[i];
        if frame.handle().contains(g) {
            return Hit::FrameHandle(frame.id);
        }
        if frame.header().contains(g) {
            return Hit::FrameHeader(frame.id);
        }
    }
    Hit::Nothing
}

/// Where a wire being dragged from `anchor` would connect if dropped at `p`:
/// a compatible port on another node, on the other side. Dropping anywhere on
/// a node picks its first free compatible port.
fn drop_target(
    f: &Frame_<'_>,
    p: Pos2,
    anchor: &Endpoint,
    side: Side,
    kind: &PortKind,
) -> Option<Endpoint> {
    let g = f.t.to_graph(p);
    let compatible = |other: &PortKind| match side {
        Side::Output => kind.feeds(other),
        Side::Input => other.feeds(kind),
    };
    for &i in f.order.iter().rev() {
        let node = &f.scene.nodes[i];
        if node.id == anchor.node {
            continue;
        }
        let mut candidates = node
            .ports
            .iter()
            .filter(|port| port.side != side && compatible(&port.kind));
        if let Some(port) = candidates.clone().find(|port| {
            port.row.contains(g) || f.t.to_screen(port.socket).distance(p) <= SOCKET_REACH
        }) {
            return Some(Endpoint::new(node.id, port.key.clone()));
        }
        if node.rect.contains(g) {
            let graph = f.project.graph();
            let free = |port: &&layout::PortGeom| {
                port.side == Side::Output
                    || graph
                        .source(&Endpoint::new(node.id, port.key.clone()))
                        .is_none()
            };
            let port = candidates
                .clone()
                .find(free)
                .or_else(|| candidates.next())?;
            return Some(Endpoint::new(node.id, port.key.clone()));
        }
    }
    None
}

fn pointer(
    ui: &egui::Ui,
    state: &mut EditorState,
    f: &Frame_<'_>,
    response: &egui::Response,
    inputs: &mut Inputs<'_>,
    edits: &mut Vec<Edit>,
) {
    let (modifiers, origin, latest) = ui.input(|i| {
        (
            i.modifiers,
            i.pointer.press_origin(),
            i.pointer.latest_pos(),
        )
    });

    if response.drag_started() {
        let start = origin.or(latest).unwrap_or_default();
        state.gesture = if response.dragged_by(PointerButton::Middle) {
            Gesture::Pan
        } else if response.dragged_by(PointerButton::Secondary) {
            let action = if modifiers.command {
                Some(StrokeAction::Cut)
            } else if modifiers.shift {
                Some(StrokeAction::Reroute)
            } else {
                None
            };
            action.map_or(Gesture::Idle, |action| Gesture::Stroke {
                points: vec![f.t.to_graph(start)],
                action,
            })
        } else if response.dragged_by(PointerButton::Primary) {
            start_primary_drag(state, f, start, modifiers)
        } else {
            Gesture::Idle
        };
    }

    if let Some(p) = latest
        && response.dragged()
    {
        drag(state, f, p, edits);
    }

    if response.drag_stopped() {
        let gesture = std::mem::take(&mut state.gesture);
        if let Some(p) = latest {
            finish(state, f, gesture, p, inputs, edits);
        }
    }

    if response.clicked_by(PointerButton::Primary)
        && let Some(p) = latest
    {
        click(state, f, p, modifiers);
    }
    if response.double_clicked_by(PointerButton::Primary)
        && let Some(p) = latest
        && let Hit::FrameHeader(id) = hit(f, p)
    {
        start_rename(state, f.project, id);
    }
}

fn start_primary_drag(
    state: &mut EditorState,
    f: &Frame_<'_>,
    start: Pos2,
    modifiers: Modifiers,
) -> Gesture {
    let graph = f.project.graph();
    match hit(f, start) {
        Hit::Port(endpoint, side) => {
            let Some(port) = f.scene.port(&endpoint, side) else {
                return Gesture::Idle;
            };
            // Dragging from a connected input picks the wire up by that end.
            if side == Side::Input
                && let Some(source) = graph.source(&endpoint)
                && let Some(source_port) = f.scene.port(source, Side::Output)
            {
                return Gesture::Link {
                    anchor: source.clone(),
                    side: Side::Output,
                    kind: source_port.kind.clone(),
                    detached: Some(endpoint),
                };
            }
            if port.kind == PortKind::Unknown {
                return Gesture::Idle;
            }
            Gesture::Link {
                anchor: endpoint,
                side,
                kind: port.kind.clone(),
                detached: None,
            }
        }
        Hit::Node(id) => {
            if !state.selected.contains(&id) {
                if modifiers.shift {
                    state.selected.insert(id);
                } else {
                    state.select_only([id]);
                }
            }
            state.active = Some(id);
            start_move(state, f, start)
        }
        Hit::FrameHeader(id) => {
            if !state.selected_frames.contains(&id) {
                if !modifiers.shift {
                    state.clear_selection();
                }
                state.selected_frames.insert(id);
            }
            start_move(state, f, start)
        }
        Hit::FrameHandle(id) => match f.project.frame(id) {
            Some(frame) => Gesture::Resize {
                id,
                start: f.t.to_graph(start),
                original: frame.clone(),
                moved: false,
            },
            None => Gesture::Idle,
        },
        Hit::Nothing => Gesture::BoxSelect {
            start: f.t.to_graph(start),
            mode: if modifiers.shift {
                BoxMode::Add
            } else if modifiers.command {
                BoxMode::Remove
            } else {
                BoxMode::Replace
            },
        },
    }
}

/// Moves the selection, and everything inside the selected frames.
fn start_move(state: &EditorState, f: &Frame_<'_>, start: Pos2) -> Gesture {
    let mut nodes = state.selected.clone();
    let mut frames = state.selected_frames.clone();
    for frame in f
        .scene
        .frames
        .iter()
        .filter(|fr| state.selected_frames.contains(&fr.id))
    {
        nodes.extend(
            f.scene
                .nodes
                .iter()
                .filter(|n| frame.rect.contains_rect(n.rect))
                .map(|n| n.id),
        );
        frames.extend(
            f.scene
                .frames
                .iter()
                .filter(|other| frame.rect.contains_rect(other.rect))
                .map(|other| other.id),
        );
    }
    Gesture::Move {
        start: f.t.to_graph(start),
        nodes: nodes
            .into_iter()
            .filter_map(|id| Some((id, f.scene.node(id)?.rect.min)))
            .collect(),
        frames: frames
            .into_iter()
            .filter_map(|id| Some((id, f.project.frame(id)?.clone())))
            .collect(),
        moved: false,
    }
}

fn drag(state: &mut EditorState, f: &Frame_<'_>, p: Pos2, edits: &mut Vec<Edit>) {
    let g = f.t.to_graph(p);
    match &mut state.gesture {
        Gesture::Move {
            start,
            nodes,
            frames,
            moved,
        } => {
            let delta = g - *start;
            if delta == Vec2::ZERO && !*moved {
                return;
            }
            *moved = true;
            let mut commands: Vec<Command> = nodes
                .iter()
                .map(|&(node, origin)| Command::MoveNode {
                    node,
                    position: position(origin + delta),
                })
                .collect();
            commands.extend(frames.iter().map(|(id, frame)| Command::SetFrame {
                id: *id,
                frame: Frame {
                    position: position(Pos2::new(frame.position.x, frame.position.y) + delta),
                    ..frame.clone()
                },
            }));
            edits.push(Edit::Drag(Command::Batch(commands)));
        }
        Gesture::Resize {
            id,
            start,
            original,
            moved,
        } => {
            let delta = g - *start;
            *moved = true;
            edits.push(Edit::Drag(Command::SetFrame {
                id: *id,
                frame: Frame {
                    width: (original.width + delta.x).max(MIN_FRAME_SIZE.x),
                    height: (original.height + delta.y).max(MIN_FRAME_SIZE.y),
                    ..original.clone()
                },
            }));
        }
        Gesture::Stroke { points, .. } => {
            let far_enough = points
                .last()
                .is_none_or(|&last| f.t.to_screen(last).distance(p) > 3.0);
            if far_enough {
                points.push(g);
            }
        }
        Gesture::Idle | Gesture::Pan | Gesture::Link { .. } | Gesture::BoxSelect { .. } => {}
    }
}

fn finish(
    state: &mut EditorState,
    f: &Frame_<'_>,
    gesture: Gesture,
    p: Pos2,
    inputs: &mut Inputs<'_>,
    edits: &mut Vec<Edit>,
) {
    match gesture {
        Gesture::Move { moved, .. } | Gesture::Resize { moved, .. } => {
            if moved {
                edits.push(Edit::EndDrag);
            }
        }
        Gesture::Link {
            anchor,
            side,
            kind,
            detached,
        } => {
            let target = drop_target(f, p, &anchor, side, &kind);
            let connect = |target: Endpoint| {
                Command::Connect(match side {
                    Side::Output => Connection {
                        from: anchor.clone(),
                        to: target,
                    },
                    Side::Input => Connection {
                        from: target,
                        to: anchor.clone(),
                    },
                })
            };
            let command = match (target, detached) {
                (Some(target), Some(detached)) if target == detached => None,
                (Some(target), Some(detached)) => Some(Command::Batch(vec![
                    Command::Disconnect { input: detached },
                    connect(target),
                ])),
                (Some(target), None) => Some(connect(target)),
                (None, Some(detached)) => Some(Command::Disconnect { input: detached }),
                (None, None) => None,
            };
            edits.extend(command.map(Edit::Apply));
        }
        Gesture::BoxSelect { start, mode } => {
            let area = Rect::from_two_pos(start, f.t.to_graph(p));
            let nodes = f
                .scene
                .nodes
                .iter()
                .filter(|n| n.rect.intersects(area))
                .map(|n| n.id);
            let frames = f
                .scene
                .frames
                .iter()
                .filter(|fr| area.contains_rect(fr.rect))
                .map(|fr| fr.id);
            match mode {
                BoxMode::Replace => {
                    state.select_only(nodes);
                    state.selected_frames = frames.collect();
                }
                BoxMode::Add => {
                    state.selected.extend(nodes);
                    state.selected_frames.extend(frames);
                }
                BoxMode::Remove => {
                    for id in nodes {
                        state.selected.remove(&id);
                    }
                    for id in frames {
                        state.selected_frames.remove(&id);
                    }
                }
            }
            if state.active.is_none_or(|id| !state.selected.contains(&id)) {
                state.active = state.selected.iter().next_back().copied();
            }
        }
        Gesture::Stroke { points, action } => {
            let commands = stroke(f, &points, action, inputs);
            if !commands.is_empty() {
                edits.push(Edit::Apply(Command::Batch(commands)));
            }
        }
        Gesture::Idle | Gesture::Pan => {}
    }
}

/// The commands for a stroke across wires.
fn stroke(
    f: &Frame_<'_>,
    points: &[Pos2],
    action: StrokeAction,
    inputs: &mut Inputs<'_>,
) -> Vec<Command> {
    let mut commands = Vec::new();
    for wire in &f.scene.wires {
        let line = wire::flatten(wire::curve(wire.from, wire.to));
        let Some(at) = wire::first_crossing(&line, points) else {
            continue;
        };
        let input = wire.connection.to.clone();
        match action {
            StrokeAction::Cut => commands.push(Command::Disconnect { input }),
            StrokeAction::Reroute => {
                let id = (inputs.new_node_id)();
                let corner = at - REROUTE_SIZE / 2.0;
                commands.extend([
                    Command::AddNode {
                        id,
                        node: Node::new(REROUTE_ID).at(corner.x, corner.y),
                    },
                    Command::Connect(Connection {
                        from: wire.connection.from.clone(),
                        to: Endpoint::new(id, "in"),
                    }),
                    // Replaces the old wire.
                    Command::Connect(Connection {
                        from: Endpoint::new(id, "out"),
                        to: input,
                    }),
                ]);
            }
        }
    }
    commands
}

fn click(state: &mut EditorState, f: &Frame_<'_>, p: Pos2, modifiers: Modifiers) {
    match hit(f, p) {
        Hit::Node(id) | Hit::Port(Endpoint { node: id, .. }, _) => {
            if modifiers.shift {
                if !state.selected.remove(&id) {
                    state.selected.insert(id);
                    state.active = Some(id);
                } else if state.active == Some(id) {
                    state.active = state.selected.iter().next_back().copied();
                }
            } else {
                state.select_only([id]);
            }
        }
        Hit::FrameHeader(id) | Hit::FrameHandle(id) => {
            if modifiers.shift {
                if !state.selected_frames.remove(&id) {
                    state.selected_frames.insert(id);
                }
            } else {
                state.clear_selection();
                state.selected_frames.insert(id);
            }
        }
        Hit::Nothing => {
            if !modifiers.shift {
                state.clear_selection();
            }
        }
    }
}

fn keyboard(
    ui: &egui::Ui,
    state: &mut EditorState,
    f: &Frame_<'_>,
    canvas: Rect,
    inputs: &mut Inputs<'_>,
    edits: &mut Vec<Edit>,
) {
    // Exact modifiers, so A doesn't also fire on Shift+A.
    let pressed = |modifiers: Modifiers, key: Key| {
        ui.input_mut(|i| {
            let hit = i.events.iter().any(|e| {
                matches!(e, Event::Key { key: k, pressed: true, modifiers: m, .. }
                    if *k == key && m.matches_exact(modifiers))
            });
            if hit {
                i.consume_key(modifiers, key);
            }
            hit
        })
    };
    let pointer = ui
        .input(|i| i.pointer.latest_pos())
        .unwrap_or(canvas.center());

    if pressed(Modifiers::SHIFT, Key::A) {
        state.search = Some(Search::new(f.t.to_graph(pointer), pointer));
    }
    if pressed(Modifiers::NONE, Key::X) || pressed(Modifiers::NONE, Key::Delete) {
        let commands: Vec<Command> = state
            .selected
            .iter()
            .map(|&id| Command::RemoveNode { id })
            .chain(
                state
                    .selected_frames
                    .iter()
                    .map(|&id| Command::RemoveFrame { id }),
            )
            .collect();
        if !commands.is_empty() {
            edits.push(Edit::Apply(Command::Batch(commands)));
        }
        state.clear_selection();
    }
    if pressed(Modifiers::SHIFT, Key::D) {
        duplicate(state, f.project, inputs, edits);
    }
    if pressed(Modifiers::NONE, Key::A) {
        state.select_only(f.scene.nodes.iter().map(|n| n.id));
        state.selected_frames = f.scene.frames.iter().map(|fr| fr.id).collect();
    }
    if pressed(Modifiers::ALT, Key::A) {
        state.clear_selection();
    }
    if pressed(Modifiers::COMMAND, Key::J) {
        frame_selection(state, f, edits);
    }
    if pressed(Modifiers::NONE, Key::F2)
        && state.selected_frames.len() == 1
        && let Some(&id) = state.selected_frames.first()
    {
        start_rename(state, f.project, id);
    }
}

fn duplicate(
    state: &mut EditorState,
    project: &Project,
    inputs: &mut Inputs<'_>,
    edits: &mut Vec<Edit>,
) {
    let graph = project.graph();
    let mut copies = BTreeMap::new();
    let mut commands = Vec::new();
    for &id in &state.selected {
        let Some(node) = graph.node(id) else { continue };
        let copy = (inputs.new_node_id)();
        copies.insert(id, copy);
        let mut node = node.clone();
        node.position = position(Pos2::new(node.position.x, node.position.y) + DUPLICATE_OFFSET);
        commands.push(Command::AddNode { id: copy, node });
    }
    // Wires between the copies, but not into them from outside, as in Blender.
    for connection in graph.connections() {
        if let (Some(&from), Some(&to)) = (
            copies.get(&connection.from.node),
            copies.get(&connection.to.node),
        ) {
            commands.push(Command::Connect(Connection {
                from: Endpoint::new(from, connection.from.port),
                to: Endpoint::new(to, connection.to.port),
            }));
        }
    }
    let mut next_frame = project.next_frame_id().0;
    let mut frame_copies = BTreeSet::new();
    for &id in &state.selected_frames {
        let Some(frame) = project.frame(id) else {
            continue;
        };
        let copy = FrameId(next_frame);
        next_frame += 1;
        frame_copies.insert(copy);
        commands.push(Command::AddFrame {
            id: copy,
            frame: Frame {
                position: position(
                    Pos2::new(frame.position.x, frame.position.y) + DUPLICATE_OFFSET,
                ),
                ..frame.clone()
            },
        });
    }
    if commands.is_empty() {
        return;
    }
    edits.push(Edit::Apply(Command::Batch(commands)));
    let active = state.active.and_then(|id| copies.get(&id).copied());
    state.select_only(copies.into_values());
    state.active = active.or(state.active);
    state.selected_frames = frame_copies;
}

/// Puts a new frame around the selected nodes.
fn frame_selection(state: &mut EditorState, f: &Frame_<'_>, edits: &mut Vec<Edit>) {
    let Some(bounds) = state
        .selected
        .iter()
        .filter_map(|&id| f.scene.node(id))
        .map(|n| n.rect)
        .reduce(Rect::union)
    else {
        return;
    };
    let rect = Rect::from_min_max(
        bounds.min - Vec2::new(FRAME_MARGIN, FRAME_MARGIN + FRAME_HEADER_HEIGHT),
        bounds.max + Vec2::splat(FRAME_MARGIN),
    );
    let id = f.project.next_frame_id();
    edits.push(Edit::Apply(Command::AddFrame {
        id,
        frame: Frame {
            label: "Frame".into(),
            position: position(rect.min),
            width: rect.width(),
            height: rect.height(),
        },
    }));
    state.selected_frames = BTreeSet::from([id]);
}

fn start_rename(state: &mut EditorState, project: &Project, id: FrameId) {
    if let Some(frame) = project.frame(id) {
        state.rename = Some(Rename {
            id,
            label: frame.label.clone(),
            focused: false,
        });
    }
}

/// The add-node search and the frame-renaming box.
fn popups(
    ui: &egui::Ui,
    state: &mut EditorState,
    f: &Frame_<'_>,
    inputs: &mut Inputs<'_>,
    edits: &mut Vec<Edit>,
) {
    if let Some(search) = &mut state.search {
        match search.show(ui.ctx(), inputs.registry) {
            search::Outcome::Open => {}
            search::Outcome::Closed => state.search = None,
            search::Outcome::Chosen(choice) => {
                let at = search.at;
                state.search = None;
                match choice {
                    Choice::Node(type_id) => {
                        let id = (inputs.new_node_id)();
                        edits.push(Edit::Apply(Command::AddNode {
                            id,
                            node: Node::new(type_id).at(at.x, at.y),
                        }));
                        state.select_only([id]);
                    }
                    Choice::Frame => {
                        let id = f.project.next_frame_id();
                        edits.push(Edit::Apply(Command::AddFrame {
                            id,
                            frame: Frame {
                                label: "Frame".into(),
                                position: position(at),
                                width: 300.0,
                                height: 200.0,
                            },
                        }));
                        state.clear_selection();
                        state.selected_frames.insert(id);
                    }
                }
            }
        }
    }

    let Some(rename) = &mut state.rename else {
        return;
    };
    let Some(frame) = f.project.frame(rename.id) else {
        state.rename = None;
        return;
    };
    let header = f.t.rect_to_screen(Rect::from_min_size(
        Pos2::new(frame.position.x, frame.position.y),
        Vec2::new(frame.width, FRAME_HEADER_HEIGHT),
    ));
    let id = egui::Id::new("noodle-editor-rename");
    let mut done = false;
    let mut cancelled = false;
    egui::Area::new(id)
        .fixed_pos(header.min + Vec2::new(4.0, 2.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut rename.label)
                    .desired_width((header.width() - 8.0).max(60.0)),
            );
            if !rename.focused {
                edit.request_focus();
                rename.focused = true;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                cancelled = true;
            } else if edit.lost_focus() {
                done = true;
            }
        });
    if cancelled {
        state.rename = None;
    } else if done {
        let label = std::mem::take(&mut rename.label);
        let id = rename.id;
        state.rename = None;
        if label != frame.label {
            edits.push(Edit::Apply(Command::SetFrame {
                id,
                frame: Frame {
                    label,
                    ..frame.clone()
                },
            }));
        }
    }
}

/// The problems with whatever's under the pointer, as tooltip text.
fn hover_problem(f: &Frame_<'_>, problems: &Problems, p: Pos2) -> Option<String> {
    let node = match hit(f, p) {
        Hit::Node(id) | Hit::Port(Endpoint { node: id, .. }, _) => Some(id),
        _ => None,
    };
    if let Some(id) = node {
        if let Some(texts) = problems.nodes.get(&id) {
            return Some(texts.join("\n"));
        }
        return f.scene.node(id)?.error.clone();
    }
    f.scene.wires.iter().find_map(|wire| {
        let texts = problems.wires.get(&wire.connection.to)?;
        let line: Vec<Pos2> = wire::flatten(wire::curve(wire.from, wire.to))
            .into_iter()
            .map(|g| f.t.to_screen(g))
            .collect();
        (wire::distance_to_polyline(p, &line) <= WIRE_REACH).then(|| texts.join("\n"))
    })
}

fn position(p: Pos2) -> Position {
    Position { x: p.x, y: p.y }
}

#[cfg(test)]
mod tests;
