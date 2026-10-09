use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Lane, LaneKernel, Layout, Level, MeterWriter, Node,
    NodeError, NodeInfo, NodeType, PerLane, Setup, Telemetry,
};

use crate::meter::voice_sum;

/// Sums its inputs. How many inputs it has is config rather than a parameter,
/// because it changes the node's ports.
///
/// It also reports each input's peak and RMS level through [`Telemetry`], one
/// meter channel per input, for the mixer view and the node's own meters.
pub struct Mix {
    telemetry: Telemetry,
}

impl Mix {
    pub fn new(telemetry: &Telemetry) -> Self {
        Self {
            telemetry: telemetry.clone(),
        }
    }
}

/// A mean square below this (-300 dB RMS) is flushed to zero.
const TINY: f32 = 1e-30;

/// How quickly the RMS level follows the signal, as for a VU meter.
const RMS_TIME_SECONDS: f32 = 0.3;

const INPUTS: ConfigInfo = ConfigInfo::int("inputs", "Inputs", 2);
const MAX_INPUTS: i64 = 64;
const OUT: usize = 0;

static CONFIG: [ConfigInfo; 1] = [INPUTS];

static INFO: NodeInfo = NodeInfo {
    id: "noodle.util.mix",
    version: 1,
    name: "Mix",
    category: "Utilities",
};

impl NodeType for Mix {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let inputs = INPUTS.get_int(config);
        if !(1..=MAX_INPUTS).contains(&inputs) {
            return Err(NodeError::config(format!(
                "Mix needs between 1 and {MAX_INPUTS} inputs, not {inputs}"
            )));
        }
        let layout = (1..=inputs).fold(Layout::realtime(), |layout, i| {
            layout.input(format!("in{i}"), format!("In {i}"))
        });
        Ok(layout.output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let inputs = setup.input_shapes.len();
        Ok(Instance::realtime(MixNode {
            sum: PerLane::new(MixKernel { inputs }, setup),
            writer: self.telemetry.open_meter(setup.node, inputs),
            mean_squares: vec![0.0; inputs].into_boxed_slice(),
            coefficient: 1.0 - (-1.0 / (RMS_TIME_SECONDS * setup.sample_rate)).exp(),
        }))
    }
}

struct MixNode {
    sum: PerLane<MixKernel>,
    writer: MeterWriter,
    /// The smoothed mean square of each input, all its channels together.
    mean_squares: Box<[f32]>,
    coefficient: f32,
}

impl Node for MixNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        for (port, mean_square) in self.mean_squares.iter_mut().enumerate() {
            let input = io.inputs[port];
            let channels = input.shape().channels;
            let mut peak = 0.0f32;
            for frame in 0..ctx.frames {
                // Voices are summed, then the channels' power is averaged.
                let mut power = 0.0;
                for channel in 0..channels {
                    let x = voice_sum(&input, channel, frame);
                    peak = peak.max(x.abs());
                    power += x * x;
                }
                let power = power / channels.max(1) as f32;
                *mean_square += self.coefficient * (power - *mean_square);
            }
            if !mean_square.is_finite() || *mean_square < TINY {
                *mean_square = 0.0;
            }
            self.writer.write(
                port,
                Level {
                    peak,
                    rms: mean_square.sqrt(),
                },
            );
        }
        self.sum.process(ctx, io);
    }

    fn reset(&mut self) {
        self.mean_squares.fill(0.0);
        self.sum.reset();
    }
}

struct MixKernel {
    inputs: usize,
}

impl LaneKernel for MixKernel {
    type State = ();

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let out = lane.outputs.get_mut(OUT);
        out.copy_from_slice(lane.inputs.get(0));
        for port in 1..self.inputs {
            for (o, x) in out.iter_mut().zip(lane.inputs.get(port)) {
                *o += x;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{Shape, Value};

    #[test]
    fn sums_a_configured_number_of_inputs() {
        let config = Config::new().with("inputs", Value::Int(3));
        let connected = [(0, Shape::MONO), (1, Shape::MONO), (2, Shape::MONO)];
        let mut h = Harness::new(
            &Mix::new(&Telemetry::new()),
            &config,
            &connected,
            48_000.0,
            4,
        )
        .unwrap();
        for (port, value) in [(0, 1.0), (1, 2.0), (2, 3.0)] {
            h.input(port, 4).fill(value);
        }
        h.run(4).unwrap();
        assert_eq!(h.output(OUT).lane(0, 0), &[6.0; 4]);
    }

    #[test]
    fn reports_each_inputs_level_apart_from_the_sum() {
        let telemetry = Telemetry::new();
        let node = noodle_engine::NodeId(0);
        let config = Config::new().with("inputs", Value::Int(2));
        let connected = [(0, Shape::MONO), (1, Shape::MONO)];
        let mut h = Harness::new(&Mix::new(&telemetry), &config, &connected, 48_000.0, 4).unwrap();
        h.input(0, 4).fill(0.5);
        h.input(1, 4).fill(-0.25);
        h.run(4).unwrap();
        let levels = telemetry.meter_reader().meter(node).unwrap();
        assert_eq!(levels.len(), 2, "one channel per input");
        assert_eq!(levels[0].peak, 0.5);
        assert_eq!(levels[1].peak, 0.25);
        assert!(levels[0].rms > levels[1].rms && levels[1].rms > 0.0);
        assert_eq!(h.output(OUT).lane(0, 0), &[0.25; 4], "the sum is untouched");
    }

    #[test]
    fn defaults_to_two_inputs() {
        assert_eq!(
            Mix::new(&Telemetry::new())
                .layout(&Config::new())
                .unwrap()
                .inputs
                .len(),
            2
        );
    }

    #[test]
    fn rejects_zero_inputs() {
        let config = Config::new().with("inputs", Value::Int(0));
        assert!(matches!(
            Mix::new(&Telemetry::new()).layout(&config),
            Err(NodeError::Config(_))
        ));
    }
}
