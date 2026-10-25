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
//! | Shift+F | Freeze the selected nodes and groups, or unfreeze them if they all are |
//! | Ctrl/Cmd+L | Auto-arrange the selected nodes, or all of them |
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

mod arrange;
mod body;
mod draw;
mod layout;
mod params;
mod search;
mod view;
mod wire;

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Sense, Vec2};
use noodle_core::group::{self, GROUP, GROUP_INPUT, GROUP_OUTPUT};
use noodle_core::spare;
use noodle_core::{Command, Connection, Endpoint, Frame, FrameId, Node, NodeId, Position, Project};
use noodle_engine::{Diagnostic, Location, NodeView, Registry};
use noodle_nodes::REROUTE_ID;

use crate::freezing::Badge;
use crate::session::{Edit, Session};
use crate::theme::{self, editor as colors};
pub use layout::friendly_type_name;
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
    pub(crate) canvas: Rect,
    gesture: Gesture,
    search: Option<Search>,
    rename: Option<Rename>,
    /// The menu opened by right-clicking a group.
    group_menu: Option<GroupMenu>,
    /// What meters and scopes show.
    bodies: body::Bodies,
    /// The last scene built, and what it was built from. It is reused while
    /// none of that changes.
    scene_cache: Option<(SceneKey, Rc<Scene>)>,
    /// What Copy and Cut last took, for Paste.
    /// Set by the Arrange menu item, done on the next frame, when the
    /// editor has the scene to work from.
    arrange_requested: bool,
    /// Copy, cut, paste or duplicate, asked for by the Edit menu.
    clip_requested: Option<crate::timeline::ClipAction>,
    /// Something carried out of the timeline and let go of over the canvas,
    /// until the app takes it.
    dropped: Option<Dropped>,
    /// Set by View → Fit Graph, done on the next frame like arranging.
    fit_requested: bool,
    /// A node whose view was asked for with a double-click, until the app
    /// takes it.
    view_request: Option<NodeId>,
    /// A node an ordinary double-click landed on, until the app takes it: a
    /// plugin node opens its editor window.
    open_request: Option<NodeId>,
    /// The group port a right-click landed on, which its menu removes.
    port_menu: Option<group::GroupPort>,
    /// The node whose monitor button was right-clicked.
    monitor_menu: Option<NodeId>,
    /// The node the context menu offers to remove unused ports of.
    unused_menu: Option<NodeId>,
    /// Which nodes are frozen or offline, and how their renders are doing.
    freeze: BTreeMap<NodeId, Badge>,
    /// The nodes whose monitor button is lit, from the session.
    monitored: Vec<NodeId>,
}

/// Nodes, the wires between them and frames, as copied: what goes on the
/// system clipboard under `application/x.noodle.node`.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Clipboard {
    nodes: Vec<(NodeId, Node)>,
    wires: Vec<Connection>,
    frames: Vec<Frame>,
}

/// What a [`Scene`] is built from: the project (by revision), the node types
/// and the group shown.
type SceneKey = (u64, u64, Option<NodeId>);

impl EditorState {
    /// The scene for the group being shown, rebuilt only if the project, the
    /// registry or the group changed since the last one.
    fn scene(&mut self, project: &Project, registry: &Registry) -> Rc<Scene> {
        let key = (project.revision(), registry.generation(), self.group);
        if let Some((cached, scene)) = &self.scene_cache
            && *cached == key
        {
            return scene.clone();
        }
        let scene = Rc::new(Scene::build(project, registry, self.group));
        self.scene_cache = Some((key, scene.clone()));
        scene
    }

    /// Like [`scene`](Self::scene), without remembering a new one.
    #[cfg(test)]
    fn peek_scene(&self, project: &Project, registry: &Registry) -> Rc<Scene> {
        let key = (project.revision(), registry.generation(), self.group);
        match &self.scene_cache {
            Some((cached, scene)) if *cached == key => scene.clone(),
            _ => Rc::new(Scene::build(project, registry, self.group)),
        }
    }
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
            group_menu: None,
            rename: None,
            bodies: body::Bodies::default(),
            scene_cache: None,
            arrange_requested: false,
            clip_requested: None,
            dropped: None,
            fit_requested: false,
            view_request: None,
            open_request: None,
            port_menu: None,
            monitor_menu: None,
            unused_menu: None,
            freeze: BTreeMap::new(),
            monitored: Vec::new(),
        }
    }
}

pub use body::{MeterAxis, MeterChannel, SpectrumTrace};

/// Draws one level as a bar in `area`.
pub fn draw_level(painter: &egui::Painter, area: Rect, level: &MeterChannel, axis: MeterAxis) {
    body::meter(painter, area, 1.0, std::slice::from_ref(level), axis);
}

/// Draws a Spectrum node's curve in `area`, as the node draws it on the canvas.
pub fn draw_spectrum(painter: &egui::Painter, area: Rect, trace: Option<&SpectrumTrace>) {
    body::spectrum(painter, area, 1.0, trace);
}

/// Draws a Spectrum node's spectrogram in `area`.
pub fn draw_spectrogram(painter: &egui::Painter, area: Rect, trace: Option<&SpectrumTrace>) {
    body::spectrogram(painter, area, trace);
}

/// Draws a scope's capture in `area`, as the node draws it on the canvas.
pub fn draw_scope(painter: &egui::Painter, area: Rect, view: Option<&noodle_engine::ScopeView>) {
    body::scope(painter, area, 1.0, view);
}

/// What was let go of over the canvas, and where it landed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dropped {
    pub item: crate::carry::Item,
    pub on: DropOn,
    /// The level being edited: the group, or `None` for the top.
    pub group: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DropOn {
    /// On a Sampler node.
    Sampler(NodeId),
    /// On the canvas, at this point in graph coordinates.
    Canvas(Pos2),
}

impl EditorState {
    /// What was dropped on the canvas since the last call, if anything.
    pub fn take_dropped(&mut self) -> Option<Dropped> {
        self.dropped.take()
    }

    /// Asks for the nodes to be auto-arranged (see [`arrange`]) on the next
    /// frame: the selection, or everything if nothing is selected.
    pub fn request_arrange(&mut self) {
        self.arrange_requested = true;
    }

    /// Asks for the selected nodes to be copied, cut or duplicated, or the
    /// copied ones pasted (at the centre of the view), on the next frame.
    pub fn request_clip(&mut self, action: crate::timeline::ClipAction) {
        self.clip_requested = Some(action);
    }

    /// Whether there are copied nodes to paste.
    pub fn can_paste(&self) -> bool {
        crate::clipboard::holds(crate::clipboard::NODE_MIME)
    }

    /// Asks for the view to be fitted around the whole graph at the current
    /// level on the next frame.
    pub fn request_fit(&mut self) {
        self.fit_requested = true;
    }

    #[cfg(test)]
    pub fn request_open_for_test(&mut self, node: NodeId) {
        self.open_request = Some(node);
    }

    /// The node an ordinary double-click landed on, once.
    pub fn take_open_request(&mut self) -> Option<NodeId> {
        self.open_request.take()
    }

    /// The node whose view a double-click asked for, once.
    pub fn take_view_request(&mut self) -> Option<NodeId> {
        self.view_request.take()
    }

    /// The level of input `channel` (from 0) of a mixer node.
    pub fn input_level(&self, node: NodeId, channel: usize) -> Option<MeterChannel> {
        self.bodies.input_meters(node).get(channel).copied()
    }

    /// What a wired parameter has been doing lately (see `Bodies::update`).
    pub fn param_live(&self, node: NodeId, key: &str) -> Option<crate::widgets::Live> {
        self.bodies.param_live(node, key)
    }

    /// Whether each node shows how much of the audio thread's time it takes.
    pub fn show_cpu(&self) -> bool {
        self.bodies.show_cpu()
    }

    pub fn set_show_cpu(&mut self, on: bool) {
        self.bodies.set_show_cpu(on);
    }

    /// What a Spectrum node last measured.
    pub fn spectrum(&self, node: NodeId) -> Option<&SpectrumTrace> {
        self.bodies.spectrum(node)
    }

    /// What a scope node last captured, for a view of it elsewhere.
    pub fn scope_view(&self, node: NodeId) -> Option<&noodle_engine::ScopeView> {
        self.bodies.scope_view(node)
    }

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

    /// Shows the top level, with `node` selected and the view fitted round
    /// the graph, so a node added from outside the editor (an automation
    /// track in the sequencer) is seen where it was made.
    pub fn reveal_top_level(&mut self, node: NodeId) {
        if self.group.is_some() {
            self.enter(None, None);
        }
        self.select_only([node]);
        self.request_fit();
    }

    pub(crate) fn select_only(&mut self, nodes: impl IntoIterator<Item = NodeId>) {
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

/// The menu a right-click on a group opens: "Save group to library...", which
/// then asks for a name.
struct GroupMenu {
    group: NodeId,
    /// Where it is drawn.
    screen: Pos2,
    /// The name being typed, once "Save group to library..." is chosen.
    name: Option<String>,
    focused: bool,
}

/// What is being renamed in place.
#[derive(Clone, PartialEq, Eq)]
enum RenameTarget {
    Frame(FrameId),
    /// A group: its name is its `name` config, which a track shares.
    Group(NodeId),
    /// A group's port, named in the boundary node inside. The box opens over
    /// the port's row on `shown_on`, the group or the boundary node itself,
    /// where the port is drawn.
    Port {
        boundary: NodeId,
        /// The port's ID on `boundary`.
        port: u32,
        shown_on: NodeId,
        side: Side,
        key: String,
    },
}

/// Renaming in place.
struct Rename {
    target: RenameTarget,
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
    state.freeze.clone_from(session.freeze_badges());
    state.monitored.clear();
    state.monitored.extend_from_slice(session.monitored());
    if session.freeze_pending() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    state
        .bodies
        .set_sample_rate(session.engine_sample_rate().unwrap_or(0.0));
    let liveness = state.bodies.update(
        session.telemetry(),
        session.project(),
        session.registry(),
        dt,
    );
    // Meters and scopes move with the audio, so keep drawing while they do,
    // and while their peaks and traces fall back. A still graph, silent or
    // with the transport stopped, costs nothing.
    if session.engine_running() {
        // Nothing else wakes a still editor when sound starts without input
        // (live MIDI with the transport stopped), so look again now and then.
        let wait = if liveness == body::Liveness::Still {
            body::IDLE_POLL
        } else {
            body::METER_FRAME
        };
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f32(wait));
    }
    let mut new_node_id = || session.new_node_id();
    let mut new_frame_id = || session.new_frame_id();
    show_project(
        ui,
        state,
        Inputs {
            project: session.project(),
            registry: session.registry(),
            library: session.library(),
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
    library: &'a noodle_core::library::Library,
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
    registry: &'a Registry,
    scene: Rc<Scene>,
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
    // A panel resized above the canvas moves its top-left; keep the graph
    // where it was on screen instead of dragging the content with the corner.
    // (The first frame has no previous canvas to compare against.)
    if state.canvas.is_positive()
        && let moved = canvas.min - state.canvas.min
        && moved != Vec2::ZERO
    {
        state.view.offset -= moved / state.view.zoom;
    }
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
    let scene = state.scene(project, inputs.registry);
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
        registry: inputs.registry,
        t: state.view.on(canvas),
        scene,
        order,
        frame_order,
    };

    if let Some(drop) = crate::carry::drop_in(ui.ctx(), canvas) {
        crate::carry::take_drop(ui.ctx());
        let sampler = match hit(&f, drop.at) {
            Hit::Node(id) | Hit::Port(Endpoint { node: id, .. }, _) => project
                .graph()
                .node(id)
                .is_some_and(|n| n.type_id == noodle_nodes::SAMPLER_ID)
                .then_some(id),
            _ => None,
        };
        state.dropped = Some(Dropped {
            item: drop.item,
            on: sampler.map_or(DropOn::Canvas(f.t.to_graph(drop.at)), DropOn::Sampler),
            group: state.group,
        });
    }

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
    // From the menu, so the pointer isn't over the canvas.
    if std::mem::take(&mut state.arrange_requested) {
        arrange_nodes(state, &f, &mut edits);
    }
    if let Some(action) = state.clip_requested.take() {
        use crate::timeline::ClipAction;
        match action {
            ClipAction::Copy => {
                copy_selection(state, &f);
            }
            ClipAction::Cut => {
                if copy_selection(state, &f) {
                    edits.extend(state.delete_selection());
                }
            }
            ClipAction::Paste => paste(
                state,
                f.t.to_graph(canvas.center()),
                &mut inputs,
                &mut edits,
            ),
            ClipAction::Duplicate => duplicate(state, &f, &mut inputs, &mut edits),
        }
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
    draw::nodes(&painter, &f, state, &problems, front, |node| {
        fields.show(&f, &state.bodies, node, &mut edits);
    });
    fields.finish(&mut edits);
    if let Some(total) = state.bodies.cpu_total() {
        let (label, color) = draw::cpu_label(noodle_engine::CpuReading {
            load: total,
            peak: total,
        });
        painter.text(
            canvas.right_top() + Vec2::new(-12.0, 10.0),
            egui::Align2::RIGHT_TOP,
            format!("Graph CPU {label}"),
            egui::FontId::monospace(12.0),
            color,
        );
        if let Some(stats) = state.bodies.block_stats() {
            let (text, color) = draw::block_label(&stats);
            painter.text(
                canvas.right_top() + Vec2::new(-12.0, 26.0),
                egui::Align2::RIGHT_TOP,
                text,
                egui::FontId::monospace(12.0),
                color,
            );
        }
    }
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
    // Home with the pointer over the canvas, or F (see `keyboard`) or View →
    // Fit Graph.
    let key =
        response.contains_pointer() && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Home));
    if (std::mem::take(&mut state.fit_requested) || key)
        && let Some(bounds) = scene.bounds()
    {
        state.view.fit(canvas, bounds);
    }
}

/// The monitor button in a node's header, in graph units, if the node has an
/// audio output to listen to.
fn monitor_button(project: &Project, node: &layout::NodeGeom) -> Option<Rect> {
    let boundary = project
        .graph()
        .node(node.id)
        .is_none_or(|n| matches!(n.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT));
    let audio_out = node
        .ports
        .iter()
        .any(|p| p.side == Side::Output && p.kind == PortKind::Audio);
    if node.reroute || node.error.is_some() || boundary || !audio_out {
        return None;
    }
    let header = node.header();
    Some(Rect::from_center_size(
        header.right_center() - Vec2::new(MONITOR_BUTTON.x / 2.0 + 8.0, 0.0),
        MONITOR_BUTTON,
    ))
}

/// How big the monitor button is, in graph units.
const MONITOR_BUTTON: Vec2 = Vec2::new(14.0, 14.0);

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
    let graph = f.project.graph();
    for &i in f.order.iter().rev() {
        let node = &f.scene.nodes[i];
        if node.id == anchor.node {
            continue;
        }
        // A lane's port only shows the lane, so no wire can land on it.
        let usable = |port: &layout::PortGeom| {
            port.side != side
                && compatible(&port.kind)
                && !layout::is_lane_port(graph, &Endpoint::new(node.id, port.key.clone()))
        };
        let mut candidates = node.ports.iter().filter(|port| usable(port));
        // The row under the pointer, then the nearest socket in reach.
        let target = candidates
            .clone()
            .find(|port| port.row.contains(g))
            .or_else(|| nearest_socket(f, node, p, usable));
        if let Some(port) = target {
            return Some(Endpoint::new(node.id, port.key.clone()));
        }
        if node.rect.contains(g) {
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

    // Right-clicking a port row of a group's input or output node offers to
    // remove the port.
    if response.secondary_clicked() {
        state.monitor_menu = latest.and_then(|p| monitor_button_at(f, p));
        state.port_menu = latest
            .filter(|_| state.monitor_menu.is_none())
            .and_then(|p| boundary_port_at(f, p));
    }
    if response.secondary_clicked() {
        state.unused_menu =
            latest
                .filter(|_| state.monitor_menu.is_none())
                .and_then(|p| match hit(f, p) {
                    Hit::Node(id) | Hit::Port(Endpoint { node: id, .. }, _)
                        if !is_group(f.project, id) =>
                    {
                        spare::remove_unused_ports(f.project, id).map(|_| id)
                    }
                    _ => None,
                });
    }
    let port = state.port_menu.clone();
    let monitor = state.monitor_menu;
    let monitored = state.monitored.clone();
    let unused = state.unused_menu;
    let mut remove = false;
    let mut remove_unused = false;
    response.context_menu(|ui| {
        if let Some(port) = &port
            && ui
                .button(format!("Remove port \"{}\"", port.name))
                .clicked()
        {
            remove = true;
            ui.close();
        }
        if unused.is_some() && ui.button("Remove unused ports").clicked() {
            remove_unused = true;
            ui.close();
        }
        if let Some(node) = monitor {
            use crate::session::MonitorMode::{Exclusive, SilenceOthers, Sum};
            let choices = [
                ("Monitor", Some(Sum), false),
                ("Add to monitor", Some(Sum), true),
                (
                    "Monitor, silence other outputs on its device",
                    Some(Exclusive),
                    false,
                ),
                (
                    "Monitor, silence all other outputs",
                    Some(SilenceOthers),
                    false,
                ),
            ];
            for (label, mode, add) in choices {
                if ui.button(label).clicked() {
                    edits.push(Edit::MonitorAs { node, mode, add });
                    ui.close();
                }
            }
            if ui
                .add_enabled(
                    monitored.contains(&node),
                    egui::Button::new("Stop monitoring"),
                )
                .clicked()
            {
                edits.push(Edit::MonitorAs {
                    node,
                    mode: None,
                    add: false,
                });
                ui.close();
            }
        }
        if port.is_none() && unused.is_none() && monitor.is_none() {
            ui.close();
        }
    });
    if remove_unused && let Some(node) = state.unused_menu.take() {
        edits.extend(spare::remove_unused_ports(f.project, node).map(Edit::Apply));
    }
    if remove && let Some(port) = state.port_menu.take() {
        edits.extend(spare::remove_group_port(f.project, &port).map(Edit::Apply));
    }

    if response.clicked_by(PointerButton::Primary)
        && let Some(p) = latest
    {
        if let Some(node) = monitor_button_at(f, p) {
            edits.push(Edit::Monitor {
                node,
                command: modifiers.command,
                shift: modifiers.shift,
            });
        } else {
            click(state, f, p, modifiers);
        }
    }
    if response.secondary_clicked()
        && let Some(p) = latest
        && !modifiers.command
        && !modifiers.shift
        && monitor_button_at(f, p).is_none()
        && let Hit::Node(id) = hit(f, p)
        && is_group(f.project, id)
    {
        state.group_menu = Some(GroupMenu {
            group: id,
            screen: p,
            name: None,
            focused: false,
        });
    }
    // Ctrl+double-click removes a node's unused ports; anywhere else it does
    // nothing, so it can't open or rename by mistake.
    if response.double_clicked_by(PointerButton::Primary)
        && modifiers.command
        && let Some(p) = latest
    {
        if let Hit::Node(id) = hit(f, p) {
            edits.extend(spare::remove_unused_ports(f.project, id).map(Edit::Apply));
        }
    } else if response.double_clicked_by(PointerButton::Primary)
        && let Some(p) = latest
    {
        match hit(f, p) {
            Hit::FrameHeader(id) => start_rename(state, f.project, id),
            // A group's port rows rename the port; the rest of the node
            // opens it.
            Hit::Node(id) if is_group(f.project, id) => {
                let port = f
                    .scene
                    .node(id)
                    .and_then(|node| port_row_at(node, f.t.to_graph(p)));
                match port {
                    Some(port) => start_port_rename(state, f.project, id, port),
                    None => state.enter(Some(id), None),
                }
            }
            // A group's input or output node: its port's row renames the port.
            Hit::Node(id) if is_boundary(f.project, id) => {
                let port = f
                    .scene
                    .node(id)
                    .and_then(|node| port_row_at(node, f.t.to_graph(p)))
                    .filter(|port| !matches!(port.kind, PortKind::Param(_)));
                if let Some(port) = port {
                    start_boundary_rename(state, id, port);
                }
            }
            // Mixers, scopes and outputs (which have a scope) have a view of
            // their own.
            Hit::Node(id)
                if f.scene
                    .node(id)
                    .is_some_and(|n| matches!(n.view, NodeView::Mixer | NodeView::Scope)) =>
            {
                state.view_request = Some(id);
            }
            Hit::Nothing => {
                if let Some(wire) = wire_at(f, p) {
                    edits.push(Edit::Apply(wire.cut()));
                }
            }
            // Any other node: the app opens its own window if it has one
            // (a plugin's editor).
            Hit::Node(id) => state.open_request = Some(id),
            _ => {}
        }
    }
}

/// The node whose monitor button is under the screen point `p`.
fn monitor_button_at(f: &Frame_<'_>, p: Pos2) -> Option<NodeId> {
    let g = f.t.to_graph(p);
    // Topmost node first, so a button under another node can't be pressed.
    let node = f
        .order
        .iter()
        .rev()
        .map(|&i| &f.scene.nodes[i])
        .find(|node| node.rect.contains(g))?;
    monitor_button(f.project, node)
        .filter(|button| button.contains(g))
        .map(|_| node.id)
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

/// Whether `id` is a group's input or output node.
fn is_boundary(project: &Project, id: NodeId) -> bool {
    project
        .graph()
        .node(id)
        .is_some_and(|n| matches!(n.type_id.as_str(), GROUP_INPUT | GROUP_OUTPUT))
}

/// The port of a group's input or output node whose row is under `p`.
fn boundary_port_at(f: &Frame_<'_>, p: Pos2) -> Option<group::GroupPort> {
    let Hit::Node(id) = hit(f, p) else {
        return None;
    };
    let port = port_row_at(f.scene.node(id)?, f.t.to_graph(p))?;
    let node = f.project.graph().node(id)?;
    let pid = group::parse_port_key(&port.key).filter(|_| is_boundary(f.project, id))?;
    node.boundary_ports()
        .iter()
        .any(|&(other, _)| other == pid)
        .then(|| group::GroupPort {
            name: port.name.clone(),
            node: id,
            id: pid,
        })
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
            // A lane's port only shows the lane; it can't take a new wire.
            if port.kind == PortKind::Unknown || layout::is_lane_port(graph, &endpoint) {
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
        .filter(|wire| wire.lane.is_none())
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
        .filter(|p| !p.spare && p.side == Side::Input && from.feeds(&p.kind))
        .min_by_key(|p| matches!(p.kind, PortKind::Param(_)))?;
    let via_out = geom
        .ports
        .iter()
        .find(|p| !p.spare && p.side == Side::Output && p.kind.feeds(to))?;
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
    let port = port_row_at(node, g)?;
    let siblings = node
        .ports
        .iter()
        .filter(|p| p.side == port.side && !p.spare)
        .count();
    (siblings > 1).then_some(port)
}

/// The port whose row is at the graph point `g`, on the title bar excepted.
/// Spare ports and ones the node doesn't really have don't count.
fn port_row_at(node: &layout::NodeGeom, g: Pos2) -> Option<&layout::PortGeom> {
    let in_row: Vec<_> = node
        .ports
        .iter()
        .filter(|p| !p.in_header && !p.spare && p.kind != PortKind::Unknown && p.row.contains(g))
        .collect();
    // Inputs and outputs can share a row, one on each half.
    match in_row.as_slice() {
        [] => None,
        [only] => Some(*only),
        _ => {
            let side = if g.x < node.rect.center().x {
                Side::Input
            } else {
                Side::Output
            };
            in_row.into_iter().find(|p| p.side == side)
        }
    }
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
            let target_end = target.clone().unwrap_or_else(|| anchor.clone());
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
            // A spare port becomes real in the same step as its first wire.
            let command = command.map(|command| {
                let mut commands = Vec::new();
                for (endpoint, side) in [(&anchor, side), (&target_end, side.other())] {
                    commands.extend(make_real(f, endpoint, side, inputs));
                }
                commands.push(command);
                if commands.len() == 1 {
                    commands.remove(0)
                } else {
                    Command::Batch(commands)
                }
            });
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

/// What it takes to make the spare port `endpoint` real, if it is one: more
/// inputs on a mixer, or a port on a group or the boundary node inside it.
fn make_real(
    f: &Frame_<'_>,
    endpoint: &Endpoint,
    side: Side,
    inputs: &mut Inputs<'_>,
) -> Vec<Command> {
    let spare_port = f.scene.port(endpoint, side).is_some_and(|p| p.spare);
    let graph = f.project.graph();
    let Some(node) = graph.node(endpoint.node).filter(|_| spare_port) else {
        return Vec::new();
    };
    match node.type_id.as_str() {
        spare::MIXER => spare::prepare_mixer_input(graph, endpoint.node, &endpoint.port),
        GROUP => {
            let side = match side {
                Side::Input => spare::Side::Input,
                Side::Output => spare::Side::Output,
            };
            let id = (inputs.new_node_id)();
            vec![spare::add_group_port(graph, endpoint.node, side, &endpoint.port, id).0]
        }
        // A spare on a boundary node adds a port to it, named as the group's
        // next spare, under the key the spare was drawn with.
        GROUP_INPUT | GROUP_OUTPUT => {
            let Some(group) = node.parent else {
                return Vec::new();
            };
            let side = if node.type_id == GROUP_INPUT {
                spare::Side::Input
            } else {
                spare::Side::Output
            };
            let name = spare::spare_group_name(graph, group, side);
            let id = (inputs.new_node_id)();
            vec![spare::add_group_port(graph, group, side, &name, id).0]
        }
        _ => Vec::new(),
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
        // Reroutes only pass audio, so they can't go on an event or spectral wire.
        if action == StrokeAction::Reroute && (wire.event || wire.spectral || wire.sample) {
            continue;
        }
        let line = wire::flatten(wire::curve(wire.from, wire.to));
        let Some(at) = wire::first_crossing(&line, points) else {
            continue;
        };
        let input = wire.connection.to.clone();
        match action {
            StrokeAction::Cut => commands.push(wire.cut()),
            // A lane has no connection to splice a reroute into.
            StrokeAction::Reroute if wire.lane.is_some() => {}
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

    if pressed(Modifiers::NONE, Key::F) {
        state.fit_requested = true;
        ui.ctx().request_repaint();
    }
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
    let copied = (copy || cut) && copy_selection(state, f);
    if cut && copied {
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
    if pressed(Modifiers::COMMAND, Key::L) {
        arrange_nodes(state, f, edits);
    }
    if pressed(Modifiers::COMMAND, Key::J) && state.group.is_none() {
        frame_selection(state, f, inputs, edits);
    }
    if pressed(Modifiers::COMMAND, Key::G) {
        group_selection(state, f, inputs, edits);
    }
    if pressed(Modifiers::SHIFT, Key::F) {
        toggle_freeze(state, f, inputs, edits);
    }
    if pressed(Modifiers::NONE, Key::Tab) {
        enter_or_leave(state, f.project);
    }
    if pressed(Modifiers::NONE, Key::F2) {
        if state.selected_frames.len() == 1
            && let Some(&id) = state.selected_frames.first()
        {
            start_rename(state, f.project, id);
        } else if state.selected.len() == 1
            && let Some(&id) = state.selected.first()
        {
            start_node_rename(state, f.project, id);
        }
    }
}

/// Auto-arranges the selected nodes, or all of them when none are selected,
/// as one undo step. Frames, and the nodes inside them, stay where they are.
fn arrange_nodes(state: &EditorState, f: &Frame_<'_>, edits: &mut Vec<Edit>) {
    // A selected frame stays put, so selecting only frames arranges nothing.
    if state.selected.is_empty() && !state.selected_frames.is_empty() {
        return;
    }
    let frames: Vec<Rect> = f.scene.frames.iter().map(|fr| fr.rect).collect();
    let framed = |rect: Rect| frames.iter().any(|fr| fr.contains_rect(rect));
    let items: BTreeMap<NodeId, arrange::Item> = f
        .scene
        .nodes
        .iter()
        .filter(|n| state.selected.is_empty() || state.selected.contains(&n.id))
        .filter(|n| !framed(n.rect))
        .map(|n| (n.id, arrange::Item { rect: n.rect }))
        .collect();
    let wires: Vec<(NodeId, NodeId)> = f
        .scene
        .wires
        .iter()
        .map(|w| (w.connection.from.node, w.connection.to.node))
        .collect();
    // Keep clear of frames, and of nodes that aren't being moved.
    let mut avoid = frames.clone();
    avoid.extend(
        f.scene
            .nodes
            .iter()
            .filter(|n| !items.contains_key(&n.id))
            .map(|n| n.rect),
    );
    let placed = arrange::arrange(&items, &wires, &avoid);
    let commands: Vec<Command> = placed
        .into_iter()
        .filter(|(node, to)| (*to - items[node].rect.min).length() > 0.01)
        .map(|(node, to)| Command::MoveNode {
            node,
            position: position(to),
        })
        .collect();
    if !commands.is_empty() {
        edits.push(Edit::Apply(Command::Batch(commands)));
    }
}

/// Remembers the selection (and what is in selected frames) for Paste.
fn copy_selection(state: &mut EditorState, f: &Frame_<'_>) -> bool {
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
    let frames: Vec<Frame> = frames
        .into_iter()
        .filter_map(|id| f.project.frame(id).cloned())
        .collect();
    if nodes.is_empty() && state.selected_frames.is_empty() {
        return false;
    }
    // The plain text is there so the window sees a paste and other apps
    // get something readable.
    let summary = format!("Noodle: {} nodes, {} frames", nodes.len(), frames.len());
    crate::clipboard::copy(
        crate::clipboard::NODE_MIME,
        &Clipboard {
            nodes,
            wires,
            frames,
        },
        &summary,
    );
    true
}

/// Puts the clipboard down with its top-left corner at `at`, as one edit.
/// The pasted nodes are selected afterwards.
fn paste(state: &mut EditorState, at: Pos2, inputs: &mut Inputs<'_>, edits: &mut Vec<Edit>) {
    let Some(clip) = crate::clipboard::paste::<Clipboard>(crate::clipboard::NODE_MIME) else {
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

/// Shift+F: freezes the selected nodes and groups that have audio to
/// freeze, or unfreezes them if they are all frozen already. One undo step.
fn toggle_freeze(state: &EditorState, f: &Frame_<'_>, inputs: &Inputs<'_>, edits: &mut Vec<Edit>) {
    let graph = f.project.graph();
    let can_freeze = |id: NodeId| {
        let Some(node) = graph.node(id) else {
            return false;
        };
        match node.type_id.as_str() {
            GROUP => !graph.group_ports(id).outputs.is_empty(),
            GROUP_INPUT | GROUP_OUTPUT | REROUTE_ID => false,
            type_id => inputs
                .registry
                .get(type_id)
                .and_then(|t| t.layout(&node.config).ok())
                .is_some_and(|layout| !layout.outputs.is_empty()),
        }
    };
    let targets: Vec<NodeId> = state
        .selected
        .iter()
        .copied()
        .filter(|&n| can_freeze(n))
        .collect();
    if targets.is_empty() {
        return;
    }
    let freeze = !targets.iter().all(|&n| f.project.is_frozen(n));
    let commands: Vec<Command> = targets
        .into_iter()
        .filter(|&node| f.project.is_frozen(node) != freeze)
        .map(|node| Command::SetFrozen {
            node,
            frozen: freeze,
        })
        .collect();
    if !commands.is_empty() {
        edits.push(Edit::Apply(Command::Batch(commands)));
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
            target: RenameTarget::Frame(id),
            label: frame.label.clone(),
            focused: false,
        });
    }
}

/// Starts renaming a group, or a group's input or output node.
fn start_node_rename(state: &mut EditorState, project: &Project, id: NodeId) {
    let graph = project.graph();
    let Some(node) = graph.node(id) else { return };
    let (target, label) = match node.type_id.as_str() {
        GROUP => (RenameTarget::Group(id), layout::group_title(graph, id)),
        spare::MIXER => (RenameTarget::Group(id), spare::mixer_name(graph, id)),
        // F2 on a group's input or output node names its first port.
        GROUP_INPUT | GROUP_OUTPUT => {
            let Some((port, name)) = node.boundary_ports().into_iter().next() else {
                return;
            };
            let (side, key) = if node.type_id == GROUP_INPUT {
                (Side::Output, group::port_key(port))
            } else {
                (Side::Input, group::port_key(port))
            };
            let target = RenameTarget::Port {
                boundary: id,
                port,
                shown_on: id,
                side,
                key,
            };
            (target, name)
        }
        _ => return,
    };
    state.rename = Some(Rename {
        target,
        label,
        focused: false,
    });
}

/// Starts renaming the port of a group node that `key` names.
fn start_port_rename(
    state: &mut EditorState,
    project: &Project,
    group: NodeId,
    port: &layout::PortGeom,
) {
    let ports = project.graph().group_ports(group);
    let found = match port.side {
        Side::Input => ports.input(&port.key),
        Side::Output => ports.output(&port.key),
    };
    if let Some(found) = found {
        state.rename = Some(Rename {
            target: RenameTarget::Port {
                boundary: found.node,
                port: found.id,
                shown_on: group,
                side: port.side,
                key: port.key.clone(),
            },
            label: found.name.clone(),
            focused: false,
        });
    }
}

/// The menu over a right-clicked group.
fn group_menu(
    ui: &egui::Ui,
    state: &mut EditorState,
    f: &Frame_<'_>,
    library: &noodle_core::library::Library,
    edits: &mut Vec<Edit>,
) {
    let Some(menu) = &mut state.group_menu else {
        return;
    };
    if !is_group(f.project, menu.group) {
        state.group_menu = None;
        return;
    }
    let default_name = layout::group_title(f.project.graph(), menu.group);
    // The template the group was made from, if the library still lists it.
    let source_name = f
        .project
        .graph()
        .node(menu.group)
        .and_then(|n| n.template.as_deref())
        .and_then(|entry| library.templates().find(|(id, _)| *id == entry))
        .map(|(_, t)| t.name.clone());
    let mut close = false;
    let area = egui::Area::new(egui::Id::new("noodle-editor-group-menu"))
        .fixed_pos(menu.screen)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| match &mut menu.name {
                None => {
                    if ui.button("Save group to library...").clicked() {
                        menu.name = Some(default_name.clone());
                    }
                    if spare::remove_unused_ports(f.project, menu.group).is_some()
                        && ui.button("Remove unused ports").clicked()
                    {
                        edits.extend(
                            spare::remove_unused_ports(f.project, menu.group).map(Edit::Apply),
                        );
                        close = true;
                    }
                    if let Some(name) = source_name.as_deref() {
                        let text = format!("Update from {name}");
                        if ui
                            .button(text)
                            .on_hover_text(
                                "Replaces what is inside with the template's current version",
                            )
                            .clicked()
                        {
                            edits.push(Edit::UpdateFromTemplate(menu.group));
                            close = true;
                        }
                    }
                }
                Some(name) => {
                    ui.label("Name in the library");
                    let edit = ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
                    if !menu.focused {
                        edit.request_focus();
                        menu.focused = true;
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let named = !name.trim().is_empty();
                    let save = ui.add_enabled(named, egui::Button::new("Save")).clicked();
                    if (save || enter) && named {
                        edits.push(Edit::Library(crate::session::LibraryEdit::Template {
                            group: menu.group,
                            name: name.trim().to_string(),
                        }));
                        close = true;
                    }
                }
            });
        })
        .response;
    let escape = ui.input(|i| i.key_pressed(Key::Escape));
    let clicked_outside = ui.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !area.rect.contains(p))
    });
    if close || escape || clicked_outside {
        state.group_menu = None;
    }
}

/// Starts renaming the port of a group's input or output node, which is
/// drawn on that node.
fn start_boundary_rename(state: &mut EditorState, boundary: NodeId, port: &layout::PortGeom) {
    let Some(id) = group::parse_port_key(&port.key) else {
        return;
    };
    state.rename = Some(Rename {
        target: RenameTarget::Port {
            boundary,
            port: id,
            shown_on: boundary,
            side: port.side,
            key: port.key.clone(),
        },
        label: port.name.clone(),
        focused: false,
    });
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
        match search.show(ui.ctx(), inputs.registry, inputs.library) {
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
                        if let Some(node_type) = inputs.registry.get(type_id) {
                            node.config = node_type.initial_config();
                        }
                        if matches!(type_id, GROUP_INPUT | GROUP_OUTPUT) {
                            // A group has one of each, holding all its ports:
                            // asking for another selects the one it has.
                            let existing = state.group.and_then(|group| {
                                f.project
                                    .graph()
                                    .children(Some(group))
                                    .find(|(_, n)| n.type_id == type_id)
                                    .map(|(id, _)| id)
                            });
                            if let Some(existing) = existing {
                                state.select_only([existing]);
                                return;
                            }
                            node.config = group::ports_config(&[]);
                        }
                        // An Automation node gets its track in the sequencer
                        // in the same undo step.
                        let add = Command::AddNode { id, node };
                        edits.push(Edit::Apply(
                            if type_id == noodle_nodes::AUTOMATION_NODE_ID {
                                Command::Batch(vec![
                                    add,
                                    crate::timeline::add_track::automation_lane(
                                        id,
                                        f.project.next_lane_id(),
                                    ),
                                ])
                            } else {
                                add
                            },
                        ));
                        state.select_only([id]);
                    }
                    Choice::Template(entry) => {
                        if let Some(template) = inputs.library.template(&entry) {
                            let (id, command) = template.instantiate(
                                &entry,
                                state.group,
                                position(at),
                                &mut *inputs.new_node_id,
                            );
                            edits.push(Edit::Apply(command));
                            state.select_only([id]);
                        }
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

    group_menu(ui, state, f, inputs.library, edits);

    let Some(rename) = &mut state.rename else {
        return;
    };
    // Where the box opens, and the width it gets.
    let anchor = match rename.target.clone() {
        RenameTarget::Frame(id) => f.project.frame(id).map(|frame| {
            Rect::from_min_size(
                Pos2::new(frame.position.x, frame.position.y),
                Vec2::new(frame.width, FRAME_HEADER_HEIGHT),
            )
        }),
        RenameTarget::Group(id) => f.scene.node(id).map(|n| n.header()),
        // The port's own row, so the box sits where the label is.
        RenameTarget::Port {
            shown_on,
            side,
            key,
            ..
        } => f
            .scene
            .node(shown_on)
            .and_then(|n| n.port(side, &key))
            .map(|port| port.row.shrink2(Vec2::new(8.0, 0.0))),
    };
    let Some(anchor) = anchor else {
        state.rename = None;
        return;
    };
    let header = f.t.rect_to_screen(anchor);
    let id = egui::Id::new("noodle-editor-rename");
    let mut done = false;
    let mut cancelled = false;
    // A port is renamed where its label is: an unframed field over the label,
    // in the label's font and colour. Other targets open a box over the header.
    let port_side = match rename.target {
        RenameTarget::Port { side, .. } => Some(side),
        _ => None,
    };
    let z = f.t.zoom;
    let font = egui::FontId::proportional(12.0 * z);
    let (pos, width) = match port_side {
        Some(side) => {
            let text =
                ui.painter()
                    .layout_no_wrap(rename.label.clone(), font.clone(), colors::TEXT);
            let width = text.size().x.max(40.0 * z) + 8.0 * z;
            let x = match side {
                Side::Input => header.left() + 10.0 * z,
                Side::Output => header.right() - 10.0 * z - width,
            };
            (Pos2::new(x, header.center().y - 9.0 * z), width)
        }
        None => (
            header.min + Vec2::new(4.0, 2.0),
            (header.width() - 8.0).max(60.0),
        ),
    };
    egui::Area::new(id)
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            let mut field = egui::TextEdit::singleline(&mut rename.label).desired_width(width);
            if port_side.is_some() {
                field = field
                    .frame(egui::Frame::NONE.fill(colors::NODE))
                    .font(font.clone())
                    .text_color(colors::TEXT);
            }
            let edit = ui.add(field);
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
        let target = rename.target.clone();
        state.rename = None;
        edits.extend(rename_edit(f.project, target, label));
    }
}

/// The edit that gives `target` the name `label`, if that changes anything.
fn rename_edit(project: &Project, target: RenameTarget, label: String) -> Option<Edit> {
    let command = match target {
        RenameTarget::Frame(id) => {
            let frame = project.frame(id)?;
            (label != frame.label).then(|| Command::SetFrame {
                id,
                frame: Frame {
                    label,
                    ..frame.clone()
                },
            })?
        }
        RenameTarget::Group(id) => {
            let graph = project.graph();
            let shown = if graph.node(id)?.type_id == spare::MIXER {
                spare::mixer_name(graph, id)
            } else {
                layout::group_title(graph, id)
            };
            if label == shown {
                return None;
            }
            // An empty name puts the default back.
            Command::SetConfig {
                node: id,
                key: group::PORT_NAME.into(),
                value: (!label.is_empty()).then_some(noodle_core::Value::Text(label)),
            }
        }
        RenameTarget::Port { boundary, port, .. } => {
            spare::rename_group_port(project, boundary, port, &label)?
        }
    };
    Some(Edit::Apply(command))
}

/// The tooltip for whatever's under the pointer: the name of a socket that
/// has no label of its own, what the port carries (a control signal, a
/// parameter's range), then any problems.
fn hover_text(f: &Frame_<'_>, problems: &Problems, p: Pos2) -> Option<String> {
    let port = match hit(f, p) {
        Hit::Port(endpoint, side) => f.scene.port(&endpoint, side),
        _ => None,
    };
    let name = port
        .filter(|port| port.in_header)
        .map(|port| port.name.clone());
    let about = port.and_then(|port| match &port.kind {
        PortKind::Param(info) => Some(format!(
            "{} to {}",
            crate::widgets::format::format_value(info, info.min),
            crate::widgets::format::format_value(info, info.max)
        )),
        _ if port.control => Some("Control signal".to_owned()),
        _ => None,
    });
    let texts: Vec<String> = name
        .into_iter()
        .chain(about)
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
    let scene = state.peek_scene(session.project(), session.registry());
    let socket = scene.node(node)?.port(side, key)?.socket;
    Some(state.view.on(state.canvas).to_screen(socket))
}
