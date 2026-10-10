//! Runs a single node outside a graph, for unit tests and benchmarks.

use crate::{
    Cancelled, Config, Context, Event, EventsOut, Instance, Io, NodeError, NodeType, Progress,
    Setup, Shape, SignalBuffer, SignalIn, SignalOut, Transport,
};

/// Hosts one node with its own buffers.
///
/// Inputs listed as connected get a buffer, which you fill with
/// [`Harness::input`]. The other inputs behave as if unconnected: they hold a
/// value, which starts at the port's default and is changed with
/// [`Harness::set`].
pub struct Harness {
    instance: Instance,
    inputs: Vec<HarnessInput>,
    outputs: Vec<SignalBuffer>,
    event_inputs: Vec<Vec<Event>>,
    event_outputs: Vec<Vec<Event>>,
    sample_rate: f32,
    position: u64,
    frames: usize,
}

struct HarnessInput {
    buffer: SignalBuffer,
    /// `None` if the input is connected.
    value: Option<f32>,
}

impl Harness {
    pub fn new(
        node_type: &dyn NodeType,
        config: &Config,
        connected: &[(usize, Shape)],
        sample_rate: f32,
        max_frames: usize,
    ) -> Result<Self, NodeError> {
        Self::with_seed(node_type, config, connected, sample_rate, max_frames, 0)
    }

    /// Like [`new`](Self::new), with the seed the node gets in [`Setup`].
    pub fn with_seed(
        node_type: &dyn NodeType,
        config: &Config,
        connected: &[(usize, Shape)],
        sample_rate: f32,
        max_frames: usize,
        seed: u64,
    ) -> Result<Self, NodeError> {
        let connected_shape =
            |port: usize| connected.iter().find(|(p, _)| *p == port).map(|(_, s)| *s);

        let layout = node_type.layout(config)?;
        let input_shapes: Vec<Shape> = (0..layout.inputs.len())
            .map(|port| connected_shape(port).unwrap_or(Shape::MONO))
            .collect();
        let output_shapes = node_type.output_shapes(config, &layout, &input_shapes)?;
        assert_eq!(
            output_shapes.len(),
            layout.outputs.len(),
            "output_shapes must return one shape per output"
        );

        let instance = node_type.instantiate(&Setup {
            node: noodle_core::NodeId(0),
            config,
            sample_rate,
            max_frames,
            input_shapes: &input_shapes,
            output_shapes: &output_shapes,
            seed,
        })?;
        assert_eq!(
            instance.mode(),
            layout.mode,
            "instance mode doesn't match layout"
        );

        let inputs = layout
            .inputs
            .iter()
            .enumerate()
            .map(|(port, info)| {
                let mut buffer = SignalBuffer::new(input_shapes[port], max_frames);
                let value = connected_shape(port)
                    .is_none()
                    .then(|| info.default_value());
                if let Some(value) = value {
                    buffer.as_out(max_frames).fill(value);
                }
                HarnessInput { buffer, value }
            })
            .collect();

        Ok(Self {
            instance,
            inputs,
            outputs: output_shapes
                .iter()
                .map(|&shape| SignalBuffer::new(shape, max_frames))
                .collect(),
            event_inputs: layout.event_inputs.iter().map(|_| Vec::new()).collect(),
            event_outputs: layout
                .event_outputs
                .iter()
                .map(|_| Vec::with_capacity(1024))
                .collect(),
            sample_rate,
            position: 0,
            frames: 0,
        })
    }

    /// Sets an unconnected input's value. Panics if the input is connected.
    pub fn set(&mut self, port: usize, value: f32) {
        let input = &mut self.inputs[port];
        assert!(
            input.value.is_some(),
            "input {port} is connected; write to it with `input`"
        );
        input.value = Some(value);
        let frames = input.buffer.max_frames();
        input.buffer.as_out(frames).fill(value);
    }

    /// A connected input's buffer, to fill before the next `run(frames)`.
    pub fn input(&mut self, port: usize, frames: usize) -> SignalOut<'_> {
        let input = &mut self.inputs[port];
        assert!(input.value.is_none(), "input {port} isn't connected");
        input.buffer.as_out(frames)
    }

    /// Sets the events an event input receives. They stay for every following
    /// `run` until replaced, so clear them (`&[]`) after the block they belong
    /// to. Times are frame offsets within the block.
    pub fn send_events(&mut self, port: usize, events: &[Event]) {
        self.event_inputs[port] = events.to_vec();
    }

    /// Processes one block. For an offline node, renders `frames` frames as
    /// the whole range.
    pub fn run(&mut self, frames: usize) -> Result<(), Cancelled> {
        let inputs: Vec<SignalIn<'_>> = self
            .inputs
            .iter()
            .map(|input| {
                let signal = input.buffer.as_in(frames);
                match input.value {
                    Some(value) => signal.with_constant(value),
                    None => signal,
                }
            })
            .collect();
        let mut outputs: Vec<SignalOut<'_>> = self
            .outputs
            .iter_mut()
            .map(|buffer| buffer.as_out(frames))
            .collect();
        let event_inputs: Vec<&[Event]> = self.event_inputs.iter().map(Vec::as_slice).collect();
        let mut event_outputs: Vec<EventsOut<'_>> =
            self.event_outputs.iter_mut().map(EventsOut::new).collect();

        let io = Io {
            inputs: &inputs,
            outputs: &mut outputs,
            event_inputs: &event_inputs,
            event_outputs: &mut event_outputs,
        };
        let ctx = Context {
            sample_rate: self.sample_rate,
            frames,
            transport: Transport {
                playing: true,
                position: self.position,
                ..Transport::default()
            },
        };
        match &mut self.instance {
            Instance::Realtime(node) => node.process(&ctx, io),
            Instance::Offline(node) => node.render(&ctx, io, &Progress::new())?,
        }

        self.position += frames as u64;
        self.frames = frames;
        Ok(())
    }

    /// The events an event output wrote in the last run.
    pub fn events(&self, port: usize) -> &[Event] {
        &self.event_outputs[port]
    }

    /// An output from the last run.
    pub fn output(&self, port: usize) -> SignalIn<'_> {
        self.outputs[port].as_in(self.frames)
    }
}
