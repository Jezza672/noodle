//! Compiles a project graph into a [`Schedule`]: which nodes run, in what
//! order, and which buffers they read and write.
//!
//! Problems don't stop compilation, so one bad node never silences the whole
//! project:
//!
//! - **Nodes that can't run** (unknown type, bad config, shapes that don't
//!   fit) are left out, and anything wired to them behaves as if unconnected.
//! - **Wires that can't work** (unknown port, audio to events, closing a loop)
//!   are ignored.
//!
//! Each problem is reported as a [`Diagnostic`], so the UI can show it on the
//! node or wire where it happened.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};
use std::fmt;
use std::sync::Arc;

use noodle_core::{Config, Endpoint, Graph, NodeId};

use crate::{Layout, Mode, NodeError, NodeType, Registry, Shape, ShapeError};

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

pub struct ScheduledNode {
    pub id: NodeId,
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
    /// Connected: reads another node's output.
    Buffer(BufferId),
    /// Unconnected: holds the value set in the project, or the port's default.
    Value(f32),
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
    /// The wire closes a loop. Loops will need to pass through a Delay node.
    Loop,
    /// The signal on this wire can't be broadcast with the node's other inputs.
    Shape(ShapeError),
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
                "this wire closes a loop, and loops aren't supported yet \
                 (they'll need to go through a Delay node)",
            ),
            Self::Shape(error) => error.fmt(f),
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
}

/// Compiles a project's graph. Groups are flattened first, so the schedule
/// only ever contains the nodes inside them.
pub fn compile(graph: &Graph, registry: &Registry) -> (Schedule, Vec<Diagnostic>) {
    let graph = &*crate::flatten::flatten(graph);
    let mut diagnostics = Vec::new();
    let mut candidates = resolve_nodes(graph, registry, &mut diagnostics);
    let wires = resolve_wires(graph, &candidates, &mut diagnostics);
    let wires = drop_loops(candidates.len(), wires, &mut diagnostics);
    let order = sort(candidates.len(), &wires);

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
        candidates.push(Candidate {
            id,
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
fn drop_loops(nodes: usize, wires: Vec<Wire>, diagnostics: &mut Vec<Diagnostic>) -> Vec<Wire> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        OnPath,
        Done,
    }

    let mut outgoing = vec![Vec::new(); nodes];
    for (w, wire) in wires.iter().enumerate() {
        outgoing[wire.from].push(w);
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
            let target = wires[w].to;
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

/// Orders the nodes so each comes after everything it reads from, breaking
/// ties by ID so the order is stable.
fn sort(nodes: usize, wires: &[Wire]) -> Vec<usize> {
    let mut outgoing = vec![Vec::new(); nodes];
    let mut waiting_on = vec![0; nodes];
    for wire in wires {
        outgoing[wire.from].push(wire.to);
        waiting_on[wire.to] += 1;
    }
    let mut ready: BinaryHeap<Reverse<usize>> = (0..nodes)
        .filter(|&i| waiting_on[i] == 0)
        .map(Reverse)
        .collect();
    let mut order = Vec::with_capacity(nodes);
    while let Some(Reverse(i)) = ready.pop() {
        order.push(i);
        for &next in &outgoing[i] {
            waiting_on[next] -= 1;
            if waiting_on[next] == 0 {
                ready.push(Reverse(next));
            }
        }
    }
    debug_assert_eq!(order.len(), nodes, "loops should have been dropped");
    order
}

fn infer_shapes(
    candidates: &mut [Candidate<'_>],
    order: &[usize],
    wires: &[Wire],
    signal_source: &[Vec<Option<usize>>],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for &i in order {
        // An input fed by a node that can't run acts as unconnected, so it's
        // mono like any other unconnected input.
        let input_shapes: Vec<Shape> = signal_source[i]
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
        let result = c
            .node_type
            .output_shapes(c.config, &c.layout, &input_shapes);
        let output_shapes = match result {
            Ok(shapes) if shapes.len() == c.layout.outputs.len() => Some(shapes),
            Ok(shapes) => {
                let message = format!(
                    "node type bug: {} output shapes for {} outputs",
                    shapes.len(),
                    c.layout.outputs.len()
                );
                diagnostics.push(Diagnostic::node(
                    c.id,
                    Problem::Node(NodeError::config(message)),
                ));
                None
            }
            Err(NodeError::Shape(error)) => {
                diagnostics.push(blame_shape(
                    c.id,
                    error,
                    &input_shapes,
                    &signal_source[i],
                    wires,
                ));
                None
            }
            Err(error) => {
                diagnostics.push(Diagnostic::node(c.id, Problem::Node(error)));
                None
            }
        };
        let c = &mut candidates[i];
        c.input_shapes = input_shapes;
        c.output_shapes = output_shapes;
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
    order: &[usize],
    wires: &[Wire],
    signal_source: &[Vec<Option<usize>>],
    event_source: &[Vec<Option<usize>>],
) -> Schedule {
    let runs: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| candidates[i].output_shapes.is_some())
        .collect();
    let mut step = vec![None; candidates.len()];
    for (s, &i) in runs.iter().enumerate() {
        step[i] = Some(s);
    }
    // A wire only carries signal if both its ends run.
    let live = |w: &usize| step[wires[*w].from].is_some() && step[wires[*w].to].is_some();

    // The last step that reads each output, keyed by (node, port).
    let mut last_read: HashMap<(Kind, usize, usize), usize> = HashMap::new();
    for (w, wire) in wires.iter().enumerate() {
        if live(&w) {
            let reader = step[wire.to].unwrap();
            let entry = last_read
                .entry((wire.kind, wire.from, wire.from_port))
                .or_insert(reader);
            *entry = (*entry).max(reader);
        }
    }

    let mut buffer_lanes = Vec::new();
    let mut free_buffers = Vec::new();
    let mut event_buffers = 0;
    let mut free_events = Vec::new();
    let mut written: HashMap<(Kind, usize, usize), usize> = HashMap::new();
    let mut nodes = Vec::with_capacity(runs.len());
    let mut candidates: Vec<Option<Candidate<'_>>> = candidates.into_iter().map(Some).collect();

    for (s, &i) in runs.iter().enumerate() {
        let c = candidates[i].take().unwrap();
        let source_buffer = |w: usize| {
            let wire = &wires[w];
            written[&(wire.kind, wire.from, wire.from_port)]
        };

        let inputs = c
            .layout
            .inputs
            .iter()
            .zip(&signal_source[i])
            .map(|(port, source)| match source.filter(live) {
                Some(w) => InputSource::Buffer(BufferId(source_buffer(w))),
                None => InputSource::Value(
                    c.params
                        .get(port.key.as_ref())
                        .copied()
                        .unwrap_or_else(|| port.default_value()),
                ),
            })
            .collect();
        let event_inputs = event_source[i]
            .iter()
            .map(|source| source.filter(live).map(|w| EventBufferId(source_buffer(w))))
            .collect();

        let output_shapes = c.output_shapes.clone().unwrap();
        let outputs: Vec<BufferId> = output_shapes
            .iter()
            .enumerate()
            .map(|(port, shape)| {
                let b = take_buffer(&mut buffer_lanes, &mut free_buffers, shape.lanes());
                written.insert((Kind::Signal, i, port), b);
                BufferId(b)
            })
            .collect();
        let event_outputs: Vec<EventBufferId> = (0..c.layout.event_outputs.len())
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
            .chain(&event_source[i])
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
