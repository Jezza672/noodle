//! The two halves of a running engine: the [`Controller`], which lives on the
//! UI thread and compiles graphs, and the [`Processor`], which lives on the
//! audio thread and renders them.
//!
//! They talk through two lock-free queues: new plans go to the processor, and
//! old plans come back to be freed off the audio thread. Parameter values are
//! shared atomics, so changing one doesn't need a recompile or a queue.

use noodle_core::{Graph, NodeId};
use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::plan::{self, Cells, Plan, PlanInfo};
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

const PLAN_QUEUE: usize = 4;
const RETURN_QUEUE: usize = 8;

pub fn engine(settings: Settings) -> (Controller, Processor) {
    let (plans, incoming) = RingBuffer::new(PLAN_QUEUE);
    let (returns, returned) = RingBuffer::new(RETURN_QUEUE);
    let controller = Controller {
        settings,
        plans,
        returned,
        pending: None,
        sent: None,
        next_generation: 0,
        cells: Cells::new(),
    };
    let processor = Processor {
        settings,
        plan: None,
        incoming,
        returns,
        position: 0,
    };
    (controller, processor)
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
}

impl Controller {
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Compiles `graph` and sends the result to the audio thread. Nodes that
    /// are unchanged keep their state, so the switch is seamless. Returns the
    /// problems found, for the UI to show.
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
    /// node has no such unconnected input in the current plan.
    pub fn set_param(&mut self, node: NodeId, key: &str, value: f32) {
        if let Some(cell) = self.cells.get(&node).and_then(|inputs| inputs.get(key)) {
            cell.set(value);
        }
    }

    /// Frees plans the audio thread has finished with and sends any plan
    /// that was waiting for room. Call this regularly, e.g. every UI frame.
    pub fn maintain(&mut self) {
        self.free_returned();
        self.send_pending();
    }

    fn free_returned(&mut self) {
        while let Ok(plan) = self.returned.pop() {
            drop(plan);
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
pub struct Processor {
    settings: Settings,
    plan: Option<Box<Plan>>,
    incoming: Consumer<Box<Plan>>,
    returns: Producer<Box<Plan>>,
    position: u64,
}

impl Processor {
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Renders interleaved audio into `output`, whose length must be a
    /// multiple of the channel count. Silent until the first plan arrives.
    pub fn process(&mut self, output: &mut [f32]) {
        let Settings {
            sample_rate,
            max_frames,
            channels,
        } = self.settings;
        debug_assert_eq!(output.len() % channels, 0);
        self.install_new_plans();

        for chunk in output.chunks_mut(max_frames * channels) {
            let frames = chunk.len() / channels;
            match &mut self.plan {
                Some(plan) => {
                    let ctx = Context {
                        sample_rate,
                        frames,
                        transport: Transport {
                            playing: true,
                            position: self.position,
                        },
                    };
                    plan.run(&ctx, chunk, channels);
                }
                None => chunk.fill(0.0),
            }
            self.position += frames as u64;
        }
    }

    /// Installs queued plans in order, sending each replaced plan back to be
    /// freed. Stops while the return queue is full, so a plan is never freed
    /// here.
    fn install_new_plans(&mut self) {
        while self.returns.slots() > 0 {
            let Ok(mut plan) = self.incoming.pop() else {
                return;
            };
            if let Some(old) = &mut self.plan {
                plan.take_state_from(old);
            }
            if let Some(old) = self.plan.replace(plan) {
                let pushed = self.returns.push(old);
                debug_assert!(pushed.is_ok(), "checked for room above");
            }
        }
    }
}

#[cfg(test)]
mod tests;
