//! Cache keys and cacheability: which nodes' output is a pure function of the
//! timeline, and what to call a render of it. See "Caching" in
//! docs/ARCHITECTURE.md.
//!
//! [`analyze`] compiles the project as the live engine would, except that
//! offline nodes stay in, then walks the schedule once, upstream first. Each
//! node gets a [`CacheKey`] hashing:
//!
//! - its ID, type, type version and config (the ID because a node's seed
//!   comes from it, so two identical noise nodes render differently),
//! - what each input holds: the constant, or the key of the output wired in,
//! - the tempo map, sample rate and length of the render,
//! - whatever the node type adds with [`NodeType::cache_inputs`] (a track
//!   input adds its clips and the content hash of every file they play).
//!
//! A node that isn't deterministic (live input), that sits on a feedback loop
//! or that follows such a node has no key. Automation lanes are nodes in the
//! compiled graph, so the points driving a node are part of its key.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use noodle_core::{CacheKey, Endpoint, KeyBuilder, NodeId, Project};

use crate::compile::{Options, compile_with};
use crate::{BufferId, EventBufferId, InputSource, Mode, Phase, Registry, Shape};

/// What a key can depend on besides the graph.
pub struct CacheEnv<'a> {
    pub project: &'a Project,
    pub sample_rate: f32,
    /// How many frames the render covers, from the start of the timeline.
    pub frames: u64,
    /// The content hash of a clip's audio file, given its path as the clip
    /// has it. `None` if the file can't be read.
    pub file_key: &'a dyn Fn(&str) -> Option<CacheKey>,
}

/// Why an output can't be cached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Uncacheable {
    /// The node type says identical inputs can give different output, such
    /// as the audio input.
    NotDeterministic(String),
    /// The node is on a feedback loop, whose output depends on itself.
    Loop,
    /// The node type couldn't key it, with its reason.
    Because(String),
    /// Something upstream can't be cached.
    Upstream(NodeId, Box<Uncacheable>),
    /// The node didn't compile.
    Broken,
}

impl fmt::Display for Uncacheable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDeterministic(name) => {
                write!(f, "{name} isn't the same every time it plays")
            }
            Self::Loop => f.write_str("it is on a feedback loop"),
            Self::Because(why) => f.write_str(why),
            Self::Upstream(node, why) => write!(f, "node {node} feeds it, and {why}"),
            Self::Broken => f.write_str("it doesn't compile"),
        }
    }
}

/// Where a signal input of a node gets its signal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputOrigin {
    /// Another node's output: its node in the flattened graph and the
    /// position of the output.
    Wire(NodeId, usize),
    /// Unconnected: the value set in the project, or the port's default.
    Value(f32),
}

#[derive(Clone, Debug)]
pub struct NodeAnalysis {
    pub type_id: String,
    pub offline: bool,
    pub key: Result<CacheKey, Uncacheable>,
    /// One per signal input, in layout order. An offline node reads these.
    pub inputs: Vec<InputOrigin>,
    /// Port keys of the signal outputs, in layout order.
    pub output_ports: Vec<String>,
    pub output_shapes: Vec<Shape>,
}

/// An output to render and store: what freezing a node or group, or an
/// offline node, produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub kind: TargetKind,
    /// Signal outputs in the flattened graph, as node and output position.
    pub outputs: Vec<(NodeId, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetKind {
    /// An offline node, whose output is always cached.
    Offline(NodeId),
    /// A node or group marked frozen.
    Frozen(NodeId),
}

impl TargetKind {
    pub fn node(self) -> NodeId {
        match self {
            Self::Offline(node) | Self::Frozen(node) => node,
        }
    }
}

#[derive(Default)]
pub struct Analysis {
    /// Every node of the flattened graph that compiled, in schedule order.
    pub order: Vec<NodeId>,
    pub nodes: BTreeMap<NodeId, NodeAnalysis>,
    /// What to render, upstream first.
    pub targets: Vec<Target>,
}

impl Analysis {
    /// The key a render of one output is stored under.
    pub fn output_key(&self, node: NodeId, output: usize) -> Result<CacheKey, Uncacheable> {
        let analysis = self.nodes.get(&node).ok_or(Uncacheable::Broken)?;
        analysis.key.clone().map(|key| output_key(&key, output))
    }

    /// Why a target can't be rendered, if it can't.
    pub fn blocked(&self, target: &Target) -> Option<Uncacheable> {
        if target.outputs.is_empty() {
            return Some(Uncacheable::Because("it has no audio output".into()));
        }
        target
            .outputs
            .iter()
            .find_map(|&(node, output)| self.output_key(node, output).err())
    }

    /// The target for a node or group the project marks frozen, or an
    /// offline node.
    pub fn target(&self, kind: TargetKind) -> Option<&Target> {
        self.targets.iter().find(|t| t.kind == kind)
    }
}

/// The key one output of a node is stored under.
pub fn output_key(node_key: &CacheKey, output: usize) -> CacheKey {
    KeyBuilder::new("output")
        .key(node_key)
        .u64(output as u64)
        .finish()
}

/// Works out the keys and the cacheable targets of a project: its offline
/// nodes, and its frozen nodes and groups.
pub fn analyze(project: &Project, registry: &Registry, env: &CacheEnv<'_>) -> Analysis {
    let lanes: Vec<_> = project.lanes().collect();
    let options = Options {
        keep_offline: true,
        ..Options::default()
    };
    let (schedule, _) = compile_with(project.graph(), &lanes, registry, &options);

    let mut analysis = Analysis::default();
    let mut signal_writers: HashMap<BufferId, (NodeId, usize)> = HashMap::new();
    let mut event_writers: HashMap<EventBufferId, (NodeId, usize)> = HashMap::new();
    for node in &schedule.nodes {
        let (id, layout) = (node.id, &node.layout);
        let info = node.node_type.info();
        let offline = layout.mode == Mode::Offline;
        let inputs: Vec<InputOrigin> = node
            .inputs
            .iter()
            .map(|source| match source {
                InputSource::Buffer(b) | InputSource::Modulated(b, _) => signal_writers
                    .get(b)
                    .map_or(InputOrigin::Value(0.0), |&(n, p)| InputOrigin::Wire(n, p)),
                InputSource::Value(v) => InputOrigin::Value(*v),
                InputSource::Absent => InputOrigin::Value(0.0),
            })
            .collect();

        // The input half of a split node doesn't get its own entry; the
        // node as a whole is on a loop, so it has no key.
        let key = if node.phase == Phase::Input {
            None
        } else if node.phase == Phase::Output {
            Some(Err(Uncacheable::Loop))
        } else if !layout.deterministic {
            Some(Err(Uncacheable::NotDeterministic(info.name.to_string())))
        } else {
            let upstream = |origin: &InputOrigin| match origin {
                InputOrigin::Wire(n, p) => Some((*n, *p)),
                InputOrigin::Value(_) => None,
            };
            let events: Vec<_> = node
                .event_inputs
                .iter()
                .map(|e| e.and_then(|e| event_writers.get(&e).copied()))
                .collect();
            Some(node_key(
                &analysis,
                env,
                node,
                &inputs.iter().filter_map(upstream).collect::<Vec<_>>(),
                &events,
            ))
        };
        if let Some(key) = key {
            analysis.order.push(id);
            analysis.nodes.insert(
                id,
                NodeAnalysis {
                    type_id: info.id.to_string(),
                    offline,
                    key,
                    inputs,
                    output_ports: layout.outputs.iter().map(|p| p.key.to_string()).collect(),
                    output_shapes: node.output_shapes.clone(),
                },
            );
        }
        for (port, &b) in node.outputs.iter().enumerate() {
            signal_writers.insert(b, (id, port));
        }
        for (port, &e) in node.event_outputs.iter().enumerate() {
            event_writers.insert(e, (id, port));
        }
    }

    analysis.targets = targets(project, &analysis);
    analysis
}

/// Part of every key, so renders made by another build of the DSP are never
/// replayed from the shared per-user cache. Bump `DSP_GENERATION` in the same
/// change as any regenerated golden render; the crate version covers releases.
const DSP_GENERATION: u32 = 1;

/// The key of a node, from everything it depends on. `wired` lists the
/// outputs wired into signal inputs in input order, and `events` those wired
/// into event inputs.
fn node_key(
    analysis: &Analysis,
    env: &CacheEnv<'_>,
    node: &crate::ScheduledNode,
    wired: &[(NodeId, usize)],
    events: &[Option<(NodeId, usize)>],
) -> Result<CacheKey, Uncacheable> {
    let info = node.node_type.info();
    // An offline node reads its inputs as whole signals, so it can't yet
    // take a modulated parameter or events. Say so, rather than render it
    // wrongly.
    if node.layout.mode == crate::Mode::Offline
        && (node
            .inputs
            .iter()
            .any(|source| matches!(source, InputSource::Modulated(..)))
            || events.iter().any(Option::is_some))
    {
        return Err(Uncacheable::Because(
            "an offline node can't take a modulated parameter or events yet".into(),
        ));
    }
    let mut b = KeyBuilder::new("node");
    b.str(env!("CARGO_PKG_VERSION"))
        .u64(u64::from(DSP_GENERATION))
        .u64(node.id.0)
        .str(info.id)
        .u64(u64::from(info.version))
        .config(&node.config)
        .f32(env.sample_rate)
        .u64(env.frames)
        .tempo_map(env.project.tempo_map());
    let upstream = |b: &mut KeyBuilder, (n, p): (NodeId, usize)| {
        analysis
            .output_key(n, p)
            .map(|key| {
                b.key(&key);
            })
            .map_err(|why| Uncacheable::Upstream(n, Box::new(why)))
    };
    let mut wires = wired.iter();
    for ((port, source), shape) in node
        .layout
        .inputs
        .iter()
        .zip(&node.inputs)
        .zip(&node.input_shapes)
    {
        b.str(&port.key)
            .u64(shape.voices as u64)
            .u64(shape.channels as u64);
        match source {
            InputSource::Value(v) => {
                b.str("value").f32(*v);
            }
            InputSource::Absent => {
                b.str("absent");
            }
            InputSource::Buffer(_) => {
                b.str("wire");
                upstream(&mut b, *wires.next().expect("a wired input has a source"))?;
            }
            InputSource::Modulated(_, base) => {
                b.str("offset").f32(*base);
                upstream(&mut b, *wires.next().expect("a wired input has a source"))?;
            }
        }
    }
    b.u64(events.len() as u64);
    for event in events {
        match event {
            Some(source) => {
                b.str("events");
                upstream(&mut b, *source)?;
            }
            None => {
                b.str("none");
            }
        }
    }
    node.node_type
        .cache_inputs(node.id, &node.config, env, &mut b)?;
    Ok(b.finish())
}

/// What to render, upstream first: each offline node's outputs, and the
/// outputs a frozen node or group exposes.
fn targets(project: &Project, analysis: &Analysis) -> Vec<Target> {
    let mut targets = Vec::new();
    for (&id, node) in &analysis.nodes {
        if node.offline {
            targets.push(Target {
                kind: TargetKind::Offline(id),
                outputs: (0..node.output_ports.len()).map(|p| (id, p)).collect(),
            });
        }
    }
    let lanes: Vec<_> = project.lanes().collect();
    let keep = crate::automation::boundary_targets(project.graph(), &lanes, &mut Vec::new());
    for &frozen in project.frozen() {
        let Some(node) = project.graph().node(frozen) else {
            continue;
        };
        let endpoints: Vec<Endpoint> = if node.type_id == noodle_core::group::GROUP {
            crate::flatten::group_outputs(project.graph(), &keep, frozen)
        } else {
            analysis
                .nodes
                .get(&frozen)
                .into_iter()
                .flat_map(|n| n.output_ports.iter())
                .map(|port| Endpoint::new(frozen, port.clone()))
                .collect()
        };
        let outputs = endpoints
            .iter()
            .filter_map(|e| {
                let node = analysis.nodes.get(&e.node)?;
                let port = node.output_ports.iter().position(|p| *p == e.port)?;
                Some((e.node, port))
            })
            .collect();
        targets.push(Target {
            kind: TargetKind::Frozen(frozen),
            outputs,
        });
    }
    // Upstream first: by the latest node a target reads from the schedule.
    let position: BTreeMap<NodeId, usize> = analysis
        .order
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();
    targets.sort_by_key(|t| {
        let last = t
            .outputs
            .iter()
            .filter_map(|(n, _)| position.get(n))
            .max()
            .copied();
        (last, t.kind)
    });
    targets
}

#[cfg(test)]
mod tests;
