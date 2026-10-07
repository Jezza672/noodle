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

use noodle_core::{Graph, NodeId, TempoMap};
use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::denormals::Flush;
use crate::plan::{self, Cells, Interleaved, Plan, PlanInfo};
use crate::tempo::TempoTable;
use crate::transport::TransportControl;
use crate::{Context, Diagnostic, Registry, Transport, compile};

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
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SampleRate(rate) => write!(f, "sample rate must be positive, not {rate}"),
            Self::MaxFrames => f.write_str("block size must be at least one frame"),
            Self::Channels => f.write_str("output needs at least one channel"),
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
        next_generation: 0,
        cells: Cells::new(),
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
    };
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
    next_generation: u64,
    cells: Cells,
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
        self.free_returned();
        let (schedule, mut diagnostics) = compile(graph, registry);
        let generation = self.next_generation;
        self.next_generation += 1;
        // Built against the last plan *sent*: an unsent pending plan never
        // reaches the audio thread, so nothing can take over from it.
        let (plan, info) = plan::build(
            schedule,
            self.sent.as_ref(),
            generation,
            self.settings.sample_rate,
            self.settings.max_frames,
            &mut self.cells,
            &mut diagnostics,
        );
        self.pending = Some((plan, info));
        self.send_pending();
        diagnostics
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
}

impl Processor {
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
                            tick,
                            bpm: self.table.bpm_at(tick),
                            signature: self.table.signature_at(tick),
                        },
                    };
                    plan.run(&ctx, input_chunk, chunk, channels);
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

    /// Applies a seek, and a new tempo table, which jump the playhead and so
    /// click unless the output is silent: they wait for it, and the output
    /// fades out meanwhile. A tempo table that leaves the playhead where it
    /// is goes in at once. Returns whether anything is still waiting.
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
            let moves = position != self.position;
            if moves && !silent {
                return true;
            }
            let Ok(new) = self.tempo_in.pop() else {
                break;
            };
            let old = mem::replace(&mut self.table, new);
            let pushed = self.tempo_returns.push(old);
            debug_assert!(pushed.is_ok(), "checked for room above");
            if moves {
                self.position = position;
                self.reset_nodes();
            }
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
