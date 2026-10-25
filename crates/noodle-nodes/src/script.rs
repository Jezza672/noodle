//! The Script node: a small program run for every sample, with ports,
//! parameters and memory of its own.

use noodle_engine::{
    Config, ConfigInfo, Context, Event, EventKind, EventsOut, Instance, Lane, LaneInputs,
    LaneKernel, LaneOutputs, Layout, NodeError, NodeInfo, NodeType, NoteId, ParamInfo, PerLane,
    Setup, Shape, Unit,
};

use crate::math::{scan_number, unary_function};

/// Runs the program in its `script` setting once per sample, per voice and
/// channel. The language is small on purpose: numbers, a few dozen maths
/// functions, variables that last between samples, arrays, `if` and loops
/// whose length is fixed in the text. A script always finishes in a bounded
/// time, so it is safe to run on the audio thread.
///
/// ```text
/// // A one-pole low-pass filter with its cutoff as a parameter.
/// input x
/// param cutoff = 1000 [20, 20000] log hz
/// output y
/// state z
/// let k = 1 - exp(-tau * cutoff / sr)
/// z += (x - z) * k
/// y = z
/// ```
///
/// - **Declarations** (at the top level) make the node's ports, in order:
///   `input x` is an audio input, `param p = 0.5 [0, 1]` a parameter (default
///   and range optional, the range being 0 to 1 unless given; flags `log`, `hz`, `db`, `s`), `output y` an output.
///   `state z = 0` is a variable that keeps its value from one sample to the
///   next (and starts at the value given, or 0).
/// - **Statements:** `let k = expr` makes a variable for this sample, `y =
///   expr` (also `+=`, `-=`, `*=`, `/=`) sets an output, state or `let`
///   variable, and `if cond { ... } else { ... }` chooses. A statement ends
///   at a new line or a `;`. `//` starts a comment. An output not set in a
///   sample is 0.
/// - **Arrays:** `array buf[480]` (or `array buf[8] = 0.5` to fill it) is
///   memory that lasts between samples, one copy per voice and channel, with
///   at most [`ARRAY_TOTAL`] values in all (keep a head in `state` wrapped, not
///   counting up for ever: an `f32` stops counting at 16 million). `buf[i]` reads and writes it (also
///   `+=` and the like); the index is rounded down and wraps round the array,
///   so a write head that wraps (`head = (head + 1) % len(buf)`)
///   makes a delay line. `len(buf)` is
///   the length.
/// - **Loops:** `for i in 0 to 8 { ... }` runs the block with `i` from 0 to 7.
///   The ends are whole numbers or `len(buf)`, so the number of steps is known
///   when the script is read: a loop runs at most 256 times, and loops inside
///   loops at most 1024 steps all told, and a script at most 32768 ops a
///   sample counting every pass. `i` can't be set.
/// - **Events:** `events in notes` is an event input and `events out played`
///   an event output. An input gives the variables `notes_on` and
///   `notes_off` (1 in the sample a note starts or ends, else 0),
///   `notes_key` and `notes_velocity` (those of the latest note event). One
///   event arrives per sample, so events at the same instant arrive on
///   consecutive samples, in order. `note_on(played, key, velocity)` and
///   `note_off(played, key)` send notes (a note's identity is its key). Events
///   have no voices, so only the first voice and channel's lane sends.
/// - **Truth:** any non-zero value is true, and so is NaN.
/// - **Expressions** are as in the Math node, plus comparisons `< <= > >= ==
///   !=` (giving 1 or 0), `&&`, `||` and `!` (any non-zero value is true).
/// - **Names:** `sr` (sample rate), `bpm`, `beat` (position in quarter
///   notes, still while the transport is stopped), `time` (seconds since the
///   node started; an `f32`, so coarse after a few minutes: keep a phase in
///   `state` rather than computing `sin(time * freq)`), `pi`, `tau`, `e`, and `rand()` (a noise sample between
///   -1 and 1; the sequence is the same every run, and every voice and channel has its own).
/// - **Functions:** those of Math, plus `db(x)` (decibels to gain), `mtof(n)`
///   (MIDI note to Hz) and `select(cond, a, b)`.
///
/// The script is config, since it decides the node's ports. It is parsed into
/// a short program when the node is built, off the audio thread; a mistake
/// fails the node with the line and what is wrong. Each sample runs that
/// program on a small stack with nothing allocated. Inputs broadcast, so a
/// mono input can drive every voice of a polyphonic one, and each voice and
/// channel has its own state.
pub struct Script;

pub const SCRIPT_ID: &str = "noodle.util.script";
/// The config key holding the script's text.
pub const SCRIPT_KEY: &str = "script";

/// What the node runs until one is set: a wire.
pub const DEFAULT_SCRIPT: &str = "input in\noutput out\nout = in\n";

static CONFIG: [ConfigInfo; 1] = [ConfigInfo::text(SCRIPT_KEY, "Script")];

static INFO: NodeInfo = NodeInfo {
    id: SCRIPT_ID,
    version: 1,
    name: "Script",
    category: "Utilities",
};

/// The most variables of all kinds a script can have.
const MAX_VARS: usize = 128;
/// The most input ports (audio and parameters together).
pub const MAX_INPUTS: usize = 16;
/// The most outputs.
pub const MAX_OUTPUTS: usize = 8;
/// The most operations in a compiled script.
const MAX_OPS: usize = 4096;
/// The deepest the evaluation stack can get, so it can live on the stack.
const DEPTH: usize = 32;
/// The most values the arrays of one lane hold altogether.
pub const ARRAY_TOTAL: usize = 8192;
/// The most arrays.
const MAX_ARRAYS: usize = 16;
/// The most event inputs, and event outputs.
pub const MAX_EVENT_PORTS: usize = 4;
/// The most times one loop runs.
const MAX_TRIPS: usize = 256;
/// The most steps of a loop and the loops inside it.
const MAX_STEPS: usize = 1024;
/// The most ops a sample may run, counting every pass of every loop.
const MAX_WORK: usize = 32_768;
/// Events waiting for a sample to arrive on, per event input.
const QUEUE: usize = 64;
/// The most note-ons remembered, to give a note-off its key.
const REMEMBERED: usize = 32;
/// The deepest the parser recurses (brackets and nested `if`s), so a hostile
/// text can't overflow the stack.
const NESTING: usize = 48;

/// Checks a script without building a node: `Ok` with the ports it makes, or
/// the message that the node would fail with. The properties panel shows this
/// while the text is being edited.
pub fn check(text: &str) -> Result<ScriptPorts, String> {
    let program = Program::compile(effective(text))?;
    Ok(ScriptPorts {
        inputs: program.inputs.iter().map(|i| i.key.clone()).collect(),
        outputs: program.outputs.clone(),
        event_inputs: program.event_in.iter().map(|e| e.name.clone()).collect(),
        event_outputs: program.event_out.clone(),
    })
}

/// The ports a script makes, by key, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptPorts {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub event_inputs: Vec<String>,
    pub event_outputs: Vec<String>,
}

fn effective(text: &str) -> &str {
    if text.trim().is_empty() {
        DEFAULT_SCRIPT
    } else {
        text
    }
}

impl Script {
    fn program(config: &Config) -> Result<Program, NodeError> {
        let text = CONFIG[0].get_text(config);
        Program::compile(effective(&text)).map_err(|e| NodeError::config(format!("Script: {e}")))
    }
}

impl NodeType for Script {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let program = Self::program(config)?;
        let mut layout = Layout::realtime();
        for input in &program.inputs {
            let name = title(&input.key);
            layout = match &input.param {
                None => layout.input(input.key.clone(), name),
                Some(p) => {
                    let mut info = ParamInfo::new(p.min, p.max, p.default).unit(p.unit);
                    if p.log {
                        info = info.log();
                    }
                    layout.param(input.key.clone(), name, info)
                }
            };
        }
        for output in &program.outputs {
            layout = layout.output(output.clone(), title(output));
        }
        for event in &program.event_in {
            layout = layout.event_input(event.name.clone(), title(&event.name));
        }
        for event in &program.event_out {
            layout = layout.event_output(event.clone(), title(event));
        }
        Ok(layout)
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let program = Self::program(setup.config)?;
        let shape = setup.output_shapes.first().copied().unwrap_or(Shape::MONO);
        let per_lane = program.array_len();
        let feeds = program
            .event_in
            .iter()
            .map(|_| Feed::new(setup.max_frames))
            .collect();
        Ok(Instance::realtime(PerLane::new(
            ScriptKernel {
                program,
                feeds,
                memory: vec![0.0; per_lane * shape.lanes()],
                per_lane,
                channels: shape.channels,
            },
            setup,
        )))
    }
}

/// `cutoff` as `Cutoff`, for port names.
fn title(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

struct ScriptKernel {
    program: Program,
    /// What each event input delivers, sample by sample, in this block.
    feeds: Vec<Feed>,
    /// The arrays of every lane, `per_lane` values each, lane by lane (voice
    /// by voice, channels within). Sized when the node is built, so a script
    /// without arrays holds none.
    memory: Vec<f32>,
    per_lane: usize,
    channels: usize,
}

/// An event waiting to arrive, or arrived: `kind` is 0 for none, 1 for a
/// note-on and 2 for a note-off.
#[derive(Clone, Copy, Default)]
struct Queued {
    kind: u8,
    /// Whether `key` is known: a note-off whose note-on has been forgotten
    /// leaves the key alone.
    has_key: bool,
    key: f32,
    velocity: f32,
    time: u32,
}

/// One event input's events, handed out one to a sample so that a chord
/// arrives as consecutive samples and none is lost. All lanes read the same
/// schedule. Nothing here allocates after the node is built.
struct Feed {
    queue: [Queued; QUEUE],
    head: usize,
    len: usize,
    /// The event arriving in each sample of the block.
    delivered: Vec<Queued>,
    /// (note id, key) of the latest note-ons, for the keys of note-offs.
    notes: [Option<(u32, u8)>; REMEMBERED],
    next_note: usize,
}

impl Feed {
    fn new(max_frames: usize) -> Self {
        Self {
            queue: [Queued::default(); QUEUE],
            head: 0,
            len: 0,
            delivered: vec![Queued::default(); max_frames],
            notes: [None; REMEMBERED],
            next_note: 0,
        }
    }

    /// Forgets the queued events and the notes remembered.
    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.notes = [None; REMEMBERED];
        self.next_note = 0;
    }

    fn fill(&mut self, frames: usize, events: &[Event]) {
        let frames = frames.min(self.delivered.len());
        self.delivered[..frames].fill(Queued::default());
        for event in events {
            let queued = match event.kind {
                EventKind::NoteOn {
                    note,
                    key,
                    velocity,
                    ..
                } => {
                    self.notes[self.next_note] = Some((note.0, key));
                    self.next_note = (self.next_note + 1) % REMEMBERED;
                    Queued {
                        kind: 1,
                        has_key: true,
                        key: f32::from(key),
                        velocity,
                        time: event.time,
                    }
                }
                EventKind::NoteOff { note, velocity } => {
                    let key = self.notes.iter().flatten().find(|n| n.0 == note.0);
                    Queued {
                        kind: 2,
                        has_key: key.is_some(),
                        key: key.map_or(0.0, |n| f32::from(n.1)),
                        velocity,
                        time: event.time,
                    }
                }
                _ => continue,
            };
            if self.len < QUEUE {
                self.queue[(self.head + self.len) % QUEUE] = queued;
                self.len += 1;
            }
        }
        for frame in 0..frames {
            if self.len > 0 && self.queue[self.head].time as usize <= frame {
                self.delivered[frame] = self.queue[self.head];
                self.head = (self.head + 1) % QUEUE;
                self.len -= 1;
            }
        }
        // What didn't fit in this block arrives at the start of the next.
        for k in 0..self.len {
            self.queue[(self.head + k) % QUEUE].time = 0;
        }
    }
}

/// One lane's memory: every variable (the state ones are the only ones read
/// before they are written), the sample count and the noise generator.
struct ScriptState {
    vars: [f32; MAX_VARS],
    /// False until the `state` variables have their starting values.
    started: bool,
    samples: u64,
    rng: u32,
}

impl Default for ScriptState {
    fn default() -> Self {
        Self {
            vars: [0.0; MAX_VARS],
            started: false,
            samples: 0,
            rng: 0x9E37_79B9,
        }
    }
}

impl LaneKernel for ScriptKernel {
    type State = ScriptState;

    fn reset(&mut self) {
        self.memory.fill(0.0);
        for feed in &mut self.feeds {
            feed.clear();
        }
    }

    fn begin_block(&mut self, ctx: &Context, events: &[&[Event]]) {
        for (feed, events) in self.feeds.iter_mut().zip(events) {
            feed.fill(ctx.frames, events);
        }
    }

    fn process_lane(&mut self, state: &mut ScriptState, ctx: &Context, lane: Lane<'_, '_>) {
        let program = &self.program;
        let at = (lane.voice * self.channels + lane.channel) * self.per_lane;
        let array = &mut self.memory[at..at + self.per_lane];
        if !state.started {
            for &(slot, value) in &program.starts {
                state.vars[slot] = value;
            }
            for decl in &program.arrays {
                array[decl.offset..decl.offset + decl.len].fill(decl.fill);
            }
            state.started = true;
            // Each lane gets its own noise: mix the lane into the seed.
            let lane = (lane.voice * 64 + lane.channel) as u32;
            state.rng ^= lane.wrapping_mul(0x85EB_CA6B).rotate_left(13) | 1;
            if state.rng == 0 {
                state.rng = 1;
            }
        }
        let Lane {
            voice,
            channel,
            inputs,
            mut outputs,
            event_outputs,
        } = lane;
        // Events have no voices: the first lane speaks for the node.
        let sink = if voice == 0 && channel == 0 {
            event_outputs
        } else {
            &mut []
        };
        let feeds = &self.feeds;
        let (inputs, outputs) = (&inputs, &mut outputs);
        match program.outputs.len() {
            0 => run_lane::<0>(program, feeds, array, state, ctx, inputs, outputs, sink),
            1 => run_lane::<1>(program, feeds, array, state, ctx, inputs, outputs, sink),
            2 => run_lane::<2>(program, feeds, array, state, ctx, inputs, outputs, sink),
            3 => run_lane::<3>(program, feeds, array, state, ctx, inputs, outputs, sink),
            4 => run_lane::<4>(program, feeds, array, state, ctx, inputs, outputs, sink),
            5 => run_lane::<5>(program, feeds, array, state, ctx, inputs, outputs, sink),
            6 => run_lane::<6>(program, feeds, array, state, ctx, inputs, outputs, sink),
            7 => run_lane::<7>(program, feeds, array, state, ctx, inputs, outputs, sink),
            _ => run_lane::<8>(program, feeds, array, state, ctx, inputs, outputs, sink),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_lane<const N: usize>(
    program: &Program,
    feeds: &[Feed],
    array: &mut [f32],
    state: &mut ScriptState,
    ctx: &Context,
    inputs: &LaneInputs<'_>,
    outputs: &mut LaneOutputs<'_, '_>,
    event_outputs: &mut [EventsOut<'_>],
) {
    let mut ins: [&[f32]; MAX_INPUTS] = [&[]; MAX_INPUTS];
    for (k, slice) in ins.iter_mut().enumerate().take(program.inputs.len()) {
        *slice = inputs.get(k);
    }
    let mut outs = outputs.get_disjoint_mut(std::array::from_fn::<usize, N, _>(|i| i));
    let frames = outs.iter().map(|o| o.len()).min().unwrap_or(ctx.frames);

    let rate = f64::from(ctx.sample_rate);
    let bpm = ctx.transport.bpm;
    let beat0 = ctx.transport.tick / noodle_core::TICKS_PER_QUARTER as f64;
    let beat_step = if ctx.transport.playing {
        bpm / 60.0 / rate
    } else {
        0.0
    };
    let mut env = Env {
        sr: ctx.sample_rate,
        bpm: bpm as f32,
        beat: 0.0,
        time: 0.0,
    };
    for i in 0..frames {
        for (k, &slot) in program.input_slots.iter().enumerate() {
            state.vars[slot] = ins[k][i];
        }
        for &slot in &program.output_slots {
            state.vars[slot] = 0.0;
        }
        for (decl, feed) in program.event_in.iter().zip(feeds) {
            let arrived = feed.delivered.get(i).copied().unwrap_or_default();
            state.vars[decl.slots[0]] = f32::from(arrived.kind == 1);
            state.vars[decl.slots[1]] = f32::from(arrived.kind == 2);
            if arrived.kind != 0 {
                if arrived.has_key {
                    state.vars[decl.slots[2]] = arrived.key;
                }
                state.vars[decl.slots[3]] = arrived.velocity;
            }
        }
        env.beat = (beat0 + i as f64 * beat_step) as f32;
        env.time = ((state.samples + i as u64) as f64 / rate) as f32;
        let mut sink = Sink {
            outs: &mut *event_outputs,
            frame: i as u32,
        };
        program.run(&mut state.vars, array, &env, &mut state.rng, &mut sink);
        for (out, &slot) in outs.iter_mut().zip(&program.output_slots) {
            out[i] = state.vars[slot];
        }
    }
    state.samples += frames as u64;
}

/// Where a script's events go, and in which sample.
struct Sink<'a, 'b> {
    outs: &'a mut [EventsOut<'b>],
    frame: u32,
}

impl Sink<'_, '_> {
    fn send(&mut self, port: u8, kind: EventKind) {
        if let Some(out) = self.outs.get_mut(usize::from(port)) {
            // A full buffer drops the event.
            let _ = out.push(Event {
                time: self.frame,
                kind,
            });
        }
    }
}

/// What a sample can read besides its inputs and variables.
struct Env {
    sr: f32,
    bpm: f32,
    beat: f32,
    time: f32,
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Push(f32),
    Load(u8),
    Store(u8),
    Sr,
    Bpm,
    Beat,
    Time,
    Rand,
    /// Copies the top of the stack.
    Dup,
    /// Replaces an index with the array's value there.
    LoadIdx(u8),
    /// Pops a value and an index and stores the value in the array.
    StoreIdx(u8),
    /// Pops a velocity and a key and sends a note-on.
    EmitOn(u8),
    /// Pops a key and sends a note-off.
    EmitOff(u8),
    Neg,
    Not,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Min,
    Max,
    Atan2,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Clamp,
    Mix,
    Select,
    Unary(fn(f32) -> f32),
    /// Pops a value and jumps to the op index if it is 0 (or NaN).
    JumpIfZero(u16),
    Jump(u16),
}

#[derive(Debug)]
struct ArrayDecl {
    name: String,
    offset: usize,
    len: usize,
    fill: f32,
}

impl ArrayDecl {
    /// Where `index` (rounded down, wrapping round) is in the lane's memory.
    fn at(&self, index: f32) -> usize {
        self.offset + (index.floor() as i64).rem_euclid(self.len as i64) as usize
    }
}

#[derive(Debug)]
struct LoopInfo {
    start: usize,
    end: usize,
    trips: usize,
}

#[derive(Debug)]
struct EventInDecl {
    name: String,
    /// The variables `_on`, `_off`, `_key` and `_velocity`.
    slots: [usize; 4],
}

#[derive(Debug)]
struct ParamDecl {
    min: f32,
    max: f32,
    default: f32,
    log: bool,
    unit: Unit,
}

#[derive(Debug)]
struct InputDecl {
    key: String,
    /// `None` for an audio input.
    param: Option<ParamDecl>,
}

/// A compiled script.
struct Program {
    ops: Vec<Op>,
    inputs: Vec<InputDecl>,
    outputs: Vec<String>,
    /// The variable slot each input and output uses.
    input_slots: Vec<usize>,
    output_slots: Vec<usize>,
    /// Starting values of the `state` variables.
    starts: Vec<(usize, f32)>,
    arrays: Vec<ArrayDecl>,
    /// Every `for` loop: the ops it spans and how many times it runs.
    loops: Vec<LoopInfo>,
    event_in: Vec<EventInDecl>,
    event_out: Vec<String>,
}

impl Program {
    fn compile(text: &str) -> Result<Self, String> {
        let mut parser = Parser::new(lex(text)?);
        parser.program()?;
        if parser.program.outputs.is_empty() && parser.program.event_out.is_empty() {
            return Err("a script needs an output, such as `output out`".into());
        }
        let program = parser.program;
        if program.max_depth() > DEPTH {
            return Err("an expression is too deeply nested".into());
        }
        let work = program.work();
        if work > MAX_WORK {
            return Err(format!(
                "the loops do too much work: about {work} steps a sample, at most {MAX_WORK}"
            ));
        }
        Ok(program)
    }

    /// The values the arrays hold in all.
    fn array_len(&self) -> usize {
        self.arrays.last().map_or(0, |a| a.offset + a.len)
    }

    /// The most ops a sample can run: every op counted once for each time the
    /// loops around it run.
    fn work(&self) -> usize {
        (0..self.ops.len())
            .map(|i| {
                self.loops
                    .iter()
                    .filter(|l| l.start <= i && i <= l.end)
                    .fold(1usize, |work, l| work.saturating_mul(l.trips.max(1)))
            })
            .fold(0usize, usize::saturating_add)
    }

    /// The most values the program holds on its stack at once.
    fn max_depth(&self) -> usize {
        let (mut depth, mut max) = (0usize, 0usize);
        for op in &self.ops {
            match op {
                Op::Push(_)
                | Op::Load(_)
                | Op::Sr
                | Op::Bpm
                | Op::Beat
                | Op::Time
                | Op::Rand
                | Op::Dup => depth += 1,
                Op::Neg | Op::Not | Op::Unary(_) | Op::Jump(_) | Op::LoadIdx(_) => {}
                Op::Clamp | Op::Mix | Op::Select | Op::StoreIdx(_) | Op::EmitOn(_) => depth -= 2,
                _ => depth -= 1,
            }
            max = max.max(depth);
        }
        max
    }

    fn run(
        &self,
        vars: &mut [f32; MAX_VARS],
        array: &mut [f32],
        env: &Env,
        rng: &mut u32,
        sink: &mut Sink<'_, '_>,
    ) {
        let mut stack = [0.0f32; DEPTH];
        let mut top = 0;
        let mut pc = 0;
        while let Some(&op) = self.ops.get(pc) {
            pc += 1;
            match op {
                Op::Push(x) => {
                    stack[top] = x;
                    top += 1;
                }
                Op::Load(slot) => {
                    stack[top] = vars[usize::from(slot)];
                    top += 1;
                }
                Op::Store(slot) => {
                    top -= 1;
                    vars[usize::from(slot)] = stack[top];
                }
                Op::Sr => {
                    stack[top] = env.sr;
                    top += 1;
                }
                Op::Bpm => {
                    stack[top] = env.bpm;
                    top += 1;
                }
                Op::Beat => {
                    stack[top] = env.beat;
                    top += 1;
                }
                Op::Time => {
                    stack[top] = env.time;
                    top += 1;
                }
                Op::Rand => {
                    // xorshift32, mapped to [-1, 1).
                    let mut x = *rng;
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    *rng = x;
                    stack[top] = (x >> 8) as f32 / 8_388_608.0 - 1.0;
                    top += 1;
                }
                Op::Dup => {
                    stack[top] = stack[top - 1];
                    top += 1;
                }
                Op::LoadIdx(a) => {
                    stack[top - 1] = array[self.arrays[usize::from(a)].at(stack[top - 1])];
                }
                Op::StoreIdx(a) => {
                    let (index, value) = (stack[top - 2], stack[top - 1]);
                    top -= 2;
                    array[self.arrays[usize::from(a)].at(index)] = value;
                }
                Op::EmitOn(port) => {
                    let (key, velocity) = (stack[top - 2], stack[top - 1]);
                    top -= 2;
                    let key = key.clamp(0.0, 127.0) as u8;
                    sink.send(
                        port,
                        EventKind::NoteOn {
                            note: NoteId(u32::from(key)),
                            channel: 0,
                            key,
                            velocity: velocity.clamp(0.0, 1.0),
                        },
                    );
                }
                Op::EmitOff(port) => {
                    top -= 1;
                    let key = stack[top].clamp(0.0, 127.0) as u8;
                    sink.send(
                        port,
                        EventKind::NoteOff {
                            note: NoteId(u32::from(key)),
                            velocity: 0.0,
                        },
                    );
                }
                Op::Neg => stack[top - 1] = -stack[top - 1],
                Op::Not => stack[top - 1] = f32::from(stack[top - 1] == 0.0),
                Op::Unary(f) => stack[top - 1] = f(stack[top - 1]),
                Op::Clamp => {
                    let (lo, hi) = (stack[top - 2], stack[top - 1]);
                    top -= 2;
                    // Like f32::clamp, without its panic on lo > hi or NaN.
                    stack[top - 1] = stack[top - 1].max(lo).min(hi);
                }
                Op::Mix => {
                    let (b, t) = (stack[top - 2], stack[top - 1]);
                    top -= 2;
                    let a = stack[top - 1];
                    stack[top - 1] = a + (b - a) * t;
                }
                Op::Select => {
                    let (a, b) = (stack[top - 2], stack[top - 1]);
                    top -= 2;
                    stack[top - 1] = if stack[top - 1] != 0.0 { a } else { b };
                }
                Op::JumpIfZero(target) => {
                    top -= 1;
                    // Zero is false; NaN, like everywhere else here, is true.
                    if stack[top] == 0.0 {
                        pc = usize::from(target);
                    }
                }
                Op::Jump(target) => pc = usize::from(target),
                binary => {
                    let (x, y) = (stack[top - 2], stack[top - 1]);
                    top -= 1;
                    stack[top - 1] = match binary {
                        Op::Add => x + y,
                        Op::Sub => x - y,
                        Op::Mul => x * y,
                        Op::Div => x / y,
                        Op::Rem => x % y,
                        Op::Pow => x.powf(y),
                        Op::Min => x.min(y),
                        Op::Max => x.max(y),
                        Op::Atan2 => x.atan2(y),
                        Op::Lt => f32::from(x < y),
                        Op::Le => f32::from(x <= y),
                        Op::Gt => f32::from(x > y),
                        Op::Ge => f32::from(x >= y),
                        Op::Eq => f32::from(x == y),
                        Op::Ne => f32::from(x != y),
                        Op::And => f32::from(x != 0.0 && y != 0.0),
                        Op::Or => f32::from(x != 0.0 || y != 0.0),
                        _ => unreachable!("handled above"),
                    };
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Number(f32),
    Name(String),
    Symbol(&'static str),
    /// A new line or `;`.
    Separator,
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::Number(n) => format!("number {n}"),
            Tok::Name(name) => format!("`{name}`"),
            Tok::Symbol(s) => format!("`{s}`"),
            Tok::Separator => "the end of the line".into(),
        }
    }
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    line: usize,
}

const SYMBOLS: [&str; 26] = [
    "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "+", "-", "*", "/", "%", "^", "(",
    ")", ",", "{", "}", "=", "<", ">", "!", "[",
];

fn lex(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let (mut i, mut line, mut brackets) = (0, 1, 0usize);
    while i < chars.len() {
        let c = chars[i];
        let mut push = |tok: Tok| tokens.push(Token { tok, line });
        if c == '\n' {
            // A line break inside brackets doesn't end the statement.
            if brackets == 0 {
                push(Tok::Separator);
            }
            line += 1;
            i += 1;
        } else if c == ';' {
            push(Tok::Separator);
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c.is_ascii_digit() || c == '.' {
            let number = scan_number(&chars, &mut i).map_err(|e| format!("line {line}: {e}"))?;
            push(Tok::Number(number));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            push(Tok::Name(name.to_ascii_lowercase()));
        } else if c == ']' {
            brackets = brackets.saturating_sub(1);
            push(Tok::Symbol("]"));
            i += 1;
        } else {
            let rest: String = chars[i..chars.len().min(i + 2)].iter().collect();
            let Some(symbol) = SYMBOLS.iter().find(|s| rest.starts_with(**s)) else {
                return Err(format!("line {line}: unexpected `{c}`"));
            };
            match *symbol {
                "(" | "[" => brackets += 1,
                ")" => brackets = brackets.saturating_sub(1),
                _ => {}
            }
            push(Tok::Symbol(symbol));
            i += symbol.len();
        }
    }
    Ok(tokens)
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Read-only: inputs and parameters.
    Input,
    Writable,
}

struct Var {
    name: String,
    slot: u8,
    kind: Kind,
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    program: Program,
    /// Variables in sight, innermost last.
    vars: Vec<Var>,
    next_slot: usize,
    depth: usize,
    /// How many times the loops around the statement being read run in all.
    steps: usize,
    /// Values in the arrays so far.
    array_total: usize,
}

/// (binding power, right-associative, op) of a binary operator.
fn binary(symbol: &str) -> Option<(u8, bool, Op)> {
    Some(match symbol {
        "||" => (1, false, Op::Or),
        "&&" => (2, false, Op::And),
        "==" => (3, false, Op::Eq),
        "!=" => (3, false, Op::Ne),
        "<" => (3, false, Op::Lt),
        "<=" => (3, false, Op::Le),
        ">" => (3, false, Op::Gt),
        ">=" => (3, false, Op::Ge),
        "+" => (4, false, Op::Add),
        "-" => (4, false, Op::Sub),
        "*" => (5, false, Op::Mul),
        "/" => (5, false, Op::Div),
        "%" => (5, false, Op::Rem),
        "^" => (7, true, Op::Pow),
        _ => return None,
    })
}

/// How tightly unary minus and `!` bind: looser than `^`, so `-a^2` is
/// `-(a^2)`.
const PREFIX: u8 = 6;

const KEYWORDS: [&str; 13] = [
    "input", "param", "output", "state", "let", "if", "else", "rand", "array", "events", "for",
    "note_on", "note_off",
];
const CONSTANTS: [&str; 7] = ["sr", "bpm", "beat", "time", "pi", "tau", "e"];
const FUNCTIONS: [&str; 10] = [
    "min", "max", "pow", "atan2", "clamp", "mix", "select", "db", "mtof", "len",
];

fn reserved(name: &str) -> bool {
    KEYWORDS.contains(&name)
        || CONSTANTS.contains(&name)
        || FUNCTIONS.contains(&name)
        || unary_function(name).is_some()
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            at: 0,
            program: Program {
                ops: Vec::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                input_slots: Vec::new(),
                output_slots: Vec::new(),
                starts: Vec::new(),
                arrays: Vec::new(),
                loops: Vec::new(),
                event_in: Vec::new(),
                event_out: Vec::new(),
            },
            vars: Vec::new(),
            next_slot: 0,
            depth: 0,
            steps: 1,
            array_total: 0,
        }
    }

    fn line(&self) -> usize {
        self.tokens
            .get(self.at)
            .or(self.tokens.last())
            .map_or(1, |t| t.line)
    }

    fn err<T>(&self, message: impl std::fmt::Display) -> Result<T, String> {
        Err(format!("line {}: {message}", self.line()))
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at).map(|t| &t.tok)
    }

    fn skip_separators(&mut self) {
        while self.peek() == Some(&Tok::Separator) {
            self.at += 1;
        }
    }

    fn eat(&mut self, symbol: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Symbol(s)) if *s == symbol) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, symbol: &str) -> Result<(), String> {
        if self.eat(symbol) {
            Ok(())
        } else {
            match self.peek() {
                Some(token) => self.err(format!("expected `{symbol}`, found {}", token.describe())),
                None => self.err(format!("expected `{symbol}` at the end")),
            }
        }
    }

    fn name(&mut self, what: &str) -> Result<String, String> {
        match self.peek().cloned() {
            Some(Tok::Name(name)) => {
                self.at += 1;
                Ok(name)
            }
            Some(other) => self.err(format!("expected {what}, found {}", other.describe())),
            None => self.err(format!("expected {what} at the end")),
        }
    }

    /// A number literal, with an optional minus.
    fn number(&mut self) -> Result<f32, String> {
        let negative = self.eat("-");
        match self.peek().cloned() {
            Some(Tok::Number(n)) => {
                self.at += 1;
                Ok(if negative { -n } else { n })
            }
            Some(other) => self.err(format!("expected a number, found {}", other.describe())),
            None => self.err("expected a number at the end"),
        }
    }

    fn program(&mut self) -> Result<(), String> {
        loop {
            self.skip_separators();
            if self.peek().is_none() {
                return Ok(());
            }
            self.statement(true)?;
        }
    }

    /// Declares a variable and gives it a slot.
    fn declare(&mut self, name: &str, kind: Kind) -> Result<u8, String> {
        if reserved(name) {
            return self.err(format!("`{name}` is a built-in name"));
        }
        if self.vars.iter().any(|v| v.name == name) || self.names_port_or_array(name) {
            return self.err(format!("`{name}` is already used"));
        }
        if self.next_slot >= MAX_VARS {
            return self.err("too many variables");
        }
        let slot = self.next_slot as u8;
        self.next_slot += 1;
        self.vars.push(Var {
            name: name.to_owned(),
            slot,
            kind,
        });
        Ok(slot)
    }

    /// Whether an array or an event port already has the name.
    fn names_port_or_array(&self, name: &str) -> bool {
        let program = &self.program;
        program.arrays.iter().any(|a| a.name == name)
            || program.event_in.iter().any(|e| e.name == name)
            || program.event_out.iter().any(|e| e == name)
    }

    fn array(&self, name: &str) -> Option<u8> {
        self.program
            .arrays
            .iter()
            .position(|a| a.name == name)
            .map(|k| k as u8)
    }

    fn statement(&mut self, top: bool) -> Result<(), String> {
        if self.program.ops.len() > MAX_OPS {
            return self.err("the script is too long");
        }
        let name = self.name("a statement")?;
        match name.as_str() {
            "input" | "param" | "output" | "state" | "array" | "events" => {
                if !top {
                    return self.err(format!("`{name}` goes at the top level, not in a block"));
                }
                self.declaration(&name)?;
            }
            "let" => {
                let name = self.name("a variable name")?;
                self.expect("=")?;
                self.expression(0)?;
                // After the value, so `let x = x` doesn't see itself.
                let slot = self.declare(&name, Kind::Writable)?;
                self.program.ops.push(Op::Store(slot));
            }
            "if" => self.if_statement()?,
            "for" => self.for_statement()?,
            "note_on" | "note_off" => self.note_statement(name == "note_on")?,
            _ => self.assignment(&name)?,
        }
        match self.peek() {
            None | Some(Tok::Separator) | Some(Tok::Symbol("}")) => Ok(()),
            Some(other) => {
                let other = other.describe();
                self.err(format!("unexpected {other}"))
            }
        }
    }

    fn declaration(&mut self, keyword: &str) -> Result<(), String> {
        match keyword {
            "events" => return self.events_declaration(),
            "array" => return self.array_declaration(),
            _ => {}
        }
        let name = self.name("a name")?;
        match keyword {
            "input" => {
                let slot = self.declare(&name, Kind::Input)?;
                self.add_input(name, slot, None)?;
            }
            "param" => {
                let mut default = None;
                if self.eat("=") {
                    default = Some(self.number()?);
                }
                let (mut min, mut max) = (0.0, 1.0);
                let ranged = self.eat("[");
                if ranged {
                    min = self.number()?;
                    self.expect(",")?;
                    max = self.number()?;
                    self.expect("]")?;
                    if min >= max {
                        return self.err(format!("`{name}`'s range must go up"));
                    }
                }
                let (mut log, mut unit) = (false, Unit::None);
                while let Some(Tok::Name(flag)) = self.peek().cloned() {
                    self.at += 1;
                    match flag.as_str() {
                        "log" => log = true,
                        "hz" => unit = Unit::Hertz,
                        "db" => unit = Unit::Decibels,
                        "s" => unit = Unit::Seconds,
                        other => {
                            return self
                                .err(format!("unknown flag `{other}` (use log, hz, db or s)"));
                        }
                    }
                }
                if log && min <= 0.0 {
                    return self.err(format!("`{name}` is log, so its range must stay above 0"));
                }
                let default = default.unwrap_or(0.0).clamp(min, max);
                let slot = self.declare(&name, Kind::Input)?;
                let decl = ParamDecl {
                    min,
                    max,
                    default,
                    log,
                    unit,
                };
                self.add_input(name, slot, Some(decl))?;
            }
            "output" => {
                if self.program.outputs.len() >= MAX_OUTPUTS {
                    return self.err(format!("at most {MAX_OUTPUTS} outputs"));
                }
                let slot = self.declare(&name, Kind::Writable)?;
                self.program.outputs.push(name);
                self.program.output_slots.push(usize::from(slot));
            }
            _ => {
                let start = if self.eat("=") { self.number()? } else { 0.0 };
                let slot = self.declare(&name, Kind::Writable)?;
                self.program.starts.push((usize::from(slot), start));
            }
        }
        Ok(())
    }

    /// `events in name` or `events out name`.
    fn events_declaration(&mut self) -> Result<(), String> {
        let direction = self.name("`in` or `out`")?;
        let name = self.name("a name")?;
        if reserved(&name) || self.lookup(&name).is_some() || self.names_port_or_array(&name) {
            return self.err(format!("`{name}` is a built-in name or already used"));
        }
        match direction.as_str() {
            "in" => {
                if self.program.event_in.len() >= MAX_EVENT_PORTS {
                    return self.err(format!("at most {MAX_EVENT_PORTS} event inputs"));
                }
                let mut slots = [0; 4];
                for (slot, suffix) in slots.iter_mut().zip(["on", "off", "key", "velocity"]) {
                    *slot = usize::from(self.declare(&format!("{name}_{suffix}"), Kind::Input)?);
                }
                self.program.event_in.push(EventInDecl { name, slots });
            }
            "out" => {
                if self.program.event_out.len() >= MAX_EVENT_PORTS {
                    return self.err(format!("at most {MAX_EVENT_PORTS} event outputs"));
                }
                self.program.event_out.push(name);
            }
            other => {
                return self.err(format!("expected `in` or `out`, found `{other}`"));
            }
        }
        Ok(())
    }

    /// `array name[length]`, optionally `= fill`.
    fn array_declaration(&mut self) -> Result<(), String> {
        let name = self.name("a name")?;
        if reserved(&name) || self.lookup(&name).is_some() || self.names_port_or_array(&name) {
            return self.err(format!("`{name}` is a built-in name or already used"));
        }
        if self.program.arrays.len() >= MAX_ARRAYS {
            return self.err(format!("at most {MAX_ARRAYS} arrays"));
        }
        self.expect("[")?;
        let len = self.number()?;
        self.expect("]")?;
        if len.fract() != 0.0 || len < 1.0 {
            return self.err(format!(
                "`{name}`'s length must be a whole number, 1 or more"
            ));
        }
        let len = len as usize;
        if self.array_total + len > ARRAY_TOTAL {
            return self.err(format!("arrays hold at most {ARRAY_TOTAL} values in all"));
        }
        let fill = if self.eat("=") { self.number()? } else { 0.0 };
        self.program.arrays.push(ArrayDecl {
            name,
            offset: self.array_total,
            len,
            fill,
        });
        self.array_total += len;
        Ok(())
    }

    /// `name[index]` after the name: pushes the index and checks the bracket.
    fn index(&mut self) -> Result<(), String> {
        self.expect("[")?;
        self.expression(0)?;
        self.expect("]")
    }

    /// A whole number or `len(array)`, for the ends of a loop.
    fn bound(&mut self) -> Result<i64, String> {
        if matches!(self.peek(), Some(Tok::Name(n)) if n == "len") {
            self.at += 1;
            self.expect("(")?;
            let name = self.name("an array")?;
            let Some(a) = self.array(&name) else {
                return self.err(format!("`{name}` is not an array"));
            };
            self.expect(")")?;
            return Ok(self.program.arrays[usize::from(a)].len as i64);
        }
        let n = self.number()?;
        if n.fract() != 0.0 || n.abs() > 1e6 {
            return self.err("the ends of a loop are whole numbers");
        }
        Ok(n as i64)
    }

    /// `for i in 0 to 8 { ... }`
    fn for_statement(&mut self) -> Result<(), String> {
        let var = self.name("a loop variable")?;
        if self.name("`in`")? != "in" {
            self.at -= 1;
            return self.err(format!("expected `in` after `{var}`"));
        }
        let from = self.bound()?;
        if self.name("`to`")? != "to" {
            self.at -= 1;
            return self.err("expected `to` between the ends of the loop");
        }
        let to = self.bound()?;
        let trips = (to - from).max(0) as usize;
        if trips > MAX_TRIPS || self.steps.saturating_mul(trips.max(1)) > MAX_STEPS {
            return self.err(format!(
                "a loop runs at most {MAX_TRIPS} times, and loops inside loops {MAX_STEPS} steps in all"
            ));
        }
        let outer = self.steps;
        self.steps *= trips.max(1);
        let scope = self.vars.len();
        let slot = self.declare(&var, Kind::Input)?;
        let ops = &mut self.program.ops;
        ops.push(Op::Push(from as f32));
        ops.push(Op::Store(slot));
        let top = ops.len();
        ops.push(Op::Load(slot));
        ops.push(Op::Push(to as f32));
        ops.push(Op::Lt);
        let exit = ops.len();
        ops.push(Op::JumpIfZero(0));
        self.block()?;
        let Ok(top16) = u16::try_from(top) else {
            return self.err("the script is too long");
        };
        let ops = &mut self.program.ops;
        ops.push(Op::Load(slot));
        ops.push(Op::Push(1.0));
        ops.push(Op::Add);
        ops.push(Op::Store(slot));
        ops.push(Op::Jump(top16));
        let end = ops.len() - 1;
        self.program.loops.push(LoopInfo {
            start: top,
            end,
            trips,
        });
        self.patch(exit)?;
        self.vars.truncate(scope);
        self.steps = outer;
        Ok(())
    }

    /// `note_on(out, key, velocity)` or `note_off(out, key)`.
    fn note_statement(&mut self, on: bool) -> Result<(), String> {
        self.expect("(")?;
        let name = self.name("an event output")?;
        let Some(port) = self.program.event_out.iter().position(|e| *e == name) else {
            return self.err(format!(
                "`{name}` is not an event output (`events out {name}`)"
            ));
        };
        self.expect(",")?;
        self.expression(0)?;
        if on {
            self.expect(",")?;
            self.expression(0)?;
        }
        self.expect(")")?;
        self.program.ops.push(if on {
            Op::EmitOn(port as u8)
        } else {
            Op::EmitOff(port as u8)
        });
        Ok(())
    }

    fn add_input(&mut self, key: String, slot: u8, param: Option<ParamDecl>) -> Result<(), String> {
        if self.program.inputs.len() >= MAX_INPUTS {
            return self.err(format!("at most {MAX_INPUTS} inputs and parameters"));
        }
        self.program.inputs.push(InputDecl { key, param });
        self.program.input_slots.push(usize::from(slot));
        Ok(())
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        self.vars.iter().rev().find(|v| v.name == name)
    }

    fn assignment(&mut self, name: &str) -> Result<(), String> {
        if let Some(array) = self.array(name) {
            return self.array_assignment(name, array);
        }
        let Some(var) = self.lookup(name) else {
            return self.err(format!("unknown name `{name}`"));
        };
        let (slot, kind) = (var.slot, var.kind);
        if kind == Kind::Input {
            return self.err(format!(
                "`{name}` is an input, so it can't be set (copy it with `let`)"
            ));
        }
        let compound = match self.peek() {
            Some(Tok::Symbol("=")) => None,
            Some(Tok::Symbol("+=")) => Some(Op::Add),
            Some(Tok::Symbol("-=")) => Some(Op::Sub),
            Some(Tok::Symbol("*=")) => Some(Op::Mul),
            Some(Tok::Symbol("/=")) => Some(Op::Div),
            Some(other) => {
                let other = other.describe();
                return self.err(format!("expected `=` after `{name}`, found {other}"));
            }
            None => return self.err(format!("expected `=` after `{name}` at the end")),
        };
        self.at += 1;
        if let Some(op) = compound {
            self.program.ops.push(Op::Load(slot));
            self.expression(0)?;
            self.program.ops.push(op);
        } else {
            self.expression(0)?;
        }
        self.program.ops.push(Op::Store(slot));
        Ok(())
    }

    fn array_assignment(&mut self, name: &str, array: u8) -> Result<(), String> {
        self.index()?;
        let compound = match self.peek() {
            Some(Tok::Symbol("=")) => None,
            Some(Tok::Symbol("+=")) => Some(Op::Add),
            Some(Tok::Symbol("-=")) => Some(Op::Sub),
            Some(Tok::Symbol("*=")) => Some(Op::Mul),
            Some(Tok::Symbol("/=")) => Some(Op::Div),
            Some(other) => {
                let other = other.describe();
                return self.err(format!("expected `=` after `{name}[...]`, found {other}"));
            }
            None => return self.err(format!("expected `=` after `{name}[...]` at the end")),
        };
        self.at += 1;
        if let Some(op) = compound {
            self.program.ops.push(Op::Dup);
            self.program.ops.push(Op::LoadIdx(array));
            self.expression(0)?;
            self.program.ops.push(op);
        } else {
            self.expression(0)?;
        }
        self.program.ops.push(Op::StoreIdx(array));
        Ok(())
    }

    fn block(&mut self) -> Result<(), String> {
        self.expect("{")?;
        self.enter()?;
        let scope = self.vars.len();
        loop {
            self.skip_separators();
            if self.eat("}") {
                break;
            }
            if self.peek().is_none() {
                return self.err("expected `}` at the end");
            }
            self.statement(false)?;
        }
        self.vars.truncate(scope);
        self.leave();
        Ok(())
    }

    fn if_statement(&mut self) -> Result<(), String> {
        self.expression(0)?;
        let skip = self.program.ops.len();
        self.program.ops.push(Op::JumpIfZero(0));
        self.block()?;
        // `else` may start on the next line.
        let after_block = self.at;
        self.skip_separators();
        if matches!(self.peek(), Some(Tok::Name(n)) if n == "else") {
            self.at += 1;
            let over = self.program.ops.len();
            self.program.ops.push(Op::Jump(0));
            self.patch(skip)?;
            if matches!(self.peek(), Some(Tok::Name(n)) if n == "if") {
                self.at += 1;
                self.enter()?;
                self.if_statement()?;
                self.leave();
            } else {
                self.block()?;
            }
            self.patch(over)?;
        } else {
            self.at = after_block;
            self.patch(skip)?;
        }
        Ok(())
    }

    /// Points the jump at `from` to the next op to be written.
    fn patch(&mut self, from: usize) -> Result<(), String> {
        let here = self.program.ops.len();
        let Ok(target) = u16::try_from(here) else {
            return self.err("the script is too long");
        };
        match &mut self.program.ops[from] {
            Op::JumpIfZero(t) | Op::Jump(t) => *t = target,
            _ => unreachable!("only jumps are patched"),
        }
        Ok(())
    }

    fn enter(&mut self) -> Result<(), String> {
        if self.depth >= NESTING {
            return self.err("too deeply nested");
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn expression(&mut self, min_power: u8) -> Result<(), String> {
        self.enter()?;
        if self.program.ops.len() > MAX_OPS {
            return self.err("the script is too long");
        }
        self.prefix()?;
        while let Some(&Tok::Symbol(symbol)) = self.peek() {
            let Some((power, right, op)) = binary(symbol) else {
                break;
            };
            if power < min_power {
                break;
            }
            self.at += 1;
            self.expression(if right { power } else { power + 1 })?;
            self.program.ops.push(op);
        }
        self.leave();
        Ok(())
    }

    fn prefix(&mut self) -> Result<(), String> {
        let Some(token) = self.peek().cloned() else {
            return self.err("the expression ends too soon");
        };
        self.at += 1;
        match token {
            Tok::Number(n) => self.program.ops.push(Op::Push(n)),
            Tok::Symbol("-") => {
                self.expression(PREFIX)?;
                self.program.ops.push(Op::Neg);
            }
            Tok::Symbol("+") => self.expression(PREFIX)?,
            Tok::Symbol("!") => {
                self.expression(PREFIX)?;
                self.program.ops.push(Op::Not);
            }
            Tok::Symbol("(") => {
                self.expression(0)?;
                self.expect(")")?;
            }
            Tok::Name(name) => {
                if self.peek() == Some(&Tok::Symbol("(")) {
                    self.at += 1;
                    self.call(&name)?;
                } else if let Some(array) = self.array(&name) {
                    if self.peek() != Some(&Tok::Symbol("[")) {
                        return self.err(format!("`{name}` is an array: use `{name}[index]`"));
                    }
                    self.index()?;
                    self.program.ops.push(Op::LoadIdx(array));
                } else if let Some(var) = self.lookup(&name) {
                    let slot = var.slot;
                    self.program.ops.push(Op::Load(slot));
                } else {
                    let op = match name.as_str() {
                        "sr" => Op::Sr,
                        "bpm" => Op::Bpm,
                        "beat" => Op::Beat,
                        "time" => Op::Time,
                        "pi" => Op::Push(std::f32::consts::PI),
                        "tau" => Op::Push(std::f32::consts::TAU),
                        "e" => Op::Push(std::f32::consts::E),
                        _ => return self.err(format!("unknown name `{name}`")),
                    };
                    self.program.ops.push(op);
                }
            }
            other => {
                self.at -= 1;
                return self.err(format!("unexpected {}", other.describe()));
            }
        }
        Ok(())
    }

    fn call(&mut self, name: &str) -> Result<(), String> {
        if name == "len" {
            let array = self.name("an array")?;
            let Some(a) = self.array(&array) else {
                return self.err(format!("`{array}` is not an array"));
            };
            self.expect(")")?;
            let len = self.program.arrays[usize::from(a)].len;
            self.program.ops.push(Op::Push(len as f32));
            return Ok(());
        }
        let (arity, op) = match (unary_function(name), name) {
            (Some(f), _) => (1, Op::Unary(f)),
            (None, "db") => (1, Op::Unary(db_to_gain)),
            (None, "mtof") => (1, Op::Unary(midi_to_hz)),
            (None, "rand") => (0, Op::Rand),
            (None, "min") => (2, Op::Min),
            (None, "max") => (2, Op::Max),
            (None, "pow") => (2, Op::Pow),
            (None, "atan2") => (2, Op::Atan2),
            (None, "clamp") => (3, Op::Clamp),
            (None, "mix") => (3, Op::Mix),
            (None, "select") => (3, Op::Select),
            _ => return self.err(format!("unknown function `{name}`")),
        };
        for n in 0..arity {
            if n > 0 {
                self.expect(",")?;
            }
            self.expression(0)?;
        }
        if self.peek() == Some(&Tok::Symbol(",")) {
            return self.err(format!("`{name}` takes {arity} argument(s)"));
        }
        self.expect(")")?;
        self.program.ops.push(op);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    fn config(text: &str) -> Config {
        let mut config = Config::new();
        config.set(SCRIPT_KEY, Value::Text(text.into()));
        config
    }

    /// Runs `text` for `frames` samples with no wires (parameters at their
    /// defaults), and returns each output's samples.
    fn run_with(text: &str, frames: usize, set: &[(usize, f32)]) -> Vec<Vec<f32>> {
        let layout = Script.layout(&config(text)).unwrap();
        let mut h = Harness::new(&Script, &config(text), &[], 48_000.0, frames).unwrap();
        for &(port, value) in set {
            h.set(port, value);
        }
        h.run(frames).unwrap();
        (0..layout.outputs.len())
            .map(|k| h.output(k).lane(0, 0).to_vec())
            .collect()
    }

    fn first(text: &str) -> f32 {
        run_with(text, 1, &[])[0][0]
    }

    #[test]
    fn expressions_follow_the_maths_rules_and_add_comparisons() {
        let at = |e: &str| first(&format!("output o\no = {e}"));
        assert_eq!(at("1 + 2 * 3"), 7.0);
        assert_eq!(at("2 ^ 3 ^ 2"), 512.0);
        assert_eq!(at("-2 ^ 2"), -4.0);
        assert_eq!(at("10 - 4 - 3"), 3.0);
        assert_eq!(at("3 < 4"), 1.0);
        assert_eq!(at("3 >= 4"), 0.0);
        assert_eq!(at("3 == 3 && 2 != 2"), 0.0);
        assert_eq!(at("0 || 5"), 1.0);
        assert_eq!(at("!0 + !7"), 1.0);
        assert_eq!(at("1 + 1 < 3"), 1.0, "comparison binds looser than +");
        assert_eq!(at("clamp(5, 0, 1) + min(2, 3) + max(2, 3)"), 6.0);
        assert_eq!(at("select(1, 10, 20) + select(0, 10, 20)"), 30.0);
        assert!((at("db(-20)") - 0.1).abs() < 1e-6);
        assert!((at("mtof(69)") - 440.0).abs() < 1e-3);
        assert_eq!(at("sr"), 48_000.0);
        assert!((at("tau") - std::f32::consts::TAU).abs() < 1e-6);
    }

    #[test]
    fn state_keeps_its_value_and_starts_where_told() {
        let out = run_with("output o\nstate n = 10\nn += 1\no = n", 4, &[]);
        assert_eq!(out[0], vec![11.0, 12.0, 13.0, 14.0]);
    }

    #[test]
    fn let_variables_and_if_else_chains() {
        let text = "
            output o
            state n
            n += 1
            let m = n % 3
            if m == 0 { o = 100 }
            else if m == 1 {
                let twice = m * 2
                o = twice
            } else { o = -1 }
        ";
        let out = run_with(text, 6, &[]);
        assert_eq!(out[0], vec![2.0, -1.0, 100.0, 2.0, -1.0, 100.0]);
    }

    #[test]
    fn unset_outputs_are_zero_and_several_outputs_work() {
        let text = "
            output a; output b; output c
            state n
            n += 1
            if n % 2 == 0 { a = n }
            b = n * 10
        ";
        let out = run_with(text, 4, &[]);
        assert_eq!(out[0], vec![0.0, 2.0, 0.0, 4.0], "unset again means 0");
        assert_eq!(out[1], vec![10.0, 20.0, 30.0, 40.0]);
        assert_eq!(out[2], vec![0.0; 4]);
    }

    #[test]
    fn parameters_get_their_defaults_ranges_and_flags() {
        let text = "param p = 0.25 [0, 1]\nparam f = 1000 [20, 20000] log hz\noutput o\no = p + f";
        let layout = Script.layout(&config(text)).unwrap();
        assert_eq!(layout.inputs.len(), 2);
        assert_eq!(layout.inputs[0].key, "p");
        assert_eq!(layout.inputs[0].name, "P");
        let noodle_engine::InputKind::Param(f) = &layout.inputs[1].kind else {
            panic!("a parameter");
        };
        assert_eq!(
            (f.min, f.max, f.default, f.unit),
            (20.0, 20_000.0, 1000.0, Unit::Hertz)
        );
        assert_eq!(f.taper, noodle_engine::Taper::Log);
        assert_eq!(run_with(text, 1, &[])[0][0], 1000.25);
        assert_eq!(run_with(text, 1, &[(0, 0.5)])[0][0], 1000.5);
    }

    #[test]
    fn a_filter_script_smooths_a_step() {
        let text = "
            input x
            param cutoff = 1000 [20, 20000] log hz
            output y
            state z
            let k = 1 - exp(-tau * cutoff / sr)
            z += (x - z) * k
            y = z
        ";
        let mut h = Harness::new(&Script, &config(text), &[], 48_000.0, 2000).unwrap();
        h.set(0, 1.0);
        h.run(2000).unwrap();
        let out = h.output(0).lane(0, 0).to_vec();
        assert!(out[0] > 0.0 && out[0] < 0.2, "rises gradually: {}", out[0]);
        assert!(out[1999] > 0.999, "settles on the input: {}", out[1999]);
        assert!(out.windows(2).all(|w| w[1] >= w[0]), "monotonic");
    }

    #[test]
    fn rand_is_noise_and_repeats_after_a_reset() {
        let a = run_with("output o\no = rand()", 2000, &[]);
        let samples = &a[0];
        assert!(samples.iter().all(|&x| (-1.0..1.0).contains(&x)));
        let mean: f32 = samples.iter().sum::<f32>() / 2000.0;
        assert!(mean.abs() < 0.1, "mean {mean}");
        assert!(samples.windows(2).any(|w| w[0] != w[1]));
        assert_eq!(a, run_with("output o\no = rand()", 2000, &[]));
    }

    #[test]
    fn every_voice_and_channel_has_its_own_noise() {
        let text = "input x\noutput o\no = rand() + x * 0";
        let connected = [(0, Shape::new(2, 2))];
        let mut h = Harness::new(&Script, &config(text), &connected, 48_000.0, 64).unwrap();
        h.input(0, 64).fill(0.0);
        h.run(64).unwrap();
        let out = h.output(0);
        let lanes = [(0, 0), (0, 1), (1, 0), (1, 1)];
        for (i, a) in lanes.iter().enumerate() {
            for b in &lanes[i + 1..] {
                assert_ne!(out.lane(a.0, a.1), out.lane(b.0, b.1), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn nan_counts_as_true_everywhere_and_unranged_params_run_zero_to_one() {
        let at = |e: &str| first(&format!("output o\no = {e}"));
        assert_eq!(at("!(0 / 0)"), 0.0);
        assert_eq!(at("(0 / 0) && 1"), 1.0);
        assert_eq!(at("select(0 / 0, 5, 6)"), 5.0);
        assert_eq!(first("output o\nif 0 / 0 { o = 5 } else { o = 6 }"), 5.0);
        let layout = Script.layout(&config("param p\noutput o")).unwrap();
        let noodle_engine::InputKind::Param(p) = &layout.inputs[0].kind else {
            panic!("a parameter");
        };
        assert_eq!((p.min, p.max, p.default), (0.0, 1.0, 0.0));
    }

    #[test]
    fn time_counts_seconds_across_blocks() {
        let text = "output o\no = time";
        let mut h = Harness::new(&Script, &config(text), &[], 1000.0, 4).unwrap();
        h.run(4).unwrap();
        h.run(4).unwrap();
        let out = h.output(0).lane(0, 0).to_vec();
        assert_eq!(out, vec![0.004, 0.005, 0.006, 0.007]);
    }

    #[test]
    fn every_voice_has_its_own_state() {
        let text = "input x\noutput o\nstate sum\nsum += x\no = sum";
        let connected = [(0, Shape::new(2, 1))];
        let mut h = Harness::new(&Script, &config(text), &connected, 48_000.0, 3).unwrap();
        let mut input = h.input(0, 3);
        input.lane_mut(0, 0).fill(1.0);
        input.lane_mut(1, 0).fill(10.0);
        h.run(3).unwrap();
        assert_eq!(h.output(0).lane(0, 0), &[1.0, 2.0, 3.0]);
        assert_eq!(h.output(0).lane(1, 0), &[10.0, 20.0, 30.0]);
    }

    #[test]
    fn the_default_script_passes_its_input_through() {
        let mut h =
            Harness::new(&Script, &Config::new(), &[(0, Shape::MONO)], 48_000.0, 2).unwrap();
        h.input(0, 2).fill(0.5);
        h.run(2).unwrap();
        assert_eq!(h.output(0).lane(0, 0), &[0.5, 0.5]);
    }

    #[test]
    fn check_reports_the_ports() {
        let ports = check("input x\nparam amount\noutput left\noutput right\nleft = x").unwrap();
        assert_eq!(ports.inputs, ["x", "amount"]);
        assert_eq!(ports.outputs, ["left", "right"]);
        assert_eq!(check("").unwrap().outputs, ["out"]);
    }

    #[test]
    fn mistakes_are_reported_with_their_line() {
        for (text, wanted) in [
            ("output o\no = ", "line 2: the expression ends too soon"),
            ("output o\no = (1", "expected `)`"),
            ("output o\no = 1 2", "unexpected number 2"),
            ("output o\no = nope", "unknown name `nope`"),
            ("output o\nnope = 1", "unknown name `nope`"),
            ("output o\no = foo(1)", "unknown function `foo`"),
            ("output o\no = sin(1, 2)", "takes 1"),
            ("output o\no = rand(1)", "expected `)`"),
            ("output o\no = max(1)", "expected `,`"),
            ("output o\no = 1 $ 2", "line 2: unexpected `$`"),
            ("input x\noutput o\nx = 1", "is an input"),
            ("output o\nlet o = 1", "already used"),
            ("output sin", "built-in name"),
            ("output o\nlet pi = 3", "built-in name"),
            ("input x", "needs an output"),
            ("output o\nif 1 { o = 1", "expected `}`"),
            ("output o\nif 1 { input x }", "top level"),
            ("output o\nif 1 { let t = 1 }\no = t", "unknown name `t`"),
            ("param p [1, 0]\noutput o", "must go up"),
            ("param p [0, 1] loud\noutput o", "unknown flag"),
            ("param p [0, 1] log\noutput o", "above 0"),
            ("output o\no", "expected `=`"),
        ] {
            let error = Program::compile(text)
                .err()
                .unwrap_or_else(|| panic!("{text:?} should fail"));
            assert!(error.contains(wanted), "{text:?}: {error}");
        }
        let many = (0..=MAX_OUTPUTS)
            .map(|i| format!("output o{i}\n"))
            .collect::<String>();
        assert!(Program::compile(&many).is_err());
        let many = (0..=MAX_INPUTS)
            .map(|i| format!("input i{i}\n"))
            .collect::<String>();
        assert!(Program::compile(&(many + "output o")).is_err());
    }

    #[test]
    fn hostile_text_is_an_error_not_a_crash() {
        let deep = format!("output o\no = {}1{}", "(".repeat(5000), ")".repeat(5000));
        assert!(Program::compile(&deep).is_err());
        let nested = format!("output o\n{}o = 1{}", "if 1 {".repeat(500), "}".repeat(500));
        assert!(Program::compile(&nested).is_err());
        let long = format!("output o\no = {}", vec!["1"; 5000].join("+"));
        assert!(Program::compile(&long).is_err(), "too long is an error");
        let vars = (0..200)
            .map(|i| format!("let v{i} = 1\n"))
            .collect::<String>();
        assert!(Program::compile(&format!("output o\n{vars}")).is_err());
        // A deep but legal chain of operators still fits the stack check.
        let wide = format!("output o\no = 1{}", "+(1".repeat(20) + &")".repeat(20));
        assert!(Program::compile(&wide).is_ok());
        let too_wide = format!("output o\no = 1{}", "+(1".repeat(40) + &")".repeat(40));
        assert!(Program::compile(&too_wide).is_err());
    }

    #[test]
    fn a_bad_script_fails_the_node_with_a_message() {
        let error = Script.layout(&config("output o\no = +")).err().unwrap();
        assert!(error.to_string().contains("Script"), "{error}");
    }

    #[test]
    fn a_loop_runs_its_block_for_each_whole_number() {
        let text = "output o\nlet sum = 0\nfor i in 1 to 5 { sum += i }\no = sum";
        assert_eq!(first(text), 10.0);
        // An empty range runs nothing, and nested loops multiply.
        assert_eq!(
            first("output o\nlet n = 7\nfor i in 3 to 3 { n = 0 }\no = n"),
            7.0
        );
        let nested = "output o\nlet n = 0\nfor i in 0 to 4 { for j in 0 to 5 { n += 1 } }\no = n";
        assert_eq!(first(nested), 20.0);
        // The loop variable is gone after the loop and can't be set.
        assert!(Program::compile("output o\nfor i in 0 to 2 { }\no = i").is_err());
        assert!(Program::compile("output o\nfor i in 0 to 2 { i = 5 }").is_err());
    }

    #[test]
    fn loops_are_bounded_when_the_script_is_read() {
        for text in [
            "output o\nfor i in 0 to 257 { }",
            "output o\nfor i in 0 to 100 { for j in 0 to 11 { } }",
            "output o\nfor i in 0 to 2.5 { }",
            "output o\nfor i in 0 to n { }",
            "output o\nfor i of 0 to 2 { }",
        ] {
            assert!(Program::compile(text).is_err(), "{text:?}");
        }
        assert!(Program::compile("output o\nfor i in 0 to 256 { }").is_ok());
        assert!(Program::compile("output o\nfor i in 0 to 32 { for j in 0 to 32 { } }").is_ok());
    }

    #[test]
    fn arrays_remember_wrap_and_make_a_delay_line() {
        // A four-sample delay: write the input, read what was written four ago.
        let text = "
            input x
            output y
            array line[4]
            state head
            y = line[head]
            line[head] = x
            head = (head + 1) % len(line)
        ";
        let mut h =
            Harness::new(&Script, &config(text), &[(0, Shape::MONO)], 48_000.0, 10).unwrap();
        for (i, v) in h.input(0, 10).lane_mut(0, 0).iter_mut().enumerate() {
            *v = (i + 1) as f32;
        }
        h.run(10).unwrap();
        let out = h.output(0).lane(0, 0).to_vec();
        assert_eq!(out, [0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        // Indices round down and wrap, negatives included.
        let text = "output o\narray a[3]\nfor i in 0 to len(a) { a[i] = i + 1 }\no = a[-1] * 100 + a[4] * 10 + a[2.9]";
        assert_eq!(first(text), 3.0 * 100.0 + 2.0 * 10.0 + 3.0);
        // Compound assignment, a fill value, and one array per lane.
        assert_eq!(
            first("output o\narray a[2] = 5\na[1] += 2\no = a[0] + a[1]"),
            12.0
        );
        let out = run_with("output o\narray a[1]\na[0] += 1\no = a[0]", 3, &[]);
        assert_eq!(out[0], [1.0, 2.0, 3.0]);
    }

    #[test]
    fn array_mistakes_are_reported() {
        for (text, wanted) in [
            ("output o\narray a[0]", "whole number"),
            ("output o\narray a[2.5]", "whole number"),
            ("output o\narray a[5000]\narray b[5000]", "at most"),
            ("output o\narray a[2]\no = a", "is an array"),
            ("output o\narray a[2]\no = a[0", "expected `]`"),
            ("output o\narray a[2]\nlet a = 1", "already used"),
            ("output o\no = len(zz)", "not an array"),
            ("output o\nzz[0] = 1", "unknown name"),
        ] {
            let error = Program::compile(text)
                .err()
                .unwrap_or_else(|| panic!("{text:?} should fail"));
            assert!(error.contains(wanted), "{text:?}: {error}");
        }
    }

    fn note_on(time: u32, id: u32, key: u8, velocity: f32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOn {
                note: NoteId(id),
                channel: 0,
                key,
                velocity,
            },
        }
    }

    fn note_off(time: u32, id: u32) -> Event {
        Event {
            time,
            kind: EventKind::NoteOff {
                note: NoteId(id),
                velocity: 0.0,
            },
        }
    }

    #[test]
    fn event_inputs_arrive_one_to_a_sample_and_event_outputs_send_notes() {
        // Transposes up a fifth, and counts the starts as a signal.
        let text = "
            events in notes
            events out played
            output starts
            state n
            if notes_on { note_on(played, notes_key + 7, notes_velocity); n += 1 }
            if notes_off { note_off(played, notes_key + 7) }
            starts = n
        ";
        let layout = Script.layout(&config(text)).unwrap();
        assert_eq!(layout.event_inputs.len(), 1);
        assert_eq!(layout.event_outputs.len(), 1);
        let ports = check(text).unwrap();
        assert_eq!(
            (ports.event_inputs, ports.event_outputs),
            (vec!["notes".to_owned()], vec!["played".to_owned()])
        );
        let mut h = Harness::new(&Script, &config(text), &[], 48_000.0, 8).unwrap();
        // A chord at one instant, then its release.
        h.send_events(
            0,
            &[
                note_on(2, 1, 60, 0.5),
                note_on(2, 2, 64, 0.75),
                note_off(6, 1),
            ],
        );
        h.run(8).unwrap();
        assert_eq!(
            h.events(0),
            vec![
                note_on(2, 67, 67, 0.5),
                note_on(3, 71, 71, 0.75),
                note_off(6, 67),
            ]
        );
        assert_eq!(
            h.output(0).lane(0, 0),
            &[0.0, 0.0, 1.0, 2.0, 2.0, 2.0, 2.0, 2.0]
        );
    }

    #[test]
    fn events_that_do_not_fit_arrive_in_the_next_block() {
        let text = "events in ev\nevents out o\noutput x\nif ev_on { note_on(o, ev_key, 1) }";
        let mut h = Harness::new(&Script, &config(text), &[], 48_000.0, 4).unwrap();
        // Six notes at the last frame of a four-frame block.
        let chord: Vec<Event> = (0..6).map(|k| note_on(3, k, 40 + k as u8, 1.0)).collect();
        h.send_events(0, &chord);
        h.run(4).unwrap();
        let first: Vec<u32> = h.events(0).iter().map(|e| e.time).collect();
        assert_eq!(first, [3]);
        h.send_events(0, &[]);
        h.run(4).unwrap();
        let second: Vec<u32> = h.events(0).iter().map(|e| e.time).collect();
        assert_eq!(second, [0, 1, 2, 3], "the rest follow, in order");
    }

    #[test]
    fn loops_are_limited_by_the_work_they_do_not_only_their_trips() {
        let body = |n: usize| "o += 1\n".repeat(n);
        // 200 trips of 100 statements is 20000 passes of two ops or more.
        let heavy = format!("output o\nfor i in 0 to 200 {{\n{}}}", body(100));
        let error = Program::compile(&heavy).err().expect("too much work");
        assert!(error.contains("too much work"), "{error}");
        let light = format!("output o\nfor i in 0 to 100 {{\n{}}}", body(5));
        assert!(Program::compile(&light).is_ok());
        // Loops one after another add up.
        let many: String = (0..6)
            .map(|k| format!("for i{k} in 0 to 200 {{\n{}}}\n", body(10)))
            .collect();
        assert!(Program::compile(&format!("output o\n{many}")).is_err());
    }

    #[test]
    fn a_note_off_for_a_forgotten_note_leaves_the_key_and_a_reset_forgets_notes() {
        let text = "events in n\noutput k\nif n_on { k = n_key }\nif n_off { k = -1 - n_key }";
        let mut h = Harness::new(&Script, &config(text), &[], 48_000.0, 4).unwrap();
        // A note-off for a note never seen: the key is unchanged (0).
        h.send_events(0, &[note_off(1, 99)]);
        h.run(4).unwrap();
        assert_eq!(h.output(0).lane(0, 0), &[0.0, -1.0, 0.0, 0.0]);
        // One that was seen reports its key.
        h.send_events(0, &[note_on(0, 5, 60, 1.0), note_off(2, 5)]);
        h.run(4).unwrap();
        assert_eq!(h.output(0).lane(0, 0), &[60.0, 0.0, -61.0, 0.0]);
    }

    #[test]
    fn event_mistakes_are_reported() {
        for (text, wanted) in [
            ("output o\nnote_on(p, 60, 1)", "not an event output"),
            ("events out p\nnote_on(p, 60)", "expected `,`"),
            ("events up p\noutput o", "expected `in` or `out`"),
            ("events in p\nevents out p\noutput o", "already used"),
            ("events in p\noutput p_on", "already used"),
        ] {
            let error = Program::compile(text)
                .err()
                .unwrap_or_else(|| panic!("{text:?} should fail"));
            assert!(error.contains(wanted), "{text:?}: {error}");
        }
        // A script of event ports alone is a script.
        assert!(Program::compile("events in a\nevents out b").is_ok());
    }

    #[test]
    fn comments_semicolons_and_line_breaks_inside_brackets() {
        let text = "output o // the result\n// nothing here\nlet a = max(1,\n  5); o = a // done";
        assert_eq!(first(text), 5.0);
    }
}
