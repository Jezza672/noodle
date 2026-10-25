//! Render plans: a compiled [`Schedule`] with its node instances and buffers,
//! ready to run on the audio thread.
//!
//! Plans are built off the audio thread by [`build`], which allocates
//! everything a plan will need. Running one ([`Plan::run`]) and swapping one in
//! ([`Plan::take_state_from`]) never allocate, lock or block.

use std::collections::{HashMap, HashSet};
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use noodle_core::NodeId;

use crate::ChannelLayout;
use crate::builtin::{INPUT_ID, OUTPUT_DEVICE, OUTPUT_ID, OUTPUT_ROUTING, parse_routing};
use crate::parallel::{StepGraph, Workers};
use crate::render::TAP_ID;
use crate::{
    BlockWriter, Context, CpuWriter, Diagnostic, Event, EventsOut, InputKind, InputSource,
    Instance, Io, Level, MAX_LATENCY, MeterWriter, Node, NodeError, ParamInfo, ParamKind,
    ParamWriter, Phase, Problem, Schedule, Settings, Setup, Shape, SignalIn, SignalOut,
    SpectralBuf, SpectralDelay, SpectralIn, SpectralOut, Telemetry,
};

/// The value of an unconnected input, shared between the controller, which
/// sets it, and every plan that reads it. Plans smooth changes themselves.
#[derive(Debug)]
pub(crate) struct ParamCell(AtomicU32);

impl ParamCell {
    fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    pub(crate) fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub(crate) fn set(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

/// Every unconnected input's cell, by node and port key.
pub(crate) type Cells = HashMap<NodeId, HashMap<String, Arc<ParamCell>>>;

/// What the controller remembers about the last plan it sent, so the next plan
/// can take over its node instances and smoothing state.
pub(crate) struct PlanInfo {
    generation: u64,
    nodes: HashMap<NodeId, (NodeKey, usize)>,
    values: HashMap<(NodeId, String), usize>,
    /// Every node the output depends on, with what feeds each of its inputs.
    audible: Wiring,
    /// Nodes that failed to instantiate and play silence instead.
    silent: HashSet<NodeId>,
    /// Each node's reported latency in samples, so a node that carries over
    /// (its instance arrives later) still counts.
    latency: HashMap<NodeId, u32>,
    /// Every delay line inserted for compensation, by the node and input it
    /// feeds: (slot, index in the slot's lines, delay in samples). Sorted.
    delays: Vec<DelayRecord>,
    /// The same without the slots, which move when the schedule shifts: what
    /// two plans must share to swap without a fade. Sorted.
    delay_keys: Vec<((NodeId, usize), u32)>,
    /// How late the mix sounds, in samples, once everything is aligned.
    pub(crate) output_latency: u32,
    /// How much of the plan can run at once.
    pub(crate) shape: PlanShape,
}

/// How a plan's steps depend on each other, which decides how much running
/// it on several threads can help.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanShape {
    /// Steps in the schedule (a node, or one half of a split node).
    pub steps: usize,
    /// The longest chain of steps that must run one after the other. Rendering
    /// can't take fewer than this many steps' worth of time, however many
    /// threads there are.
    pub critical_path: usize,
    /// The most steps that could run at the same time.
    pub width: usize,
}

/// (node, input index) -> (slot, index among the slot's delay lines, delay).
type DelayRecord = ((NodeId, usize), (usize, usize, u32));

/// For each node, where each of its inputs (signal inputs, then event
/// inputs) comes from.
type Wiring = HashMap<NodeId, Sources>;

/// One node's inputs: `Some((node, output index))` if connected.
type Sources = Vec<Option<(NodeId, usize)>>;

/// A node instance can carry over to a new plan only if all of this is
/// unchanged. Otherwise the node is rebuilt.
#[derive(PartialEq)]
struct NodeKey {
    type_id: &'static str,
    config: noodle_core::Config,
    input_shapes: Vec<Shape>,
    output_shapes: Vec<Shape>,
}

pub(crate) struct Plan {
    generation: u64,
    /// The plan this one replaces, which `migrations` index into.
    previous: Option<u64>,
    /// Whether this plan can take over from the previous one without a fade:
    /// everything the output depends on carries over, wired the same way.
    seamless: bool,
    nodes: Vec<PlanNode>,
    /// Every signal buffer, back to back.
    pool: Box<[f32]>,
    /// One silence flag per lane of the pool, at the lane's pool offset
    /// divided by `max_frames`. See [`SignalOut::set_silent`].
    silent: Box<[bool]>,
    events: Vec<Vec<Event>>,
    spectral: Vec<SpectralBuf>,
    /// `max_frames` zeros, for the placeholder a split node's other half
    /// sees where its loop input (or everything else) is.
    zeros: Box<[f32]>,
    values: Vec<ValueInput>,
    /// Wired parameters that offset their value, by index into the plan's
    /// offsets.
    offsets: Vec<OffsetInput>,
    /// (slot in the previous plan, slot in this one) for node instances that
    /// carry over.
    migrations: Vec<(usize, usize)>,
    /// The same for unconnected inputs' smoothing state.
    value_migrations: Vec<(usize, usize)>,
    /// ((slot, line) in the previous plan, (slot, line) in this one) for
    /// delay lines whose contents carry over.
    delay_migrations: Vec<((usize, usize), (usize, usize))>,
    spectral_delay_migrations: Vec<((usize, usize), (usize, usize))>,
    max_frames: usize,
    /// What each step waits for, to run the plan on several threads.
    graph: StepGraph,
    /// For each node (the slot of a split node's output half), the nodes that
    /// read its outputs.
    readers: Vec<Vec<usize>>,
    /// Whether a node can be read by one that runs before it (a feedback
    /// loop), so the liveness pass needs to repeat.
    loops: bool,
    /// Per node slot, worked out at the start of each block by
    /// [`Plan::update_liveness`]: the node has a listener, and runs.
    live: Box<[bool]>,
    /// The node was live in the previous block.
    was_live: Box<[bool]>,
    /// The node doesn't run, but something live reads its outputs, which have
    /// to be zeros.
    zero: Box<[bool]>,
    /// The node's outputs are zero whatever its inputs hold.
    silenced: Box<[bool]>,
    /// Reports how long each block takes, when something is watching.
    blocks: Option<BlockWriter>,
}

struct PlanNode {
    /// `None` until a carried-over instance arrives from the previous plan.
    /// Always `None` for the input half of a split node, which runs its
    /// `owner`'s instance.
    instance: Option<Box<dyn Node>>,
    /// Which half of the node this step runs.
    phase: Phase,
    /// For the input half of a split node: the slot of its output half, which
    /// comes earlier and holds the instance.
    owner: Option<usize>,
    inputs: Vec<Input>,
    outputs: Vec<View>,
    event_inputs: Vec<Option<usize>>,
    event_outputs: Vec<usize>,
    /// The buffer behind each spectral input and the input's shape.
    spectral_inputs: Vec<Option<(usize, Shape)>>,
    spectral_outputs: Vec<(usize, Shape)>,
    /// An Output node, whose input is mixed into the device output.
    is_output: bool,
    /// Where an Output node mixes to: the first output channel and the
    /// number of channels. `None` for an Output node that plays nothing
    /// (its device isn't open) and for every other node.
    bus: Option<(usize, usize)>,
    /// For an Output node whose signal has more channels than its bus and
    /// both name a layout: the downmix (rows are bus channels, columns signal
    /// channels), so a 5.1 mix on a stereo bus folds down instead of losing
    /// its centre and rears. For one with a `routing`: that routing as a
    /// matrix of the same shape.
    downmix: Option<Box<[f32]>>,
    /// An Input node, whose output the executor fills from the device input.
    is_input: bool,
    /// Reports the signals wired into parameters, by input index.
    probes: Vec<(usize, ParamWriter)>,
    /// Reports the level arriving at each wired audio input, by input index.
    input_meters: Vec<(usize, MeterWriter)>,
    /// Reports how long the node takes, when something is watching.
    cpu: Option<CpuWriter>,
    /// The node runs whatever is downstream of it: an Output node, a tap, or
    /// a type with [`NodeType::always_runs`](crate::NodeType::always_runs).
    always_runs: bool,
    /// The settled value of each input for the current block, `None` for
    /// inputs that are wired or still moving. Filled to ask the node whether
    /// it is [silenced](Node::silenced_by).
    constants: Vec<Option<f32>>,
    /// Where each signal input comes from, if it is wired: the source node's
    /// slot and its output index.
    sources: Vec<Option<(usize, usize)>>,
    /// Delay lines that hold some of this node's inputs back to line up with
    /// the slowest one, run just before the node.
    delays: Vec<DelayLine>,
    /// The same for spectral inputs.
    spectral_delays: Vec<SpectralDelayLine>,
    scratch: Scratch,
    /// Where an Output node mixes to when the plan runs on several threads:
    /// `frames * channels` samples, added to the device output at the end
    /// of the block in schedule order. Empty unless `bus` is set.
    bus_out: Box<[f32]>,
}

/// The level of one channel of `signal` over a block, with its voices summed
/// as the Meter node does. The RMS is that of the block alone.
fn input_level(signal: &SignalIn<'_>, channel: usize) -> Level {
    let voices = signal.shape().voices;
    if (0..voices).all(|voice| signal.is_silent(voice, channel)) {
        return Level::default();
    }
    let (mut peak, mut power) = (0.0f32, 0.0f32);
    for frame in 0..signal.frames() {
        let x: f32 = (0..voices)
            .map(|voice| signal.lane(voice, channel)[frame])
            .sum();
        peak = peak.max(x.abs());
        power += x * x;
    }
    Level {
        peak,
        rms: (power / signal.frames().max(1) as f32).sqrt(),
    }
}

/// Where a signal lives in the pool.
#[derive(Clone, Copy)]
struct View {
    offset: usize,
    shape: Shape,
}

#[derive(Clone, Copy)]
enum Input {
    Buffer(View),
    /// An unconnected input, by index into the plan's values.
    Value(usize),
    /// A wired parameter that offsets its value, by index into the plan's
    /// offsets.
    Offset(usize),
    /// Not read in this step: a silent mono placeholder.
    Absent,
}

/// Vectors with room for one node's views, so building its [`Io`] each block
/// doesn't allocate. They're always empty between blocks; see [`recycle`].
#[derive(Default)]
struct Scratch {
    inputs: Vec<SignalIn<'static>>,
    outputs: Vec<SignalOut<'static>>,
    event_inputs: Vec<&'static [Event]>,
    event_outputs: Vec<EventsOut<'static>>,
    spectral_inputs: Vec<SpectralIn<'static>>,
    spectral_outputs: Vec<SpectralOut<'static>>,
}

/// Builds a plan from a schedule, instantiating every node that can't carry
/// over from `previous`. Nodes that fail to instantiate are kept, as silence,
/// so the buffers downstream of them stay valid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    schedule: Schedule,
    previous: Option<&PlanInfo>,
    generation: u64,
    settings: Settings,
    buses: Option<&[Bus]>,
    telemetry: Option<&Telemetry>,
    cells: &mut Cells,
    position: u64,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Box<Plan>, PlanInfo) {
    let Settings {
        sample_rate,
        max_frames,
        channels,
    } = settings;
    let routing = Routing { channels, buses };
    // The main device's own ID is the main output, so it can't also be
    // another Output node's device.
    let main = buses
        .and_then(|b| b.first())
        .map(|b| b.device.as_str())
        .filter(|d| !d.is_empty());
    let canonical = |device: String| {
        if Some(device.as_str()) == main {
            String::new()
        } else {
            device
        }
    };
    // At most one Output node plays on each named device: the lowest ID.
    let mut device_owners: HashMap<String, NodeId> = HashMap::new();
    for scheduled in &schedule.nodes {
        if scheduled.node_type.info().id == OUTPUT_ID
            && !crate::monitor::is_monitor_config(&scheduled.config)
        {
            let device = canonical(OUTPUT_DEVICE.get_text(&scheduled.config));
            let owner = device_owners.entry(device).or_insert(scheduled.id);
            *owner = (*owner).min(scheduled.id);
        }
    }
    let mut offsets = Vec::with_capacity(schedule.buffer_lanes.len());
    let mut pool_len = 0;
    for lanes in &schedule.buffer_lanes {
        offsets.push(pool_len);
        pool_len += lanes * max_frames;
    }
    let view = |buffer: crate::BufferId, shape: Shape| View {
        offset: offsets[buffer.0],
        shape,
    };

    let mut info = PlanInfo {
        generation,
        nodes: HashMap::new(),
        values: HashMap::new(),
        audible: HashMap::new(),
        silent: HashSet::new(),
        latency: HashMap::new(),
        delays: Vec::new(),
        delay_keys: Vec::new(),
        output_latency: 0,
        shape: PlanShape::default(),
    };
    let mut slot_info: Vec<SlotInfo> = Vec::with_capacity(schedule.nodes.len());
    let mut nodes = Vec::with_capacity(schedule.nodes.len());
    let mut values = Vec::new();
    let mut offsets_in = Vec::new();
    let mut migrations = Vec::new();
    let mut value_migrations = Vec::new();
    let mut live_cells: Cells = HashMap::new();
    // Which node output last wrote each buffer, so each input's source is
    // known even though buffers are reused.
    let mut signal_writers = HashMap::new();
    let mut event_writers = HashMap::new();
    let mut spectral_writers = HashMap::new();
    let mut wiring: Vec<(NodeId, Sources)> = Vec::with_capacity(schedule.nodes.len());
    let mut carried_ids = HashSet::new();
    // The slot of each split node's output half, for its input half to find.
    let mut output_halves: HashMap<NodeId, usize> = HashMap::new();

    for (slot, scheduled) in schedule.nodes.into_iter().enumerate() {
        let id = scheduled.id;
        let input_half = scheduled.phase == Phase::Input;
        if scheduled.phase == Phase::Output {
            output_halves.insert(id, slot);
        }
        // The live keys are left out, so a change to them keeps the instance.
        let live = scheduled.node_type.live_config();
        let mut key_config = scheduled.config.clone();
        for key in live {
            key_config.remove(key);
        }
        let key = NodeKey {
            type_id: scheduled.node_type.info().id,
            config: key_config,
            input_shapes: scheduled.input_shapes.clone(),
            output_shapes: scheduled.output_shapes.clone(),
        };

        let carried = previous
            .and_then(|p| p.nodes.get(&id))
            .filter(|(old_key, _)| *old_key == key && !input_half);
        // A node that fails to instantiate plays silence, and isn't recorded
        // as able to carry over, so the next update retries it and reports
        // the problem again.
        let (instance, carries_over) = match carried {
            None if input_half => (None, false),
            Some(&(_, old_slot)) => {
                if !live.is_empty() {
                    scheduled.node_type.retune(id, &scheduled.config);
                }
                migrations.push((old_slot, slot));
                carried_ids.insert(id);
                (None, true)
            }
            None => match instantiate(&scheduled, sample_rate, max_frames, position) {
                Ok(node) => {
                    // A stateless node (an automation lane's source, a
                    // Remap) rebuilt with a new config and the same shapes
                    // changes only the value it writes, like a parameter
                    // being moved: no fade needed. A new node, or one whose
                    // shapes changed, is a change of wiring and still fades.
                    if scheduled.node_type.stateless()
                        && previous.is_some_and(|p| {
                            p.nodes.get(&id).is_some_and(|(old_key, _)| {
                                old_key.type_id == key.type_id
                                    && old_key.input_shapes == key.input_shapes
                                    && old_key.output_shapes == key.output_shapes
                            })
                        })
                    {
                        carried_ids.insert(id);
                    }
                    (Some(node), true)
                }
                Err(error) => {
                    info.silent.insert(id);
                    diagnostics.push(Diagnostic::node(id, Problem::Node(error)));
                    (Some(Box::new(Silence) as Box<dyn Node>), false)
                }
            },
        };

        let latency = if input_half {
            0
        } else {
            match (&instance, carried) {
                (Some(node), _) => node.latency(),
                (None, Some(_)) => previous
                    .and_then(|p| p.latency.get(&id).copied())
                    .unwrap_or(0),
                (None, None) => 0,
            }
            .min(MAX_LATENCY)
        };
        if !input_half {
            info.latency.insert(id, latency);
        }

        let mut inputs = Vec::with_capacity(scheduled.inputs.len());
        for ((port, source), shape) in scheduled
            .layout
            .inputs
            .iter()
            .zip(&scheduled.inputs)
            .zip(&scheduled.input_shapes)
        {
            // The value a parameter holds when nothing drives it, or the base
            // that a wire offsets.
            let base = match *source {
                InputSource::Value(value) | InputSource::Modulated(_, value) => Some(value),
                InputSource::Buffer(_) | InputSource::Absent => None,
            };
            let value_slot = base.map(|value| {
                // A hand-edited file can hold inf or NaN, which would
                // leave nodes stuck. Use the port's default instead.
                let value = match &port.kind {
                    _ if value.is_finite() => value,
                    InputKind::Param(param) => param.default,
                    InputKind::Audio => 0.0,
                };
                let key = port.key.to_string();
                let cell = cells
                    .get(&id)
                    .and_then(|node| node.get(&key))
                    .cloned()
                    .unwrap_or_else(|| Arc::new(ParamCell::new(value)));
                // The project is the source of truth, so a value set there
                // (by undo, say) wins over whatever the cell held.
                cell.set(value);
                let smoothing = match &port.kind {
                    InputKind::Param(param) => match param.kind {
                        ParamKind::Continuous { smoothing_ms } => {
                            (smoothing_ms * sample_rate / 1000.0).round() as usize
                        }
                        ParamKind::Stepped { .. } => 0,
                    },
                    InputKind::Audio => 0,
                };
                let value_slot = values.len();
                if let Some(&old) = previous.and_then(|p| p.values.get(&(id, key.clone()))) {
                    value_migrations.push((old, value_slot));
                }
                values.push(ValueInput::new(Arc::clone(&cell), max_frames, smoothing));
                live_cells.entry(id).or_default().insert(key.clone(), cell);
                info.values.insert((id, key), value_slot);
                value_slot
            });
            inputs.push(match (*source, value_slot, &port.kind) {
                (InputSource::Absent, ..) => Input::Absent,
                (InputSource::Buffer(b), ..) => Input::Buffer(view(b, *shape)),
                (InputSource::Modulated(b, _), Some(value), InputKind::Param(param)) => {
                    offsets_in.push(OffsetInput::new(
                        value,
                        view(b, *shape),
                        param.clone(),
                        max_frames,
                    ));
                    Input::Offset(offsets_in.len() - 1)
                }
                (_, Some(value), _) => Input::Value(value),
                (_, None, _) => unreachable!("every unconnected input has a value slot"),
            });
        }

        let sources = scheduled
            .inputs
            .iter()
            .map(|source| match source {
                InputSource::Buffer(b) | InputSource::Modulated(b, _) => {
                    signal_writers.get(b).copied()
                }
                InputSource::Value(_) | InputSource::Absent => None,
            })
            .chain(
                scheduled
                    .event_inputs
                    .iter()
                    .map(|e| e.and_then(|e| event_writers.get(&e).copied())),
            )
            .chain(
                scheduled
                    .spectral_inputs
                    .iter()
                    .map(|s| s.and_then(|s| spectral_writers.get(&s).copied())),
            )
            .collect();
        let owner = input_half.then(|| output_halves[&id]);
        if let Some(owner) = owner {
            // The node's sources are those of both halves together.
            let merged: &mut Sources = &mut wiring[owner].1;
            for (slot, source) in merged.iter_mut().zip(sources) {
                *slot = slot.or(source);
            }
            wiring.push((id, Vec::new()));
        } else {
            wiring.push((id, sources));
        }
        for (port, &b) in scheduled.outputs.iter().enumerate() {
            signal_writers.insert(b, (id, port));
        }
        for (port, &e) in scheduled.event_outputs.iter().enumerate() {
            event_writers.insert(e, (id, port));
        }
        for (port, &b) in scheduled.spectral_outputs.iter().enumerate() {
            spectral_writers.insert(b, (id, port));
        }

        let outputs: Vec<View> = scheduled
            .outputs
            .iter()
            .zip(&scheduled.output_shapes)
            .map(|(&b, &shape)| view(b, shape))
            .collect();
        let event_inputs: Vec<Option<usize>> = scheduled
            .event_inputs
            .iter()
            .map(|e| e.map(|e| e.0))
            .collect();
        let event_outputs: Vec<usize> = scheduled.event_outputs.iter().map(|e| e.0).collect();
        // The spectral shapes follow the signal shapes.
        let signals_in = scheduled.layout.inputs.len();
        let signals_out = scheduled.layout.outputs.len();
        let spectral_inputs: Vec<Option<(usize, Shape)>> = scheduled
            .spectral_inputs
            .iter()
            .zip(&scheduled.input_shapes[signals_in..])
            .map(|(b, &shape)| b.map(|b| (b.0, shape)))
            .collect();
        let spectral_outputs: Vec<(usize, Shape)> = scheduled
            .spectral_outputs
            .iter()
            .zip(&scheduled.output_shapes[signals_out..])
            .map(|(b, &shape)| (b.0, shape))
            .collect();
        let scratch = Scratch {
            inputs: Vec::with_capacity(inputs.len()),
            outputs: Vec::with_capacity(outputs.len()),
            event_inputs: Vec::with_capacity(event_inputs.len()),
            event_outputs: Vec::with_capacity(event_outputs.len()),
            spectral_inputs: Vec::with_capacity(spectral_inputs.len()),
            spectral_outputs: Vec::with_capacity(spectral_outputs.len()),
        };

        let probes: Vec<(usize, ParamWriter)> = telemetry
            .into_iter()
            .flat_map(|telemetry| {
                scheduled
                    .layout
                    .inputs
                    .iter()
                    .zip(&scheduled.inputs)
                    .enumerate()
                    .filter(|(_, (port, source))| {
                        matches!(port.kind, InputKind::Param(_))
                            && matches!(source, InputSource::Buffer(_) | InputSource::Modulated(..))
                    })
                    .map(move |(index, (port, _))| (index, telemetry.open_param(id, &port.key)))
            })
            .collect();

        let input_meters: Vec<(usize, MeterWriter)> = telemetry
            .into_iter()
            .flat_map(|telemetry| {
                scheduled
                    .layout
                    .inputs
                    .iter()
                    .zip(&scheduled.inputs)
                    .zip(&scheduled.input_shapes)
                    .enumerate()
                    .filter(|(_, ((port, source), _))| {
                        matches!(port.kind, InputKind::Audio)
                            && matches!(source, InputSource::Buffer(_))
                    })
                    .map(move |(index, ((port, _), shape))| {
                        (
                            index,
                            telemetry.open_input_meter(id, &port.key, shape.channels),
                        )
                    })
            })
            .collect();

        let is_output = scheduled.node_type.info().id == OUTPUT_ID;
        let mut bus = None;
        if is_output {
            let device = canonical(OUTPUT_DEVICE.get_text(&scheduled.config));
            let device = device.as_str();
            if !device.is_empty()
                && device_owners.get(device) != Some(&id)
                && !crate::monitor::is_monitor_config(&scheduled.config)
            {
                diagnostics.push(Diagnostic::node(
                    id,
                    Problem::DeviceTaken(device.to_owned()),
                ));
            } else {
                bus = routing.route(device);
                if bus.is_none() {
                    diagnostics.push(Diagnostic::node(
                        id,
                        Problem::DeviceUnavailable(device.to_owned()),
                    ));
                }
            }
        }
        if carries_over {
            info.nodes.insert(id, (key, slot));
        }
        slot_info.push(SlotInfo {
            id,
            latency,
            sink: (is_output && bus.is_some()) || scheduled.node_type.info().id == TAP_ID,
            inputs: scheduled
                .inputs
                .iter()
                .map(|source| match source {
                    InputSource::Buffer(b) | InputSource::Modulated(b, _) => Some(*b),
                    _ => None,
                })
                .collect(),
            outputs: scheduled.outputs.clone(),
            spectral_inputs: scheduled.spectral_inputs.clone(),
            spectral_outputs: scheduled.spectral_outputs.clone(),
            consumes: scheduled.phase != Phase::Input,
        });
        nodes.push(PlanNode {
            instance,
            phase: scheduled.phase,
            owner,
            inputs,
            outputs,
            event_inputs,
            event_outputs,
            spectral_inputs,
            spectral_outputs,
            is_output,
            bus,
            downmix: bus.and_then(|(_, count)| {
                let signal = scheduled.input_shapes.first()?.channels;
                // A routing the node sets decides where every channel goes.
                if is_output
                    && let Ok(routes) = parse_routing(&OUTPUT_ROUTING.get_text(&scheduled.config))
                    && !routes.is_empty()
                {
                    let mut matrix = vec![0.0; count * signal];
                    for (source, dest) in routes.iter().enumerate().take(signal) {
                        if let Some(dest) = dest.filter(|&d| d < count) {
                            matrix[dest * signal + source] = 1.0;
                        }
                    }
                    return Some(matrix.into_boxed_slice());
                }
                if signal <= count {
                    return None;
                }
                let from = ChannelLayout::from_channels(signal)?;
                let to = ChannelLayout::from_channels(count)?;
                Some(ChannelLayout::convert_matrix(from, to).into_boxed_slice())
            }),
            is_input: scheduled.node_type.info().id == INPUT_ID,
            probes,
            input_meters,
            cpu: telemetry
                .filter(|_| !input_half)
                .map(|telemetry| telemetry.open_cpu(id)),
            always_runs: is_output
                || scheduled.node_type.info().id == TAP_ID
                || scheduled.node_type.always_runs(),
            constants: vec![None; scheduled.inputs.len()],
            sources: Vec::new(),
            delays: Vec::new(),
            spectral_delays: Vec::new(),
            scratch,
            bus_out: match bus {
                Some(_) => vec![0.0; max_frames * channels].into_boxed_slice(),
                None => Box::default(),
            },
        });
    }
    let mut spectral_lanes = schedule.spectral_lanes;
    let (delay_migrations, spectral_delay_migrations) = compensate(
        &mut nodes,
        &slot_info,
        &mut offsets_in,
        &mut pool_len,
        &mut spectral_lanes,
        max_frames,
        previous,
        &mut info,
    );
    // Forget the cells of inputs that no longer exist or are now connected.
    *cells = live_cells;

    info.audible = audible_wiring(&wiring, &nodes);
    let seamless = previous
        .is_some_and(|p| p.audible == info.audible && p.delay_keys == info.delay_keys)
        && info.audible.keys().all(|id| {
            // A node that was silent and still is sounds the same, though
            // it's rebuilt each time.
            carried_ids.contains(id)
                || (info.silent.contains(id) && previous.is_some_and(|p| p.silent.contains(id)))
        });

    let graph = step_graph(&nodes, &offsets_in);
    let readers = readers_of(&wiring, &mut nodes);
    let slots = nodes.len();
    let nodes_have_loops = nodes.iter().any(|node| node.owner.is_some());
    info.shape = PlanShape {
        steps: graph.steps(),
        critical_path: graph.critical_path(),
        width: graph.width(),
    };
    let plan = Plan {
        generation,
        previous: previous.map(|p| p.generation),
        seamless,
        nodes,
        pool: vec![0.0; pool_len].into_boxed_slice(),
        silent: vec![false; pool_len / max_frames.max(1)].into_boxed_slice(),
        zeros: vec![0.0; max_frames].into_boxed_slice(),
        events: (0..schedule.event_buffers)
            .map(|_| Vec::with_capacity(EVENT_CAPACITY))
            .collect(),
        spectral: spectral_lanes
            .iter()
            .map(|&lanes| SpectralBuf::new(lanes, max_frames))
            .collect(),
        values,
        offsets: offsets_in,
        migrations,
        value_migrations,
        delay_migrations,
        spectral_delay_migrations,
        max_frames,
        graph,
        loops: nodes_have_loops,
        readers,
        live: vec![true; slots].into_boxed_slice(),
        was_live: vec![true; slots].into_boxed_slice(),
        zero: vec![false; slots].into_boxed_slice(),
        silenced: vec![false; slots].into_boxed_slice(),
        blocks: telemetry.map(Telemetry::block_writer),
    };
    (Box::new(plan), info)
}

/// The wiring of every node an Output node depends on, found by walking
/// back from the Output nodes. `wiring` and `nodes` are both by slot.
fn audible_wiring(wiring: &[(NodeId, Sources)], nodes: &[PlanNode]) -> Wiring {
    let slots: HashMap<NodeId, usize> = wiring
        .iter()
        .enumerate()
        .filter(|(slot, _)| nodes[*slot].owner.is_none())
        .map(|(slot, (id, _))| (*id, slot))
        .collect();
    let mut audible = HashMap::new();
    let mut stack: Vec<usize> = (0..nodes.len()).filter(|&s| nodes[s].is_output).collect();
    while let Some(slot) = stack.pop() {
        let (id, sources) = &wiring[slot];
        if audible.contains_key(id) {
            continue;
        }
        stack.extend(sources.iter().flatten().map(|(source, _)| slots[source]));
        audible.insert(*id, sources.clone());
    }
    audible
}

/// For each node (by slot, a split node counting as its output half), the
/// nodes that read something it writes.
/// Also fills in where each node's signal inputs come from.
fn readers_of(wiring: &[(NodeId, Sources)], nodes: &mut [PlanNode]) -> Vec<Vec<usize>> {
    let slots: HashMap<NodeId, usize> = wiring
        .iter()
        .enumerate()
        .filter(|(slot, _)| nodes[*slot].owner.is_none())
        .map(|(slot, (id, _))| (*id, slot))
        .collect();
    let mut readers = vec![Vec::new(); nodes.len()];
    for (reader, (_, sources)) in wiring.iter().enumerate() {
        for (source, _) in sources.iter().flatten() {
            let source = slots[source];
            if source != reader && !readers[source].contains(&reader) {
                readers[source].push(reader);
            }
        }
        let signals = nodes[reader].inputs.len();
        nodes[reader].sources = sources
            .iter()
            .take(signals)
            .map(|source| source.map(|(id, port)| (slots[&id], port)))
            .collect();
    }
    readers
}

/// How many events one event output can produce per block.
const EVENT_CAPACITY: usize = 1024;

fn instantiate(
    scheduled: &crate::ScheduledNode,
    sample_rate: f32,
    max_frames: usize,
    position: u64,
) -> Result<Box<dyn Node>, NodeError> {
    let setup = Setup {
        node: scheduled.id,
        config: &scheduled.config,
        sample_rate,
        max_frames,
        input_shapes: &scheduled.input_shapes,
        output_shapes: &scheduled.output_shapes,
        seed: seed_for(scheduled.id),
        position,
    };
    match scheduled.node_type.instantiate(&setup)? {
        Instance::Realtime(node) => Ok(node),
        Instance::Offline(_) => Err(NodeError::config(
            "node type bug: offline instance for a real-time layout",
        )),
    }
}

/// A well-mixed seed from a node ID (SplitMix64), so neighbouring IDs get
/// unrelated seeds.
pub(crate) fn seed_for(id: NodeId) -> u64 {
    let mut z = id.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Stands in for a node that couldn't be instantiated.
struct Silence;

impl Node for Silence {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        for output in io.outputs {
            output.fill(0.0);
        }
    }
}

/// The silence flag of a placeholder input.
static ABSENT_SILENT: [bool; 1] = [true];

impl Plan {
    /// Whether this plan can be installed without fading the output. See
    /// [`Processor`](crate::Processor).
    pub(crate) fn is_seamless(&self) -> bool {
        self.seamless
    }

    /// Moves carried-over node instances and smoothing state out of the plan
    /// this one replaces. Real-time safe.
    pub(crate) fn take_state_from(&mut self, old: &mut Plan) {
        debug_assert_eq!(self.previous, Some(old.generation), "plans arrive in order");
        if self.previous != Some(old.generation) {
            return;
        }
        for &(from, to) in &self.migrations {
            self.nodes[to].instance = old.nodes[from].instance.take();
        }
        for &(from, to) in &self.value_migrations {
            self.values[to].continue_from(&old.values[from]);
        }
        for &(from, to) in &self.migrations {
            self.was_live[to] = old.was_live[from];
        }
        for &((old_slot, old_line), (slot, line)) in &self.delay_migrations {
            self.nodes[slot].delays[line].continue_from(&mut old.nodes[old_slot].delays[old_line]);
        }
        for &((old_slot, old_line), (slot, line)) in &self.spectral_delay_migrations {
            self.nodes[slot].spectral_delays[line]
                .line
                .continue_from(&mut old.nodes[old_slot].spectral_delays[old_line].line);
        }
    }

    /// Clears every node's internal state, for when the transport jumps.
    pub(crate) fn reset_nodes(&mut self) {
        for node in &mut self.nodes {
            if let Some(instance) = &mut node.instance {
                instance.reset();
            }
            for delay in &mut node.delays {
                delay.clear();
            }
            for delay in &mut node.spectral_delays {
                delay.line.clear();
            }
        }
    }

    /// Works out which nodes have to run this block: those that are always
    /// run, and any whose outputs a running node reads. A node whose outputs
    /// nothing reads, or only nodes that are [silenced](Node::silenced_by)
    /// (a muted track's stage, a VCA at level 0), stops, and so does
    /// everything that only feeds it, back to the sources. Real-time safe.
    fn update_liveness(&mut self) {
        // In schedule order, so a source's constants are known before the
        // nodes it feeds ask about its outputs.
        for slot in 0..self.nodes.len() {
            self.silenced[slot] = false;
            if self.nodes[slot].owner.is_some() {
                continue;
            }
            for index in 0..self.nodes[slot].inputs.len() {
                let constant = match self.nodes[slot].inputs[index] {
                    Input::Value(value) => self.values[value].constant(),
                    Input::Absent => Some(0.0),
                    // A wire from an output that holds one value.
                    Input::Buffer(_) => self.nodes[slot]
                        .sources
                        .get(index)
                        .copied()
                        .flatten()
                        .and_then(|(source, output)| {
                            let source = &self.nodes[source];
                            source
                                .instance
                                .as_ref()?
                                .constant_output(output, &source.constants)
                        }),
                    Input::Offset(_) => None,
                };
                self.nodes[slot].constants[index] = constant;
            }
            let node = &self.nodes[slot];
            if let Some(instance) = &node.instance {
                self.silenced[slot] = instance.silenced_by(&node.constants);
            }
        }
        for (live, node) in self.live.iter_mut().zip(&self.nodes) {
            *live = node.always_runs;
        }
        // The input half of a split node follows its output half.
        loop {
            let mut changed = false;
            for node in (0..self.nodes.len()).rev() {
                if self.live[node] {
                    continue;
                }
                if self.readers[node]
                    .iter()
                    .any(|&reader| self.live[reader] && !self.silenced[reader])
                {
                    self.live[node] = true;
                    changed = true;
                }
            }
            if !changed || !self.loops {
                break;
            }
        }
        for node in 0..self.nodes.len() {
            let live = self.live[node];
            self.zero[node] = !live && self.readers[node].iter().any(|&reader| self.live[reader]);
            if !live
                && self.was_live[node]
                && let Some(instance) = &mut self.nodes[node].instance
            {
                instance.stopped();
            }
            if live && !self.was_live[node] {
                let node = &mut self.nodes[node];
                if let Some(instance) = &mut node.instance {
                    instance.reset();
                }
                for delay in &mut node.delays {
                    delay.clear();
                }
                for delay in &mut node.spectral_delays {
                    delay.line.clear();
                }
            }
            self.was_live[node] = live;
        }
    }

    /// Whether something is watching how long blocks take.
    pub(crate) fn times_blocks(&self) -> bool {
        self.blocks.is_some()
    }

    /// Reports that a callback of `frames` frames took `used_ns`.
    pub(crate) fn record_block(&self, used_ns: u64, frames: usize, sample_rate: f32) {
        if let Some(blocks) = &self.blocks {
            blocks.record(used_ns, frames as f64 * 1e9 / f64::from(sample_rate));
        }
    }

    /// Renders one block into `output`, interleaved with `channels` channels,
    /// with `input` feeding Input nodes. Real-time safe. With `workers`, steps
    /// that don't depend on each other run on several threads when the plan
    /// is wide enough for that to pay.
    pub(crate) fn run(
        &mut self,
        ctx: &Context,
        input: Interleaved<'_>,
        output: &mut [f32],
        channels: usize,
        workers: Option<&Workers>,
    ) {
        let frames = ctx.frames;
        debug_assert!(frames <= self.max_frames);
        debug_assert_eq!(output.len(), frames * channels);
        debug_assert_eq!(input.samples.len(), frames * input.channels);

        for value in &mut self.values {
            value.prepare(frames);
        }
        output.fill(0.0);
        self.update_liveness();

        // Views are made from raw pointers because one node's outputs and
        // another's inputs can be the same buffer at different times, which
        // safe borrows can't express.
        let frame = Frame {
            nodes: self.nodes.as_mut_ptr(),
            pool: self.pool.as_mut_ptr(),
            silent: self.silent.as_mut_ptr(),
            events: self.events.as_mut_ptr(),
            spectral: self.spectral.as_mut_ptr(),
            values: self.values.as_ptr(),
            offsets: self.offsets.as_mut_ptr(),
            zeros: &self.zeros,
            max_frames: self.max_frames,
            channels,
            ctx,
            input,
            live: &self.live,
            zero: &self.zero,
        };

        match workers.filter(|w| w.worth(self.graph.shape())) {
            None => {
                for index in 0..self.nodes.len() {
                    // SAFETY: one step at a time, in schedule order.
                    unsafe { run_step(&frame, index, output.as_mut_ptr(), false) };
                }
            }
            Some(workers) => {
                let run = ParallelRun {
                    frame: &frame,
                    graph: &self.graph,
                };
                self.graph.reset();
                // SAFETY: the graph runs each step once, after every step
                // whose buffers it touches; see `step_graph`.
                unsafe { workers.run(ParallelRun::work, (&run as *const ParallelRun).cast()) };
                // In schedule order, so the sum doesn't depend on which
                // thread got there first.
                for node in &self.nodes {
                    for (out, &x) in output.iter_mut().zip(&node.bus_out[..]) {
                        *out += x;
                    }
                }
            }
        }
        if self.graph.poisoned()
            && let Some(blocks) = &self.blocks
        {
            blocks.note_panic();
        }
    }
}

/// Everything a block needs to run steps, as raw pointers into a [`Plan`] so
/// that several threads can each work on different steps.
struct Frame<'a> {
    nodes: *mut PlanNode,
    pool: *mut f32,
    silent: *mut bool,
    events: *mut Vec<Event>,
    spectral: *mut SpectralBuf,
    values: *const ValueInput,
    offsets: *mut OffsetInput,
    zeros: &'a [f32],
    max_frames: usize,
    channels: usize,
    ctx: &'a Context,
    input: Interleaved<'a>,
    /// By slot of the node's output half. See [`Plan::update_liveness`].
    live: &'a [bool],
    zero: &'a [bool],
}

/// A block shared between threads.
struct ParallelRun<'a> {
    frame: &'a Frame<'a>,
    graph: &'a StepGraph,
}

impl ParallelRun<'_> {
    /// SAFETY: `data` is a `ParallelRun` that outlives the call.
    unsafe fn work(data: *const ()) {
        let run = unsafe { &*data.cast::<ParallelRun>() };
        run.graph.work(|index| {
            // SAFETY: the graph hands each step to one thread, after
            // everything it must follow.
            let mix = unsafe { (*run.frame.nodes.add(index)).bus_out.as_mut_ptr() };
            if unsafe { run_step(run.frame, index, mix, true) } {
                run.graph.poison();
            }
        });
    }
}

/// Runs one step: builds the node's views, runs it, and mixes it into
/// `mix_out` if it is an Output node. With `fresh`, `mix_out` is a scratch
/// buffer that starts empty and a panic in the node is caught (its outputs
/// are silent for the block and the result is true), so a block run by several
/// threads always finishes; otherwise `mix_out` is the device output and a
/// panic propagates.
///
/// SAFETY: no other thread may touch this node, the buffers it reads or
/// writes, its offsets or its owner's instance, and every step it must follow
/// has finished.
unsafe fn run_step(frame: &Frame<'_>, index: usize, mix_out: *mut f32, fresh: bool) -> bool {
    let &Frame {
        nodes,
        pool,
        silent,
        events,
        spectral,
        values,
        offsets,
        zeros,
        max_frames,
        channels,
        ctx,
        input,
        ..
    } = frame;
    let frames = ctx.frames;
    let node = unsafe { &mut *nodes.add(index) };
    let home = node.owner.unwrap_or(index);
    if !frame.live[home] {
        // Nothing listens to this node, so it doesn't run. If something that
        // ignores it still reads its outputs, they have to hold silence.
        if frame.zero[home] {
            // SAFETY: as for the views below; nothing else touches this
            // node's outputs now.
            unsafe {
                for view in &node.outputs {
                    let mut out = view.write(pool, silent, frames, max_frames);
                    out.fill(0.0);
                    for lane in 0..view.shape.lanes() {
                        out.set_silent(lane / view.shape.channels, lane % view.shape.channels);
                    }
                }
                for &buffer in &node.event_outputs {
                    (*events.add(buffer)).clear();
                }
                for &(buffer, _) in &node.spectral_outputs {
                    (*spectral.add(buffer)).clear();
                }
            }
        }
        return false;
    }
    let mut inputs: Vec<SignalIn<'_>> = recycle(mem::take(&mut node.scratch.inputs));
    let mut outputs: Vec<SignalOut<'_>> = recycle(mem::take(&mut node.scratch.outputs));
    let mut event_inputs: Vec<&[Event]> = recycle(mem::take(&mut node.scratch.event_inputs));
    let mut event_outputs: Vec<EventsOut<'_>> = recycle(mem::take(&mut node.scratch.event_outputs));
    let mut spectral_inputs: Vec<SpectralIn<'_>> =
        recycle(mem::take(&mut node.scratch.spectral_inputs));
    let mut spectral_outputs: Vec<SpectralOut<'_>> =
        recycle(mem::take(&mut node.scratch.spectral_outputs));

    for delay in &mut node.delays {
        // SAFETY: the source buffer is one of this node's inputs, so it holds
        // this block's signal and nothing writes it now; the destination is
        // private to the delay line.
        unsafe { delay.run(pool, silent, frames, max_frames) };
    }
    for delay in &mut node.spectral_delays {
        // SAFETY: as above, for the spectral buffers: the source holds this
        // block's frames, and the destination is private to the line.
        unsafe { delay.run(spectral, frames) };
    }
    for input in &node.inputs {
        if let Input::Offset(k) = *input {
            let offset = unsafe { &mut *offsets.add(k) };
            // SAFETY: `pool` is the plan's pool, and nothing writes the
            // source buffer while the offset is computed.
            unsafe { offset.compute(pool, frames, &*values.add(offset.value)) };
        }
    }

    // SAFETY: every view is in bounds of the pool or event buffers, which
    // outlive this block. The compiler guarantees that a node's outputs share
    // no buffer with its inputs or each other, and only this node's views
    // exist while it runs.
    unsafe {
        inputs.extend(node.inputs.iter().map(|input| match *input {
            Input::Buffer(view) => view.read(pool, silent, frames, max_frames),
            Input::Value(slot) => (*values.add(slot)).signal(frames),
            Input::Offset(k) => (*offsets.add(k)).signal(frames),
            Input::Absent => {
                SignalIn::new(&zeros[..frames], Shape::MONO, frames).with_silent(&ABSENT_SILENT)
            }
        }));
        outputs.extend(
            node.outputs
                .iter()
                .map(|view| view.write(pool, silent, frames, max_frames)),
        );
        event_inputs.extend(node.event_inputs.iter().map(|buffer| match buffer {
            Some(b) => (*events.add(*b)).as_slice(),
            None => &[],
        }));
        event_outputs.extend(
            node.event_outputs
                .iter()
                .map(|&b| EventsOut::new(&mut *events.add(b))),
        );
        spectral_inputs.extend(node.spectral_inputs.iter().map(|input| match input {
            Some((b, shape)) => (*spectral.add(*b)).as_in(*shape),
            None => SpectralIn::empty(),
        }));
        spectral_outputs.extend(
            node.spectral_outputs
                .iter()
                .map(|&(b, shape)| SpectralOut::new(&mut *spectral.add(b), shape)),
        );
    }

    for (index, probe) in &node.probes {
        let signal = &inputs[*index];
        if signal.shape().lanes() > 0 {
            probe.write(signal.lane(0, 0));
        }
    }

    for (index, meter) in &node.input_meters {
        let signal = &inputs[*index];
        let shape = signal.shape();
        for channel in 0..shape.channels.min(meter.channels()) {
            meter.write(channel, input_level(signal, channel));
        }
    }

    let io = Io {
        inputs: &inputs,
        outputs: &mut outputs,
        event_inputs: &event_inputs,
        event_outputs: &mut event_outputs,
        spectral_inputs: &spectral_inputs,
        spectral_outputs: &mut spectral_outputs,
    };
    // A split node's input half runs the instance of its output half, which
    // is earlier.
    let instance = match node.owner {
        Some(owner) => unsafe { &mut (*nodes.add(owner)).instance },
        None => &mut node.instance,
    };
    let started = node.cpu.is_some().then(Instant::now);
    let phase = node.phase;
    let call = move || match (instance, phase) {
        (Some(instance), Phase::Whole) => instance.process(ctx, io),
        (Some(instance), Phase::Output) => instance.process_output(ctx, io),
        (Some(instance), Phase::Input) => instance.process_input(ctx, io),
        // Only possible if plans arrived out of order.
        (None, Phase::Input) => {}
        (None, _) => Silence.process(ctx, io),
    };
    let panicked = if fresh {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)).is_err()
    } else {
        call();
        false
    };
    if panicked {
        for output in &mut outputs {
            output.fill(0.0);
        }
    }
    if let (Some(cpu), Some(started)) = (&mut node.cpu, started) {
        cpu.record(
            started.elapsed().as_nanos() as u64,
            frames as f64 * 1e9 / f64::from(ctx.sample_rate),
        );
    }
    if node.is_input {
        read_input(input, &mut outputs[0]);
    }
    // Mixed now, not after the whole schedule: the compiler frees a buffer
    // after its last reader, so a later node may reuse it.
    if let Some((first, count)) = node.bus {
        // SAFETY: `mix_out` holds `frames * channels` samples.
        let out = unsafe { std::slice::from_raw_parts_mut(mix_out, frames * channels) };
        if fresh {
            out.fill(0.0);
        }
        match &node.downmix {
            Some(matrix) => mix_matrix_into(inputs[0], matrix, out, channels, first, count),
            None => mix_into(inputs[0], out, channels, first, count),
        }
    }

    node.scratch.inputs = recycle(inputs);
    node.scratch.outputs = recycle(outputs);
    node.scratch.event_inputs = recycle(event_inputs);
    node.scratch.event_outputs = recycle(event_outputs);
    node.scratch.spectral_inputs = recycle(spectral_inputs);
    node.scratch.spectral_outputs = recycle(spectral_outputs);
    panicked
}

/// Works out which steps must wait for which, from the buffers each reads and
/// writes. The compiler reuses buffers, so besides a step waiting for the
/// writer of what it reads, it waits for the readers (and writer) of what it
/// overwrites. A split node's input half also waits for its output half,
/// which holds the instance.
fn step_graph(nodes: &[PlanNode], offsets: &[OffsetInput]) -> StepGraph {
    // Buffers (and event buffers) are told apart by where they start: a view
    // always begins at its buffer's start.
    #[derive(Default)]
    struct Track {
        writer: Option<usize>,
        readers: Vec<usize>,
    }
    let mut pool: HashMap<usize, Track> = HashMap::new();
    let mut events: HashMap<usize, Track> = HashMap::new();
    let mut spectral: HashMap<usize, Track> = HashMap::new();
    let mut waits: Vec<Vec<usize>> = Vec::with_capacity(nodes.len());
    for (step, node) in nodes.iter().enumerate() {
        let mut wait = Vec::new();
        wait.extend(node.owner);
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        for input in &node.inputs {
            match *input {
                Input::Buffer(view) if view.shape.lanes() > 0 => reads.push(view.offset),
                Input::Offset(k) if offsets[k].source.shape.lanes() > 0 => {
                    reads.push(offsets[k].source.offset);
                }
                _ => {}
            }
        }
        for delay in &node.delays {
            if delay.source.shape.lanes() > 0 {
                reads.push(delay.source.offset);
                writes.push(delay.dest.offset);
            }
        }
        writes.extend(
            node.outputs
                .iter()
                .filter(|v| v.shape.lanes() > 0)
                .map(|v| v.offset),
        );
        let event_reads: Vec<usize> = node.event_inputs.iter().flatten().copied().collect();
        let event_writes = &node.event_outputs;
        let mut spectral_reads: Vec<usize> = node
            .spectral_inputs
            .iter()
            .flatten()
            .map(|&(b, _)| b)
            .collect();
        let mut spectral_writes: Vec<usize> =
            node.spectral_outputs.iter().map(|&(b, _)| b).collect();
        for delay in &node.spectral_delays {
            spectral_reads.push(delay.source);
            spectral_writes.push(delay.dest);
        }
        for (tracks, reads, writes) in [
            (&mut pool, &reads[..], &writes[..]),
            (&mut events, &event_reads[..], &event_writes[..]),
            (&mut spectral, &spectral_reads[..], &spectral_writes[..]),
        ] {
            for &key in reads {
                let track = tracks.entry(key).or_default();
                wait.extend(track.writer);
                track.readers.push(step);
            }
            for &key in writes {
                let track = tracks.entry(key).or_default();
                wait.extend(track.writer);
                wait.extend(track.readers.iter().copied());
                track.writer = Some(step);
                track.readers.clear();
            }
        }
        wait.retain(|&w| w != step);
        wait.sort_unstable();
        wait.dedup();
        waits.push(wait);
    }
    StepGraph::new(&waits)
}

impl View {
    /// SAFETY: `pool` must be the plan's pool, and nothing may write this
    /// buffer while the view lives.
    unsafe fn read<'a>(
        self,
        pool: *const f32,
        silent: *const bool,
        frames: usize,
        max_frames: usize,
    ) -> SignalIn<'a> {
        let flags = unsafe {
            std::slice::from_raw_parts(
                silent.add(self.offset / max_frames.max(1)),
                self.shape.lanes(),
            )
        };
        unsafe { self.read_samples(pool, frames) }.with_silent(flags)
    }

    /// Like [`read`](Self::read), without the silence flags.
    ///
    /// SAFETY: as for `read`.
    unsafe fn read_samples<'a>(self, pool: *const f32, frames: usize) -> SignalIn<'a> {
        let len = self.shape.lanes() * frames;
        let data = unsafe { std::slice::from_raw_parts(pool.add(self.offset), len) };
        SignalIn::new(data, self.shape, frames)
    }

    /// SAFETY: `pool` must be the plan's pool, and nothing else may read or
    /// write this buffer while the view lives.
    unsafe fn write<'a>(
        self,
        pool: *mut f32,
        silent: *mut bool,
        frames: usize,
        max_frames: usize,
    ) -> SignalOut<'a> {
        let lanes = self.shape.lanes();
        let data = unsafe { std::slice::from_raw_parts_mut(pool.add(self.offset), lanes * frames) };
        let flags = unsafe {
            std::slice::from_raw_parts_mut(silent.add(self.offset / max_frames.max(1)), lanes)
        };
        SignalOut::new(data, self.shape, frames).with_silent(flags)
    }
}

/// Reuses an empty vector's allocation for a different lifetime, so scratch
/// vectors can hold views that borrow the pool for just one block.
fn recycle<T, U>(mut vec: Vec<T>) -> Vec<U> {
    const {
        assert!(size_of::<T>() == size_of::<U>() && align_of::<T>() == align_of::<U>());
    }
    vec.clear();
    let mut vec = mem::ManuallyDrop::new(vec);
    // SAFETY: the vector is empty, so no values change type, and `T` and `U`
    // have the same size and alignment, so the allocation's layout is right
    // for `U`. Only ever called with `T` and `U` differing in lifetimes.
    unsafe { Vec::from_raw_parts(vec.as_mut_ptr().cast::<U>(), 0, vec.capacity()) }
}

/// Device input for one block: interleaved, `channels` channels. Empty, with
/// no channels, when there's no input device.
#[derive(Clone, Copy)]
pub(crate) struct Interleaved<'a> {
    pub(crate) samples: &'a [f32],
    pub(crate) channels: usize,
}

impl Interleaved<'_> {
    pub(crate) const NONE: Interleaved<'static> = Interleaved {
        samples: &[],
        channels: 0,
    };
}

/// Fills an Input node's output from device input. Channel n comes from
/// device channel n, a mono device feeds every channel, and channels the
/// device doesn't have are silent.
pub(crate) fn read_input(input: Interleaved<'_>, signal: &mut SignalOut<'_>) {
    let Interleaved { samples, channels } = input;
    let shape = signal.shape();
    for channel in 0..shape.channels {
        let source = match channels {
            1 => Some(0),
            n if channel < n => Some(channel),
            _ => None,
        };
        for voice in 0..shape.voices {
            let lane = signal.lane_mut(voice, channel);
            match source {
                Some(source) => {
                    let frames = samples.iter().skip(source).step_by(channels);
                    for (out, &x) in lane.iter_mut().zip(frames) {
                        *out = x;
                    }
                }
                None => lane.fill(0.0),
            }
        }
    }
}

/// Adds a signal into the `count` channels of interleaved output that start
/// at `first`. A mono signal goes to the first two channels (left and right,
/// not the centre, LFE and rears of a surround bus); otherwise channel n goes
/// to channel n, and voices are summed.
fn mix_into(signal: SignalIn<'_>, output: &mut [f32], channels: usize, first: usize, count: usize) {
    let shape = signal.shape();
    for channel in 0..count {
        let source = match shape.channels {
            1 if channel < 2 => 0,
            1 => continue,
            n if channel < n => channel,
            _ => continue,
        };
        for voice in 0..shape.voices {
            let lane = signal.lane(voice, source);
            for (frame, sample) in lane.iter().enumerate() {
                output[frame * channels + first + channel] += sample;
            }
        }
    }
}

/// Adds a signal into the `count` channels of interleaved output that start at
/// `first`, through `matrix` (one row per output channel, one column per
/// signal channel), summing voices.
fn mix_matrix_into(
    signal: SignalIn<'_>,
    matrix: &[f32],
    output: &mut [f32],
    channels: usize,
    first: usize,
    count: usize,
) {
    let shape = signal.shape();
    for (channel, gains) in matrix.chunks(shape.channels).take(count).enumerate() {
        for (source, &gain) in gains.iter().enumerate() {
            if gain == 0.0 {
                continue;
            }
            for voice in 0..shape.voices {
                for (frame, sample) in signal.lane(voice, source).iter().enumerate() {
                    output[frame * channels + first + channel] += sample * gain;
                }
            }
        }
    }
}

/// One audio device's share of the engine's interleaved output: `channels`
/// channels, after those of the buses before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bus {
    /// The device's ID, or empty if it is unknown. The first bus is the main
    /// output, which Output nodes with no device use as well.
    pub device: String,
    pub channels: usize,
}

/// How the engine's output channels are shared out, for placing Output nodes.
#[derive(Clone, Copy)]
pub(crate) struct Routing<'a> {
    /// The engine's channel count.
    pub channels: usize,
    /// The devices sharing them, or `None` to mix every Output node into all
    /// of them, as when rendering offline.
    pub buses: Option<&'a [Bus]>,
}

impl Routing<'_> {
    /// Where an Output node tied to `device` mixes to: the first output
    /// channel and the channel count. `None` if no bus has that device.
    fn route(&self, device: &str) -> Option<(usize, usize)> {
        let Some(buses) = self.buses else {
            return Some((0, self.channels));
        };
        let mut first = 0;
        for (index, bus) in buses.iter().enumerate() {
            if (index == 0 && device.is_empty()) || (!device.is_empty() && bus.device == device) {
                return Some((first, bus.channels));
            }
            first += bus.channels;
        }
        None
    }
}

/// A wired parameter that moves its value along its travel: the parameter's
/// own value is the base, and the wire's signal is added to the base's
/// position (see [`Modulation::Offset`](crate::Modulation::Offset)).
struct OffsetInput {
    /// The base value's slot in the plan's values.
    value: usize,
    /// The wire's signal.
    source: View,
    info: ParamInfo,
    /// The effective value, in the shape of the wire's signal.
    scratch: Box<[f32]>,
    /// The value of every sample in the block, if there is one.
    constant: Option<f32>,
}

impl OffsetInput {
    fn new(value: usize, source: View, info: ParamInfo, max_frames: usize) -> Self {
        Self {
            value,
            source,
            info,
            scratch: vec![0.0; source.shape.lanes() * max_frames].into_boxed_slice(),
            constant: None,
        }
    }

    /// SAFETY: `pool` must be the plan's pool, and nothing may write the
    /// source buffer meanwhile.
    unsafe fn compute(&mut self, pool: *const f32, frames: usize, base: &ValueInput) {
        // SAFETY: as promised by the caller.
        let source = unsafe { self.source.read_samples(pool, frames) };
        let shape = source.shape();
        let info = &self.info;
        let constant = base.constant().map(|value| info.position(value));
        let base = base.samples(frames);
        // The one value of every sample of every lane, while there is one.
        let mut uniform: Option<Option<f32>> = None;
        for voice in 0..shape.voices {
            for channel in 0..shape.channels {
                let start = (voice * shape.channels + channel) * frames;
                let out = &mut self.scratch[start..start + frames];
                let signal = source.lane(voice, channel);
                // A held signal (a gate, a velocity, a sustaining envelope)
                // costs one conversion rather than one per sample.
                let held = constant
                    .zip(signal.first())
                    .filter(|&(_, &first)| first.is_finite() && signal.iter().all(|&x| x == first));
                if let Some((position, &x)) = held {
                    let value = info.value_at(position + x);
                    out.fill(value);
                    uniform = Some(match uniform {
                        None => Some(value),
                        Some(Some(u)) if u == value => Some(u),
                        Some(_) => None,
                    });
                    continue;
                }
                uniform = Some(None);
                for (i, (out, &x)) in out.iter_mut().zip(signal).enumerate() {
                    // A non-finite signal must not poison the node's state.
                    let x = if x.is_finite() { x } else { 0.0 };
                    let position = constant.unwrap_or_else(|| info.position(base[i]));
                    *out = info.value_at(position + x);
                }
            }
        }
        self.constant = uniform.flatten();
    }

    fn signal(&self, frames: usize) -> SignalIn<'_> {
        let shape = self.source.shape;
        let signal = SignalIn::new(&self.scratch[..shape.lanes() * frames], shape, frames);
        match self.constant {
            Some(value) => signal.with_constant(value),
            None => signal,
        }
    }
}

/// An unconnected input: a buffer holding its value, smoothed on change.
struct ValueInput {
    cell: Arc<ParamCell>,
    buffer: Box<[f32]>,
    current: f32,
    target: f32,
    step: f32,
    /// Samples left in the current ramp.
    remaining: usize,
    /// Ramp length in samples; 0 jumps straight to new values.
    smoothing: usize,
    /// The whole buffer holds `current`.
    settled: bool,
}

impl ValueInput {
    fn new(cell: Arc<ParamCell>, max_frames: usize, smoothing: usize) -> Self {
        let value = cell.get();
        Self {
            cell,
            buffer: vec![value; max_frames].into_boxed_slice(),
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
            smoothing,
            settled: true,
        }
    }

    fn prepare(&mut self, frames: usize) {
        let target = self.cell.get();
        // Non-finite values are ignored: ramping from one gives NaN, and a
        // node's state can't recover from either.
        if target != self.target && target.is_finite() {
            self.target = target;
            if self.smoothing == 0 {
                self.current = target;
                self.remaining = 0;
                self.settled = false;
            } else {
                self.remaining = self.smoothing;
                self.step = (target - self.current) / self.smoothing as f32;
            }
        }

        if self.remaining == 0 {
            if !self.settled {
                self.buffer.fill(self.current);
                self.settled = true;
            }
            return;
        }
        let ramp = frames.min(self.remaining);
        for sample in &mut self.buffer[..ramp] {
            self.current += self.step;
            *sample = self.current;
        }
        self.remaining -= ramp;
        if self.remaining == 0 {
            self.current = self.target;
        }
        self.buffer[ramp..frames].fill(self.current);
        self.settled = false;
    }

    /// The value, if it's the same for the whole block.
    fn constant(&self) -> Option<f32> {
        self.settled.then_some(self.current)
    }

    fn samples(&self, frames: usize) -> &[f32] {
        &self.buffer[..frames]
    }

    fn signal(&self, frames: usize) -> SignalIn<'_> {
        let signal = SignalIn::new(&self.buffer[..frames], Shape::MONO, frames);
        if self.settled {
            signal.with_constant(self.current)
        } else {
            signal
        }
    }

    /// Picks up where the same input in the previous plan left off, so a ramp
    /// in progress carries on smoothly.
    fn continue_from(&mut self, old: &ValueInput) {
        self.current = old.current;
        self.target = old.target;
        self.step = old.step;
        self.remaining = old.remaining;
        self.settled = false;
    }
}

/// Spectral delay lines are recorded in `PlanInfo::delays` under this plus
/// their port, which no signal input index reaches, so a change in a node's
/// signal inputs can't make a signal line take over a spectral one's state.
const SPECTRAL_KEY_BASE: usize = 1 << 20;

/// `(old slot, old line)` to `(slot, line)` for delay lines that take over
/// the contents of the previous plan's.
type DelayMigrations = Vec<((usize, usize), (usize, usize))>;

/// What delay compensation needs to know about a scheduled step.
struct SlotInfo {
    id: NodeId,
    latency: u32,
    /// Where the mix is heard (an Output node that plays, or a tap), which
    /// every such step is aligned to.
    sink: bool,
    /// The buffer behind each signal input, if connected.
    inputs: Vec<Option<crate::BufferId>>,
    outputs: Vec<crate::BufferId>,
    /// The buffer behind each spectral input, if connected, and the spectral
    /// outputs. Lateness travels along them.
    spectral_inputs: Vec<Option<crate::SpectralBufferId>>,
    spectral_outputs: Vec<crate::SpectralBufferId>,
    /// False for the input half of a split node, which only reads its loop
    /// input and makes no output.
    consumes: bool,
}

/// Plugin delay compensation. A node reports how late its outputs sound
/// ([`Node::latency`]); the signal at each buffer is that many samples later
/// than the timeline, summed along the path to it. Where paths meet (a node
/// with several signal inputs, or the sinks), the faster inputs get a delay
/// line so everything lines up with the slowest. Lines read their source at
/// the consumer's step and write a private region added to the end of the
/// pool, so the compiler's buffer reuse is untouched.
///
/// Lateness also travels along spectral wires, so a pair of Analyze and
/// Synthesize after a latent plugin, or two pairs in a row, is as late as the
/// whole path. Spectral inputs are lined up like signal inputs, with a
/// [`SpectralDelay`] that holds the frames of the earlier paths back, so a
/// Cross of paths of different lateness sees frames of the same moment.
///
/// Events aren't compensated, and a loop's own input never is (the loop
/// already carries its delay). Returns the delay lines that can take over
/// the contents of the previous plan's.
#[allow(clippy::too_many_arguments)]
fn compensate(
    nodes: &mut [PlanNode],
    slots: &[SlotInfo],
    offsets: &mut [OffsetInput],
    pool_len: &mut usize,
    spectral_lanes: &mut Vec<usize>,
    max_frames: usize,
    previous: Option<&PlanInfo>,
    info: &mut PlanInfo,
) -> (DelayMigrations, DelayMigrations) {
    if slots.iter().all(|s| s.latency == 0) {
        return (Vec::new(), Vec::new());
    }
    // How late each buffer is, as of the step being looked at; buffers are
    // reused, so this follows the schedule.
    let mut arrival: HashMap<crate::BufferId, u32> = HashMap::new();
    let mut spectral_arrival: HashMap<crate::SpectralBufferId, u32> = HashMap::new();
    // The latest a step's spectral inputs arrive.
    let mut spectral_slowest: Vec<u32> = Vec::with_capacity(slots.len());
    let mut spectral_input_arrival: Vec<Vec<Option<u32>>> = Vec::with_capacity(slots.len());
    let mut input_arrival: Vec<Vec<Option<u32>>> = Vec::with_capacity(slots.len());
    let mut sink_latency = 0;
    for slot in slots {
        let arrivals: Vec<Option<u32>> = slot
            .inputs
            .iter()
            .map(|b| b.map(|b| arrival.get(&b).copied().unwrap_or(0)))
            .collect();
        let spectral_arrivals: Vec<Option<u32>> = slot
            .spectral_inputs
            .iter()
            .map(|b| b.map(|b| spectral_arrival.get(&b).copied().unwrap_or(0)))
            .collect();
        let spectral = spectral_arrivals
            .iter()
            .flatten()
            .copied()
            .max()
            .unwrap_or(0);
        spectral_input_arrival.push(spectral_arrivals);
        spectral_slowest.push(spectral);
        let slowest = arrivals
            .iter()
            .flatten()
            .copied()
            .max()
            .unwrap_or(0)
            .max(spectral);
        if slot.consumes {
            for &b in &slot.outputs {
                arrival.insert(b, slowest + slot.latency);
            }
            for &b in &slot.spectral_outputs {
                spectral_arrival.insert(b, slowest + slot.latency);
            }
        }
        if slot.sink {
            sink_latency = sink_latency.max(slowest);
        }
        input_arrival.push(arrivals);
    }
    info.output_latency = sink_latency;

    let mut migrations = Vec::new();
    let mut spectral_migrations = Vec::new();
    for (index, (slot, arrivals)) in slots.iter().zip(&input_arrival).enumerate() {
        if !slot.consumes {
            continue;
        }
        let target = if slot.sink {
            sink_latency
        } else {
            arrivals
                .iter()
                .flatten()
                .copied()
                .max()
                .unwrap_or(0)
                .max(spectral_slowest[index])
        };
        // Spectral inputs have their own lines, keyed after the signal inputs.
        for (port, arrived) in spectral_input_arrival[index].iter().enumerate() {
            let Some(arrived) = *arrived else { continue };
            if arrived >= target {
                continue;
            }
            let Some((source, shape)) = nodes[index].spectral_inputs[port] else {
                continue;
            };
            let delay = (target - arrived) as usize;
            let dest = spectral_lanes.len();
            spectral_lanes.push(shape.lanes());
            let line_index = nodes[index].spectral_delays.len();
            nodes[index].spectral_delays.push(SpectralDelayLine {
                source,
                dest,
                line: SpectralDelay::new(shape.lanes(), delay, max_frames),
                shape,
            });
            nodes[index].spectral_inputs[port] = Some((dest, shape));
            let key = (slot.id, SPECTRAL_KEY_BASE + port);
            info.delays.push((key, (index, line_index, delay as u32)));
            let old = previous.and_then(|p| {
                p.delays
                    .iter()
                    .find(|(k, (_, _, d))| *k == key && *d == delay as u32)
            });
            if let Some((_, (old_slot, old_line, _))) = old {
                spectral_migrations.push(((*old_slot, *old_line), (index, line_index)));
            }
        }
        for (input, arrived) in arrivals.iter().enumerate() {
            let Some(arrived) = *arrived else { continue };
            if arrived >= target {
                continue;
            }
            let delay = (target - arrived) as usize;
            let source = match nodes[index].inputs[input] {
                Input::Buffer(view) => view,
                Input::Offset(k) => offsets[k].source,
                _ => continue,
            };
            let dest = View {
                offset: *pool_len,
                shape: source.shape,
            };
            *pool_len += source.shape.lanes() * max_frames;
            let line = DelayLine::new(source, dest, delay);
            match nodes[index].inputs[input] {
                Input::Buffer(_) => nodes[index].inputs[input] = Input::Buffer(dest),
                Input::Offset(k) => offsets[k].source = dest,
                _ => unreachable!(),
            }
            let id = slot.id;
            let line_index = nodes[index].delays.len();
            nodes[index].delays.push(line);
            info.delays
                .push(((id, input), (index, line_index, delay as u32)));
            let old = previous.and_then(|p| {
                p.delays
                    .iter()
                    .find(|(key, (_, _, d))| *key == (id, input) && *d == delay as u32)
            });
            if let Some((_, (old_slot, old_line, _))) = old {
                migrations.push(((*old_slot, *old_line), (index, line_index)));
            }
        }
    }
    info.delays.sort_by_key(|(key, _)| *key);
    info.delay_keys = info
        .delays
        .iter()
        .map(|&(key, (_, _, delay))| (key, delay))
        .collect();
    (migrations, spectral_migrations)
}

/// Holds one spectral input back, reading its source at the consumer's step
/// and writing a private buffer appended to the plan's spectral buffers.
struct SpectralDelayLine {
    source: usize,
    dest: usize,
    shape: Shape,
    line: SpectralDelay,
}

impl SpectralDelayLine {
    /// SAFETY: `spectral` must be the plan's, nothing may write the source
    /// buffer meanwhile, and nothing else may touch the destination.
    unsafe fn run(&mut self, spectral: *mut SpectralBuf, frames: usize) {
        // SAFETY: the two buffers are different entries of the plan's list.
        let (input, mut out) = unsafe {
            (
                (*spectral.add(self.source)).as_in(self.shape),
                SpectralOut::new(&mut *spectral.add(self.dest), self.shape),
            )
        };
        self.line.process(&input, frames, &mut out);
    }
}

/// Holds one signal back by a fixed number of samples.
struct DelayLine {
    source: View,
    dest: View,
    delay: usize,
    /// `delay` samples for each lane, back to back.
    ring: Box<[f32]>,
    /// Where in each lane's ring the oldest sample is.
    pos: usize,
}

impl DelayLine {
    fn new(source: View, dest: View, delay: usize) -> Self {
        Self {
            source,
            dest,
            delay,
            ring: vec![0.0; source.shape.lanes() * delay].into_boxed_slice(),
            pos: 0,
        }
    }

    fn clear(&mut self) {
        self.ring.fill(0.0);
    }

    /// Takes over what `old` was holding, so a plan swap doesn't drop the
    /// samples in flight.
    fn continue_from(&mut self, old: &mut DelayLine) {
        if self.delay == old.delay && self.ring.len() == old.ring.len() {
            mem::swap(&mut self.ring, &mut old.ring);
            self.pos = old.pos;
        }
    }

    /// SAFETY: `pool` and `silent` must be the plan's, nothing may write the
    /// source buffer meanwhile, and nothing else may touch the destination.
    unsafe fn run(&mut self, pool: *mut f32, silent: *mut bool, frames: usize, max_frames: usize) {
        let lanes = self.source.shape.lanes();
        let delay = self.delay;
        for lane in 0..lanes {
            // SAFETY: in bounds of the pool, and the two regions are disjoint.
            let (input, output) = unsafe {
                (
                    std::slice::from_raw_parts(
                        pool.add(self.source.offset + lane * frames),
                        frames,
                    ),
                    std::slice::from_raw_parts_mut(
                        pool.add(self.dest.offset + lane * frames),
                        frames,
                    ),
                )
            };
            let ring = &mut self.ring[lane * delay..(lane + 1) * delay];
            let mut pos = self.pos;
            for (out, &x) in output.iter_mut().zip(input) {
                *out = mem::replace(&mut ring[pos], x);
                pos += 1;
                if pos == delay {
                    pos = 0;
                }
            }
        }
        self.pos = (self.pos + frames) % delay;
        // SAFETY: the destination's flags are in bounds of the silence flags.
        unsafe {
            std::slice::from_raw_parts_mut(silent.add(self.dest.offset / max_frames.max(1)), lanes)
        }
        .fill(false);
    }
}
