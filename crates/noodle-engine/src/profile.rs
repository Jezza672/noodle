//! Profiling a project: render it as fast as it will go and report where the
//! time went.

use std::time::{Duration, Instant};

use noodle_core::{NodeId, Project};

use crate::{
    BlockStats, Diagnostic, PlanShape, Registry, RenderError, Settings, Telemetry,
    engine_with_workers,
};

/// What rendering a project cost.
#[derive(Clone, Debug)]
pub struct Profile {
    /// Audio rendered, in frames.
    pub frames: usize,
    pub sample_rate: f32,
    /// How long the render took.
    pub wall: Duration,
    /// Threads helping the audio thread.
    pub workers: usize,
    /// How much of the graph could run side by side.
    pub shape: PlanShape,
    /// Each block's time against its budget (the time it lasts as audio).
    pub blocks: BlockStats,
    /// Each node's total time, slowest first. Group contents are the nodes
    /// themselves, since a group is flattened away.
    pub nodes: Vec<NodeProfile>,
    pub diagnostics: Vec<Diagnostic>,
}

/// One node's cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeProfile {
    pub id: NodeId,
    /// Time spent in the node over the whole render.
    pub total: Duration,
    /// Blocks it ran.
    pub blocks: u64,
}

impl Profile {
    /// The audio rendered, in seconds.
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate)
    }

    /// How many times faster than real time the render ran.
    pub fn realtime_factor(&self) -> f64 {
        self.seconds() / self.wall.as_secs_f64().max(1e-9)
    }

    /// The share of one core's real-time budget a node used on average: 0.1
    /// is ten percent.
    pub fn load(&self, node: &NodeProfile) -> f64 {
        node.total.as_secs_f64() / self.seconds().max(1e-9)
    }

    /// The time of every node added up.
    pub fn work(&self) -> Duration {
        self.nodes.iter().map(|n| n.total).sum()
    }
}

/// Renders `project` for `frames` frames with `workers` helper threads
/// (`always` as for [`Processor::start_workers`](crate::Processor::start_workers))
/// and reports how long it took and where. Nothing is written anywhere.
pub fn profile_project(
    project: &Project,
    registry: &Registry,
    settings: Settings,
    frames: usize,
    workers: usize,
    always: bool,
) -> Result<Profile, RenderError> {
    const CHUNK: usize = 4096;
    let telemetry = Telemetry::new();
    let (mut controller, mut processor) = engine_with_workers(settings, workers, always)?;
    controller.set_telemetry(&telemetry);
    let diagnostics = controller.update_project(project, registry);
    let shape = controller.plan_shape();
    let mut chunk = vec![0.0; CHUNK.min(frames.max(1)) * settings.channels];
    // The first block installs the plan with a fade; start the clock after it
    // so the figures are the steady state.
    processor.process(&mut chunk[..settings.max_frames.min(CHUNK) * settings.channels]);
    telemetry.reset_blocks();
    let started = Instant::now();
    let mut done = 0;
    while done < frames {
        let n = (chunk.len() / settings.channels).min(frames - done);
        processor.process(&mut chunk[..n * settings.channels]);
        done += n;
    }
    let wall = started.elapsed();
    let mut nodes: Vec<_> = telemetry
        .read_all_cpu_totals()
        .into_iter()
        .map(|(id, totals)| NodeProfile {
            id,
            total: Duration::from_nanos(totals.nanos),
            blocks: totals.blocks,
        })
        .collect();
    nodes.sort_by_key(|n| std::cmp::Reverse(n.total));
    Ok(Profile {
        frames,
        sample_rate: settings.sample_rate,
        wall,
        workers,
        shape,
        blocks: telemetry.read_blocks(),
        nodes,
        diagnostics,
    })
}
