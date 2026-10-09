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
//! | X, Delete, Backspace | Delete the selection |
//! | Double-click a wire | Break it |
//! | Ctrl/Cmd+C, X, V | Copy, cut and paste the selection, at the pointer |
//! | Shift+D | Duplicate the selection (a frame with what is in it) |
//! | A, Alt+A | Select all, select none |
//! | Ctrl+J | Put the selected nodes in a new frame (top level only) |
//! | Ctrl+G | Fold the selected nodes into a new group |
//! | Tab | Enter the selected group, or leave the current one |
//! | Double-click a group | Enter it |
//! | Double-click a frame title, F2 | Rename the frame |
//!
//! Each frame the editor rebuilds where everything goes from the project (see
//! [`layout`]), handles input against that, then draws it. It never changes
//! the project itself: it returns [`Edit`]s for the session to apply.

mod body;
mod draw;
mod layout;
mod params;
mod search;
mod view;
mod wire;

use std::collections::{BTreeMap, BTreeSet};

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Sense, Vec2};
use noodle_core::group::{self, GROUP, GROUP_INPUT, GROUP_OUTPUT};
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
/// Space left between a frame and its copy, which goes below it.
const DUPLICATE_FRAME_GAP: f32 = 20.0;
/// Space around nodes when framing them.
const FRAME_MARGIN: f32 = 20.0;
const MIN_FRAME_SIZE: Vec2 = Vec2::new(80.0, 60.0);

/// The canvas's widget ID. The app needs it to tell the canvas having focus
/// (so it receives Tab) from a text field having it.
pub fn canvas_id() -> egui::Id {
    egui::Id::new("noodle-editor-canvas")
}

/// What the editor remembers between frames.
pub struct EditorState {
    pub selected: BTreeSet<NodeId>,
    /// The node the properties panel shows. Always one of `selected`.
    pub active: Option<NodeId>,
    pub selected_frames: BTreeSet<FrameId>,
    pub view: View,
    /// The group being edited, or `None` for the top level. The editor shows
    /// only the nodes directly inside it.
    pub group: Option<NodeId>,
    /// The groups from the top down to `group`, as of the last frame, so if
    /// `group` vanishes (an undo) the editor can land on the nearest group
    /// that survives.
    path: Vec<NodeId>,
    /// Where each level's view was left, so leaving a group puts the view
    /// back. Keyed by the group, `None` for the top level.
    views: BTreeMap<Option<NodeId>, View>,
    /// Where the canvas was last drawn, on screen.
    canvas: Rect,
    gesture: Gesture,
    search: Option<Search>,
    rename: Option<Rename>,
    /// What meters and scopes show.
    bodies: body::Bodies,
    /// What Copy and Cut last took, for Paste.
    clipboard: Option<Clipboard>,
}

/// Nodes, the wires between them and frames, as copied.
#[derive(Clone, Default)]
struct Clipboard {
    nodes: Vec<(NodeId, Node)>,
    wires: Vec<Connection>,
    frames: Vec<Frame>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            selected: BTreeSet::new(),
            active: None,
            selected_frames: BTreeSet::new(),
            view: View::default(),
            group: None,
            path: Vec::new(),
            views: BTreeMap::new(),
            canvas: Rect::NOTHING,
            gesture: Gesture::Idle,
            search: None,
            rename: None,
            bodies: body::Bodies::default(),
            clipboard: None,
        }
    }
}

impl EditorState {
    /// Where a graph point is on screen, as of the last frame.
    #[cfg(test)]
    pub fn to_screen(&self, p: Pos2) -> Pos2 {
        self.view.on(self.canvas).to_screen(p)
    }

    /// Forgets nodes and frames that no longer exist, e.g. after an undo.
    pub fn retain_existing(&mut self, session: &Session) {
        self.retain_in(session.project());
    }

    fn retain_in(&mut self, project: &Project) {
        let graph = project.graph();
        // E.g. an undo removed the group being edited: land on the nearest
        // group that's left, or the top level.
        if self.group.is_some_and(|g| graph.node(g).is_none()) {
            self.group = self
                .path
                .iter()
                .rev()
                .copied()
                .find(|&g| graph.node(g).is_some());
            self.view = self.views.remove(&self.group).unwrap_or_default();
            self.gesture = Gesture::Idle;
            self.search = None;
            self.rename = None;
        }
        self.path = match self.group {
            Some(g) => {
                let mut path = graph.ancestors(g);
                path.push(g);
                path
            }
            None => Vec::new(),
        };
        self.views
            .retain(|group, _| group.is_none_or(|g| graph.node(g).is_some()));
        self.selected.retain(|&id| graph.node(id).is_some());
        if self.active.is_some_and(|id| !self.selected.contains(&id)) {
            self.active = None;
        }
        self.selected_frames
            .retain(|&id| project.frame(id).is_some());
    }

    /// Shows the inside of `group`, or the top level for `None`, with
    /// `select` selected (the group just left, say).
    fn enter(&mut self, group: Option<NodeId>, select: Option<NodeId>) {
        let left = std::mem::take(&mut self.view);
        self.views.insert(self.group, left);
        self.view = self.views.remove(&group).unwrap_or_default();
        self.group = group;
        self.gesture = Gesture::Idle;
        self.search = None;
        self.rename = None;
        self.clear_selection();
        if let Some(id) = select {
            self.select_only([id]);
        }
    }

    fn select_only(&mut self, nodes: impl IntoIterator<Item = NodeId>) {
        self.selected = nodes.into_iter().collect();
        self.active = self.selected.iter().next_back().copied();
        self.selected_frames.clear();
    }

    pub fn has_selection(&self) -> bool {
        !self.selected.is_empty() || !self.selected_frames.is_empty()
    }

    /// Deleting the selection as one edit, and forgetting the selection.
    /// `None` if nothing is selected.
    pub fn delete_selection(&mut self) -> Option<Edit> {
        let commands: Vec<Command> = self
            .selected
            .iter()
            .map(|&id| Command::RemoveNode { id })
            .chain(
                self.selected_frames
                    .iter()
                    .map(|&id| Command::RemoveFrame { id }),
            )
            .collect();
        self.clear_selection();
        (!commands.is_empty()).then_some(Edit::Apply(Command::Batch(commands)))
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
    /// Dragging a port up or down its column. `target` is where it would
    /// land among the side's other ports.
    Reorder {
        node: NodeId,
        side: Side,
        key: String,
        target: usize,
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
    let dt = ui.input(|i| i.stable_dt);
    state
        .bodies
        .update(session.telemetry(), session.project(), dt);
    // Meters and scopes move with the audio, so keep drawing while it plays.
    if session.is_playing() && state.bodies.is_live() {
        ui.ctx().request_repaint();
    }
    let mut new_node_id = || session.new_node_id();
    let mut new_frame_id = || session.new_frame_id();
    show_project(
        ui,
        state,
        Inputs {
            project: session.project(),
            registry: session.registry(),
            diagnostics: session.diagnostics(),
            new_node_id: &mut new_node_id,
            new_frame_id: &mut new_frame_id,
        },
    )
}

/// What the editor reads: the parts of a [`Session`] it uses.
struct Inputs<'a> {
    project: &'a Project,
    registry: &'a Registry,
    diagnostics: &'a [Diagnostic],
    new_node_id: &'a mut dyn FnMut() -> NodeId,
    new_frame_id: &'a mut dyn FnMut() -> FrameId,
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
    let (canvas, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    let response = ui.interact(canvas, canvas_id(), Sense::click_and_drag());
    state.canvas = canvas;
    // Tab is how egui moves focus, so the canvas has to hold focus, and say
    // it wants Tab, for the editor to be given it.
    if response.hovered() && ui.memory(|m| m.focused().is_none()) {
        response.request_focus();
    }
    if response.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: false,
                    vertical_arrows: false,
                    escape: false,
                },
            );
        });
    }
    let scene = Scene::build(project, inputs.registry, state.group);
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
    // Not mid-drag, in the editor (its gestures are all canvas drags) or in
    // any other widget (a slider in the properties panel, say): deleting a
    // node being moved, or switching the active node under a field being
    // dragged, would leave the drag acting on things that are gone.
    if response.contains_pointer()
        && state.search.is_none()
        && state.rename.is_none()
        && ui.ctx().dragged_id().is_none()
        && (!ui.ctx().egui_wants_keyboard_input() || response.has_focus())
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
    let splice = splice_while_dragging(state, &f, ui.input(|i| i.modifiers.alt));
    draw::wires(&painter, &f, state, &problems, detached, splice.as_ref());
    let front = pointer_pos.and_then(|p| match hit(&f, p) {
        Hit::Port(endpoint, _) => Some(endpoint.node),
        Hit::Node(id) => Some(id),
        _ => None,
    });
    let mut fields = params::Fields::new(ui, canvas, front);
    draw::nodes(&painter, &f, state, &problems, |node| {
        fields.show(&f, node, &mut edits);
    });
    fields.finish(&mut edits);
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
    breadcrumb(ui, state, project, canvas);
    popups(ui, state, &f, &mut inputs, &mut edits);

    if response.hovered()
        && matches!(state.gesture, Gesture::Idle)
        && let Some(p) = pointer_pos
        && let Some(text) = hover_text(&f, &problems, p)
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

/// How close to one of `node`'s sockets counts as on it, in screen points.
/// Smaller when zoomed out, so neighbouring sockets don't overlap and a
/// reroute keeps a middle that can be grabbed to move it.
fn socket_reach(f: &Frame_<'_>, node: &layout::NodeGeom) -> f32 {
    let limit = if node.reroute {
        layout::REROUTE_SIZE.x / 4.0
    } else {
        layout::ROW_HEIGHT / 2.0
    };
    SOCKET_REACH.min(f.t.scale(limit))
}

/// The socket of `node` nearest `p`, if any is within reach.
fn nearest_socket<'n>(
    f: &Frame_<'_>,
    node: &'n layout::NodeGeom,
    p: Pos2,
    filter: impl Fn(&layout::PortGeom) -> bool,
) -> Option<&'n layout::PortGeom> {
    let reach = socket_reach(f, node);
    node.ports
        .iter()
        .filter(|port| filter(port))
        .map(|port| (f.t.to_screen(port.socket).distance(p), port))
        .filter(|&(distance, _)| distance <= reach)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, port)| port)
}

fn hit(f: &Frame_<'_>, p: Pos2) -> Hit {
    let g = f.t.to_graph(p);
    // Topmost first, each node's sockets with its body, so a node in front
    // hides the sockets of nodes behind it.
    for &i in f.order.iter().rev() {
        let node = &f.scene.nodes[i];
        if let Some(port) = nearest_socket(f, node, p, |_| true) {
            return Hit::Port(Endpoint::new(node.id, port.key.clone()), port.side);
        }
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
        // The row under the pointer, then the nearest socket in reach.
        let target = candidates
            .clone()
            .find(|port| port.row.contains(g))
            .or_else(|| {
                nearest_socket(f, node, p, |port| {
                    port.side != side && compatible(&port.kind)
                })
            });
        if let Some(port) = target {
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

    // A parameter field on a node takes any drag that starts on it, but only
    // plain primary drags (and right-clicks) are its own. Hand panning and
    // strokes to the canvas, before the field sees the drag.
    let (middle, secondary) = ui.input(|i| (i.pointer.middle_down(), i.pointer.secondary_down()));
    let canvas_drag = middle || (secondary && (modifiers.command || modifiers.shift));
    let stolen = canvas_drag
        && state.search.is_none()
        && state.rename.is_none()
        && origin.is_some_and(|p| state.canvas.contains(p))
        && ui.ctx().dragged_id().is_some_and(|id| id != response.id);
    if stolen {
        ui.ctx().set_dragged_id(response.id);
        // Catch up with the movement that decided it was a drag.
        if middle && let (Some(origin), Some(latest)) = (origin, latest) {
            state.view.pan(latest - origin);
        }
    }

    if response.drag_started() || stolen {
        let start = origin.or(latest).unwrap_or_default();
        state.gesture = if middle {
            Gesture::Pan
        } else if secondary {
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
        } else if ui.input(|i| i.pointer.primary_down()) {
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
            // The pointer may have moved in the same frame it was released.
            state.gesture = gesture;
            drag(state, f, p, edits);
            let gesture = std::mem::take(&mut state.gesture);
            finish(state, f, gesture, p, modifiers.alt, inputs, edits);
        }
    }

    if response.clicked_by(PointerButton::Primary)
        && let Some(p) = latest
    {
        click(state, f, p, modifiers);
    }
    if response.double_clicked_by(PointerButton::Primary)
        && let Some(p) = latest
    {
        match hit(f, p) {
            Hit::FrameHeader(id) => start_rename(state, f.project, id),
            Hit::Node(id) if is_group(f.project, id) => state.enter(Some(id), None),
            Hit::Nothing => {
                if let Some(wire) = wire_at(f, p) {
                    let input = wire.connection.to.clone();
                    edits.push(Edit::Apply(Command::Disconnect { input }));
                }
            }
            _ => {}
        }
    }
}

/// The wire under the screen point `p`, the nearest if several are in reach.
fn wire_at<'a>(f: &'a Frame_<'_>, p: Pos2) -> Option<&'a layout::WireGeom> {
    f.scene
        .wires
        .iter()
        .map(|wire| {
            let line: Vec<Pos2> = wire::flatten(wire::curve(wire.from, wire.to))
                .into_iter()
                .map(|g| f.t.to_screen(g))
                .collect();
            (wire::distance_to_polyline(p, &line), wire)
        })
        .filter(|(d, _)| *d <= WIRE_REACH)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, wire)| wire)
}

fn is_group(project: &Project, id: NodeId) -> bool {
    project.graph().node(id).is_some_and(|n| n.type_id == GROUP)
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
            // A port's label is its handle: dragging it reorders the column.
            if let Some(port) = f
                .scene
                .node(id)
                .and_then(|node| label_at(node, f.t.to_graph(start)))
            {
                return Gesture::Reorder {
                    node: id,
                    side: port.side,
                    key: port.key.clone(),
                    target: slot(f, id, port.side, &port.key, f.t.to_graph(start).y),
                };
            }
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

/// Dropping a node onto a wire: the wire is replaced by one into the node and
/// one out of it.
#[derive(Clone, Debug, PartialEq)]
struct Splice {
    /// The wire it replaces.
    wire: Connection,
    /// The node's ports that take its place.
    node: NodeId,
    via_in: String,
    via_out: String,
}

impl Splice {
    fn commands(&self) -> Vec<Command> {
        vec![
            Command::Connect(Connection {
                from: self.wire.from.clone(),
                to: Endpoint::new(self.node, self.via_in.clone()),
            }),
            // Replaces the wire this node is being put into.
            Command::Connect(Connection {
                from: Endpoint::new(self.node, self.via_out.clone()),
                to: self.wire.to.clone(),
            }),
        ]
    }
}

/// How a node at `rect` would splice into a wire it lies on, if it can: it
/// has no wires yet, and has an input and an output that suit the wire's
/// signal, the main signal port before a parameter port.
fn splice_for(f: &Frame_<'_>, node: NodeId, rect: Rect) -> Option<Splice> {
    let graph = f.project.graph();
    if graph
        .connections()
        .any(|c| c.from.node == node || c.to.node == node)
    {
        return None;
    }
    let geom = f.scene.node(node)?;
    // The wire nearest the node's middle among those that cross it.
    let centre = rect.center();
    let wire = f
        .scene
        .wires
        .iter()
        .filter_map(|wire| {
            let line = wire::flatten(wire::curve(wire.from, wire.to));
            line.iter()
                .any(|&p| rect.contains(p))
                .then(|| (wire::distance_to_polyline(centre, &line), wire))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))?
        .1;
    let from = &f.scene.port(&wire.connection.from, Side::Output)?.kind;
    let to = &f.scene.port(&wire.connection.to, Side::Input)?.kind;
    let via_in = geom
        .ports
        .iter()
        .filter(|p| p.side == Side::Input && from.feeds(&p.kind))
        .min_by_key(|p| matches!(p.kind, PortKind::Param(_)))?;
    let via_out = geom
        .ports
        .iter()
        .find(|p| p.side == Side::Output && p.kind.feeds(to))?;
    Some(Splice {
        wire: wire.connection.clone(),
        node,
        via_in: via_in.key.clone(),
        via_out: via_out.key.clone(),
    })
}

/// The splice a node being dragged would make where it is now.
fn splice_while_dragging(state: &EditorState, f: &Frame_<'_>, alt: bool) -> Option<Splice> {
    if alt {
        return None;
    }
    match &state.gesture {
        Gesture::Move {
            nodes,
            frames,
            moved: true,
            ..
        } if frames.is_empty() => match nodes.as_slice() {
            [(id, _)] => splice_for(f, *id, f.scene.node(*id)?.rect),
            _ => None,
        },
        _ => None,
    }
}

/// The port whose label is at the graph point `g`, if it can be reordered:
/// not on the title bar, not one the node refers to without having, and not
/// the only one on its side.
fn label_at(node: &layout::NodeGeom, g: Pos2) -> Option<&layout::PortGeom> {
    let in_row: Vec<_> = node
        .ports
        .iter()
        .filter(|p| !p.in_header && p.kind != PortKind::Unknown && p.row.contains(g))
        .collect();
    // Inputs and outputs can share a row, one on each half.
    let port = match in_row.as_slice() {
        [] => return None,
        [only] => *only,
        _ => {
            let side = if g.x < node.rect.center().x {
                Side::Input
            } else {
                Side::Output
            };
            in_row.into_iter().find(|p| p.side == side)?
        }
    };
    let siblings = node.ports.iter().filter(|p| p.side == port.side).count();
    (siblings > 1).then_some(port)
}

/// Where a port dragged to graph height `y` would land among the other ports
/// on its side: the number of them whose rows are above `y`.
fn slot(f: &Frame_<'_>, node: NodeId, side: Side, key: &str, y: f32) -> usize {
    f.scene.node(node).map_or(0, |node| {
        node.ports
            .iter()
            .filter(|p| p.side == side && p.key != key)
            .filter(|p| p.row.center().y < y)
            .count()
    })
}

/// The keys of `node`'s ports in the order a drag of `key` to `target` would
/// leave them: that side moved, the other side as it is.
fn reordered(node: &layout::NodeGeom, side: Side, key: &str, target: usize) -> Vec<String> {
    let mut mine: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for port in &node.ports {
        if port.side != side {
            other.push(port.key.clone());
        } else if port.key != key {
            mine.push(port.key.clone());
        }
    }
    mine.insert(target.min(mine.len()), key.to_owned());
    mine.extend(other);
    mine
}

/// The selected nodes and frames, and everything inside the selected frames,
/// which come along when they're moved or copied.
fn with_contents(state: &EditorState, scene: &Scene) -> (BTreeSet<NodeId>, BTreeSet<FrameId>) {
    let mut nodes = state.selected.clone();
    let mut frames = state.selected_frames.clone();
    for frame in scene
        .frames
        .iter()
        .filter(|fr| state.selected_frames.contains(&fr.id))
    {
        nodes.extend(
            scene
                .nodes
                .iter()
                .filter(|n| frame.rect.contains_rect(n.rect))
                .map(|n| n.id),
        );
        frames.extend(
            scene
                .frames
                .iter()
                .filter(|other| frame.rect.contains_rect(other.rect))
                .map(|other| other.id),
        );
    }
    (nodes, frames)
}

/// Moves the selection, and everything inside the selected frames.
fn start_move(state: &EditorState, f: &Frame_<'_>, start: Pos2) -> Gesture {
    let (nodes, frames) = with_contents(state, &f.scene);
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
        Gesture::Reorder {
            node,
            side,
            key,
            target,
        } => *target = slot(f, *node, *side, key, g.y),
        Gesture::Idle | Gesture::Pan | Gesture::Link { .. } | Gesture::BoxSelect { .. } => {}
    }
}

fn finish(
    state: &mut EditorState,
    f: &Frame_<'_>,
    gesture: Gesture,
    p: Pos2,
    alt: bool,
    inputs: &mut Inputs<'_>,
    edits: &mut Vec<Edit>,
) {
    match gesture {
        Gesture::Move {
            start,
            nodes,
            frames,
            moved,
        } => {
            if moved {
                // Where the node ended up, which the scene hasn't seen yet.
                let splice = match nodes.as_slice() {
                    [(id, origin)] if frames.is_empty() && !alt => {
                        let size = f.scene.node(*id).map(|n| n.rect.size());
                        size.and_then(|size| {
                            let at = *origin + (f.t.to_graph(p) - start);
                            splice_for(f, *id, Rect::from_min_size(at, size))
                        })
                    }
                    _ => None,
                };
                // In the move's undo step, so one undo puts both back.
                if let Some(splice) = splice {
                    edits.push(Edit::Drag(Command::Batch(splice.commands())));
                }
                edits.push(Edit::EndDrag);
            }
        }
        Gesture::Resize { moved, .. } => {
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
        Gesture::Stroke { mut points, action } => {
            // Always end where the pointer was released, however close.
            points.push(f.t.to_graph(p));
            let commands = stroke(f, &points, action, inputs);
            if !commands.is_empty() {
                edits.push(Edit::Apply(Command::Batch(commands)));
            }
        }
        Gesture::Reorder {
            node,
            side,
            key,
            target,
        } => {
            if let Some(geom) = f.scene.node(node) {
                let order = reordered(geom, side, &key, target);
                // Dropping a port where it was is no change.
                let on_side = |keys: &mut dyn Iterator<Item = &str>| {
                    keys.map(str::to_owned).collect::<Vec<_>>()
                };
                let before = on_side(
                    &mut geom
                        .ports
                        .iter()
                        .filter(|p| p.side == side)
                        .map(|p| p.key.as_str()),
                );
                let after = on_side(
                    &mut order
                        .iter()
                        .filter(|k| before.contains(k))
                        .map(String::as_str),
                );
                if before != after {
                    edits.push(Edit::Apply(Command::SetPortOrder { node, order }));
                }
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
        // Reroutes only pass audio, so they can't go on an event wire.
        if action == StrokeAction::Reroute && wire.event {
            continue;
        }
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
        state.search = Some(Search::new(
            f.t.to_graph(pointer),
            pointer,
            state.group.is_some(),
        ));
    }
    // Backspace too: it's the key a Mac calls Delete.
    // Every key is checked, so each press is consumed.
    let mut delete = false;
    for key in [Key::X, Key::Delete, Key::Backspace] {
        delete |= pressed(Modifiers::NONE, key);
    }
    if delete {
        edits.extend(state.delete_selection());
    }
    if pressed(Modifiers::SHIFT, Key::D) {
        duplicate(state, f, inputs, edits);
    }
    // egui turns Ctrl/Cmd+C, X and V into their own events, not key presses.
    let event = |wanted: fn(&Event) -> bool| ui.input(|i| i.events.iter().any(wanted));
    let copy = event(|e| matches!(e, Event::Copy)) || pressed(Modifiers::COMMAND, Key::C);
    let cut = event(|e| matches!(e, Event::Cut)) || pressed(Modifiers::COMMAND, Key::X);
    let paste = event(|e| matches!(e, Event::Paste(_))) || pressed(Modifiers::COMMAND, Key::V);
    if copy || cut {
        copy_selection(state, f);
    }
    if cut {
        edits.extend(state.delete_selection());
    }
    if paste {
        self::paste(state, f.t.to_graph(pointer), inputs, edits);
    }
    if pressed(Modifiers::NONE, Key::A) {
        state.select_only(f.scene.nodes.iter().map(|n| n.id));
        state.selected_frames = f.scene.frames.iter().map(|fr| fr.id).collect();
    }
    if pressed(Modifiers::ALT, Key::A) {
        state.clear_selection();
    }
    if pressed(Modifiers::COMMAND, Key::J) && state.group.is_none() {
        frame_selection(state, f, inputs, edits);
    }
    if pressed(Modifiers::COMMAND, Key::G) {
        group_selection(state, f, inputs, edits);
    }
    if pressed(Modifiers::NONE, Key::Tab) {
        enter_or_leave(state, f.project);
    }
    if pressed(Modifiers::NONE, Key::F2)
        && state.selected_frames.len() == 1
        && let Some(&id) = state.selected_frames.first()
    {
        start_rename(state, f.project, id);
    }
}

/// Remembers the selection (and what is in selected frames) for Paste.
fn copy_selection(state: &mut EditorState, f: &Frame_<'_>) {
    let graph = f.project.graph();
    let (nodes, frames) = with_contents(state, &f.scene);
    let nodes: Vec<(NodeId, Node)> = nodes
        .into_iter()
        .filter_map(|id| Some((id, graph.node(id)?.clone())))
        // A copied group would be empty, and a copied group port would
        // clash with the original's name.
        .filter(|(_, node)| !matches!(node.type_id.as_str(), GROUP | GROUP_INPUT | GROUP_OUTPUT))
        .collect();
    let copied: BTreeSet<NodeId> = nodes.iter().map(|(id, _)| *id).collect();
    let wires = graph
        .connections()
        .filter(|c| copied.contains(&c.from.node) && copied.contains(&c.to.node))
        .collect();
    let frames = frames
        .into_iter()
        .filter_map(|id| f.project.frame(id).cloned())
        .collect();
    if nodes.is_empty() && state.selected_frames.is_empty() {
        return;
    }
    state.clipboard = Some(Clipboard {
        nodes,
        wires,
        frames,
    });
}

/// Puts the clipboard down with its top-left corner at `at`, as one edit.
/// The pasted nodes are selected afterwards.
fn paste(state: &mut EditorState, at: Pos2, inputs: &mut Inputs<'_>, edits: &mut Vec<Edit>) {
    let Some(clip) = state.clipboard.clone() else {
        return;
    };
    let corner = clip
        .nodes
        .iter()
        .map(|(_, n)| Pos2::new(n.position.x, n.position.y))
        .chain(
            clip.frames
                .iter()
                .map(|fr| Pos2::new(fr.position.x, fr.position.y)),
        )
        .reduce(|a, b| Pos2::new(a.x.min(b.x), a.y.min(b.y)));
    let Some(corner) = corner else { return };
    let offset = at - corner;
    let mut commands = Vec::new();
    let mut copies = BTreeMap::new();
    for (id, node) in &clip.nodes {
        let copy = (inputs.new_node_id)();
        copies.insert(*id, copy);
        let mut node = node.clone();
        node.position = position(Pos2::new(node.position.x, node.position.y) + offset);
        // Into the group being edited, wherever the nodes came from.
        node.parent = state.group;
        commands.push(Command::AddNode { id: copy, node });
    }
    for wire in &clip.wires {
        if let (Some(&from), Some(&to)) = (copies.get(&wire.from.node), copies.get(&wire.to.node)) {
            commands.push(Command::Connect(Connection {
                from: Endpoint::new(from, wire.from.port.clone()),
                to: Endpoint::new(to, wire.to.port.clone()),
            }));
        }
    }
    // Frames belong to the top level.
    let mut frame_ids = Vec::new();
    if state.group.is_none() {
        for frame in &clip.frames {
            let id = (inputs.new_frame_id)();
            frame_ids.push(id);
            commands.push(Command::AddFrame {
                id,
                frame: Frame {
                    position: position(Pos2::new(frame.position.x, frame.position.y) + offset),
                    ..frame.clone()
                },
            });
        }
    }
    if commands.is_empty() {
        return;
    }
    edits.push(Edit::Apply(Command::Batch(commands)));
    state.select_only(copies.values().copied());
    state.selected_frames = frame_ids.into_iter().collect();
}

fn duplicate(
    state: &mut EditorState,
    f: &Frame_<'_>,
    inputs: &mut Inputs<'_>,
    edits: &mut Vec<Edit>,
) {
    let project = f.project;
    let graph = project.graph();
    // A selected frame is copied with what's in it, as dragging it moves it.
    let (nodes, frames) = with_contents(state, &f.scene);
    // A frame owns what its rectangle holds, so a copy that overlapped the
    // original would also pick the original's nodes up when dragged. With
    // frames, the copies go below the tallest of them instead.
    let offset = state
        .selected_frames
        .iter()
        .filter_map(|&id| project.frame(id))
        .map(|frame| frame.height)
        .reduce(f32::max)
        .map_or(DUPLICATE_OFFSET, |height| {
            Vec2::new(0.0, height + DUPLICATE_FRAME_GAP)
        });
    let mut copies = BTreeMap::new();
    let mut commands = Vec::new();
    for &id in &nodes {
        let Some(node) = graph.node(id) else { continue };
        // A copied group would be empty, and a copied group port would
        // clash with the original's name.
        if matches!(node.type_id.as_str(), GROUP | GROUP_INPUT | GROUP_OUTPUT) {
            continue;
        }
        let copy = (inputs.new_node_id)();
        copies.insert(id, copy);
        let mut node = node.clone();
        node.position = position(Pos2::new(node.position.x, node.position.y) + offset);
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
    let mut frame_copies = BTreeMap::new();
    for &id in &frames {
        let Some(frame) = project.frame(id) else {
            continue;
        };
        let copy = (inputs.new_frame_id)();
        frame_copies.insert(id, copy);
        commands.push(Command::AddFrame {
            id: copy,
            frame: Frame {
                position: position(Pos2::new(frame.position.x, frame.position.y) + offset),
                ..frame.clone()
            },
        });
    }
    if commands.is_empty() {
        return;
    }
    edits.push(Edit::Apply(Command::Batch(commands)));
    // The copies of what was selected, not of what came along with it.
    let active = state.active.and_then(|id| copies.get(&id).copied());
    let nodes: Vec<NodeId> = state
        .selected
        .iter()
        .filter_map(|id| copies.get(id).copied())
        .collect();
    let frames = state
        .selected_frames
        .iter()
        .filter_map(|id| frame_copies.get(id).copied())
        .collect();
    state.select_only(nodes);
    state.active = active.or(state.active);
    state.selected_frames = frames;
}

/// While inside a group, the path to it, top left: click a name to go back
/// up to that level.
fn breadcrumb(ui: &egui::Ui, state: &mut EditorState, project: &Project, canvas: Rect) {
    let Some(current) = state.group else { return };
    let graph = project.graph();
    let mut path: Vec<Option<NodeId>> = vec![None];
    path.extend(graph.ancestors(current).into_iter().map(Some));
    path.push(Some(current));
    let mut go = None;
    egui::Area::new(egui::Id::new("noodle-editor-breadcrumb"))
        .fixed_pos(canvas.min + Vec2::new(8.0, 8.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, &level) in path.iter().enumerate() {
                        if i > 0 {
                            ui.weak("›");
                        }
                        let label = match level {
                            None => "Project".to_owned(),
                            Some(id) => layout::group_title(graph, id),
                        };
                        let here = i == path.len() - 1;
                        let button = ui.add_enabled(!here, egui::Button::new(label).frame(false));
                        if button.clicked() {
                            go = Some(level);
                        }
                    }
                    ui.weak("(Tab leaves)");
                });
            });
        });
    if let Some(level) = go {
        // Select the group we came out of, as Tab does.
        let came_from = path
            .iter()
            .position(|&l| l == level)
            .and_then(|i| path.get(i + 1).copied().flatten());
        state.enter(level, came_from);
    }
}

/// Folds the selected nodes into a new group.
fn group_selection(
    state: &mut EditorState,
    f: &Frame_<'_>,
    inputs: &mut Inputs<'_>,
    edits: &mut Vec<Edit>,
) {
    let selected: Vec<NodeId> = state.selected.iter().copied().collect();
    if let Ok((id, command)) = group::group_nodes(f.project, &selected, &mut *inputs.new_node_id) {
        edits.push(Edit::Apply(command));
        state.select_only([id]);
    }
}

/// Tab: enters the one selected group, or else leaves the current group,
/// selecting it in its parent so Tab again goes back in.
fn enter_or_leave(state: &mut EditorState, project: &Project) {
    let graph = project.graph();
    let selected_group = match state.selected.iter().collect::<Vec<_>>()[..] {
        [&id] if graph.node(id).is_some_and(|n| n.type_id == GROUP) => Some(id),
        _ => None,
    };
    if let Some(id) = selected_group {
        state.enter(Some(id), None);
    } else if let Some(current) = state.group {
        let parent = graph.node(current).and_then(|n| n.parent);
        state.enter(parent, Some(current));
    }
}

/// A name for a new group input or output that no other port of the same
/// kind in the group has: `in1`, `in2`, … or `out1`, `out2`, …
fn free_port_name(project: &Project, group: Option<NodeId>, type_id: &str) -> String {
    let graph = project.graph();
    let (prefix, taken): (_, Vec<String>) = match group {
        Some(g) if type_id == GROUP_INPUT => (
            "in",
            graph
                .group_ports(g)
                .inputs
                .into_iter()
                .map(|p| p.name)
                .collect(),
        ),
        Some(g) => (
            "out",
            graph
                .group_ports(g)
                .outputs
                .into_iter()
                .map(|p| p.name)
                .collect(),
        ),
        None => ("port", Vec::new()),
    };
    (1..)
        .map(|n| format!("{prefix}{n}"))
        .find(|name| !taken.contains(name))
        .expect("an unbounded range has a free name")
}

/// Puts a new frame around the selected nodes.
fn frame_selection(
    state: &mut EditorState,
    f: &Frame_<'_>,
    inputs: &mut Inputs<'_>,
    edits: &mut Vec<Edit>,
) {
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
    let id = (inputs.new_frame_id)();
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
                        let mut node = Node::new(type_id).at(at.x, at.y);
                        node.parent = state.group;
                        if matches!(type_id, GROUP_INPUT | GROUP_OUTPUT) {
                            node.config = noodle_core::Config::new().with(
                                group::PORT_NAME,
                                noodle_core::Value::Text(free_port_name(
                                    f.project,
                                    state.group,
                                    type_id,
                                )),
                            );
                        }
                        edits.push(Edit::Apply(Command::AddNode { id, node }));
                        state.select_only([id]);
                    }
                    Choice::Frame => {
                        let id = (inputs.new_frame_id)();
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

/// The tooltip for whatever's under the pointer: the name of a socket that
/// has no label of its own, then any problems.
fn hover_text(f: &Frame_<'_>, problems: &Problems, p: Pos2) -> Option<String> {
    let name = match hit(f, p) {
        Hit::Port(endpoint, side) => f
            .scene
            .port(&endpoint, side)
            .filter(|port| port.in_header)
            .map(|port| port.name.clone()),
        _ => None,
    };
    let texts: Vec<String> = name
        .into_iter()
        .chain(hover_problem(f, problems, p))
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
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

/// Where a port's socket is on screen, for tests that drive the whole app
/// through the editor.
#[cfg(test)]
pub(crate) fn socket_on_screen(
    state: &EditorState,
    session: &Session,
    node: NodeId,
    output: bool,
    key: &str,
) -> Option<Pos2> {
    let side = if output { Side::Output } else { Side::Input };
    let scene = Scene::build(session.project(), session.registry(), state.group);
    let socket = scene.node(node)?.port(side, key)?.socket;
    Some(state.view.on(state.canvas).to_screen(socket))
}
