use noodle_engine::{
    Cancelled, Config, Context, Instance, Io, Layout, NodeError, NodeInfo, NodeType, OfflineNode,
    ParamInfo, Ports, Progress, Setup, Unit,
};

/// Scales its input so the whole range reaches a chosen level: the loudest
/// sample (peak) or the average power (RMS). It needs the whole range before
/// it can know the gain, so it's an offline node and its output is always a
/// cached render.
///
/// All lanes share one gain, so a stereo image keeps its balance. The level
/// can be modulated: the gain that reaches it is then worked out for every
/// sample.
pub struct Normalize;

pub const NORMALIZE_ID: &str = "noodle.offline.normalize";

#[derive(Ports)]
#[ports(offline)]
struct NormalizePorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "level",
        "Level",
        ParamInfo::new(-60.0, 12.0, -1.0).unit(Unit::Decibels)
    )]
    level: (),
    #[param("mode", "Measure", ParamInfo::choice(["Peak", "RMS"]))]
    mode: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = NormalizePorts::INPUT;
const LEVEL: usize = NormalizePorts::LEVEL;
const MODE: usize = NormalizePorts::MODE;
const OUT: usize = NormalizePorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: NORMALIZE_ID,
    version: 1,
    name: "Normalize",
    category: "Offline",
};

impl NodeType for Normalize {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(NormalizePorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::offline(NormalizeNode))
    }
}

struct NormalizeNode;

/// Below this the input counts as silence (-180 dB), and passes through.
const SILENT: f64 = 1e-9;

impl OfflineNode for NormalizeNode {
    fn render(
        &mut self,
        ctx: &Context,
        io: Io<'_, '_>,
        progress: &Progress,
    ) -> Result<(), Cancelled> {
        let input = io.inputs[IN];
        let level = io.inputs[LEVEL].lane(0, 0);
        let rms = io.inputs[MODE]
            .lane(0, 0)
            .first()
            .is_some_and(|&m| m >= 0.5);
        let out = &mut io.outputs[OUT];
        let shape = out.shape();
        let lanes = shape.lanes();

        // One measurement over every lane, so the lanes keep their balance.
        let mut peak = 0.0f64;
        let mut power = 0.0f64;
        for lane in 0..lanes {
            let samples = input.lane(lane / shape.channels, lane % shape.channels);
            for &x in samples {
                let x = f64::from(if x.is_finite() { x } else { 0.0 });
                peak = peak.max(x.abs());
                power += x * x;
            }
            progress.report(0.5 * (lane + 1) as f32 / lanes as f32)?;
        }
        let measured = if rms {
            (power / (ctx.frames.max(1) * lanes.max(1)) as f64).sqrt()
        } else {
            peak
        };

        for lane in 0..lanes {
            let (voice, channel) = (lane / shape.channels, lane % shape.channels);
            let samples = out.lane_mut(voice, channel);
            let source = input.lane(voice, channel);
            if measured < SILENT {
                samples.copy_from_slice(source);
            } else {
                for ((out, &x), &db) in samples.iter_mut().zip(source).zip(level) {
                    let target = 10f64.powf(f64::from(db) / 20.0);
                    let x = if x.is_finite() { x } else { 0.0 };
                    *out = (f64::from(x) * target / measured) as f32;
                }
            }
            progress.report(0.5 + 0.5 * (lane + 1) as f32 / lanes as f32)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    const FRAMES: usize = 8;

    fn harness(shape: Shape) -> Harness {
        Harness::new(&Normalize, &Config::new(), &[(IN, shape)], 48_000.0, FRAMES).unwrap()
    }

    #[test]
    fn the_peak_reaches_the_level() {
        let mut h = harness(Shape::MONO);
        h.set(LEVEL, -6.0);
        h.input(IN, FRAMES)
            .lane_mut(0, 0)
            .copy_from_slice(&[0.0, 0.1, -0.25, 0.05, 0.0, 0.0, 0.0, 0.0]);
        h.run(FRAMES).unwrap();
        let out = h.output(OUT).lane(0, 0).to_vec();
        let peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!((peak - 10f32.powf(-6.0 / 20.0)).abs() < 1e-6, "{peak}");
        // The shape is kept: the same ratios, the same sign.
        assert!((out[1] / out[2] + 0.4).abs() < 1e-6);
    }

    #[test]
    fn lanes_share_one_gain() {
        let mut h = harness(Shape::STEREO);
        h.set(LEVEL, 0.0);
        let mut input = h.input(IN, FRAMES);
        input.lane_mut(0, 0).fill(0.5);
        input.lane_mut(0, 1).fill(0.125);
        h.run(FRAMES).unwrap();
        assert!((h.output(OUT).lane(0, 0)[0] - 1.0).abs() < 1e-6);
        assert!((h.output(OUT).lane(0, 1)[0] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn rms_mode_measures_average_power() {
        let mut h = harness(Shape::MONO);
        h.set(LEVEL, -20.0);
        h.set(MODE, 1.0);
        // Alternating ±0.5: an RMS of 0.5.
        let input: Vec<f32> = (0..FRAMES)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        h.input(IN, FRAMES).lane_mut(0, 0).copy_from_slice(&input);
        h.run(FRAMES).unwrap();
        for (out, x) in h.output(OUT).lane(0, 0).iter().zip(&input) {
            assert!((out - x * 0.2).abs() < 1e-6, "{out}");
        }
    }

    #[test]
    fn silence_passes_through() {
        let mut h = harness(Shape::MONO);
        h.input(IN, FRAMES).lane_mut(0, 0).fill(0.0);
        h.run(FRAMES).unwrap();
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn a_modulated_level_moves_the_gain() {
        let mut h = Harness::new(
            &Normalize,
            &Config::new(),
            &[(IN, Shape::MONO), (LEVEL, Shape::MONO)],
            48_000.0,
            FRAMES,
        )
        .unwrap();
        h.input(IN, FRAMES).lane_mut(0, 0).fill(0.5);
        let level: Vec<f32> = (0..FRAMES)
            .map(|i| if i < FRAMES / 2 { 0.0 } else { -6.0 })
            .collect();
        h.input(LEVEL, FRAMES)
            .lane_mut(0, 0)
            .copy_from_slice(&level);
        h.run(FRAMES).unwrap();
        let out = h.output(OUT).lane(0, 0);
        assert!((out[0] - 1.0).abs() < 1e-6);
        assert!((out[FRAMES - 1] - 10f32.powf(-6.0 / 20.0)).abs() < 1e-6);
    }
}
