use noodle_engine::{
    Config, Context, Instance, Io, Layout, Level, MeterWriter, Node, NodeError, NodeInfo, NodeType,
    Setup, SignalIn, Telemetry,
};

/// Reports its input's peak and RMS level, per channel, through
/// [`Telemetry`]. Voices are summed first, as the Output node does.
pub struct Meter {
    telemetry: Telemetry,
}

impl Meter {
    pub fn new(telemetry: &Telemetry) -> Self {
        Self {
            telemetry: telemetry.clone(),
        }
    }
}

/// The Meter's type ID.
pub const METER_ID: &str = "noodle.view.meter";

const IN: usize = 0;

/// A mean square below this (-300 dB RMS) is flushed to zero.
const TINY: f32 = 1e-30;

/// How quickly the RMS level follows the signal, as for a VU meter.
const RMS_TIME_SECONDS: f32 = 0.3;

static INFO: NodeInfo = NodeInfo {
    id: METER_ID,
    version: 1,
    name: "Meter",
    category: "Views",
};

impl NodeType for Meter {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let channels = setup.input_shapes[IN].channels;
        Ok(Instance::realtime(MeterNode {
            writer: self.telemetry.open_meter(setup.node, channels),
            mean_squares: vec![0.0; channels].into_boxed_slice(),
            coefficient: 1.0 - (-1.0 / (RMS_TIME_SECONDS * setup.sample_rate)).exp(),
        }))
    }
}

struct MeterNode {
    writer: MeterWriter,
    /// The smoothed mean square of each channel.
    mean_squares: Box<[f32]>,
    /// The one-pole smoothing coefficient per sample.
    coefficient: f32,
}

impl Node for MeterNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let input = &io.inputs[IN];
        for (channel, mean_square) in self.mean_squares.iter_mut().enumerate() {
            let mut peak = 0.0f32;
            for frame in 0..ctx.frames {
                let x = voice_sum(input, channel, frame);
                peak = peak.max(x.abs());
                *mean_square += self.coefficient * (x * x - *mean_square);
            }
            // Recover from a NaN or infinity once the input does.
            // Flush a level too small to matter, so silence doesn't leave it
            // decaying through the slow subnormals (see `svf.rs`).
            if !mean_square.is_finite() || *mean_square < TINY {
                *mean_square = 0.0;
            }
            let level = Level {
                peak,
                rms: mean_square.sqrt(),
            };
            self.writer.write(channel, level);
        }
    }

    fn reset(&mut self) {
        self.mean_squares.fill(0.0);
    }
}

/// One sample of `channel` with every voice summed.
pub(crate) fn voice_sum(input: &SignalIn<'_>, channel: usize, frame: usize) -> f32 {
    (0..input.shape().voices)
        .map(|voice| input.lane(voice, channel)[frame])
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;
    use noodle_engine::{NodeId, Shape};

    const NODE: NodeId = NodeId(0);

    #[test]
    fn measures_each_channel_with_voices_summed() {
        let telemetry = Telemetry::new();
        let shape = Shape::new(2, 2);
        let frames = 48_000;
        let mut h = Harness::new(
            &Meter::new(&telemetry),
            &Config::new(),
            &[(IN, shape)],
            48_000.0,
            frames,
        )
        .unwrap();
        // Left: a 0.25 square wave in each voice, so 0.5 summed. Right: silent
        // but for one spike.
        let mut input = h.input(IN, frames);
        for voice in 0..2 {
            for (i, x) in input.lane_mut(voice, 0).iter_mut().enumerate() {
                *x = if i % 2 == 0 { 0.25 } else { -0.25 };
            }
        }
        input.lane_mut(1, 1)[100] = -0.8;
        // Several RMS time constants, so the level has settled.
        for _ in 0..3 {
            h.run(frames).unwrap();
        }

        let levels = telemetry.meter(NODE).unwrap();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].peak, 0.5);
        assert!((levels[0].rms - 0.5).abs() < 1e-3, "{:?}", levels[0]);
        assert_eq!(levels[1].peak, 0.8);
        assert!(levels[1].rms < 0.01, "{:?}", levels[1]);
    }

    #[test]
    fn recovers_from_non_finite_input() {
        let telemetry = Telemetry::new();
        let mut h = Harness::new(
            &Meter::new(&telemetry),
            &Config::new(),
            &[(IN, Shape::MONO)],
            48_000.0,
            4,
        )
        .unwrap();
        h.input(IN, 4).fill(f32::INFINITY);
        h.run(4).unwrap();
        h.input(IN, 4).fill(0.5);
        h.run(4).unwrap();
        let level = telemetry.meter(NODE).unwrap()[0];
        assert!(level.rms.is_finite() && level.rms > 0.0, "{level:?}");
    }

    /// Without the engine's flush-to-zero, as the harness runs it.
    #[test]
    fn silence_decays_to_zero_not_to_subnormals() {
        let telemetry = Telemetry::new();
        // A low rate, so the level decays past the subnormals in few samples.
        let rate = 1000.0;
        let frames = 1000;
        let mut h = Harness::new(
            &Meter::new(&telemetry),
            &Config::new(),
            &[(IN, Shape::MONO)],
            rate,
            frames,
        )
        .unwrap();
        h.input(IN, frames).fill(1.0);
        h.run(frames).unwrap();
        h.input(IN, frames).fill(0.0);
        // 60 s: 200 time constants, far past where f32 underflows.
        for _ in 0..60 {
            h.run(frames).unwrap();
        }
        assert_eq!(telemetry.meter(NODE).unwrap()[0].rms, 0.0);
    }
}
