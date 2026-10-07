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

use crate::builtin::OUTPUT_ID;
use crate::{
    Context, Diagnostic, Event, EventsOut, InputKind, InputSource, Instance, Io, Node, NodeError,
    ParamKind, Problem, Schedule, Setup, Shape, SignalIn, SignalOut,
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
    /// An Output node, whose input is mixed into the device output.
    is_output: bool,
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

/// Builds a plan from a schedule, instantiating every node that can't carry
/// over from `previous`. Nodes that fail to instantiate are kept, as silence,
/// so the buffers downstream of them stay valid.
pub(crate) fn build(
    schedule: Schedule,
    previous: Option<&PlanInfo>,
    generation: u64,
    sample_rate: f32,
    max_frames: usize,
    cells: &mut Cells,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Box<Plan>, PlanInfo) {
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
                Ok(node) => (Some(node), true),
                Err(error) => {
                    info.silent.insert(id);
                    diagnostics.push(Diagnostic::node(id, Problem::Node(error)));
                    (Some(Box::new(Silence) as Box<dyn Node>), false)
                }
            },
        };

        let mut inputs = Vec::with_capacity(scheduled.inputs.len());
        for ((port, source), shape) in scheduled
            .layout
            .inputs
            .iter()
            .zip(&scheduled.inputs)
            .zip(&scheduled.input_shapes)
        {
            inputs.push(match *source {
                InputSource::Buffer(b) => Input::Buffer(view(b, *shape)),
                InputSource::Value(value) => {
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
                    Input::Value(value_slot)
                }
            });
        }

        let sources = scheduled
            .inputs
            .iter()
            .map(|source| match source {
                InputSource::Buffer(b) => signal_writers.get(b).copied(),
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

        if carries_over {
            info.nodes.insert(id, (key, slot));
        }
        nodes.push(PlanNode {
            instance,
            inputs,
            outputs,
            event_inputs,
            event_outputs,
            is_output: scheduled.node_type.info().id == OUTPUT_ID,
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

    /// Renders one block into `output`, interleaved with `channels` channels.
    /// Real-time safe.
    pub(crate) fn run(&mut self, ctx: &Context, output: &mut [f32], channels: usize) {
        let frames = ctx.frames;
        debug_assert!(frames <= self.max_frames);
        debug_assert_eq!(output.len(), frames * channels);

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

            // SAFETY: every view is in bounds of the pool or event buffers,
            // which outlive this block. The compiler guarantees that a node's
            // outputs share no buffer with its inputs or each other, and only
            // this node's views exist while it runs.
            unsafe {
                inputs.extend(node.inputs.iter().map(|input| match *input {
                    Input::Buffer(view) => view.read(pool, frames),
                    Input::Value(slot) => self.values[slot].signal(frames),
                }));
                outputs.extend(node.outputs.iter().map(|view| view.write(pool, frames)));
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
            // Mixed now, not after the whole schedule: the compiler frees a
            // buffer after its last reader, so a later node may reuse it.
            if node.is_output {
                mix_into(inputs[0], output, channels);
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

/// Adds a signal into interleaved output. A mono signal goes to every
/// channel; otherwise channel n goes to channel n, and voices are summed.
fn mix_into(signal: SignalIn<'_>, output: &mut [f32], channels: usize) {
    let shape = signal.shape();
    for channel in 0..channels {
        let source = match shape.channels {
            1 => 0,
            n if channel < n => channel,
            _ => continue,
        };
        for voice in 0..shape.voices {
            let lane = signal.lane(voice, source);
            for (frame, sample) in lane.iter().enumerate() {
                output[frame * channels + channel] += sample;
            }
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
        if target != self.target && !target.is_nan() {
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
