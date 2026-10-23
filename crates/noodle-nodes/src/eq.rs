//! The Parametric EQ node.

use std::f32::consts::TAU;

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType,
    ParamInfo, PerLane, Setup, Skip, Unit,
};

/// A chain of biquad bands, each a bell, shelf, cut or notch, in series. How
/// many bands (`bands`, 1 to 8) is config, since each has its own ports.
///
/// Each band has a `type`, a `freq`, a `gain` in dB (bells and shelves only)
/// and a `q` (the bell's width, the shelf's steepness, the cuts' resonance).
/// A bell or shelf at 0 dB does nothing, so a new band is transparent until
/// its gain moves. The filters are the RBJ cookbook's, so what
/// [`eq_response_db`] draws is what they do.
///
/// The coefficients follow the parameters every 32 samples when they are
/// modulated, and once per block when they are not.
pub struct ParametricEq;

pub const EQ_ID: &str = "noodle.effect.eq";

const BANDS: ConfigInfo = ConfigInfo::int("bands", "Bands", 4);
pub const MAX_BANDS: i64 = 8;

static CONFIG: [ConfigInfo; 1] = [BANDS];

static INFO: NodeInfo = NodeInfo {
    id: EQ_ID,
    version: 1,
    name: "Parametric EQ",
    category: "Effects",
};

/// What a band does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EqType {
    Bell,
    LowShelf,
    HighShelf,
    /// A high-pass filter.
    LowCut,
    /// A low-pass filter.
    HighCut,
    Notch,
}

pub const EQ_TYPES: [&str; 6] = [
    "Bell",
    "Low shelf",
    "High shelf",
    "Low cut",
    "High cut",
    "Notch",
];

impl EqType {
    pub fn from_value(value: f32) -> Self {
        match value.round() as i32 {
            ..=0 => Self::Bell,
            1 => Self::LowShelf,
            2 => Self::HighShelf,
            3 => Self::LowCut,
            4 => Self::HighCut,
            _ => Self::Notch,
        }
    }
}

/// One band's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqBand {
    pub kind: EqType,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
}

/// The index of a parameter of band `band` (from 0) among the node's inputs:
/// the audio input is 0, then each band's `type`, `freq`, `gain`, `q`.
pub const fn param_index(band: usize, which: usize) -> usize {
    1 + band * 4 + which
}
pub const TYPE: usize = 0;
pub const FREQ: usize = 1;
pub const GAIN: usize = 2;
pub const Q: usize = 3;

/// The default centre of band `band` (from 0): 100 Hz, then two octaves up
/// each time.
pub fn default_freq(band: usize) -> f32 {
    (100.0 * 4f32.powi(band as i32)).min(16_000.0)
}

impl ParametricEq {
    fn bands(config: &Config) -> Result<usize, NodeError> {
        let bands = BANDS.get_int(config);
        if (1..=MAX_BANDS).contains(&bands) {
            Ok(bands as usize)
        } else {
            Err(NodeError::config(format!(
                "Parametric EQ needs between 1 and {MAX_BANDS} bands, not {bands}"
            )))
        }
    }
}

impl NodeType for ParametricEq {
    fn view(&self, _config: &Config) -> noodle_engine::NodeView {
        noodle_engine::NodeView::Graph(noodle_engine::GraphKind::Eq)
    }

    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        let bands = Self::bands(config)?;
        let mut layout = Layout::realtime().input("in", "In");
        for n in 0..bands {
            let i = n + 1;
            layout = layout
                .param(
                    format!("type{i}"),
                    format!("Type {i}"),
                    ParamInfo::choice(EQ_TYPES),
                )
                .param(
                    format!("freq{i}"),
                    format!("Frequency {i}"),
                    ParamInfo::new(20.0, 20_000.0, default_freq(n))
                        .log()
                        .unit(Unit::Hertz)
                        .offset(),
                )
                .param(
                    format!("gain{i}"),
                    format!("Gain {i}"),
                    ParamInfo::new(-24.0, 24.0, 0.0)
                        .unit(Unit::Decibels)
                        .offset()
                        .featured(),
                )
                .param(
                    format!("q{i}"),
                    format!("Q {i}"),
                    ParamInfo::new(0.1, 18.0, std::f32::consts::FRAC_1_SQRT_2)
                        .log()
                        .offset(),
                );
        }
        Ok(layout.output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let bands = Self::bands(setup.config)?;
        Ok(Instance::realtime(PerLane::new(EqKernel { bands }, setup)))
    }
}

/// Biquad coefficients, normalised so `a0` is 1.
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    const THROUGH: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn new(band: EqBand, sample_rate: f32) -> Self {
        let freq = band.freq.clamp(10.0, sample_rate * 0.49);
        let q = band.q.clamp(0.05, 50.0);
        let gain = band.gain_db.clamp(-48.0, 48.0);
        let w0 = TAU * freq / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a = 10f32.powf(gain / 40.0);
        let root = 2.0 * a.sqrt() * alpha;
        let (b0, b1, b2, a0, a1, a2) = match band.kind {
            EqType::Bell => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            EqType::LowShelf => (
                a * ((a + 1.0) - (a - 1.0) * cos + root),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - root),
                (a + 1.0) + (a - 1.0) * cos + root,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - root,
            ),
            EqType::HighShelf => (
                a * ((a + 1.0) + (a - 1.0) * cos + root),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - root),
                (a + 1.0) - (a - 1.0) * cos + root,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - root,
            ),
            EqType::LowCut => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            EqType::HighCut => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            EqType::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// The gain (linear) at `hz`.
    fn magnitude(&self, hz: f32, sample_rate: f32) -> f32 {
        let w = TAU * hz.clamp(0.0, sample_rate * 0.5) / sample_rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let num = ((self.b0 + self.b1 * c1 + self.b2 * c2).powi(2)
            + (self.b1 * s1 + self.b2 * s2).powi(2))
        .sqrt();
        let den = ((1.0 + self.a1 * c1 + self.a2 * c2).powi(2)
            + (self.a1 * s1 + self.a2 * s2).powi(2))
        .sqrt();
        num / den.max(1e-12)
    }
}

/// The combined gain of `bands` in series at `hz`, in dB, for drawing the
/// curve.
pub fn eq_response_db(bands: &[EqBand], hz: f32, sample_rate: f32) -> f32 {
    let gain: f32 = bands
        .iter()
        .map(|&band| Biquad::new(band, sample_rate).magnitude(hz, sample_rate))
        .product();
    20.0 * gain.max(1e-9).log10()
}

/// How often modulated coefficients are refreshed, in samples.
const CONTROL_RATE: usize = 32;

struct EqKernel {
    bands: usize,
}

/// Transposed direct form II memory for each band.
#[derive(Default)]
struct EqState {
    z: [[f32; 2]; MAX_BANDS as usize],
}

impl LaneKernel for EqKernel {
    type State = EqState;

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[0])
    }

    fn is_idle(&self, state: &EqState) -> bool {
        state
            .z
            .iter()
            .all(|z| z[0].abs() < 1e-7 && z[1].abs() < 1e-7)
    }

    fn process_lane(&mut self, state: &mut EqState, ctx: &Context, lane: Lane<'_, '_>) {
        let Lane {
            inputs,
            mut outputs,
            ..
        } = lane;
        let input = inputs.get(0);
        let out = outputs.get_mut(0);
        let constant = (0..self.bands)
            .all(|n| (TYPE..=Q).all(|which| inputs.constant(param_index(n, which)).is_some()));
        let step = if constant {
            input.len().max(1)
        } else {
            CONTROL_RATE
        };

        let mut coefficients = [Biquad::THROUGH; MAX_BANDS as usize];
        let mut start = 0;
        while start < input.len() {
            let end = (start + step).min(input.len());
            for (n, c) in coefficients.iter_mut().enumerate().take(self.bands) {
                let at = |which: usize| inputs.get(param_index(n, which))[start];
                *c = Biquad::new(
                    EqBand {
                        kind: EqType::from_value(at(TYPE)),
                        freq: at(FREQ),
                        gain_db: at(GAIN),
                        q: at(Q),
                    },
                    ctx.sample_rate,
                );
            }
            for i in start..end {
                let mut x = input[i];
                for (c, z) in coefficients.iter().zip(&mut state.z).take(self.bands) {
                    let y = c.b0 * x + z[0];
                    z[0] = c.b1 * x - c.a1 * y + z[1];
                    z[1] = c.b2 * x - c.a2 * y;
                    x = y;
                }
                out[i] = x;
            }
            start = end;
        }
        // Reset memory an infinite input made non-finite, and flush the tiny
        // so it doesn't decay into subnormals.
        for z in state.z.iter_mut().flatten() {
            if !z.is_finite() || z.abs() < 1e-30 {
                *z = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::Value;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;

    fn harness(bands: i64) -> Harness {
        let config = Config::new().with("bands", Value::Int(bands));
        Harness::new(&ParametricEq, &config, &[(0, Shape::MONO)], RATE, 4_800).unwrap()
    }

    /// The amplitude of a sine of `hz` through the EQ, in dB.
    fn measure(h: &mut Harness, hz: f32) -> f32 {
        let mut energy = 0.0;
        let mut count = 0;
        for block in 0..4 {
            for (i, x) in h.input(0, 4_800).lane_mut(0, 0).iter_mut().enumerate() {
                *x = (TAU * hz * (block * 4_800 + i) as f32 / RATE).sin();
            }
            h.run(4_800).unwrap();
            if block >= 2 {
                energy += h.output(0).lane(0, 0).iter().map(|x| x * x).sum::<f32>();
                count += 4_800;
            }
        }
        // A unit sine has a mean square of 0.5.
        10.0 * (2.0 * energy / count as f32).log10()
    }

    fn set_band(h: &mut Harness, n: usize, kind: EqType, freq: f32, gain: f32, q: f32) {
        h.set(param_index(n, TYPE), kind as i32 as f32);
        h.set(param_index(n, FREQ), freq);
        h.set(param_index(n, GAIN), gain);
        h.set(param_index(n, Q), q);
    }

    #[test]
    fn new_bands_change_nothing() {
        let mut h = harness(4);
        for hz in [50.0, 440.0, 5_000.0] {
            assert!(measure(&mut h, hz).abs() < 0.01, "{hz}");
        }
    }

    #[test]
    fn a_bell_boosts_at_its_centre_and_leaves_the_far_ends() {
        let mut h = harness(1);
        set_band(&mut h, 0, EqType::Bell, 1_000.0, 12.0, 2.0);
        assert!((measure(&mut h, 1_000.0) - 12.0).abs() < 0.05);
        assert!(measure(&mut h, 50.0).abs() < 0.3);
        assert!(measure(&mut h, 15_000.0).abs() < 0.5);
    }

    #[test]
    fn shelves_and_cuts_do_their_jobs() {
        let mut h = harness(1);
        set_band(&mut h, 0, EqType::LowShelf, 200.0, 9.0, 0.7);
        assert!((measure(&mut h, 30.0) - 9.0).abs() < 0.3);
        assert!(measure(&mut h, 8_000.0).abs() < 0.2);

        let mut h = harness(1);
        set_band(&mut h, 0, EqType::HighShelf, 4_000.0, -9.0, 0.7);
        assert!((measure(&mut h, 18_000.0) + 9.0).abs() < 0.5);
        assert!(measure(&mut h, 100.0).abs() < 0.2);

        let mut h = harness(1);
        set_band(&mut h, 0, EqType::LowCut, 1_000.0, 0.0, 0.707);
        assert!(
            (measure(&mut h, 1_000.0) + 3.0).abs() < 0.2,
            "-3 dB at the corner"
        );
        assert!(measure(&mut h, 100.0) < -18.0);

        let mut h = harness(1);
        set_band(&mut h, 0, EqType::HighCut, 1_000.0, 0.0, 0.707);
        assert!(measure(&mut h, 10_000.0) < -18.0);
        assert!(measure(&mut h, 100.0).abs() < 0.1);

        let mut h = harness(1);
        set_band(&mut h, 0, EqType::Notch, 1_000.0, 0.0, 4.0);
        assert!(measure(&mut h, 1_000.0) < -40.0);
    }

    #[test]
    fn bands_in_series_add_in_decibels() {
        let mut h = harness(2);
        set_band(&mut h, 0, EqType::Bell, 1_000.0, 6.0, 1.0);
        set_band(&mut h, 1, EqType::Bell, 1_000.0, 6.0, 1.0);
        assert!((measure(&mut h, 1_000.0) - 12.0).abs() < 0.05);
    }

    #[test]
    fn the_drawn_response_matches_the_filter() {
        let bands = [
            EqBand {
                kind: EqType::LowShelf,
                freq: 150.0,
                gain_db: 5.0,
                q: 0.8,
            },
            EqBand {
                kind: EqType::Bell,
                freq: 2_500.0,
                gain_db: -7.0,
                q: 3.0,
            },
        ];
        let mut h = harness(2);
        for (n, b) in bands.iter().enumerate() {
            set_band(&mut h, n, b.kind, b.freq, b.gain_db, b.q);
        }
        for hz in [60.0, 150.0, 900.0, 2_500.0, 7_000.0] {
            let drawn = eq_response_db(&bands, hz, RATE);
            let measured = measure(&mut h, hz);
            assert!(
                (drawn - measured).abs() < 0.05,
                "{hz}: {drawn} vs {measured}"
            );
        }
    }

    #[test]
    fn modulated_parameters_match_constant_ones() {
        let run = |connected: bool| {
            let config = Config::new().with("bands", Value::Int(1));
            let wired = [(0, Shape::MONO), (param_index(0, FREQ), Shape::MONO)];
            let mut h = Harness::new(
                &ParametricEq,
                &config,
                if connected { &wired } else { &wired[..1] },
                RATE,
                480,
            )
            .unwrap();
            if !connected {
                set_band(&mut h, 0, EqType::Bell, 800.0, 10.0, 1.0);
            } else {
                h.set(param_index(0, TYPE), 0.0);
                h.set(param_index(0, GAIN), 10.0);
                h.set(param_index(0, Q), 1.0);
                h.input(param_index(0, FREQ), 480).fill(800.0);
            }
            for (i, x) in h.input(0, 480).lane_mut(0, 0).iter_mut().enumerate() {
                *x = (i as f32 * 0.1).sin();
            }
            h.run(480).unwrap();
            h.output(0).lane(0, 0).to_vec()
        };
        assert_eq!(run(false), run(true));
    }

    #[test]
    fn it_recovers_from_an_infinite_input() {
        let mut h = harness(1);
        set_band(&mut h, 0, EqType::Bell, 1_000.0, 6.0, 1.0);
        h.input(0, 64).lane_mut(0, 0)[3] = f32::INFINITY;
        h.run(64).unwrap();
        h.input(0, 64).fill(0.5);
        h.run(64).unwrap();
        h.input(0, 64).fill(0.5);
        h.run(64).unwrap();
        assert!(h.output(0).lane(0, 0).iter().all(|x| x.is_finite()));
    }

    #[test]
    fn a_bad_band_count_is_an_error() {
        let config = |n| Config::new().with("bands", Value::Int(n));
        assert!(ParametricEq.layout(&config(0)).is_err());
        assert!(ParametricEq.layout(&config(9)).is_err());
        assert_eq!(ParametricEq.layout(&config(3)).unwrap().inputs.len(), 13);
    }
}
