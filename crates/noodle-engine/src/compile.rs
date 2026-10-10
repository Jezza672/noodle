//! Compiles a project graph into a [`Schedule`]: which nodes run, in what
//! order, and which buffers they read and write.
//!
//! Problems don't stop compilation, so one bad node never silences the whole
//! project:
//!
//! - **Nodes that can't run** (unknown type, bad config, shapes that don't
//!   fit) are left out, and anything wired to them behaves as if unconnected.
//! - **Wires that can't work** (unknown port, audio to events, closing a loop
//!   that no node breaks) are ignored.
//!
//! **Feedback loops.** A wire may close a loop if the loop passes through a
//! node that names a [`NodeType::loop_input`], such as Delay: the node runs in
//! two steps, one that writes its outputs and one, later, that reads the loop
//! input (see [`Phase`]). That turns the loop into a chain with the node's two
//! halves at its ends. A node that isn't on a loop runs whole, as usual.
//!
//! Each problem is reported as a [`Diagnostic`], so the UI can show it on the
//! node or wire where it happened.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};
use std::fmt;
use std::sync::Arc;

use noodle_core::{Config, Endpoint, Graph, NodeId};

use crate::{InputKind, Lanes, Layout, Mode, NodeError, NodeType, Registry, Shape, ShapeError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventBufferId(pub usize);

/// The result of compiling a graph. Nothing here has been instantiated yet.
pub struct Schedule {
    /// In execution order: every node comes after the nodes it reads from.
    pub nodes: Vec<ScheduledNode>,
    /// How many lanes each signal buffer must hold, where a lane holds one
    /// block of samples. Buffers are reused once nothing reads them any more,
    /// so this is usually far shorter than the number of outputs.
    pub buffer_lanes: Vec<usize>,
    pub event_buffers: usize,
}

/// Which part of a node a schedule step runs. Only a node on a feedback loop
/// (see [`NodeType::loop_input`]) is split in two; everything else runs whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The node's whole job: reads its inputs, writes its outputs.
    Whole,
    /// Writes the outputs, reading every input but the loop input (which is
    /// [`InputSource::Absent`]).
    Output,
    /// Reads the loop input and nothing else. There are no outputs.
    Input,
}

pub struct ScheduledNode {
    pub id: NodeId,
    pub phase: Phase,
    pub node_type: Arc<dyn NodeType>,
    pub config: Config,
    pub layout: Layout,
    pub input_shapes: Vec<Shape>,
    pub output_shapes: Vec<Shape>,
    /// One per signal input, in layout order.
    pub inputs: Vec<InputSource>,
    /// One per signal output, in layout order. Never shared with this node's
    /// inputs or with each other.
    pub outputs: Vec<BufferId>,
    /// One per event input; `None` if unconnected.
    pub event_inputs: Vec<Option<EventBufferId>>,
    pub event_outputs: Vec<EventBufferId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputSource {
    /// Not read in this step: the other half of a split node reads it, and
    /// this half sees an empty silent placeholder.
    Absent,
    /// Connected: reads another node's output.
    Buffer(BufferId),
    /// Unconnected: holds the value set in the project, or the port's default.
    Value(f32),
    /// Connected to an offsetting parameter ([`Modulation::Offset`]): reads
    /// another node's output, which moves the parameter from the value set in
    /// the project (or the port's default) along its travel.
    Modulated(BufferId, f32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub location: Location,
    pub problem: Problem,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Location {
    Node(NodeId),
    /// A wire, identified by the input it goes into.
    Wire(Endpoint),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    UnknownNodeType(String),
    /// The node type rejected the node's config or input shapes.
    Node(NodeError),
    /// Offline nodes need cached renders, which come in M4.
    OfflineUnsupported,
    /// The project sets a value for an input the node doesn't have. The
    /// value is ignored but kept in the project.
    UnknownParam(String),
    UnknownPort(Endpoint),
    /// A wire between an audio port and an event port.
    KindMismatch,
    /// The wire closes a loop, and the loop doesn't pass through a node that
    /// can break it (a Delay), or it feeds a parameter of that node rather
    /// than the input the node reads last.
    Loop,
    /// The signal on this wire can't be broadcast with the node's other inputs.
    Shape(ShapeError),
    /// An automation lane targets an input that isn't a parameter.
    NotAParam(String),
    /// An automation lane targets a group's solo, which is read when the
    /// project is compiled and can't change along the timeline.
    LaneOnSolo,
    /// A wire feeds a parameter that also has an automation lane. The wire
    /// wins, and the lane does nothing.
    LaneOverridden,
    /// An Output node is tied to a device that isn't open: unplugged, or
    /// not one the audio settings could start. The node plays nothing.
    DeviceUnavailable(String),
    /// Another Output node already plays on this device, and at most one may.
    /// This one plays nothing.
    DeviceTaken(String),
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNodeType(id) => write!(f, "unknown node type `{id}`"),
            Self::Node(error) => error.fmt(f),
            Self::OfflineUnsupported => {
                f.write_str("offline nodes need cached renders, which aren't built yet")
            }
            Self::UnknownParam(key) => write!(f, "this node has no input `{key}`"),
            Self::UnknownPort(endpoint) => {
                write!(f, "node {} has no port `{}`", endpoint.node, endpoint.port)
            }
            Self::KindMismatch => f.write_str("can't connect an audio port to an event port"),
            Self::Loop => f.write_str(
                "this wire closes a loop, which has to pass through a Delay \
                 (into its signal input) or another node that breaks loops",
            ),
            Self::Shape(error) => error.fmt(f),
            Self::NotAParam(key) => {
                write!(f, "`{key}` isn't a parameter, so it can't be automated")
            }
            Self::LaneOnSolo => {
                f.write_str("solo is read when the project is compiled, so it can't be automated")
            }
            Self::LaneOverridden => {
                f.write_str("this input has a wire, which wins over its automation lane")
            }
            Self::DeviceUnavailable(device) => {
                write!(
                    f,
                    "output device `{device}` isn't available, so this plays nothing"
                )
            }
            Self::DeviceTaken(device) => write!(
                f,
                "another output already plays on `{device}`, and there can be only one"
            ),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.problem)
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(id) => write!(f, "node {id}"),
            Self::Wire(input) => write!(f, "wire into {input}"),
        }
    }
}

impl Diagnostic {
    pub(crate) fn node(id: NodeId, problem: Problem) -> Self {
        Self {
            location: Location::Node(id),
            problem,
        }
    }

    pub(crate) fn wire(input: &Endpoint, problem: Problem) -> Self {
        Self {
            location: Location::Wire(input.clone()),
            problem,
        }
    }
}

/// A node that resolved to a known type and layout.
struct Candidate<'a> {
    id: NodeId,
    node_type: Arc<dyn NodeType>,
    config: &'a Config,
    params: &'a BTreeMap<String, f32>,
    layout: Layout,
    input_shapes: Vec<Shape>,
    /// `None` until shape inference, and stays `None` if the node can't run.
    output_shapes: Option<Vec<Shape>>,
    /// The position of the input the node reads after writing its outputs,
    /// if it can break loops.
    loop_input: Option<usize>,
    /// Runs in two steps, because a feedback loop passes through it.
    split: bool,
}

/// A step of the schedule: a node, or half of one.
#[derive(Clone, Copy)]
struct Step {
    node: usize,
    phase: Phase,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Kind {
    Signal,
    Event,
}

/// A wire between two candidates, with its ports resolved to indices.
struct Wire {
    from: usize,
    from_port: usize,
    to: usize,
    to_port: usize,
    kind: Kind,
    input: Endpoint,
    /// Where the wire starts and ends in the graph of steps (see `vertex`).
    from_v: usize,
    to_v: usize,
}

/// Compiles a project's graph. Groups are flattened first, so the schedule
/// only ever contains the nodes inside them.
pub fn compile(graph: &Graph, registry: &Registry) -> (Schedule, Vec<Diagnostic>) {
    compile_with_lanes(graph, &[], registry)
}

/// Compiles a graph along with the automation lanes that drive its
/// parameters. Each lane becomes a hidden source node wired into its target.
pub fn compile_with_lanes(
    graph: &Graph,
    lanes: &Lanes<'_>,
    registry: &Registry,
) -> (Schedule, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let keep = crate::automation::boundary_targets(graph, lanes, &mut diagnostics);
    let graph = crate::flatten::flatten_keeping(graph, &keep);
    let graph = &*crate::automation::add_lanes(&graph, lanes, registry, &mut diagnostics);
    let mut candidates = resolve_nodes(graph, registry, &mut diagnostics);
    let mut wires = resolve_wires(graph, &candidates, &mut diagnostics);
    split_loop_nodes(&mut candidates, &wires);
    assign_vertices(&candidates, &mut wires);
    let wires = drop_loops(2 * candidates.len(), wires, &mut diagnostics);
    let order = sort(&candidates, &wires);

    // Which wire feeds each input, if any.
    let mut signal_source = vec![Vec::new(); candidates.len()];
    let mut event_source = vec![Vec::new(); candidates.len()];
    for (i, c) in candidates.iter().enumerate() {
        signal_source[i] = vec![None; c.layout.inputs.len()];
        event_source[i] = vec![None; c.layout.event_inputs.len()];
    }
    for (w, wire) in wires.iter().enumerate() {
        match wire.kind {
            Kind::Signal => signal_source[wire.to][wire.to_port] = Some(w),
            Kind::Event => event_source[wire.to][wire.to_port] = Some(w),
        }
    }

    infer_shapes(
        &mut candidates,
        &order,
        &wires,
        &signal_source,
        &mut diagnostics,
    );
    let schedule = allocate(candidates, &order, &wires, &signal_source, &event_source);
    (schedule, diagnostics)
}

fn resolve_nodes<'a>(
    graph: &'a Graph,
    registry: &Registry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Candidate<'a>> {
    let mut candidates = Vec::new();
    for (id, node) in graph.nodes() {
        let Some(node_type) = registry.get(&node.type_id) else {
            diagnostics.push(Diagnostic::node(
                id,
                Problem::UnknownNodeType(node.type_id.clone()),
            ));
            continue;
        };
        let layout = match node_type.layout(&node.config) {
            Ok(layout) => layout,
            Err(error) => {
                diagnostics.push(Diagnostic::node(id, Problem::Node(error)));
                continue;
            }
        };
        if layout.mode == Mode::Offline {
            diagnostics.push(Diagnostic::node(id, Problem::OfflineUnsupported));
            continue;
        }
        for key in node.params.keys() {
            if !layout.inputs.iter().any(|port| port.key == key.as_str()) {
                diagnostics.push(Diagnostic::node(id, Problem::UnknownParam(key.clone())));
            }
        }
        let loop_input = node_type
            .loop_input(&node.config)
            .and_then(|key| layout.inputs.iter().position(|p| p.key == key));
        candidates.push(Candidate {
            id,
            loop_input,
            split: false,
            node_type: Arc::clone(node_type),
            config: &node.config,
            params: &node.params,
            layout,
            input_shapes: Vec::new(),
            output_shapes: None,
        });
    }
    candidates
}

fn resolve_wires(
    graph: &Graph,
    candidates: &[Candidate<'_>],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Wire> {
    let index: HashMap<NodeId, usize> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id, i))
        .collect();

    let mut wires = Vec::new();
    for connection in graph.connections() {
        // A wire touching a node that didn't resolve is already covered by
        // that node's diagnostic.
        let (Some(&from), Some(&to)) = (
            index.get(&connection.from.node),
            index.get(&connection.to.node),
        ) else {
            continue;
        };
        let output = output_port(&candidates[from].layout, &connection.from.port);
        let input = input_port(&candidates[to].layout, &connection.to.port);
        let problem = match (output, input) {
            (None, _) => Problem::UnknownPort(connection.from.clone()),
            (_, None) => Problem::UnknownPort(connection.to.clone()),
            (Some((a, _)), Some((b, _))) if a != b => Problem::KindMismatch,
            (Some((kind, from_port)), Some((_, to_port))) => {
                wires.push(Wire {
                    from_v: 0,
                    to_v: 0,
                    from,
                    from_port,
                    to,
                    to_port,
                    kind,
                    input: connection.to,
                });
                continue;
            }
        };
        diagnostics.push(Diagnostic::wire(&connection.to, problem));
    }
    wires
}

fn output_port(layout: &Layout, key: &str) -> Option<(Kind, usize)> {
    let signal = layout.outputs.iter().position(|p| p.key == key);
    let event = || layout.event_outputs.iter().position(|p| p.key == key);
    signal
        .map(|i| (Kind::Signal, i))
        .or_else(|| event().map(|i| (Kind::Event, i)))
}

fn input_port(layout: &Layout, key: &str) -> Option<(Kind, usize)> {
    let signal = layout.inputs.iter().position(|p| p.key == key);
    let event = || layout.event_inputs.iter().position(|p| p.key == key);
    signal
        .map(|i| (Kind::Signal, i))
        .or_else(|| event().map(|i| (Kind::Event, i)))
}

/// Drops every wire that closes a loop, found as the back edges of a
/// depth-first search. The search visits nodes and wires in ID order, so the
/// same graph always loses the same wires.
fn drop_loops(vertices: usize, wires: Vec<Wire>, diagnostics: &mut Vec<Diagnostic>) -> Vec<Wire> {
    let nodes = vertices;
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        OnPath,
        Done,
    }

    let mut outgoing = vec![Vec::new(); nodes];
    for (w, wire) in wires.iter().enumerate() {
        outgoing[wire.from_v].push(w);
    }
    let mut state = vec![State::Unvisited; nodes];
    let mut closes_loop = vec![false; wires.len()];

    for root in 0..nodes {
        if state[root] != State::Unvisited {
            continue;
        }
        state[root] = State::OnPath;
        // Each entry is a node on the current path and how many of its
        // outgoing wires have been followed.
        let mut path = vec![(root, 0)];
        while let Some(top) = path.last_mut() {
            let (node, followed) = *top;
            let Some(&w) = outgoing[node].get(followed) else {
                state[node] = State::Done;
                path.pop();
                continue;
            };
            top.1 += 1;
            let target = wires[w].to_v;
            match state[target] {
                State::Unvisited => {
                    state[target] = State::OnPath;
                    path.push((target, 0));
                }
                State::OnPath => closes_loop[w] = true,
                State::Done => {}
            }
        }
    }

    wires
        .into_iter()
        .zip(closes_loop)
        .filter_map(|(wire, closes_loop)| {
            if closes_loop {
                diagnostics.push(Diagnostic::wire(&wire.input, Problem::Loop));
                None
            } else {
                Some(wire)
            }
        })
        .collect()
}

/// Marks the loop-breaking nodes that a feedback loop actually passes
/// through, by the wire into their loop input.
fn split_loop_nodes(candidates: &mut [Candidate<'_>], wires: &[Wire]) {
    let mut outgoing = vec![Vec::new(); candidates.len()];
    for wire in wires {
        outgoing[wire.from].push(wire.to);
    }
    for i in 0..candidates.len() {
        let Some(port) = candidates[i].loop_input else {
            continue;
        };
        // Everything downstream of the node, itself included.
        let mut reached = vec![false; candidates.len()];
        let mut stack = vec![i];
        reached[i] = true;
        while let Some(node) = stack.pop() {
            for &next in &outgoing[node] {
                if !reached[next] {
                    reached[next] = true;
                    stack.push(next);
                }
            }
        }
        candidates[i].split = wires
            .iter()
            .any(|w| w.to == i && w.kind == Kind::Signal && w.to_port == port && reached[w.from]);
    }
}

/// The step graph: node `i` is vertex `i`, and a split node's output half is
/// vertex `n + i` (the vertex `i` then being its input half). Wires start at
/// the output half and end at the input half only if they feed the loop
/// input; every other wire into a split node feeds its output half.
fn assign_vertices(candidates: &[Candidate<'_>], wires: &mut [Wire]) {
    let n = candidates.len();
    for wire in wires {
        wire.from_v = if candidates[wire.from].split {
            n + wire.from
        } else {
            wire.from
        };
        let to = &candidates[wire.to];
        let feeds_loop = wire.kind == Kind::Signal && to.loop_input == Some(wire.to_port);
        wire.to_v = if to.split && !feeds_loop {
            n + wire.to
        } else {
            wire.to
        };
    }
}

/// Orders the steps so each comes after everything it reads from, breaking
/// ties by node so the order is stable. A split node's output half comes
/// before its input half.
fn sort(candidates: &[Candidate<'_>], wires: &[Wire]) -> Vec<Step> {
    let n = candidates.len();
    let exists = |v: usize| v < n || candidates[v - n].split;
    let mut outgoing = vec![Vec::new(); 2 * n];
    let mut waiting_on = vec![0; 2 * n];
    let mut edge = |from: usize, to: usize| {
        outgoing[from].push(to);
        waiting_on[to] += 1;
    };
    for wire in wires {
        edge(wire.from_v, wire.to_v);
    }
    for (i, c) in candidates.iter().enumerate() {
        if c.split {
            edge(n + i, i);
        }
    }
    let key = |v: usize| Reverse((v % n, v));
    let mut ready: BinaryHeap<Reverse<(usize, usize)>> = (0..2 * n)
        .filter(|&v| exists(v) && waiting_on[v] == 0)
        .map(key)
        .collect();
    let mut order = Vec::with_capacity(n);
    while let Some(Reverse((_, v))) = ready.pop() {
        order.push(Step {
            node: v % n,
            phase: match (v >= n, candidates[v % n].split) {
                (true, _) => Phase::Output,
                (false, true) => Phase::Input,
                (false, false) => Phase::Whole,
            },
        });
        for &next in &outgoing[v] {
            waiting_on[next] -= 1;
            if waiting_on[next] == 0 {
                ready.push(key(next));
            }
        }
    }
    debug_assert_eq!(
        order.len(),
        (0..2 * n).filter(|&v| exists(v)).count(),
        "loops should have been dropped"
    );
    order
}

/// How many times shape inference may start over for loops (see below).
const SHAPE_PASSES: usize = 4;

fn infer_shapes(
    candidates: &mut [Candidate<'_>],
    order: &[Step],
    wires: &[Wire],
    signal_source: &[Vec<Option<usize>>],
    diagnostics: &mut Vec<Diagnostic>,
) {
    // A split node's outputs are inferred before its loop input exists, so
    // they assume the input is whatever shape the last pass found it to be,
    // mono to begin with. If the input turns out different, start over with
    // that shape. Most loops settle at once; a few passes settle the rest.
    let mut assumed = vec![Shape::MONO; candidates.len()];
    for pass in 1..=SHAPE_PASSES {
        let mut found = Vec::new();
        let mut restart = false;
        for c in candidates.iter_mut() {
            c.output_shapes = None;
        }
        for step in order {
            let i = step.node;
            // An input fed by a node that can't run acts as unconnected, so
            // it's mono like any other unconnected input.
            let mut input_shapes: Vec<Shape> = signal_source[i]
                .iter()
                .map(|source| {
                    source
                        .and_then(|w| {
                            let wire = &wires[w];
                            let shapes = candidates[wire.from].output_shapes.as_ref()?;
                            Some(shapes[wire.from_port])
                        })
                        .unwrap_or(Shape::MONO)
                })
                .collect();
            let c = &candidates[i];
            if step.phase == Phase::Output {
                input_shapes[c.loop_input.expect("split nodes have a loop input")] = assumed[i];
            }
            if step.phase == Phase::Input && c.output_shapes.is_none() {
                continue;
            }
            let result = c
                .node_type
                .output_shapes(c.config, &c.layout, &input_shapes);
            if step.phase == Phase::Input {
                // The loop input is known now. Did the outputs assume right?
                let port = c.loop_input.expect("split nodes have a loop input");
                if result.as_ref().ok() != c.output_shapes.as_ref() {
                    let real = input_shapes[port];
                    if pass < SHAPE_PASSES && real != assumed[i] {
                        assumed[i] = real;
                        restart = true;
                    } else {
                        let problem = match result {
                            Err(error) => error,
                            Ok(_) => NodeError::config(
                                "the shapes round this feedback loop don't settle",
                            ),
                        };
                        found.push(Diagnostic::node(c.id, Problem::Node(problem)));
                        // Nodes downstream took their shapes from the
                        // outputs; they stay as inferred, and this node
                        // simply doesn't run.
                        candidates[i].output_shapes = None;
                    }
                } else {
                    candidates[i].input_shapes = input_shapes;
                }
                continue;
            }
            let output_shapes = match result {
                Ok(shapes) if shapes.len() == c.layout.outputs.len() => Some(shapes),
                Ok(shapes) => {
                    let message = format!(
                        "node type bug: {} output shapes for {} outputs",
                        shapes.len(),
                        c.layout.outputs.len()
                    );
                    found.push(Diagnostic::node(
                        c.id,
                        Problem::Node(NodeError::config(message)),
                    ));
                    None
                }
                Err(NodeError::Shape(error)) => {
                    found.push(blame_shape(
                        c.id,
                        error,
                        &input_shapes,
                        &signal_source[i],
                        wires,
                    ));
                    None
                }
                Err(error) => {
                    found.push(Diagnostic::node(c.id, Problem::Node(error)));
                    None
                }
            };
            let c = &mut candidates[i];
            c.input_shapes = input_shapes;
            c.output_shapes = output_shapes;
        }
        if !restart {
            diagnostics.extend(found);
            return;
        }
    }
}

/// Puts a broadcasting error on the first wire whose shape doesn't fit the
/// inputs before it, or on the node if no single wire is to blame.
fn blame_shape(
    node: NodeId,
    error: ShapeError,
    input_shapes: &[Shape],
    sources: &[Option<usize>],
    wires: &[Wire],
) -> Diagnostic {
    let mut so_far = Shape::MONO;
    for (shape, source) in input_shapes.iter().zip(sources) {
        match so_far.broadcast(*shape) {
            Ok(shape) => so_far = shape,
            Err(error) => {
                if let Some(w) = source {
                    return Diagnostic::wire(&wires[*w].input, Problem::Shape(error));
                }
            }
        }
    }
    Diagnostic::node(node, Problem::Node(NodeError::Shape(error)))
}

/// Assigns buffers to every input and output of the nodes that can run.
///
/// A buffer is freed once the last node reading it has run, and reused for a
/// later output. A node's outputs are allocated before its own inputs are
/// freed, so they never share a buffer with them.
fn allocate(
    candidates: Vec<Candidate<'_>>,
    order: &[Step],
    wires: &[Wire],
    signal_source: &[Vec<Option<usize>>],
    event_source: &[Vec<Option<usize>>],
) -> Schedule {
    let n = candidates.len();
    let runs: Vec<Step> = order
        .iter()
        .copied()
        .filter(|step| candidates[step.node].output_shapes.is_some())
        .collect();
    // Steps by vertex (see `assign_vertices`).
    let mut step = vec![None; 2 * n];
    for (s, run) in runs.iter().enumerate() {
        step[if run.phase == Phase::Output {
            n + run.node
        } else {
            run.node
        }] = Some(s);
    }
    // A wire only carries signal if both its ends run.
    let live = |w: &usize| step[wires[*w].from_v].is_some() && step[wires[*w].to_v].is_some();

    // The last step that reads each output, keyed by (node, port).
    let mut last_read: HashMap<(Kind, usize, usize), usize> = HashMap::new();
    for (w, wire) in wires.iter().enumerate() {
        if live(&w) {
            let reader = step[wire.to_v].unwrap();
            let entry = last_read
                .entry((wire.kind, wire.from, wire.from_port))
                .or_insert(reader);
            *entry = (*entry).max(reader);
        }
    }

    // An automation lane's value is the parameter's value, so a lane never
    // offsets, whatever the parameter does with other wires.
    let from_lane: Vec<bool> = candidates
        .iter()
        .map(|c| c.node_type.info().id == crate::AUTOMATION_ID)
        .collect();

    let mut buffer_lanes = Vec::new();
    let mut free_buffers = Vec::new();
    let mut event_buffers = 0;
    let mut free_events = Vec::new();
    let mut written: HashMap<(Kind, usize, usize), usize> = HashMap::new();
    let mut nodes = Vec::with_capacity(runs.len());
    let mut candidates: Vec<Option<Candidate<'_>>> = candidates.into_iter().map(Some).collect();

    for (s, run) in runs.iter().enumerate() {
        let (i, phase) = (run.node, run.phase);
        // The output half leaves the candidate for the input half.
        let c = if phase == Phase::Output {
            let c = candidates[i].as_ref().unwrap();
            Candidate {
                id: c.id,
                node_type: Arc::clone(&c.node_type),
                config: c.config,
                params: c.params,
                layout: c.layout.clone(),
                input_shapes: c.input_shapes.clone(),
                output_shapes: c.output_shapes.clone(),
                loop_input: c.loop_input,
                split: c.split,
            }
        } else {
            candidates[i].take().unwrap()
        };
        // Whether this step reads the input at `port`.
        let reads = |port: usize| match phase {
            Phase::Whole => true,
            Phase::Output => Some(port) != c.loop_input,
            Phase::Input => Some(port) == c.loop_input,
        };
        let source_buffer = |w: usize| {
            let wire = &wires[w];
            written[&(wire.kind, wire.from, wire.from_port)]
        };

        let inputs = c
            .layout
            .inputs
            .iter()
            .zip(&signal_source[i])
            .enumerate()
            .map(|(index, (port, source))| {
                if !reads(index) {
                    return InputSource::Absent;
                }
                let value = c
                    .params
                    .get(port.key.as_ref())
                    .copied()
                    .unwrap_or_else(|| port.default_value());
                match source.filter(live) {
                    Some(w) => {
                        let buffer = BufferId(source_buffer(w));
                        match &port.kind {
                            InputKind::Param(info)
                                if info.is_offset() && !from_lane[wires[w].from] =>
                            {
                                InputSource::Modulated(buffer, value)
                            }
                            _ => InputSource::Buffer(buffer),
                        }
                    }
                    None => InputSource::Value(value),
                }
            })
            .collect();
        // Events belong to the whole node or its output half.
        let events = if phase == Phase::Input { 0 } else { usize::MAX };
        let event_inputs = event_source[i]
            .iter()
            .map(|source| {
                source
                    .filter(live)
                    .filter(|_| events > 0)
                    .map(|w| EventBufferId(source_buffer(w)))
            })
            .collect();

        let output_shapes = c.output_shapes.clone().unwrap();
        let written_here = if phase == Phase::Input { 0 } else { usize::MAX };
        let outputs: Vec<BufferId> = output_shapes
            .iter()
            .take(written_here)
            .enumerate()
            .map(|(port, shape)| {
                let b = take_buffer(&mut buffer_lanes, &mut free_buffers, shape.lanes());
                written.insert((Kind::Signal, i, port), b);
                BufferId(b)
            })
            .collect();
        let event_outputs: Vec<EventBufferId> = (0..c.layout.event_outputs.len().min(events))
            .map(|port| {
                let b = free_events.pop().unwrap_or_else(|| {
                    event_buffers += 1;
                    event_buffers - 1
                });
                written.insert((Kind::Event, i, port), b);
                EventBufferId(b)
            })
            .collect();

        // Free whatever this step was the last to read, then any outputs
        // nothing reads at all.
        let finished: BTreeSet<(Kind, usize, usize)> = signal_source[i]
            .iter()
            .enumerate()
            .filter(|(port, _)| reads(*port))
            .map(|(_, source)| source)
            .chain(event_source[i].iter().take(events))
            .flatten()
            .filter(|w| live(w))
            .map(|&w| (wires[w].kind, wires[w].from, wires[w].from_port))
            .filter(|key| last_read[key] == s)
            .collect();
        let unread = (0..outputs.len())
            .map(|port| (Kind::Signal, i, port))
            .chain((0..event_outputs.len()).map(|port| (Kind::Event, i, port)))
            .filter(|key| !last_read.contains_key(key));
        for key in finished.into_iter().chain(unread) {
            match key.0 {
                Kind::Signal => free_buffers.push(written[&key]),
                Kind::Event => free_events.push(written[&key]),
            }
        }

        nodes.push(ScheduledNode {
            id: c.id,
            phase,
            node_type: c.node_type,
            config: c.config.clone(),
            layout: c.layout,
            input_shapes: c.input_shapes,
            output_shapes,
            inputs,
            outputs,
            event_inputs,
            event_outputs,
        });
    }

    Schedule {
        nodes,
        buffer_lanes,
        event_buffers,
    }
}

/// Takes the smallest free buffer that's big enough. Failing that, it grows
/// the biggest free buffer, and failing that, it adds a new one.
fn take_buffer(buffer_lanes: &mut Vec<usize>, free: &mut Vec<usize>, lanes: usize) -> usize {
    let big_enough = free
        .iter()
        .enumerate()
        .filter(|(_, b)| buffer_lanes[**b] >= lanes)
        .min_by_key(|(_, b)| buffer_lanes[**b])
        .map(|(i, _)| i);
    let biggest = || {
        free.iter()
            .enumerate()
            .max_by_key(|(_, b)| buffer_lanes[**b])
            .map(|(i, _)| i)
    };
    match big_enough.or_else(biggest) {
        Some(i) => {
            let b = free.swap_remove(i);
            buffer_lanes[b] = buffer_lanes[b].max(lanes);
            b
        }
        None => {
            buffer_lanes.push(lanes);
            buffer_lanes.len() - 1
        }
    }
}

#[cfg(test)]
mod tests;
