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

use noodle_core::NodeId;

use crate::builtin::{INPUT_ID, OUTPUT_DEVICE, OUTPUT_ID};
use crate::{
    Context, Diagnostic, Event, EventsOut, InputKind, InputSource, Instance, Io, Node, NodeError,
    ParamInfo, ParamKind, Problem, Schedule, Settings, Setup, Shape, SignalIn, SignalOut,
    TapWriter, Telemetry,
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
}

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
    events: Vec<Vec<Event>>,
    values: Vec<ValueInput>,
    /// Wired parameters that offset their value, by index into the plan's
    /// offsets.
    offsets: Vec<OffsetInput>,
    /// (slot in the previous plan, slot in this one) for node instances that
    /// carry over.
    migrations: Vec<(usize, usize)>,
    /// The same for unconnected inputs' smoothing state.
    value_migrations: Vec<(usize, usize)>,
    max_frames: usize,
}

struct PlanNode {
    /// `None` until a carried-over instance arrives from the previous plan.
    instance: Option<Box<dyn Node>>,
    inputs: Vec<Input>,
    outputs: Vec<View>,
    event_inputs: Vec<Option<usize>>,
    event_outputs: Vec<usize>,
    /// Taps on the node's wired parameters, as (input index, tap).
    taps: Vec<(usize, TapWriter)>,
    /// An Output node, whose input is mixed into the device output.
    is_output: bool,
    /// Where an Output node mixes to: the first output channel and the
    /// number of channels. `None` for an Output node that plays nothing
    /// (its device isn't open) and for every other node.
    bus: Option<(usize, usize)>,
    /// An Input node, whose output the executor fills from the device input.
    is_input: bool,
    scratch: Scratch,
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
}

/// Vectors with room for one node's views, so building its [`Io`] each block
/// doesn't allocate. They're always empty between blocks; see [`recycle`].
#[derive(Default)]
struct Scratch {
    inputs: Vec<SignalIn<'static>>,
    outputs: Vec<SignalOut<'static>>,
    event_inputs: Vec<&'static [Event]>,
    event_outputs: Vec<EventsOut<'static>>,
}

/// What a plan is built for: the engine's settings, the output devices it
/// mixes to, and the hub its taps report to.
#[derive(Clone, Copy)]
pub(crate) struct Env<'a> {
    pub(crate) settings: Settings,
    pub(crate) buses: Option<&'a [Bus]>,
    pub(crate) telemetry: &'a Telemetry,
}

/// Builds a plan from a schedule, instantiating every node that can't carry
/// over from `previous`. Nodes that fail to instantiate are kept, as silence,
/// so the buffers downstream of them stay valid.
pub(crate) fn build(
    schedule: Schedule,
    previous: Option<&PlanInfo>,
    generation: u64,
    env: Env<'_>,
    cells: &mut Cells,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Box<Plan>, PlanInfo) {
    let Env {
        settings,
        buses,
        telemetry,
    } = env;
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
        if scheduled.node_type.info().id == OUTPUT_ID {
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
    };
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
    let mut wiring = Vec::with_capacity(schedule.nodes.len());
    let mut carried_ids = HashSet::new();

    for (slot, scheduled) in schedule.nodes.into_iter().enumerate() {
        let id = scheduled.id;
        let key = NodeKey {
            type_id: scheduled.node_type.info().id,
            config: scheduled.config.clone(),
            input_shapes: scheduled.input_shapes.clone(),
            output_shapes: scheduled.output_shapes.clone(),
        };

        let carried = previous
            .and_then(|p| p.nodes.get(&id))
            .filter(|(old_key, _)| *old_key == key);
        // A node that fails to instantiate plays silence, and isn't recorded
        // as able to carry over, so the next update retries it and reports
        // the problem again.
        let (instance, carries_over) = match carried {
            Some(&(_, old_slot)) => {
                migrations.push((old_slot, slot));
                carried_ids.insert(id);
                (None, true)
            }
            None => match instantiate(&scheduled, sample_rate, max_frames) {
                Ok(node) => {
                    // A lane's source has no state, so rebuilding it for new
                    // points changes only the value it writes, like a
                    // parameter being moved: no fade needed. A lane that is
                    // new is a change of wiring and still fades.
                    if scheduled.node_type.info().id == crate::AUTOMATION_ID
                        && previous.is_some_and(|p| p.nodes.contains_key(&id))
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

        let mut inputs = Vec::with_capacity(scheduled.inputs.len());
        let mut taps = Vec::new();
        for (index, ((port, source), shape)) in scheduled
            .layout
            .inputs
            .iter()
            .zip(&scheduled.inputs)
            .zip(&scheduled.input_shapes)
            .enumerate()
        {
            // The value a parameter holds when nothing drives it, or the base
            // that a wire offsets.
            let base = match *source {
                InputSource::Value(value) | InputSource::Modulated(_, value) => Some(value),
                InputSource::Buffer(_) => None,
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
            // Every wired parameter can report its live value.
            if let (InputSource::Buffer(_) | InputSource::Modulated(..), InputKind::Param(_)) =
                (*source, &port.kind)
            {
                taps.push((index, telemetry.open_tap(id, &port.key)));
            }
        }

        let sources = scheduled
            .inputs
            .iter()
            .map(|source| match source {
                InputSource::Buffer(b) | InputSource::Modulated(b, _) => {
                    signal_writers.get(b).copied()
                }
                InputSource::Value(_) => None,
            })
            .chain(
                scheduled
                    .event_inputs
                    .iter()
                    .map(|e| e.and_then(|e| event_writers.get(&e).copied())),
            )
            .collect();
        wiring.push((id, sources));
        for (port, &b) in scheduled.outputs.iter().enumerate() {
            signal_writers.insert(b, (id, port));
        }
        for (port, &e) in scheduled.event_outputs.iter().enumerate() {
            event_writers.insert(e, (id, port));
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
        let scratch = Scratch {
            inputs: Vec::with_capacity(inputs.len()),
            outputs: Vec::with_capacity(outputs.len()),
            event_inputs: Vec::with_capacity(event_inputs.len()),
            event_outputs: Vec::with_capacity(event_outputs.len()),
        };

        let is_output = scheduled.node_type.info().id == OUTPUT_ID;
        let mut bus = None;
        if is_output {
            let device = canonical(OUTPUT_DEVICE.get_text(&scheduled.config));
            let device = device.as_str();
            if !device.is_empty() && device_owners.get(device) != Some(&id) {
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
        nodes.push(PlanNode {
            instance,
            inputs,
            outputs,
            event_inputs,
            event_outputs,
            taps,
            is_output,
            bus,
            is_input: scheduled.node_type.info().id == INPUT_ID,
            scratch,
        });
    }
    // Forget the cells of inputs that no longer exist or are now connected.
    *cells = live_cells;

    info.audible = audible_wiring(&wiring, &nodes);
    let seamless = previous.is_some_and(|p| p.audible == info.audible)
        && info.audible.keys().all(|id| {
            // A node that was silent and still is sounds the same, though
            // it's rebuilt each time.
            carried_ids.contains(id)
                || (info.silent.contains(id) && previous.is_some_and(|p| p.silent.contains(id)))
        });

    let plan = Plan {
        generation,
        previous: previous.map(|p| p.generation),
        seamless,
        nodes,
        pool: vec![0.0; pool_len].into_boxed_slice(),
        events: (0..schedule.event_buffers)
            .map(|_| Vec::with_capacity(EVENT_CAPACITY))
            .collect(),
        values,
        offsets: offsets_in,
        migrations,
        value_migrations,
        max_frames,
    };
    (Box::new(plan), info)
}

/// The wiring of every node an Output node depends on, found by walking
/// back from the Output nodes. `wiring` and `nodes` are both by slot.
fn audible_wiring(wiring: &[(NodeId, Sources)], nodes: &[PlanNode]) -> Wiring {
    let slots: HashMap<NodeId, usize> = wiring
        .iter()
        .enumerate()
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

/// How many events one event output can produce per block.
const EVENT_CAPACITY: usize = 1024;

fn instantiate(
    scheduled: &crate::ScheduledNode,
    sample_rate: f32,
    max_frames: usize,
) -> Result<Box<dyn Node>, NodeError> {
    let setup = Setup {
        node: scheduled.id,
        config: &scheduled.config,
        sample_rate,
        max_frames,
        input_shapes: &scheduled.input_shapes,
        output_shapes: &scheduled.output_shapes,
        seed: seed_for(scheduled.id),
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
fn seed_for(id: NodeId) -> u64 {
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
    }

    /// Clears every node's internal state, for when the transport jumps.
    pub(crate) fn reset_nodes(&mut self) {
        for node in &mut self.nodes {
            if let Some(instance) = &mut node.instance {
                instance.reset();
            }
        }
    }

    /// Renders one block into `output`, interleaved with `channels` channels,
    /// with `input` feeding Input nodes. Real-time safe.
    pub(crate) fn run(
        &mut self,
        ctx: &Context,
        input: Interleaved<'_>,
        output: &mut [f32],
        channels: usize,
    ) {
        let frames = ctx.frames;
        debug_assert!(frames <= self.max_frames);
        debug_assert_eq!(output.len(), frames * channels);
        debug_assert_eq!(input.samples.len(), frames * input.channels);

        for value in &mut self.values {
            value.prepare(frames);
        }
        output.fill(0.0);

        // Views are made from raw pointers because one node's outputs and
        // another's inputs can be the same buffer at different times, which
        // safe borrows can't express.
        let pool = self.pool.as_mut_ptr();
        let events = self.events.as_mut_ptr();

        for node in &mut self.nodes {
            let mut inputs: Vec<SignalIn<'_>> = recycle(mem::take(&mut node.scratch.inputs));
            let mut outputs: Vec<SignalOut<'_>> = recycle(mem::take(&mut node.scratch.outputs));
            let mut event_inputs: Vec<&[Event]> =
                recycle(mem::take(&mut node.scratch.event_inputs));
            let mut event_outputs: Vec<EventsOut<'_>> =
                recycle(mem::take(&mut node.scratch.event_outputs));

            for input in &node.inputs {
                if let Input::Offset(k) = *input {
                    let offset = &mut self.offsets[k];
                    // SAFETY: `pool` is the plan's pool, and nothing writes
                    // the source buffer while the offset is computed.
                    unsafe { offset.compute(pool, frames, &self.values[offset.value]) };
                }
            }

            // SAFETY: every view is in bounds of the pool or event buffers,
            // which outlive this block. The compiler guarantees that a node's
            // outputs share no buffer with its inputs or each other, and only
            // this node's views exist while it runs.
            unsafe {
                inputs.extend(node.inputs.iter().map(|input| match *input {
                    Input::Buffer(view) => view.read(pool, frames),
                    Input::Value(slot) => self.values[slot].signal(frames),
                    Input::Offset(k) => self.offsets[k].signal(frames),
                }));
                outputs.extend(node.outputs.iter().map(|view| view.write(pool, frames)));
                for (index, tap) in &node.taps {
                    if tap.wanted() {
                        report(tap, &inputs[*index]);
                    }
                }
                event_inputs.extend(node.event_inputs.iter().map(|buffer| match buffer {
                    Some(b) => (*events.add(*b)).as_slice(),
                    None => &[],
                }));
                event_outputs.extend(
                    node.event_outputs
                        .iter()
                        .map(|&b| EventsOut::new(&mut *events.add(b))),
                );
            }

            let io = Io {
                inputs: &inputs,
                outputs: &mut outputs,
                event_inputs: &event_inputs,
                event_outputs: &mut event_outputs,
            };
            match &mut node.instance {
                Some(instance) => instance.process(ctx, io),
                // Only possible if plans arrived out of order.
                None => Silence.process(ctx, io),
            }
            if node.is_input {
                read_input(input, &mut outputs[0]);
            }
            // Mixed now, not after the whole schedule: the compiler frees a
            // buffer after its last reader, so a later node may reuse it.
            if let Some((first, count)) = node.bus {
                mix_into(inputs[0], output, channels, first, count);
            }

            node.scratch.inputs = recycle(inputs);
            node.scratch.outputs = recycle(outputs);
            node.scratch.event_inputs = recycle(event_inputs);
            node.scratch.event_outputs = recycle(event_outputs);
        }
    }
}

impl View {
    /// SAFETY: `pool` must be the plan's pool, and nothing may write this
    /// buffer while the view lives.
    unsafe fn read<'a>(self, pool: *const f32, frames: usize) -> SignalIn<'a> {
        let len = self.shape.lanes() * frames;
        let data = unsafe { std::slice::from_raw_parts(pool.add(self.offset), len) };
        SignalIn::new(data, self.shape, frames)
    }

    /// SAFETY: `pool` must be the plan's pool, and nothing else may read or
    /// write this buffer while the view lives.
    unsafe fn write<'a>(self, pool: *mut f32, frames: usize) -> SignalOut<'a> {
        let len = self.shape.lanes() * frames;
        let data = unsafe { std::slice::from_raw_parts_mut(pool.add(self.offset), len) };
        SignalOut::new(data, self.shape, frames)
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
/// at `first`. A mono signal goes to every channel; otherwise channel n goes
/// to channel n, and voices are summed.
fn mix_into(signal: SignalIn<'_>, output: &mut [f32], channels: usize, first: usize, count: usize) {
    let shape = signal.shape();
    for channel in 0..count {
        let source = match shape.channels {
            1 => 0,
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
}

impl OffsetInput {
    fn new(value: usize, source: View, info: ParamInfo, max_frames: usize) -> Self {
        Self {
            value,
            source,
            info,
            scratch: vec![0.0; source.shape.lanes() * max_frames].into_boxed_slice(),
        }
    }

    /// SAFETY: `pool` must be the plan's pool, and nothing may write the
    /// source buffer meanwhile.
    unsafe fn compute(&mut self, pool: *const f32, frames: usize, base: &ValueInput) {
        // SAFETY: as promised by the caller.
        let source = unsafe { self.source.read(pool, frames) };
        let shape = source.shape();
        let info = &self.info;
        let constant = base.constant().map(|value| info.position(value));
        let base = base.samples(frames);
        for voice in 0..shape.voices {
            for channel in 0..shape.channels {
                let start = (voice * shape.channels + channel) * frames;
                let out = &mut self.scratch[start..start + frames];
                let signal = source.lane(voice, channel);
                for (i, (out, &x)) in out.iter_mut().zip(signal).enumerate() {
                    // A non-finite signal must not poison the node's state.
                    let x = if x.is_finite() { x } else { 0.0 };
                    let position = constant.unwrap_or_else(|| info.position(base[i]));
                    *out = info.value_at(position + x);
                }
            }
        }
    }

    fn signal(&self, frames: usize) -> SignalIn<'_> {
        let shape = self.source.shape;
        SignalIn::new(&self.scratch[..shape.lanes() * frames], shape, frames)
    }
}

/// Reports the range of a wired parameter's value to its tap: across all
/// lanes, and the last sample of the first.
fn report(tap: &TapWriter, signal: &SignalIn<'_>) {
    let shape = signal.shape();
    let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
    for voice in 0..shape.voices {
        for channel in 0..shape.channels {
            for &x in signal.lane(voice, channel) {
                if x.is_finite() {
                    min = min.min(x);
                    max = max.max(x);
                }
            }
        }
    }
    let last = signal.lane(0, 0).last().copied().unwrap_or(0.0);
    if min <= max && last.is_finite() {
        tap.write(min, max, last);
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
