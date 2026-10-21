//! The two halves of a running engine: the [`Controller`], which lives on the
//! UI thread and compiles graphs, and the [`Processor`], which lives on the
//! audio thread and renders them.
//!
//! They talk through two lock-free queues: new plans go to the processor, and
//! old plans come back to be freed off the audio thread. Parameter values are
//! shared atomics, so changing one doesn't need a recompile or a queue.

use std::fmt;
use std::mem;
use std::sync::Arc;
use std::time::Duration;

use noodle_core::{Graph, NodeId, Project, TempoMap};
use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::compile::{Options, compile_with};
use crate::denormals::Flush;
use crate::parallel::Workers;
pub use crate::plan::Bus;
use crate::plan::{self, Cells, Interleaved, Plan, PlanInfo, PlanShape};
use crate::tempo::TempoTable;
use crate::transport::TransportControl;
use crate::{
    Context, Diagnostic, Lanes, Monitor, Registry, Replacements, TapSpec, Telemetry, Transport,
};

/// Fixed for an engine's lifetime. Changing the device or its settings means
/// making a new engine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub sample_rate: f32,
    /// The most frames rendered per node call. Longer device buffers are
    /// rendered in several blocks.
    pub max_frames: usize,
    /// Channels in the interleaved device output.
    pub channels: usize,
}

impl Settings {
    /// Checks the settings an engine relies on. They usually come from an
    /// audio device, so they're checked rather than trusted.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if !(self.sample_rate.is_finite() && self.sample_rate > 0.0) {
            return Err(SettingsError::SampleRate(self.sample_rate));
        }
        if self.max_frames == 0 {
            return Err(SettingsError::MaxFrames);
        }
        if self.channels == 0 {
            return Err(SettingsError::Channels);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingsError {
    SampleRate(f32),
    MaxFrames,
    Channels,
    /// The buses' channels don't add up to the engine's.
    Buses,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SampleRate(rate) => write!(f, "sample rate must be positive, not {rate}"),
            Self::MaxFrames => f.write_str("block size must be at least one frame"),
            Self::Channels => f.write_str("output needs at least one channel"),
            Self::Buses => f.write_str("the outputs' channels must add up to the engine's"),
        }
    }
}

impl std::error::Error for SettingsError {}

const PLAN_QUEUE: usize = 4;
/// How long the output fades out before a plan that changes what's audible,
/// and back in after it.
const FADE_SECONDS: f32 = 0.005;
const RETURN_QUEUE: usize = 8;
const TEMPO_QUEUE: usize = 4;

pub fn engine(settings: Settings) -> Result<(Controller, Processor), SettingsError> {
    settings.validate()?;
    let (plans, incoming) = RingBuffer::new(PLAN_QUEUE);
    let (returns, returned) = RingBuffer::new(RETURN_QUEUE);
    let (tempo_out, tempo_in) = RingBuffer::new(TEMPO_QUEUE);
    let (tempo_returns, tempo_returned) = RingBuffer::new(TEMPO_QUEUE);
    let control = TransportControl::new();
    let controller = Controller {
        settings,
        plans,
        returned,
        pending: None,
        sent: None,
        parallel: false,
        next_generation: 0,
        cells: Cells::new(),
        telemetry: None,
        buses: None,
        monitor: None,
        control: control.clone(),
        tempo_map: TempoMap::default(),
        tempo_out,
        tempo_returned,
        tempo_pending: None,
    };
    let fade_len = ((settings.sample_rate * FADE_SECONDS).round() as usize).max(1);
    let processor = Processor {
        settings,
        plan: None,
        fade_len,
        level: fade_len,
        flush: Flush::detect(),
        fading_out: false,
        incoming,
        returns,
        position: 0,
        control,
        table: Box::new(TempoTable::new(&TempoMap::default(), settings.sample_rate)),
        tempo_in,
        tempo_returns,
        seen_seek: 0,
        pending_seek: None,
        workers: None,
    };
    Ok((controller, processor))
}

/// [`engine`] whose processor has `workers` threads helping it, and whose
/// controller compiles plans that can use them (`always` as for
/// `Processor::start_workers`). Spawns the threads, so call it off the
/// audio thread.
pub fn engine_with_workers(
    settings: Settings,
    workers: usize,
    always: bool,
) -> Result<(Controller, Processor), SettingsError> {
    let (mut controller, mut processor) = engine(settings)?;
    processor.start_workers(workers, always);
    controller.parallel = workers > 0;
    Ok((controller, processor))
}

/// Compiles graphs into plans and sends them to the [`Processor`].
pub struct Controller {
    settings: Settings,
    plans: Producer<Box<Plan>>,
    returned: Consumer<Box<Plan>>,
    /// A plan waiting for room in the queue. A newer plan replaces it.
    pending: Option<(Box<Plan>, PlanInfo)>,
    /// The last plan sent, which the next plan takes over from.
    sent: Option<PlanInfo>,
    /// Plans are compiled to run on worker threads (see [`engine_with_workers`]).
    pub(crate) parallel: bool,
    next_generation: u64,
    cells: Cells,
    /// Where wired parameters report their signals, if anything reads them.
    telemetry: Option<Telemetry>,
    /// The devices sharing the output channels. `None` sends every Output
    /// node to all of them, as in an offline render.
    buses: Option<Vec<Bus>>,
    /// A node being listened to, added to every plan (see [`Monitor`]).
    monitor: Option<Monitor>,
    control: Arc<TransportControl>,
    tempo_map: TempoMap,
    tempo_out: Producer<Box<TempoTable>>,
    tempo_returned: Consumer<Box<TempoTable>>,
    /// A tempo table waiting for room in the queue. A newer one replaces it.
    tempo_pending: Option<Box<TempoTable>>,
}

impl Controller {
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Play, stop, seek and loop. The handle can go to other threads.
    pub fn transport(&self) -> Arc<TransportControl> {
        self.control.clone()
    }

    /// Splits the engine's output channels among devices, in order: the
    /// first bus is the main output, which Output nodes with no device play
    /// on. Each Output node then plays on the bus whose device it names, and
    /// on none if no bus does (it is reported by the next
    /// [`update`](Self::update)). The channel counts must add up to
    /// [`Settings::channels`]. Without this, every Output node mixes into all
    /// channels. Call it before the first update; it takes effect with the
    /// next one.
    pub fn set_buses(&mut self, buses: Vec<Bus>) -> Result<(), SettingsError> {
        let total = buses.iter().map(|bus| bus.channels).sum::<usize>();
        if total != self.settings.channels || buses.is_empty() {
            return Err(SettingsError::Buses);
        }
        self.buses = Some(buses);
        Ok(())
    }

    /// Listens to `monitor`'s nodes from the next update on, or stops
    /// listening if it's `None`. Offline renders never include it.
    pub fn set_monitor(&mut self, monitor: Option<Monitor>) {
        self.monitor = monitor;
    }

    /// The tempo map the engine is using.
    pub fn tempo_map(&self) -> &TempoMap {
        &self.tempo_map
    }

    /// Switches the engine to a new tempo map. The playhead keeps its tick,
    /// so its place in the music stays put and its place in time moves. If
    /// that moves it, the output fades out and back in around the change.
    pub fn set_tempo_map(&mut self, map: &TempoMap) {
        self.tempo_map = map.clone();
        self.tempo_pending = Some(Box::new(TempoTable::new(map, self.settings.sample_rate)));
        self.send_tempo();
    }

    /// Compiles `graph` and sends the result to the audio thread. Nodes that
    /// are unchanged keep their state. If everything the output depends on is
    /// unchanged, the switch is seamless; otherwise the output dips briefly
    /// (see [`Processor`]). Returns the problems found, for the UI to show.
    pub fn update(&mut self, graph: &Graph, registry: &Registry) -> Vec<Diagnostic> {
        self.update_with_lanes(graph, &[], registry)
    }

    /// [`update`](Self::update) for a whole project: its automation lanes
    /// drive their parameters, and its tempo map is used if the engine isn't
    /// already using it.
    pub fn update_project(&mut self, project: &Project, registry: &Registry) -> Vec<Diagnostic> {
        if self.tempo_map != *project.tempo_map() {
            self.set_tempo_map(project.tempo_map());
        }
        let lanes: Vec<_> = project.lanes().collect();
        self.update_with_lanes(project.graph(), &lanes, registry)
    }

    /// [`update`](Self::update) for a graph with automation lanes.
    pub fn update_with_lanes(
        &mut self,
        graph: &Graph,
        lanes: &Lanes<'_>,
        registry: &Registry,
    ) -> Vec<Diagnostic> {
        self.update_with(graph, lanes, registry, &Options::default())
    }

    /// [`update_project`](Self::update_project) with some outputs played
    /// from `replacements`: cached audio for frozen nodes and offline nodes
    /// (see [`analyze`](crate::analyze)).
    pub fn update_project_replacing(
        &mut self,
        project: &Project,
        registry: &Registry,
        replacements: &Replacements,
    ) -> Vec<Diagnostic> {
        if self.tempo_map != *project.tempo_map() {
            self.set_tempo_map(project.tempo_map());
        }
        let lanes: Vec<_> = project.lanes().collect();
        let options = Options {
            replacements: Some(replacements),
            ..Options::default()
        };
        self.update_with(project.graph(), &lanes, registry, &options)
    }

    /// [`update_project`](Self::update_project) cut down to what feeds
    /// `taps`, which watch outputs. This is how the renders that fill the
    /// cache see a node's output.
    pub(crate) fn update_project_tapping(
        &mut self,
        project: &Project,
        registry: &Registry,
        replacements: &Replacements,
        taps: &[TapSpec],
    ) -> Vec<Diagnostic> {
        if self.tempo_map != *project.tempo_map() {
            self.set_tempo_map(project.tempo_map());
        }
        let lanes: Vec<_> = project.lanes().collect();
        let options = Options {
            replacements: Some(replacements),
            taps,
            ..Options::default()
        };
        self.update_with(project.graph(), &lanes, registry, &options)
    }

    fn update_with(
        &mut self,
        graph: &Graph,
        lanes: &Lanes<'_>,
        registry: &Registry,
        options: &Options<'_>,
    ) -> Vec<Diagnostic> {
        self.free_returned();
        let main = self
            .buses
            .as_deref()
            .and_then(|buses| buses.first())
            .map(|bus| bus.device.as_str())
            .filter(|device| !device.is_empty());
        let options = &Options {
            independent_buffers: self.parallel,
            monitor: self.monitor.as_ref().map(|monitor| (monitor, main)),
            ..*options
        };
        let (schedule, mut diagnostics) = compile_with(graph, lanes, registry, options);
        let generation = self.next_generation;
        self.next_generation += 1;
        // Built against the last plan *sent*: an unsent pending plan never
        // reaches the audio thread, so nothing can take over from it.
        let (plan, info) = plan::build(
            schedule,
            self.sent.as_ref(),
            generation,
            self.settings,
            self.buses.as_deref(),
            self.telemetry.as_ref(),
            &mut self.cells,
            self.control.position(),
            &mut diagnostics,
        );
        self.pending = Some((plan, info));
        self.send_pending();
        diagnostics
    }

    /// Has the plans built from now on report the signal wired into each
    /// parameter to `telemetry`, for [`Telemetry::read_param`]. Takes effect
    /// at the next update.
    pub fn set_telemetry(&mut self, telemetry: &Telemetry) {
        self.telemetry = Some(telemetry.clone());
    }

    /// How many samples later than the timeline the mix sounds, because of
    /// plugin delay compensation: the slowest path's latency, which every
    /// other path is delayed to match. Zero when no node reports latency.
    /// An offline render drops this many leading frames so its audio lines
    /// up with the timeline; live playback just sounds this much late.
    pub fn output_latency(&self) -> usize {
        self.pending
            .as_ref()
            .map(|(_, info)| info)
            .or(self.sent.as_ref())
            .map_or(0, |info| info.output_latency as usize)
    }

    /// How much of the latest plan could run at once, for judging whether
    /// worker threads would help.
    pub fn plan_shape(&self) -> PlanShape {
        self.pending
            .as_ref()
            .map(|(_, info)| info)
            .or(self.sent.as_ref())
            .map_or_else(PlanShape::default, |info| info.shape)
    }

    /// Sets an unconnected input's value without recompiling. The change is
    /// smoothed if the input is a continuous parameter. Does nothing if the
    /// node has no such unconnected input in the current plan, or if `value`
    /// is infinite or NaN.
    pub fn set_param(&mut self, node: NodeId, key: &str, value: f32) {
        if !value.is_finite() {
            return;
        }
        if let Some(cell) = self.cells.get(&node).and_then(|inputs| inputs.get(key)) {
            cell.set(value);
        }
    }

    /// Frees plans the audio thread has finished with and sends any plan
    /// that was waiting for room. Call this regularly, e.g. every UI frame.
    pub fn maintain(&mut self) {
        self.free_returned();
        self.send_pending();
        self.send_tempo();
    }

    fn free_returned(&mut self) {
        while let Ok(plan) = self.returned.pop() {
            drop(plan);
        }
        while let Ok(table) = self.tempo_returned.pop() {
            drop(table);
        }
    }

    fn send_tempo(&mut self) {
        self.free_returned();
        if let Some(table) = self.tempo_pending.take()
            && let Err(PushError::Full(table)) = self.tempo_out.push(table)
        {
            self.tempo_pending = Some(table);
        }
    }

    fn send_pending(&mut self) {
        let Some((plan, info)) = self.pending.take() else {
            return;
        };
        match self.plans.push(plan) {
            Ok(()) => self.sent = Some(info),
            Err(PushError::Full(plan)) => self.pending = Some((plan, info)),
        }
    }
}

/// Renders the current plan. Everything here is real-time safe.
///
/// A plan that changes what's audible (a node on the output's path is added,
/// removed, rewired or rebuilt) would switch the sound abruptly and click. So
/// the processor fades the output out over a few milliseconds, installs the
/// plan, and fades back in. Other plans go in at once. A seek, and a tempo map
/// that moves the playhead, are handled the same way.
pub struct Processor {
    settings: Settings,
    plan: Option<Box<Plan>>,
    /// Fade length in frames.
    fade_len: usize,
    /// Output gain in frames of fade: `fade_len` is full volume, 0 silence.
    level: usize,
    /// Fading out, to install a plan that isn't seamless at silence.
    fading_out: bool,
    /// How to flush subnormals on this CPU, found when the engine is made.
    flush: Flush,
    incoming: Consumer<Box<Plan>>,
    returns: Producer<Box<Plan>>,
    /// Timeline position of the next frame, in samples.
    position: u64,
    control: Arc<TransportControl>,
    table: Box<TempoTable>,
    tempo_in: Consumer<Box<TempoTable>>,
    tempo_returns: Producer<Box<TempoTable>>,
    /// The last seek counted.
    seen_seek: u64,
    /// A seek waiting for the output to go silent.
    pending_seek: Option<noodle_core::Tick>,
    /// Threads that share out a wide plan's steps with the audio thread.
    workers: Option<Workers>,
}

impl Processor {
    /// Starts `count` worker threads that help render plans whose steps can
    /// overlap, replacing any already running; 0 stops them. Spawns threads,
    /// so it is only reachable through [`engine_with_workers`], which also
    /// makes the controller compile plans that can use them.
    ///
    /// Only plans wide enough to pay for the hand-off run in parallel, unless
    /// `always` is set. Output differs from the single-threaded render at
    /// most in the last bits of a float sum.
    pub(crate) fn start_workers(&mut self, count: usize, always: bool) {
        let period = Duration::from_secs_f64(
            self.settings.max_frames as f64 / f64::from(self.settings.sample_rate),
        );
        self.workers = (count > 0).then(|| Workers::start(count, always, self.flush, period));
    }

    /// Whether the workers would take part in rendering a plan of this shape.
    pub(crate) fn parallel_for(&self, shape: PlanShape) -> bool {
        self.workers.as_ref().is_some_and(|w| w.worth(shape))
    }

    /// How many worker threads got real-time priority, and how many a raised
    /// priority short of that (the rest are ordinary threads). The workers
    /// ask as they start, so read it a moment after
    /// [`engine_with_workers`].
    pub fn worker_priorities(&self) -> (usize, usize) {
        self.workers.as_ref().map_or((0, 0), Workers::priorities)
    }

    /// How many workers have parked and woken at least once (see
    /// [`workers`](Self::workers) for how many there are). Idle workers park
    /// after a moment; a test that counts allocations waits until all have,
    /// so the first use of parking is not inside what it counts.
    pub fn workers_cycled(&self) -> usize {
        self.workers.as_ref().map_or(0, Workers::cycled)
    }

    /// How many worker threads help the audio thread.
    pub fn workers(&self) -> usize {
        self.workers.as_ref().map_or(0, Workers::count)
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Renders interleaved audio into `output`, whose length must be a
    /// multiple of the channel count. Silent until the first plan arrives.
    /// Input nodes are silent; see [`process_with_input`](Self::process_with_input).
    ///
    /// Subnormal floats are flushed to zero while it runs (see
    /// `denormals.rs`), and the thread's previous mode is restored after.
    pub fn process(&mut self, output: &mut [f32]) {
        self.render(Interleaved::NONE, output);
    }

    /// Renders like [`process`](Self::process), with Input nodes playing
    /// `input`: interleaved, with `input_channels` channels and as many
    /// frames as `output`. Input of the wrong length is a bug in the caller;
    /// it's ignored, so Input nodes go silent.
    pub fn process_with_input(&mut self, input: &[f32], input_channels: usize, output: &mut [f32]) {
        let frames = output.len() / self.settings.channels;
        let fits = input_channels > 0 && input.len() == frames * input_channels;
        debug_assert!(fits, "{} input samples for {frames} frames", input.len());
        let input = if fits {
            Interleaved {
                samples: input,
                channels: input_channels,
            }
        } else {
            Interleaved::NONE
        };
        self.render(input, output);
    }

    fn render(&mut self, input: Interleaved<'_>, output: &mut [f32]) {
        let Settings {
            sample_rate,
            max_frames,
            channels,
        } = self.settings;
        debug_assert_eq!(output.len() % channels, 0);
        let _flush = self.flush.enable();
        // The deadline is the whole callback, not the chunks it is cut into.
        let started = self
            .plan
            .as_ref()
            .is_some_and(|plan| plan.times_blocks())
            .then(std::time::Instant::now);
        let callback_frames = output.len() / channels;

        let mut rest = output;
        let mut input_rest = input.samples;
        while !rest.is_empty() {
            self.install_new_plans();
            let playing = self.control.is_playing();
            let looping = self.loop_samples();
            let mut frames = (rest.len() / channels).min(max_frames);
            if self.fading_out && self.level > 0 {
                // End the chunk where the fade does, so the plan goes in there.
                frames = frames.min(self.level);
            }
            if let Some((_, end)) = looping
                && playing
                && self.position < end
            {
                // End the chunk at the loop's end, so the wrap is exact.
                frames = frames.min((end - self.position).min(max_frames as u64) as usize);
            }
            let (chunk, tail) = mem::take(&mut rest).split_at_mut(frames * channels);
            rest = tail;
            let (input_chunk, input_tail) = input_rest.split_at(frames * input.channels);
            input_rest = input_tail;
            let input_chunk = Interleaved {
                samples: input_chunk,
                channels: input.channels,
            };

            match &mut self.plan {
                Some(plan) => {
                    let tick = self.table.tick_at(self.position);
                    let ctx = Context {
                        sample_rate,
                        frames,
                        transport: Transport {
                            playing,
                            position: self.position,
                            loop_range: looping,
                            tick,
                            bpm: self.table.bpm_at(tick),
                            signature: self.table.signature_at(tick),
                            signature_start: self.table.signature_start_at(tick),
                        },
                    };
                    plan.run(&ctx, input_chunk, chunk, channels, self.workers.as_ref());
                }
                None => chunk.fill(0.0),
            }
            self.apply_fade(chunk, channels);
            if playing {
                self.position += frames as u64;
                if let Some((start, end)) = looping
                    && self.position == end
                {
                    self.position = start;
                }
            }
        }
        if let (Some(started), Some(plan)) = (started, &self.plan) {
            plan.record_block(
                started.elapsed().as_nanos() as u64,
                callback_frames,
                sample_rate,
            );
        }
        self.control.publish(self.position);
    }

    /// The loop's start and end in samples, if it's on and not empty.
    fn loop_samples(&self) -> Option<(u64, u64)> {
        let (start, end) = self.control.loop_range()?;
        let start = self.table.sample_at_tick(start);
        let end = self.table.sample_at_tick(end);
        (start < end).then_some((start, end))
    }

    /// Installs queued plans in order, sending each replaced plan back to be
    /// freed. Stops while the return queue is full, so a plan is never freed
    /// here.
    ///
    /// A plan that isn't seamless waits, and the output starts fading out.
    /// Once it's silent, every queued plan goes in and the output fades back
    /// in.
    fn install_new_plans(&mut self) {
        let silent = self.level == 0;
        let jump_waiting = self.install_time_changes(silent);
        if jump_waiting {
            self.fading_out = true;
        }
        while self.returns.slots() > 0 {
            let Ok(next) = self.incoming.peek() else {
                break;
            };
            if !(silent || self.plan.is_none() || next.is_seamless()) {
                self.fading_out = true;
                return;
            }
            let Ok(mut plan) = self.incoming.pop() else {
                break;
            };
            if let Some(old) = &mut self.plan {
                plan.take_state_from(old);
            }
            if let Some(old) = self.plan.replace(plan) {
                let pushed = self.returns.push(old);
                debug_assert!(pushed.is_ok(), "checked for room above");
            }
        }
        // If the return queue filled up, stay silent until every waiting plan
        // is in.
        if silent && self.incoming.is_empty() && !jump_waiting {
            self.fading_out = false;
        }
    }

    /// Applies a seek, which jumps the playhead and so clicks unless the
    /// output is silent: it waits for that, and the output fades out
    /// meanwhile. A new tempo table goes in at once. It keeps the playhead's
    /// tick, so everything that reads ticks carries on unbroken, and it
    /// doesn't reset nodes, so a tempo drag leaves reverb tails alone. Only
    /// something reading the sample position can notice the move, which in
    /// M2 means a clip schedule; when one exists it must raise its own
    /// discontinuity. Returns whether a seek is still waiting.
    fn install_time_changes(&mut self, silent: bool) -> bool {
        if let Some(tick) = self.control.take_seek(&mut self.seen_seek) {
            self.pending_seek = Some(tick);
        }
        if let Some(tick) = self.pending_seek {
            if !silent {
                return true;
            }
            self.position = self.table.sample_at_tick(tick);
            self.pending_seek = None;
            self.reset_nodes();
        }
        while self.tempo_returns.slots() > 0 {
            let Ok(next) = self.tempo_in.peek() else {
                break;
            };
            // The playhead keeps its tick, so its sample position moves.
            let tick = self.table.tick_at(self.position);
            let position = next.sample_at(tick);
            let Ok(new) = self.tempo_in.pop() else {
                break;
            };
            let old = mem::replace(&mut self.table, new);
            let pushed = self.tempo_returns.push(old);
            debug_assert!(pushed.is_ok(), "checked for room above");
            self.position = position;
        }
        // A table is still waiting only if the return queue is full.
        !self.tempo_in.is_empty()
    }

    fn reset_nodes(&mut self) {
        if let Some(plan) = &mut self.plan {
            plan.reset_nodes();
        }
    }

    /// Scales `chunk` by the fade, frame by frame, and moves the fade on.
    fn apply_fade(&mut self, chunk: &mut [f32], channels: usize) {
        if !self.fading_out && self.level == self.fade_len {
            return;
        }
        for frame in chunk.chunks_mut(channels) {
            self.level = if self.fading_out {
                self.level.saturating_sub(1)
            } else {
                (self.level + 1).min(self.fade_len)
            };
            let gain = self.level as f32 / self.fade_len as f32;
            for sample in frame {
                *sample *= gain;
            }
        }
    }
}

#[cfg(test)]
mod tests;
