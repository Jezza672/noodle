use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Lane, LaneKernel, Layout, Node, NodeError, NodeInfo,
    NodeType, ParamInfo, PerLane, Setup, SignalIn, Skip, Telemetry, Unit,
};

use crate::meter::{LevelProbe, voice_sum};

/// Sums its inputs, each through its own gain and mute. How many inputs it
/// has is config rather than a parameter, because it changes the node's
/// ports.
///
/// The ports are the `inputs` audio inputs `in1`…`inN`, then the gains
/// `gain1`…`gainN` in dB, then the mutes `mute1`…`muteN`. The gain and mute
/// are parameters, so they can be wired or automated, and they are what the
/// mixer view's faders and mute buttons set.
///
/// It also reports each input's peak and RMS level, after its gain and mute,
/// through [`Telemetry`], one meter channel per input, for the mixer view and
/// the node's own meters.
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
        let layout = (1..=inputs).fold(layout, |layout, i| {
            layout.param(
                format!("gain{i}"),
                format!("Gain {i}"),
                ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels),
            )
        });
        let layout = (1..=inputs).fold(layout, |layout, i| {
            layout.param(
                format!("mute{i}"),
                format!("Mute {i}"),
                ParamInfo::choice(["Off", "On"]),
            )
        });
        Ok(layout.output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let inputs = INPUTS.get_int(setup.config).max(1) as usize;
        Ok(Instance::realtime(MixNode {
            sum: PerLane::new(MixKernel { inputs }, setup),
            probe: LevelProbe::new(&self.telemetry, setup.node, inputs, setup.sample_rate),
        }))
    }
}

/// The linear gain of an input with `db` of gain, silent if `mute` is on.
fn input_gain(db: f32, mute: f32) -> f32 {
    if mute >= 0.5 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

struct MixNode {
    sum: PerLane<MixKernel>,
    probe: LevelProbe,
}

impl Node for MixNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let inputs = self.probe.slots();
        for port in 0..inputs {
            let input = io.inputs[port];
            let channels = input.shape().channels;
            let (gain, mute) = (&io.inputs[inputs + port], &io.inputs[2 * inputs + port]);
            let fixed = gain
                .constant()
                .zip(mute.constant())
                .map(|(g, m)| input_gain(g, m));
            self.probe.measure(port, ctx.frames, |frame| {
                let k = fixed.unwrap_or_else(|| {
                    input_gain(modulation(gain, frame), modulation(mute, frame))
                });
                // Voices are summed, then the channels' power is averaged.
                let (mut power, mut peak) = (0.0, 0.0f32);
                for channel in 0..channels {
                    let x = voice_sum(&input, channel, frame) * k;
                    peak = peak.max(x.abs());
                    power += x * x;
                }
                (power / channels.max(1) as f32, peak)
            });
        }
        self.sum.process(ctx, io);
    }

    fn reset(&mut self) {
        self.probe.reset();
        self.sum.reset();
    }
}

/// A modulated parameter's value at `frame`, as its first lane has it.
fn modulation(signal: &SignalIn<'_>, frame: usize) -> f32 {
    signal.lane(0, 0)[frame]
}

struct MixKernel {
    inputs: usize,
}

impl LaneKernel for MixKernel {
    type State = ();

    fn skip(&self) -> Skip {
        Skip::AllSilentBelow(self.inputs)
    }

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        let n = self.inputs;
        let out = lane.outputs.get_mut(OUT);
        out.fill(0.0);
        for port in 0..n {
            let input = lane.inputs.get(port);
            // Gain and mute are rarely modulated, so usually work out the
            // gain once per block.
            match lane
                .inputs
                .constant(n + port)
                .zip(lane.inputs.constant(2 * n + port))
            {
                Some((db, mute)) => {
                    let k = input_gain(db, mute);
                    for (o, x) in out.iter_mut().zip(input) {
                        *o += x * k;
                    }
                }
                None => {
                    let (gain, mute) = (lane.inputs.get(n + port), lane.inputs.get(2 * n + port));
                    for (((o, x), db), mute) in out.iter_mut().zip(input).zip(gain).zip(mute) {
                        *o += x * input_gain(*db, *mute);
                    }
                }
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

    /// A harness for a mixer of `inputs` mono inputs, all wired.
    fn mixer(telemetry: &Telemetry, inputs: i64) -> Harness {
        let config = Config::new().with("inputs", Value::Int(inputs));
        let connected: Vec<_> = (0..inputs as usize).map(|i| (i, Shape::MONO)).collect();
        Harness::new(&Mix::new(telemetry), &config, &connected, 48_000.0, 4).unwrap()
    }

    #[test]
    fn each_input_has_its_own_gain_and_mute() {
        let telemetry = Telemetry::new();
        let mut h = mixer(&telemetry, 3);
        for port in 0..3 {
            h.input(port, 4).fill(1.0);
        }
        // Gains follow the inputs, then the mutes.
        h.set(3, -6.0206);
        h.set(4, 12.0);
        h.set(6 + 2, 1.0);
        h.run(4).unwrap();
        let expected = 0.5 + 10f32.powf(12.0 / 20.0);
        assert!(
            h.output(OUT)
                .lane(0, 0)
                .iter()
                .all(|x| (x - expected).abs() < 1e-3)
        );
    }

    #[test]
    fn the_meters_are_after_the_gain_and_mute() {
        let telemetry = Telemetry::new();
        let mut h = mixer(&telemetry, 2);
        h.input(0, 4).fill(1.0);
        h.input(1, 4).fill(1.0);
        h.set(2, -20.0);
        h.set(5, 1.0);
        h.run(4).unwrap();
        let levels = telemetry
            .meter_reader()
            .meter(noodle_engine::NodeId(0))
            .unwrap();
        assert!((levels[0].peak - 0.1).abs() < 1e-4);
        assert_eq!(levels[1].peak, 0.0);
    }

    #[test]
    fn modulated_gain_matches_constant_gain() {
        let telemetry = Telemetry::new();
        let config = Config::new().with("inputs", Value::Int(1));
        let connected = [(0, Shape::MONO), (1, Shape::MONO)];
        let mut h = Harness::new(&Mix::new(&telemetry), &config, &connected, 48_000.0, 4).unwrap();
        h.input(0, 4).fill(1.0);
        h.input(1, 4).fill(-20.0);
        h.run(4).unwrap();
        assert!(
            h.output(OUT)
                .lane(0, 0)
                .iter()
                .all(|x| (x - 0.1).abs() < 1e-4)
        );
        let levels = telemetry
            .meter_reader()
            .meter(noodle_engine::NodeId(0))
            .unwrap();
        assert!((levels[0].peak - 0.1).abs() < 1e-4);
    }

    #[test]
    fn defaults_to_two_inputs() {
        assert_eq!(
            Mix::new(&Telemetry::new())
                .layout(&Config::new())
                .unwrap()
                .inputs
                .iter()
                .filter(|port| port.key.starts_with("in"))
                .count(),
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

    #[test]
    fn a_lane_is_silent_only_when_every_input_is() {
        let config = Config::new().with("inputs", Value::Int(2));
        let poly = Shape::new(2, 1);
        let mut h = Harness::new(
            &Mix::new(&Telemetry::new()),
            &config,
            &[(0, poly), (1, poly)],
            48_000.0,
            4,
        )
        .unwrap();
        let mut a = h.input(0, 4);
        a.lane_mut(0, 0).fill(1.0);
        a.silence(1, 0);
        let mut b = h.input(1, 4);
        b.silence(0, 0);
        b.silence(1, 0);
        h.run(4).unwrap();
        assert_eq!(h.output(0).lane(0, 0), &[1.0; 4]);
        assert!(!h.output(0).is_silent(0, 0));
        assert!(h.output(0).is_silent(1, 0));
    }
}
