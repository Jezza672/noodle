//! Offline nodes: processing that needs the whole input before it can produce
//! output.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use noodle_core::{Config, NodeId};

use crate::{
    Context, Instance, Io, NodeError, NodeType, Setup, Shape, SignalIn, SignalOut, Transport,
    plan::seed_for,
};

/// An instance of an offline node (see [`Mode::Offline`](crate::Mode)).
///
/// `render` gets the whole range at once through the same [`Io`] views
/// real-time nodes get; `ctx.frames` is the length of the range. It runs on a
/// worker thread, so it may allocate and take its time, but it should call
/// [`Progress::report`] regularly and stop if that returns `Err`.
pub trait OfflineNode: Send + 'static {
    fn render(
        &mut self,
        ctx: &Context,
        io: Io<'_, '_>,
        progress: &Progress,
    ) -> Result<(), Cancelled>;
}

/// Shared between a render and whatever is showing its progress.
#[derive(Debug)]
pub struct Progress {
    fraction: AtomicU32,
    cancelled: AtomicBool,
    /// Where in the whole the current stretch starts and how long it is, as
    /// `f32` bits. A job made of several renders gives each its stretch.
    window: [AtomicU32; 2],
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            fraction: AtomicU32::new(0),
            cancelled: AtomicBool::new(false),
            window: [
                AtomicU32::new(0.0f32.to_bits()),
                AtomicU32::new(1.0f32.to_bits()),
            ],
        }
    }
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the following [`report`](Self::report)s (0 to 1) fill the part
    /// of the whole from `start` to `start + span`, both fractions of it.
    pub fn set_window(&self, start: f32, span: f32) {
        self.window[0].store(start.to_bits(), Ordering::Relaxed);
        self.window[1].store(span.to_bits(), Ordering::Relaxed);
    }

    /// Records how far the render has got, from 0 to 1. Returns
    /// `Err(Cancelled)` if the render should stop.
    pub fn report(&self, fraction: f32) -> Result<(), Cancelled> {
        let start = f32::from_bits(self.window[0].load(Ordering::Relaxed));
        let span = f32::from_bits(self.window[1].load(Ordering::Relaxed));
        let whole = start + fraction.clamp(0.0, 1.0) * span;
        self.fraction
            .store(whole.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        if self.cancelled.load(Ordering::Relaxed) {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn fraction(&self) -> f32 {
        f32::from_bits(self.fraction.load(Ordering::Relaxed))
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// What an offline node reads on one input.
pub enum OfflineInput {
    /// Unconnected: a constant for the whole range.
    Constant(f32),
    /// The signal over the whole range, planar: each lane's `frames` samples
    /// in turn, voice-major.
    Signal { shape: Shape, data: Vec<f32> },
}

/// What an offline node wrote to one output, in the same planar layout.
pub struct OfflineOutput {
    pub shape: Shape,
    pub data: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OfflineError {
    Node(NodeError),
    /// The node type produced a real-time node.
    NotOffline,
    Cancelled,
    /// The range is too long to hold in memory.
    TooLong,
}

impl From<Cancelled> for OfflineError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl From<NodeError> for OfflineError {
    fn from(error: NodeError) -> Self {
        Self::Node(error)
    }
}

impl std::fmt::Display for OfflineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Node(error) => error.fmt(f),
            Self::NotOffline => {
                f.write_str("node type bug: an offline layout made a real-time node")
            }
            Self::Cancelled => f.write_str("the render was cancelled"),
            Self::TooLong => f.write_str("the range is too long to hold in memory"),
        }
    }
}

impl std::error::Error for OfflineError {}

/// Runs an offline node over a whole range: builds it as the compiler
/// would (same seed for the same `node`), hands it `inputs` and collects its
/// outputs. The caller supplies the inputs by rendering what feeds them.
#[allow(clippy::too_many_arguments)]
pub fn render_offline_node(
    node_type: &dyn NodeType,
    config: &Config,
    node: NodeId,
    inputs: &[OfflineInput],
    frames: usize,
    sample_rate: f32,
    progress: &Progress,
) -> Result<Vec<OfflineOutput>, OfflineError> {
    let layout = node_type.layout(config)?;
    let input_shapes: Vec<Shape> = inputs
        .iter()
        .map(|input| match input {
            OfflineInput::Constant(_) => Shape::MONO,
            OfflineInput::Signal { shape, .. } => *shape,
        })
        .collect();
    let output_shapes = node_type.output_shapes(config, &layout, &input_shapes)?;
    let instance = node_type.instantiate(&Setup {
        node,
        config,
        sample_rate,
        max_frames: frames,
        input_shapes: &input_shapes,
        output_shapes: &output_shapes,
        seed: seed_for(node),
    })?;
    let Instance::Offline(mut offline) = instance else {
        return Err(OfflineError::NotOffline);
    };

    // A constant input is a mono signal holding the value.
    let constants: Vec<Vec<f32>> = inputs
        .iter()
        .map(|input| match input {
            OfflineInput::Constant(value) => vec![*value; frames],
            OfflineInput::Signal { .. } => Vec::new(),
        })
        .collect();
    let signals: Vec<SignalIn<'_>> = inputs
        .iter()
        .zip(&constants)
        .zip(&input_shapes)
        .map(|((input, constant), &shape)| match input {
            OfflineInput::Constant(value) => {
                SignalIn::new(constant, shape, frames).with_constant(*value)
            }
            OfflineInput::Signal { data, .. } => SignalIn::new(data, shape, frames),
        })
        .collect();
    let mut outputs: Vec<OfflineOutput> = Vec::with_capacity(output_shapes.len());
    for &shape in &output_shapes {
        let len = shape
            .lanes()
            .checked_mul(frames)
            .ok_or(OfflineError::TooLong)?;
        let mut data = Vec::new();
        data.try_reserve_exact(len)
            .map_err(|_| OfflineError::TooLong)?;
        data.resize(len, 0.0);
        outputs.push(OfflineOutput { shape, data });
    }
    let mut views: Vec<SignalOut<'_>> = outputs
        .iter_mut()
        .map(|o| SignalOut::new(&mut o.data, o.shape, frames))
        .collect();
    let ctx = Context {
        sample_rate,
        frames,
        transport: Transport {
            playing: true,
            ..Transport::default()
        },
    };
    let io = Io {
        inputs: &signals,
        outputs: &mut views,
        event_inputs: &[],
        event_outputs: &mut [],
    };
    offline.render(&ctx, io, progress)?;
    drop(views);
    Ok(outputs)
}
