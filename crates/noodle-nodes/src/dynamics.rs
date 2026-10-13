//! The Dynamics node: compressor, limiter, gate and expander.

use noodle_engine::{
    Config, ConfigInfo, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType,
    ParamInfo, ParamWriter, Ports, Setup, Shape, ShapeError, Telemetry, Unit,
};

/// Turns the level of a signal into a gain, and applies it. One node covers
/// the whole family, chosen by `mode`:
///
/// - **Compressor**: above `threshold`, every `ratio` dB in comes out as 1 dB.
/// - **Limiter**: a compressor of infinite ratio; nothing rises above the
///   threshold (apart from the time `attack` takes, or `lookahead` hides).
/// - **Gate**: below the threshold the signal is closed, by up to 80 dB.
/// - **Expander**: below the threshold, every 1 dB down becomes `ratio` dB.
///
/// Compressor and limiter take gain away as the level goes up, so `attack` is
/// how fast they clamp down and `release` how fast they let go. A gate and an
/// expander take gain away as the level goes *down*, so for them `attack` is
/// how fast they open and `release` how fast they close. Both are the time the
/// gain takes to cover 63% of the distance, smoothed in decibels.
///
/// **Detection.** The level is the peak of the loudest channel in the detector
/// signal, which is the input, or the `sidechain` input when `detect` is
/// Sidechain, so one signal can duck another. All channels get the same gain,
/// so a stereo image doesn't move. Each voice has its own gain.
///
/// **Knee.** Over `knee` dB around the threshold the curve bends gently
/// instead of turning a corner.
///
/// **Makeup** is a fixed gain after the dynamics, in dB.
///
/// **Gain reduction** comes out of the second output in dB, positive when the
/// signal is turned down (zero when idle), one channel per voice, to meter or
/// to modulate with. The node also reports its input level and the reduction
/// to [`Telemetry`] as `level` and `reduction`, for the dot on its graph.
///
/// **Lookahead.** The audio is delayed by `lookahead` (milliseconds, 0 to 20)
/// while the detector sees it undelayed, so the gain is in place before a peak
/// arrives. The delay is reported as latency, and is zero when `lookahead` is
/// 0. It is config rather than a parameter because the plan fixes latency
/// before audio runs.
pub struct Dynamics {
    telemetry: Telemetry,
}

impl Dynamics {
    pub fn new(telemetry: &Telemetry) -> Self {
        Self {
            telemetry: telemetry.clone(),
        }
    }
}

pub const DYNAMICS_ID: &str = "noodle.effect.dynamics";

/// The most lookahead, in milliseconds.
pub const MAX_LOOKAHEAD_MS: i64 = 20;

const LOOKAHEAD: ConfigInfo = ConfigInfo::int("lookahead", "Lookahead (ms)", 0);

static CONFIG: [ConfigInfo; 1] = [LOOKAHEAD];

#[derive(Ports)]
struct DynamicsPorts {
    #[input("in", "In")]
    input: (),
    #[input("sidechain", "Sidechain")]
    sidechain: (),
    #[param(
        "mode",
        "Mode",
        ParamInfo::choice(["Compressor", "Limiter", "Gate", "Expander"])
    )]
    mode: (),
    #[param("detect", "Detect", ParamInfo::choice(["Input", "Sidechain"]))]
    detect: (),
    #[param(
        "threshold",
        "Threshold",
        ParamInfo::new(-80.0, 0.0, -18.0).unit(Unit::Decibels).offset()
    )]
    threshold: (),
    #[param("ratio", "Ratio", ParamInfo::new(1.0, 100.0, 4.0).log().offset())]
    ratio: (),
    #[param(
        "attack",
        "Attack",
        ParamInfo::new(0.0001, 0.5, 0.01).log().unit(Unit::Seconds).offset()
    )]
    attack: (),
    #[param(
        "release",
        "Release",
        ParamInfo::new(0.005, 5.0, 0.1).log().unit(Unit::Seconds).offset()
    )]
    release: (),
    #[param(
        "knee",
        "Knee",
        ParamInfo::new(0.0, 24.0, 6.0).unit(Unit::Decibels).offset()
    )]
    knee: (),
    #[param(
        "makeup",
        "Makeup",
        ParamInfo::new(-24.0, 36.0, 0.0).unit(Unit::Decibels).offset()
    )]
    makeup: (),
    #[output("out", "Out")]
    out: (),
    #[output("reduction", "Reduction")]
    reduction: (),
}

static INFO: NodeInfo = NodeInfo {
    id: DYNAMICS_ID,
    version: 1,
    name: "Dynamics",
    category: "Effects",
};

/// What `mode` selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicsMode {
    Compressor,
    Limiter,
    Gate,
    Expander,
}

impl DynamicsMode {
    /// From the parameter's value, rounded and clamped.
    pub fn from_value(value: f32) -> Self {
        match value.round() as i32 {
            ..=0 => Self::Compressor,
            1 => Self::Limiter,
            2 => Self::Gate,
            _ => Self::Expander,
        }
    }

    /// True if the gain falls as the level falls (a gate or expander), so
    /// `attack` governs the gain coming back up.
    pub fn is_downward(self) -> bool {
        matches!(self, Self::Gate | Self::Expander)
    }
}

/// The most a gate or expander turns the signal down, in dB.
const FLOOR_DB: f32 = 80.0;

/// The gain reduction in decibels (0 or more) that the static curve gives a
/// signal at `level_db`. This is the shape drawn as the transfer curve.
pub fn reduction_db(
    mode: DynamicsMode,
    threshold: f32,
    ratio: f32,
    knee: f32,
    level_db: f32,
) -> f32 {
    let ratio = ratio.max(1.0);
    // How many dB of reduction each dB past the threshold costs.
    let slope = match mode {
        DynamicsMode::Compressor => 1.0 - 1.0 / ratio,
        DynamicsMode::Limiter => 1.0,
        DynamicsMode::Expander => ratio - 1.0,
        DynamicsMode::Gate => FLOOR_DB,
    };
    let depth = if mode.is_downward() {
        threshold - level_db
    } else {
        level_db - threshold
    };
    let knee = knee.max(0.0);
    let past = if 2.0 * depth <= -knee {
        0.0
    } else if 2.0 * depth < knee {
        let x = depth + knee / 2.0;
        x * x / (2.0 * knee)
    } else {
        depth
    };
    (slope * past).clamp(0.0, FLOOR_DB)
}

/// The output level for an input level, on the static curve, before makeup.
pub fn transfer_db(
    mode: DynamicsMode,
    threshold: f32,
    ratio: f32,
    knee: f32,
    level_db: f32,
) -> f32 {
    level_db - reduction_db(mode, threshold, ratio, knee, level_db)
}

/// The gain reduction `seconds` after the level steps from where there is
/// none to where the curve asks for `target_db`, with the one-pole smoothing
/// the node uses, for drawing the attack. Use the release time and
/// [`release_response`] for the way back.
pub fn attack_response(target_db: f32, attack: f32, seconds: f32) -> f32 {
    if attack <= 0.0 {
        return target_db;
    }
    target_db * (1.0 - (-seconds / attack).exp())
}

/// The gain reduction `seconds` after the level steps back down from `from_db`
/// of reduction to none.
pub fn release_response(from_db: f32, release: f32, seconds: f32) -> f32 {
    if release <= 0.0 {
        return 0.0;
    }
    from_db * (-seconds / release).exp()
}

impl NodeType for Dynamics {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn config(&self) -> &[ConfigInfo] {
        &CONFIG
    }

    fn layout(&self, config: &Config) -> Result<Layout, NodeError> {
        lookahead_ms(config)?;
        Ok(DynamicsPorts::layout())
    }

    fn output_shapes(
        &self,
        _config: &Config,
        _layout: &Layout,
        inputs: &[Shape],
    ) -> Result<Vec<Shape>, NodeError> {
        let signal = inputs[DynamicsPorts::INPUT];
        let side = inputs[DynamicsPorts::SIDECHAIN];
        // The detector is read a voice at a time.
        if side.voices != 1 && side.voices != signal.voices {
            return Err(ShapeError(signal, side).into());
        }
        Ok(vec![signal, Shape::new(signal.voices, 1)])
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let shape = setup.output_shapes[DynamicsPorts::OUT];
        let lookahead =
            (lookahead_ms(setup.config)? as f32 * setup.sample_rate / 1_000.0).round() as usize;
        Ok(Instance::realtime(DynamicsNode {
            shape,
            sample_rate: setup.sample_rate,
            lookahead,
            lines: (0..shape.lanes())
                .map(|_| vec![0.0; lookahead.max(1)].into_boxed_slice())
                .collect(),
            at: 0,
            gain: vec![0.0; shape.voices],
            attack: Smoother::default(),
            release: Smoother::default(),
            level: vec![0.0; setup.max_frames].into_boxed_slice(),
            level_tap: self.telemetry.open_param(setup.node, "level"),
            reduction_tap: self.telemetry.open_param(setup.node, "reduction"),
        }))
    }
}

fn lookahead_ms(config: &Config) -> Result<i64, NodeError> {
    let ms = LOOKAHEAD.get_int(config);
    if (0..=MAX_LOOKAHEAD_MS).contains(&ms) {
        Ok(ms)
    } else {
        Err(NodeError::config(format!(
            "Dynamics lookahead is 0 to {MAX_LOOKAHEAD_MS} ms, not {ms}"
        )))
    }
}

/// The one-pole coefficient for a time, remembered until the time changes.
#[derive(Default)]
struct Smoother {
    time: f32,
    coefficient: f32,
}

impl Smoother {
    fn coefficient(&mut self, time: f32, sample_rate: f32) -> f32 {
        if time != self.time || self.coefficient == 0.0 && time > 0.0 {
            self.time = time;
            self.coefficient = if time > 0.0 {
                (-1.0 / (time * sample_rate)).exp()
            } else {
                0.0
            };
        }
        self.coefficient
    }
}

struct DynamicsNode {
    shape: Shape,
    sample_rate: f32,
    lookahead: usize,
    /// One delay line per lane, `lookahead` long.
    lines: Vec<Box<[f32]>>,
    /// Where the next sample is written in every line.
    at: usize,
    /// Each voice's gain reduction now, in dB.
    gain: Vec<f32>,
    attack: Smoother,
    release: Smoother,
    /// The first voice's detector level in dB, for the graph's dot.
    level: Box<[f32]>,
    level_tap: ParamWriter,
    reduction_tap: ParamWriter,
}

/// 20·log10(e), to turn `ln` into decibels.
const DB_PER_NEPER: f32 = 8.685_89;

fn amplitude_to_db(x: f32) -> f32 {
    DB_PER_NEPER * x.max(1e-9).ln()
}

fn db_to_amplitude(db: f32) -> f32 {
    (db / DB_PER_NEPER).exp()
}

impl Node for DynamicsNode {
    fn process(&mut self, ctx: &Context, io: Io<'_, '_>) {
        let frames = ctx.frames;
        let Io {
            inputs, outputs, ..
        } = io;
        let first = |port: usize| inputs[port].lane(0, 0).first().copied().unwrap_or(0.0);
        let mode = DynamicsMode::from_value(first(DynamicsPorts::MODE));
        let sidechain = first(DynamicsPorts::DETECT) >= 0.5;
        let channels = self.shape.channels;
        let downward = mode.is_downward();
        let [out, reduction] = outputs
            .get_disjoint_mut([DynamicsPorts::OUT, DynamicsPorts::REDUCTION])
            .expect("distinct outputs");

        for voice in 0..self.shape.voices {
            let detector = if sidechain {
                DynamicsPorts::SIDECHAIN
            } else {
                DynamicsPorts::INPUT
            };
            let param = |port: usize| inputs[port].lane(voice, 0);
            let (threshold, ratio, attack, release, knee, makeup) = (
                param(DynamicsPorts::THRESHOLD),
                param(DynamicsPorts::RATIO),
                param(DynamicsPorts::ATTACK),
                param(DynamicsPorts::RELEASE),
                param(DynamicsPorts::KNEE),
                param(DynamicsPorts::MAKEUP),
            );
            let all_silent =
                (0..channels).all(|c| inputs[DynamicsPorts::INPUT].is_silent(voice, c));
            if all_silent
                && !sidechain
                && !downward
                && self.gain[voice] < 1e-6
                && self.lookahead == 0
                && makeup.iter().all(|m| m.is_finite())
            {
                self.gain[voice] = 0.0;
                for c in 0..channels {
                    out.silence(voice, c);
                }
                reduction.lane_mut(voice, 0).fill(0.0);
                continue;
            }

            let mut gain = self.gain[voice];
            let red = reduction.lane_mut(voice, 0);
            for i in 0..frames {
                let mut peak = 0.0f32;
                for c in 0..channels {
                    let x = inputs[detector].lane(voice, c)[i].abs();
                    // A NaN would poison the gain for good.
                    if x.is_finite() {
                        peak = peak.max(x);
                    }
                }
                let level = amplitude_to_db(peak);
                let target = reduction_db(mode, threshold[i], ratio[i], knee[i], level);
                // For a gate the gain coming back up is the attack.
                let attacking = (target > gain) != downward;
                let time = if attacking { attack[i] } else { release[i] };
                let smoother = if attacking {
                    &mut self.attack
                } else {
                    &mut self.release
                };
                let a = smoother.coefficient(time, self.sample_rate);
                gain = target + a * (gain - target);
                if !gain.is_finite() {
                    gain = 0.0;
                }
                red[i] = gain;
                if voice == 0 {
                    self.level[i] = level;
                }
                let k = db_to_amplitude(makeup[i] - gain);
                for c in 0..channels {
                    let lane = voice * channels + c;
                    let x = inputs[DynamicsPorts::INPUT].lane(voice, c)[i];
                    let delayed = if self.lookahead == 0 {
                        x
                    } else {
                        let line = &mut self.lines[lane];
                        // The slot holds the sample from `lookahead` samples
                        // ago; the new one takes its place.
                        let slot = (self.at + i) % self.lookahead;
                        std::mem::replace(&mut line[slot], x)
                    };
                    out.lane_mut(voice, c)[i] = delayed * k;
                }
            }
            self.gain[voice] = gain;
            if voice == 0 {
                self.level_tap.write(&self.level[..frames]);
                self.reduction_tap
                    .write(&reduction.lane_mut(0, 0)[..frames]);
            }
        }
        if self.lookahead > 0 {
            self.at = (self.at + frames) % self.lookahead;
        }
    }

    fn latency(&self) -> u32 {
        self.lookahead as u32
    }

    fn reset(&mut self) {
        self.gain.fill(0.0);
        for line in &mut self.lines {
            line.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;
    const IN: usize = DynamicsPorts::INPUT;
    const SIDE: usize = DynamicsPorts::SIDECHAIN;
    const OUT: usize = DynamicsPorts::OUT;
    const RED: usize = DynamicsPorts::REDUCTION;

    fn harness(lookahead: i64, connected: &[(usize, Shape)]) -> Harness {
        let config = Config::new().with("lookahead", noodle_core::Value::Int(lookahead));
        Harness::new(
            &Dynamics::new(&Telemetry::new()),
            &config,
            connected,
            RATE,
            4_800,
        )
        .unwrap()
    }

    fn feed(h: &mut Harness, port: usize, amplitude: f32, blocks: usize) {
        for _ in 0..blocks {
            h.input(port, 4_800).fill(amplitude);
            h.run(4_800).unwrap();
        }
    }

    fn last(h: &Harness, port: usize) -> f32 {
        *h.output(port).lane(0, 0).last().unwrap()
    }

    #[test]
    fn the_curve_is_a_corner_without_a_knee() {
        use DynamicsMode::*;
        assert_eq!(reduction_db(Compressor, -20.0, 4.0, 0.0, -30.0), 0.0);
        assert_eq!(reduction_db(Compressor, -20.0, 4.0, 0.0, -20.0), 0.0);
        assert!((reduction_db(Compressor, -20.0, 4.0, 0.0, 0.0) - 15.0).abs() < 1e-5);
        assert!((reduction_db(Limiter, -6.0, 4.0, 0.0, 0.0) - 6.0).abs() < 1e-5);
        assert!((reduction_db(Expander, -40.0, 2.0, 0.0, -60.0) - 20.0).abs() < 1e-5);
        assert_eq!(reduction_db(Expander, -40.0, 2.0, 0.0, -30.0), 0.0);
        assert_eq!(reduction_db(Gate, -40.0, 2.0, 0.0, -100.0), FLOOR_DB);
        assert_eq!(reduction_db(Gate, -40.0, 2.0, 0.0, -30.0), 0.0);
    }

    #[test]
    fn the_knee_is_continuous_and_halfway_at_the_threshold() {
        use DynamicsMode::*;
        let r = |x| reduction_db(Compressor, -20.0, 4.0, 10.0, x);
        assert_eq!(r(-25.1), 0.0);
        assert!((r(-25.0) - 0.0).abs() < 1e-5);
        // At the threshold: slope · (W/2)² / 2W = 0.75 · 25 / 20.
        assert!((r(-20.0) - 0.75 * 25.0 / 20.0).abs() < 1e-5);
        assert!((r(-15.0) - 0.75 * 5.0).abs() < 1e-4, "joins the line");
        assert!((r(-15.01) - r(-14.99)).abs() < 0.05);
    }

    #[test]
    fn a_steady_loud_signal_settles_at_the_curve() {
        let mut h = harness(0, &[(IN, Shape::MONO)]);
        h.set(DynamicsPorts::THRESHOLD, -20.0);
        h.set(DynamicsPorts::RATIO, 4.0);
        h.set(DynamicsPorts::KNEE, 0.0);
        h.set(DynamicsPorts::ATTACK, 0.001);
        feed(&mut h, IN, 1.0, 3);
        assert!((last(&h, RED) - 15.0).abs() < 0.01, "{}", last(&h, RED));
        let expected = db_to_amplitude(-15.0);
        assert!((last(&h, OUT) - expected).abs() < 1e-3);
    }

    #[test]
    fn makeup_adds_gain() {
        let mut h = harness(0, &[(IN, Shape::MONO)]);
        h.set(DynamicsPorts::THRESHOLD, 0.0);
        h.set(DynamicsPorts::MAKEUP, 6.0206);
        feed(&mut h, IN, 0.25, 2);
        assert!((last(&h, OUT) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn it_releases_after_the_signal_stops() {
        let mut h = harness(0, &[(IN, Shape::MONO)]);
        h.set(DynamicsPorts::THRESHOLD, -20.0);
        h.set(DynamicsPorts::ATTACK, 0.001);
        h.set(DynamicsPorts::RELEASE, 0.01);
        feed(&mut h, IN, 1.0, 2);
        assert!(last(&h, RED) > 10.0);
        feed(&mut h, IN, 0.0, 2);
        assert!(last(&h, RED) < 0.01);
    }

    #[test]
    fn a_limiter_holds_the_peak_at_the_threshold() {
        let mut h = harness(0, &[(IN, Shape::MONO)]);
        h.set(DynamicsPorts::MODE, 1.0);
        h.set(DynamicsPorts::THRESHOLD, -6.0206);
        h.set(DynamicsPorts::ATTACK, 0.0001);
        h.set(DynamicsPorts::KNEE, 0.0);
        feed(&mut h, IN, 1.0, 2);
        assert!((last(&h, OUT) - 0.5).abs() < 2e-3, "{}", last(&h, OUT));
    }

    #[test]
    fn a_gate_closes_below_the_threshold_and_opens_above() {
        let mut h = harness(0, &[(IN, Shape::MONO)]);
        h.set(DynamicsPorts::MODE, 2.0);
        h.set(DynamicsPorts::THRESHOLD, -30.0);
        h.set(DynamicsPorts::KNEE, 0.0);
        h.set(DynamicsPorts::ATTACK, 0.001);
        h.set(DynamicsPorts::RELEASE, 0.005);
        feed(&mut h, IN, 0.001, 3);
        assert!(last(&h, OUT).abs() < 1e-5, "closed: {}", last(&h, OUT));
        feed(&mut h, IN, 0.5, 2);
        assert!(
            (last(&h, OUT) - 0.5).abs() < 1e-3,
            "open: {}",
            last(&h, OUT)
        );
    }

    #[test]
    fn an_expander_is_gentler_than_a_gate() {
        let mut h = harness(0, &[(IN, Shape::MONO)]);
        h.set(DynamicsPorts::MODE, 3.0);
        h.set(DynamicsPorts::THRESHOLD, -20.0);
        h.set(DynamicsPorts::RATIO, 2.0);
        h.set(DynamicsPorts::KNEE, 0.0);
        h.set(DynamicsPorts::RELEASE, 0.005);
        // -40 dB in: 20 below the threshold, so 20 dB more down.
        feed(&mut h, IN, 0.01, 3);
        assert!((last(&h, RED) - 20.0).abs() < 0.05, "{}", last(&h, RED));
    }

    #[test]
    fn the_sidechain_ducks_the_input() {
        let both = [(IN, Shape::MONO), (SIDE, Shape::MONO)];
        let mut h = harness(0, &both);
        h.set(DynamicsPorts::DETECT, 1.0);
        h.set(DynamicsPorts::THRESHOLD, -20.0);
        h.set(DynamicsPorts::ATTACK, 0.001);
        h.input(SIDE, 4_800).fill(0.0);
        h.input(IN, 4_800).fill(0.1);
        h.run(4_800).unwrap();
        assert!(
            (last(&h, OUT) - 0.1).abs() < 1e-4,
            "a quiet sidechain leaves it"
        );
        for _ in 0..2 {
            h.input(SIDE, 4_800).fill(1.0);
            h.input(IN, 4_800).fill(0.1);
            h.run(4_800).unwrap();
        }
        assert!(
            last(&h, OUT) < 0.03,
            "a loud one ducks it: {}",
            last(&h, OUT)
        );
    }

    #[test]
    fn stereo_channels_share_one_gain() {
        let mut h = harness(0, &[(IN, Shape::STEREO)]);
        h.set(DynamicsPorts::THRESHOLD, -20.0);
        h.set(DynamicsPorts::KNEE, 0.0);
        h.set(DynamicsPorts::ATTACK, 0.0001);
        for _ in 0..2 {
            h.input(IN, 4_800).lane_mut(0, 0).fill(1.0);
            h.input(IN, 4_800).lane_mut(0, 1).fill(0.1);
            h.run(4_800).unwrap();
        }
        let (l, r) = (
            *h.output(OUT).lane(0, 0).last().unwrap(),
            *h.output(OUT).lane(0, 1).last().unwrap(),
        );
        assert!((l / r - 10.0).abs() < 0.01, "the balance is kept: {l} {r}");
    }

    /// The latency a node reports, which the harness doesn't expose.
    fn latency_of(config: &Config) -> u32 {
        let node = Dynamics::new(&Telemetry::new());
        let layout = node.layout(config).unwrap();
        let inputs = vec![Shape::MONO; layout.inputs.len()];
        let outputs = node.output_shapes(config, &layout, &inputs).unwrap();
        let setup = Setup {
            node: noodle_core::NodeId(0),
            config,
            sample_rate: RATE,
            max_frames: 64,
            input_shapes: &inputs,
            output_shapes: &outputs,
            seed: 0,
            position: 0,
        };
        match node.instantiate(&setup).unwrap() {
            Instance::Realtime(n) => n.latency(),
            Instance::Offline(_) => 0,
        }
    }

    #[test]
    fn lookahead_delays_the_audio_and_reports_it() {
        let ms = |n| Config::new().with("lookahead", noodle_core::Value::Int(n));
        assert_eq!(latency_of(&ms(5)), 240);
        assert_eq!(latency_of(&ms(0)), 0);

        // An impulse comes out `lookahead` samples later, with its gain
        // already applied.
        let mut h = harness(5, &[(IN, Shape::MONO)]);
        h.set(DynamicsPorts::THRESHOLD, -20.0);
        h.set(DynamicsPorts::ATTACK, 0.0001);
        h.set(DynamicsPorts::KNEE, 0.0);
        let mut input = h.input(IN, 4_800);
        input.lane_mut(0, 0).fill(0.0);
        input.lane_mut(0, 0)[100] = 1.0;
        h.run(4_800).unwrap();
        let out = h.output(OUT).lane(0, 0);
        assert_eq!(out[..340].iter().filter(|x| **x != 0.0).count(), 0);
        assert!(
            out[340] > 0.0 && out[340] < 0.9,
            "turned down: {}",
            out[340]
        );
    }

    #[test]
    fn a_bad_lookahead_is_an_error() {
        let config = Config::new().with("lookahead", noodle_core::Value::Int(500));
        assert!(Dynamics::new(&Telemetry::new()).layout(&config).is_err());
    }

    #[test]
    fn silence_costs_nothing_and_stays_silent() {
        let mut h = harness(0, &[(IN, Shape::MONO)]);
        feed(&mut h, IN, 0.0, 2);
        assert!(h.output(OUT).lane(0, 0).iter().all(|&x| x == 0.0));
        assert!(h.output(RED).lane(0, 0).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn a_nan_does_not_poison_the_gain() {
        let mut h = harness(0, &[(IN, Shape::MONO)]);
        h.input(IN, 4_800).fill(f32::NAN);
        h.run(4_800).unwrap();
        feed(&mut h, IN, 0.1, 1);
        assert!(last(&h, OUT).is_finite());
    }
}
