//! Cache keys and cacheability: which nodes' output is a pure function of the
//! timeline, and what to call a render of it. See "Caching" in
//! docs/architecture/caching.md.
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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use noodle_core::{CacheKey, Endpoint, KeyBuilder, NodeId, Project, TICKS_PER_QUARTER, Tick};

use crate::compile::{Options, compile_with};
use crate::{
    BufferId, EventBufferId, InputSource, Memory, Mode, Phase, Registry, Shape, TempoTable,
};

/// What a key can depend on besides the graph.
pub struct CacheEnv<'a> {
    pub project: &'a Project,
    pub sample_rate: f32,
    /// How many frames the render covers, from the start of the timeline.
    /// Only the keys of range-dependent (offline) nodes and what follows them
    /// hold it: a real-time node's output up to a frame doesn't depend on how
    /// long the render goes on, so its key doesn't either.
    pub frames: u64,
    /// The most frames any block has, which the nodes were built with.
    pub max_frames: usize,
    /// Whether the session is feeding a node live right now (a monitored or
    /// auditioned track input). Such a node and everything downstream of it
    /// can't be cached while it lasts. This is a property of the session, not
    /// of the node type, so it can't be a `Layout::deterministic`: that would
    /// turn off every track's cache for good.
    pub live: &'a dyn Fn(NodeId) -> bool,
    /// The content hash of a clip's audio file, given its path as the clip
    /// has it. `None` if the file can't be read.
    pub file_key: &'a dyn Fn(&str) -> Option<CacheKey>,
    /// The cost below which the playback cache doesn't keep a cluster (see
    /// [`Layout::cost`](crate::Layout::cost) and `partition`). 0 caches every cluster there is.
    pub min_cost: u32,
}

/// The cost a cluster has to reach to be worth caching, in the units of
/// [`Layout::cost`](crate::Layout::cost): about a reverb, or a chain of thirty
/// gain stages. Storage is per audio-second whatever the cluster is, so a
/// cluster that is cheap to render is cheaper to render than to read.
pub const DEFAULT_MIN_COST: u32 = 32;

/// Why an output can't be cached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Uncacheable {
    /// The node type says identical inputs can give different output, such
    /// as the audio input.
    NotDeterministic(String),
    /// The node is on a feedback loop, whose output depends on itself.
    Loop,
    /// The session is feeding the node live (monitoring, an auditioned key).
    Live,
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
            Self::Live => f.write_str("it is being played live"),
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
    /// A wire into a parameter that offsets its value (see
    /// [`Modulation::Offset`](crate::Modulation::Offset)): the source and
    /// the base value the wire moves along the parameter's travel.
    Modulated(NodeId, usize, f32),
    /// Unconnected: the value set in the project, or the port's default.
    Value(f32),
}

#[derive(Clone, Debug)]
pub struct NodeAnalysis {
    pub type_id: String,
    pub offline: bool,
    /// How long the output remembers its input, from a fresh start: the
    /// node's own memory chained after the longest of what feeds it (a chain
    /// sums, parallel branches take the longest, a feedback loop is
    /// unbounded). See [`Memory`].
    pub memory: Memory,
    /// What rendering a second of the node costs ([`Layout::cost`](crate::Layout::cost)).
    pub cost: u32,
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
    /// A top-level group the playback cache picked by itself: cut before its
    /// outputs' gain, mute and solo, and cached per pass start (ADR 0010).
    Auto(NodeId),
    /// A cut the playback cache made inside the graph, at the outputs of a
    /// node: a join's branch, or a source several nodes read (see
    /// `partition`). Cached per pass start like [`Auto`](Self::Auto).
    Cut(NodeId),
}

impl TargetKind {
    pub fn node(self) -> NodeId {
        match self {
            Self::Offline(node) | Self::Frozen(node) | Self::Auto(node) | Self::Cut(node) => node,
        }
    }

    /// Whether the playback cache picked it by itself, and so caches it per
    /// pass start rather than over the whole timeline.
    pub fn is_auto(self) -> bool {
        matches!(self, Self::Auto(_) | Self::Cut(_))
    }
}

#[derive(Default)]
pub struct Analysis {
    /// Every node of the flattened graph that compiled, in schedule order.
    pub order: Vec<NodeId>,
    pub nodes: BTreeMap<NodeId, NodeAnalysis>,
    /// What to render, upstream first.
    pub targets: Vec<Target>,
    /// The tempo map the keys were made against, and the rate, to place an
    /// entry's origin (see [`Analysis::entries`]).
    tempo: TempoTable,
    sample_rate: f32,
}

impl Analysis {
    /// The key a render of one output is stored under.
    pub fn output_key(&self, node: NodeId, output: usize) -> Result<CacheKey, Uncacheable> {
        let analysis = self.nodes.get(&node).ok_or(Uncacheable::Broken)?;
        analysis.key.clone().map(|key| output_key(&key, output))
    }

    /// The key one output of a target is stored under for a pass from
    /// `start`: the output's key and the start. Explicit freezes and offline
    /// nodes render from 0 for the whole project and use [`output_key`](Self::output_key).
    pub fn pass_key(
        &self,
        node: NodeId,
        output: usize,
        start: Tick,
    ) -> Result<CacheKey, Uncacheable> {
        self.output_key(node, output)
            .map(|key| pass_key(&key, start))
    }

    /// How long a target remembers its input: the longest of its outputs'.
    pub fn memory(&self, target: &Target) -> Memory {
        target
            .outputs
            .iter()
            .filter_map(|(node, _)| self.nodes.get(node))
            .map(|info| info.memory)
            .max()
            .unwrap_or(Memory::Unbounded)
    }

    /// The entries that can serve `pass` for an automatic target, best first
    /// (the first is the one to render when none exists). Empty means the
    /// target can't be cached for this pass, and stays live.
    ///
    /// The tier of the cluster decides (ADR 0010):
    ///
    /// - *Position-pure* (`Memory::None`): one entry from 0 serves every
    ///   start, in a loop too.
    /// - *Bounded*: an entry rendered from a fresh state at or before `S−n`
    ///   serves S, because what it remembers of the time before has died away
    ///   by then. Origins are on a bar grid, and the one from 0 serves as
    ///   well, so a seek within a stretch reuses the entry. In a loop it is
    ///   a steady lap that includes the tail of the loop's end, and serves
    ///   every start inside the loop.
    /// - *Unbounded*: only an entry from a fresh state at S itself. In a loop
    ///   that is the lap from the loop's start, reset at the wrap; a pass
    ///   starting elsewhere in the loop leaves the cluster live.
    pub fn entries(&self, target: &Target, pass: &Pass) -> Vec<EntrySpec> {
        let tempo = &self.tempo;
        let rate = f64::from(self.sample_rate);
        let at = |tick: Tick| (tick, tempo.sample_at_tick(tick));
        let linear = |(origin, origin_frame): (Tick, u64)| EntrySpec {
            origin,
            origin_frame,
            start: origin_frame,
            frames: (pass.frame.saturating_sub(origin_frame) as usize) + pass.frames,
            looping: None,
        };
        let lap = |from: Tick, to: Tick, kind: LoopKind| {
            let (origin, origin_frame) = at(from);
            EntrySpec {
                origin,
                origin_frame,
                start: origin_frame,
                frames: tempo.sample_at_tick(to).saturating_sub(origin_frame) as usize,
                looping: Some((from, to, kind)),
            }
        };
        match (self.memory(target), pass.looping) {
            (Memory::None, _) => vec![linear((Tick(0), 0))],
            (Memory::Bounded(ms), Some((from, to))) => {
                if pass.tick < from {
                    return Vec::new();
                }
                let lap_frames = (tempo.sample_at_tick(to) - tempo.sample_at_tick(from)).max(1);
                let remembered = (f64::from(ms) * rate / 1000.0).ceil() as u64;
                let warm = (remembered.div_ceil(lap_frames) as usize).min(MAX_WARM_LAPS);
                vec![lap(from, to, LoopKind::Steady { warm })]
            }
            (Memory::Bounded(ms), None) => {
                let remembered = (f64::from(ms) * rate / 1000.0).ceil() as u64;
                let before = pass.frame.saturating_sub(remembered);
                let tick = (tempo.tick_at(before).floor() as i64).max(0);
                let grid = Tick(tick.div_euclid(GRID_TICKS) * GRID_TICKS);
                let mut specs = vec![linear(at(grid))];
                if grid != Tick(0) {
                    specs.push(linear((Tick(0), 0)));
                }
                specs
            }
            (Memory::Unbounded, Some((from, to))) => {
                if pass.tick != from {
                    return Vec::new();
                }
                let fade = (WRAP_FADE * self.sample_rate) as usize;
                vec![lap(from, to, LoopKind::Faded { fade })]
            }
            (Memory::Unbounded, None) => vec![linear(at(pass.tick))],
        }
    }

    /// Everything that has to be in the cache to serve `pass`, in the order
    /// it plays: each item is the candidates for one entry, best first. A
    /// plain pass has one. A bounded cluster in a loop has one per lap until
    /// the loop is in a steady state, then the steady lap, which plays on:
    ///
    /// - lap 1 is an ordinary entry from before the start, because the tail of
    ///   the loop's end has not been played yet;
    /// - laps 2 to `w` are rendered by the engine looping from that entry's
    ///   origin, because each carries a different amount of what the laps
    ///   before it left (the tail is longer than a lap);
    /// - from lap `w + 1` the past that matters is all in whole laps, and the
    ///   steady lap of [`entries`](Self::entries) serves every one.
    ///
    /// `w` is the cluster's memory in laps, rounded up. Empty if the pass
    /// can't be cached.
    pub fn requirements(&self, target: &Target, pass: &Pass) -> Vec<Vec<EntrySpec>> {
        let steady = self.entries(target, pass);
        let (
            Memory::Bounded(_),
            Some((from, to)),
            Some(&EntrySpec {
                looping: Some((_, _, LoopKind::Steady { warm }), ..),
                ..
            }),
        ) = (self.memory(target), pass.looping, steady.first())
        else {
            return if steady.is_empty() {
                Vec::new()
            } else {
                vec![steady]
            };
        };
        let firsts = self.entries(
            target,
            &Pass {
                looping: None,
                ..*pass
            },
        );
        let origin = firsts[0];
        let len = steady[0].frames;
        let lead = (self
            .tempo
            .sample_at_tick(to)
            .saturating_sub(origin.origin_frame)) as usize;
        let mut required = vec![firsts];
        for index in 2..=warm {
            required.push(vec![EntrySpec {
                origin: origin.origin,
                origin_frame: origin.origin_frame,
                start: steady[0].start,
                frames: len,
                looping: Some((from, to, LoopKind::Lap { index, lead })),
            }]);
        }
        required.push(steady);
        required
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

/// Where a pass begins, and how much of it an entry has to cover. A pass is
/// one run of the transport from a start (ADR 0010); its entries are rendered
/// from a fresh state there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pass {
    /// The start, which is what an entry is keyed by.
    pub tick: Tick,
    /// The same place in samples, as of the tempo map the keys hold.
    pub frame: u64,
    /// How many frames from the start the pass plays before it ends or
    /// reaches the end of the loop, which is where it wraps.
    pub frames: usize,
    /// The transport's loop, if the pass will wrap at its end: the start is
    /// before the loop's end. See [`Analysis::entries`] for the loop rule.
    pub looping: Option<(Tick, Tick)>,
}

impl Pass {
    /// A pass from `tick` that needs `frames` frames and doesn't loop.
    pub fn new(tick: Tick, table: &crate::TempoTable, frames: usize) -> Self {
        Self {
            tick,
            frame: table.sample_at_tick(tick),
            frames,
            looping: None,
        }
    }

    /// The same pass, wrapping at the end of the loop from `from` to `to`.
    #[must_use]
    pub fn looping(self, from: Tick, to: Tick) -> Self {
        Self {
            looping: (self.tick < to).then_some((from, to)),
            ..self
        }
    }
}

/// How an entry for a loop is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopKind {
    /// For a cluster that remembers for a bounded time: the engine runs the
    /// loop `warm` laps before the one that is kept, so the tail of the end
    /// is in the start of it, as it is in a live loop.
    Steady { warm: usize },
    /// For a cluster that never forgets: one lap from a fresh state, which
    /// is where the cluster is when it resets at the wrap. The last `fade`
    /// frames fade out, so the reset doesn't click.
    Faded { fade: usize },
    /// Lap `index` (from 2) of a pass whose first lap came from an ordinary
    /// entry: the engine runs from the entry's origin through the end of the
    /// loop (`lead` frames), then around the loop, and the lap is kept.
    Lap { index: usize, lead: usize },
}

/// A cached entry: the audio of a cluster, rendered from a fresh state at
/// `origin`. What the tiers decide (ADR 0010) is which origins serve a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntrySpec {
    /// Where the render starts from a fresh state; the entry's first frame.
    pub origin: Tick,
    pub origin_frame: u64,
    /// The timeline frame the entry's first frame belongs to: the origin's,
    /// or the loop's start for a lap of a loop.
    pub start: u64,
    /// How many frames from the origin the entry must hold. A loop's entry
    /// is exactly one lap.
    pub frames: usize,
    /// Set for an entry made for a loop, which is not the same audio as a
    /// plain entry from the same origin.
    pub looping: Option<(Tick, Tick, LoopKind)>,
}

impl EntrySpec {
    /// The key one output of the cluster is stored under.
    pub fn key(&self, output_key: &CacheKey) -> CacheKey {
        match self.looping {
            None => pass_key(output_key, self.origin),
            Some((from, to, kind)) => KeyBuilder::new("loop")
                .key(output_key)
                .u64(from.0 as u64)
                .u64(to.0 as u64)
                .u64(match kind {
                    LoopKind::Steady { .. } => 0,
                    LoopKind::Faded { .. } => 1,
                    LoopKind::Lap { index, .. } => 2 + index as u64,
                })
                .u64(match kind {
                    // A lap depends on where the pass began.
                    LoopKind::Lap { .. } => self.origin.0 as u64,
                    _ => 0,
                })
                .finish(),
        }
    }

    /// Whether the entry is exactly `frames` long, rather than allowed to
    /// grow (a loop's lap is the loop).
    pub fn exact(&self) -> bool {
        self.looping.is_some()
    }

    /// The pass a render of this entry stands in for, to find the entries
    /// of the clusters upstream of it.
    pub fn pass(&self) -> Pass {
        Pass {
            tick: self.origin,
            frame: self.origin_frame,
            frames: self.frames,
            looping: match self.looping {
                Some((from, to, LoopKind::Steady { .. } | LoopKind::Lap { .. })) => {
                    Some((from, to))
                }
                _ => None,
            },
        }
    }
}

/// Bounded entries start on a grid of this many ticks (a bar of 4/4), so the
/// starts in one stretch share an entry.
const GRID_TICKS: i64 = 4 * TICKS_PER_QUARTER;
/// The most laps a loop render is warmed for.
const MAX_WARM_LAPS: usize = 64;
/// How long a reset at a loop's wrap takes to fade, in seconds.
const WRAP_FADE: f32 = 0.004;

/// The key a cluster's output is stored under for a pass from `start`. The
/// length of the pass is not in it: a render that is prefix-stable only grows.
pub fn pass_key(output_key: &CacheKey, start: Tick) -> CacheKey {
    KeyBuilder::new("pass")
        .key(output_key)
        .u64(start.0 as u64)
        .finish()
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

    let mut analysis = Analysis {
        tempo: TempoTable::new(project.tempo_map(), env.sample_rate),
        sample_rate: env.sample_rate,
        ..Analysis::default()
    };
    let mut signal_writers: HashMap<BufferId, (NodeId, usize)> = HashMap::new();
    let mut event_writers: HashMap<EventBufferId, (NodeId, usize)> = HashMap::new();
    let mut spectral_writers: HashMap<crate::SpectralBufferId, (NodeId, usize)> = HashMap::new();
    let mut memories: HashMap<NodeId, Memory> = HashMap::new();
    for node in &schedule.nodes {
        let (id, layout) = (node.id, &node.layout);
        let info = node.node_type.info();
        let offline = layout.mode == Mode::Offline;
        let inputs: Vec<InputOrigin> = node
            .inputs
            .iter()
            .map(|source| match source {
                InputSource::Buffer(b) => signal_writers
                    .get(b)
                    .map_or(InputOrigin::Value(0.0), |&(n, p)| InputOrigin::Wire(n, p)),
                InputSource::Modulated(b, base) => signal_writers
                    .get(b)
                    .map_or(InputOrigin::Value(*base), |&(n, p)| {
                        InputOrigin::Modulated(n, p, *base)
                    }),
                InputSource::Value(v) => InputOrigin::Value(*v),
                InputSource::Absent => InputOrigin::Value(0.0),
            })
            .collect();

        // Spectral wires are keyed like event wires, after them.
        let events: Vec<_> = node
            .event_inputs
            .iter()
            .map(|e| e.and_then(|e| event_writers.get(&e).copied()))
            .chain(
                node.spectral_inputs
                    .iter()
                    .map(|s| s.and_then(|s| spectral_writers.get(&s).copied())),
            )
            .collect();

        // The memory of what feeds the node: the longest of its sources'.
        let fed_by = inputs
            .iter()
            .filter_map(|origin| match origin {
                InputOrigin::Wire(n, _) | InputOrigin::Modulated(n, _, _) => Some(*n),
                InputOrigin::Value(_) => None,
            })
            .chain(events.iter().flatten().map(|&(n, _)| n))
            .filter_map(|n| memories.get(&n).copied())
            .fold(Memory::None, Memory::alongside);
        // The output half of a split node is on a feedback loop.
        let memory = if node.phase == Phase::Output {
            Memory::Unbounded
        } else if layout.mode == Mode::Offline {
            // Reads its whole input at once, so it depends on all of it.
            Memory::Unbounded
        } else {
            fed_by.then(layout.memory)
        };
        if node.phase != Phase::Input {
            memories.insert(id, memory);
        }

        // The input half of a split node doesn't get its own entry; the
        // node as a whole is on a loop, so it has no key.
        let key = if node.phase == Phase::Input {
            None
        } else if node.phase == Phase::Output {
            Some(Err(Uncacheable::Loop))
        } else if !layout.deterministic {
            Some(Err(Uncacheable::NotDeterministic(info.name.to_string())))
        } else if (env.live)(id) {
            Some(Err(Uncacheable::Live))
        } else {
            let upstream = |origin: &InputOrigin| match origin {
                InputOrigin::Wire(n, p) | InputOrigin::Modulated(n, p, _) => Some((*n, *p)),
                InputOrigin::Value(_) => None,
            };
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
                    memory,
                    cost: layout.cost,
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
        // After the signal outputs, so a spectral output's key differs from
        // any signal output's.
        for (port, &b) in node.spectral_outputs.iter().enumerate() {
            spectral_writers.insert(b, (id, node.outputs.len() + port));
        }
    }

    analysis.targets = targets(project, &analysis, env.min_cost);
    analysis
}

/// Part of every key, so renders made by another build of the DSP are never
/// replayed from the shared per-user cache. Bump `DSP_GENERATION` in the same
/// change as any regenerated golden render; the crate version covers releases.
const DSP_GENERATION: u32 = 7;

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
    // An offline node reads its inputs as whole signals, which a wire into a
    // parameter is too (see `apply_offset`), but it has no way to read
    // events yet. Say so, rather than render it wrongly.
    if node.layout.mode == crate::Mode::Offline && events.iter().any(Option::is_some) {
        return Err(Uncacheable::Because(
            "an offline node can't take events yet".into(),
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
        .u64(env.max_frames as u64)
        .tempo_map(env.project.tempo_map());
    // A render that is the whole range at once depends on its length; a
    // real-time node's output at a frame doesn't (it only reads the past).
    if node.layout.mode == crate::Mode::Offline {
        b.u64(env.frames);
    }
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

/// The outputs of the flattened graph that `endpoints` name, as node and
/// output position.
fn outputs_of(analysis: &Analysis, endpoints: &[Endpoint]) -> Vec<(NodeId, usize)> {
    endpoints
        .iter()
        .filter_map(|e| {
            let node = analysis.nodes.get(&e.node)?;
            let port = node.output_ports.iter().position(|p| *p == e.port)?;
            Some((e.node, port))
        })
        .collect()
}

/// What to render, upstream first: each offline node's outputs, the outputs
/// a frozen node or group exposes, and the groups the playback cache picks.
fn targets(project: &Project, analysis: &Analysis, min_cost: u32) -> Vec<Target> {
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
    let mut keep = crate::automation::boundary_targets(project.graph(), &lanes, &mut Vec::new());
    // As compiled (see `compile_with`), so a frozen group's outputs are the
    // stages that meter them.
    keep.extend(crate::flatten::output_stages(project.graph()));
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
        let outputs = outputs_of(analysis, &endpoints);
        targets.push(Target {
            kind: TargetKind::Frozen(frozen),
            outputs,
        });
    }
    // The playback cache's own picks: every top-level group that isn't
    // frozen and can be cached whole, cut before its outputs' stages, and
    // the cuts inside and between them. What can't be cached (a live input,
    // a loop) is left to play live, and doesn't hold the others back.
    let groups: Vec<Target> = project
        .graph()
        .nodes()
        .filter(|(id, node)| {
            node.type_id == noodle_core::group::GROUP
                && node.parent.is_none()
                && !project.frozen().contains(id)
        })
        .map(|(id, _)| Target {
            kind: TargetKind::Auto(id),
            outputs: outputs_of(
                analysis,
                &crate::flatten::group_sources(project.graph(), &keep, id),
            ),
        })
        .filter(|target| analysis.blocked(target).is_none())
        .collect();
    let taken: BTreeSet<NodeId> = targets
        .iter()
        .flat_map(|t| t.outputs.iter().map(|&(n, _)| n))
        .collect();
    targets.extend(partition::picks(
        project, analysis, groups, &taken, min_cost,
    ));
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

mod partition;

#[cfg(test)]
mod tests;
