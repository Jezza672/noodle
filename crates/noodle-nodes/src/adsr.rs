//! The ADSR envelope.

use noodle_engine::{
    Config, Context, Instance, Lane, LaneKernel, Layout, NodeError, NodeInfo, NodeType, ParamInfo,
    PerLane, Ports, Setup, Unit,
};

/// An attack, decay, sustain, release envelope driven by a gate signal.
///
/// The gate is high (0.5 or more) while the note is held. When it rises, the
/// envelope climbs linearly to 1 over the attack time, then falls to the
/// sustain level over the decay time. When it falls, the envelope drops to 0
/// over the release time. The decay and release are exponential, and their
/// times are how long they take to cover all but a thousandth of their
/// distance (60 dB), so a long release doesn't spend its end near silence.
///
/// A new gate while the envelope is still releasing climbs from where it is,
/// so retriggering never jumps. Every lane has its own envelope, so a
/// polyphonic gate gives a polyphonic envelope.
pub struct Adsr;

pub const ADSR_ID: &str = "noodle.mod.adsr";

#[derive(Ports)]
struct AdsrPorts {
    #[input("gate", "Gate")]
    gate: (),
    #[param(
        "attack",
        "Attack",
        ParamInfo::new(0.001, 10.0, 0.01).log().unit(Unit::Seconds)
    )]
    attack: (),
    #[param(
        "decay",
        "Decay",
        ParamInfo::new(0.001, 10.0, 0.2).log().unit(Unit::Seconds)
    )]
    decay: (),
    #[param("sustain", "Sustain", ParamInfo::new(0.0, 1.0, 0.7))]
    sustain: (),
    #[param(
        "release",
        "Release",
        ParamInfo::new(0.001, 10.0, 0.3).log().unit(Unit::Seconds)
    )]
    release: (),
    #[output("out", "Out")]
    out: (),
}

const GATE: usize = AdsrPorts::GATE;
const ATTACK: usize = AdsrPorts::ATTACK;
const DECAY: usize = AdsrPorts::DECAY;
const SUSTAIN: usize = AdsrPorts::SUSTAIN;
const RELEASE: usize = AdsrPorts::RELEASE;
const OUT: usize = AdsrPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: ADSR_ID,
    version: 1,
    name: "ADSR",
    category: "Modulation",
};

impl NodeType for Adsr {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(AdsrPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(AdsrKernel, setup)))
    }
}

/// -ln(0.001): the exponent at which a decay has covered 99.9% of its distance.
const SIXTY_DB: f32 = 6.907_755;

/// Below this the release is over and the envelope goes idle.
const SILENT: f32 = 1e-5;

/// Within this of the sustain level, the decay is over.
const SETTLED: f32 = 1e-4;

struct AdsrKernel;

#[derive(Clone, Copy, Default, PartialEq)]
enum Stage {
    #[default]
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Default)]
struct AdsrState {
    stage: Stage,
    level: f32,
    gate: bool,
}

impl AdsrState {
    fn tick(&mut self, gate: f32, rates: Rates) -> f32 {
        let gate = gate >= 0.5;
        if gate && !self.gate {
            self.stage = Stage::Attack;
        } else if !gate && self.gate && self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
        self.gate = gate;

        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += rates.attack;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = rates.sustain + (self.level - rates.sustain) * rates.decay;
                if (self.level - rates.sustain).abs() < SETTLED {
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = rates.sustain,
            Stage::Release => {
                self.level *= rates.release;
                if self.level < SILENT {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }
}

/// One sample's worth of the envelope's settings, as per-sample steps.
#[derive(Clone, Copy)]
struct Rates {
    /// Level gained per sample while attacking.
    attack: f32,
    /// What the level is multiplied by per sample, towards its target.
    decay: f32,
    release: f32,
    sustain: f32,
}

impl Rates {
    fn new(attack: f32, decay: f32, sustain: f32, release: f32, sample_rate: f32) -> Self {
        Self {
            attack: Self::attack_step(attack, sample_rate),
            decay: Self::coefficient(decay, sample_rate),
            release: Self::coefficient(release, sample_rate),
            sustain: sustain.clamp(0.0, 1.0),
        }
    }

    fn attack_step(seconds: f32, sample_rate: f32) -> f32 {
        1.0 / (seconds.max(1e-4) * sample_rate)
    }

    fn coefficient(seconds: f32, sample_rate: f32) -> f32 {
        (-SIXTY_DB / (seconds.max(1e-4) * sample_rate)).exp()
    }
}

impl LaneKernel for AdsrKernel {
    type State = AdsrState;

    fn process_lane(&mut self, state: &mut AdsrState, ctx: &Context, mut lane: Lane<'_, '_>) {
        let rate = ctx.sample_rate;
        let gate = lane.inputs.get(GATE);
        let out = lane.outputs.get_mut(OUT);

        // The times cost an exp() each, so work out the ones that hold still
        // once per block and only the moving ones (a slider being smoothed,
        // a wire) per sample.
        let constants = (
            lane.inputs.constant(ATTACK),
            lane.inputs.constant(DECAY),
            lane.inputs.constant(SUSTAIN),
            lane.inputs.constant(RELEASE),
        );
        if let (Some(a), Some(d), Some(s), Some(r)) = constants {
            let rates = Rates::new(a, d, s, r, rate);
            for (o, &g) in out.iter_mut().zip(gate) {
                *o = state.tick(g, rates);
            }
        } else {
            let (a, d) = (lane.inputs.get(ATTACK), lane.inputs.get(DECAY));
            let (s, r) = (lane.inputs.get(SUSTAIN), lane.inputs.get(RELEASE));
            let fixed = (
                constants.0.map(|a| Rates::attack_step(a, rate)),
                constants.1.map(|d| Rates::coefficient(d, rate)),
                constants.2.map(|s| s.clamp(0.0, 1.0)),
                constants.3.map(|r| Rates::coefficient(r, rate)),
            );
            for (i, (o, &g)) in out.iter_mut().zip(gate).enumerate() {
                let rates = Rates {
                    attack: fixed.0.unwrap_or_else(|| Rates::attack_step(a[i], rate)),
                    decay: fixed.1.unwrap_or_else(|| Rates::coefficient(d[i], rate)),
                    sustain: fixed.2.unwrap_or_else(|| s[i].clamp(0.0, 1.0)),
                    release: fixed.3.unwrap_or_else(|| Rates::coefficient(r[i], rate)),
                };
                *o = state.tick(g, rates);
            }
        }

        // A non-finite value in a time or the gate must not stick.
        if !state.level.is_finite() {
            *state = AdsrState::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;

    fn harness(frames: usize, gate_shape: Shape) -> Harness {
        Harness::new(&Adsr, &Config::new(), &[(GATE, gate_shape)], RATE, frames).unwrap()
    }

    /// Runs one block with the gate held at `gate`, returning the output.
    fn run(h: &mut Harness, frames: usize, gate: f32) -> Vec<f32> {
        h.input(GATE, frames).fill(gate);
        h.run(frames).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    fn set_times(h: &mut Harness, attack: f32, decay: f32, sustain: f32, release: f32) {
        h.set(ATTACK, attack);
        h.set(DECAY, decay);
        h.set(SUSTAIN, sustain);
        h.set(RELEASE, release);
    }

    #[test]
    fn rises_decays_to_sustain_and_releases() {
        let frames = 4_800; // 100 ms
        let mut h = harness(frames, Shape::MONO);
        set_times(&mut h, 0.01, 0.01, 0.5, 0.01);

        let held = run(&mut h, frames, 1.0);
        // The attack takes 10 ms: about 480 samples to reach 1.
        let peak = held.iter().position(|&x| x >= 1.0).unwrap();
        assert!((470..=490).contains(&peak), "peak at {peak}");
        assert!(held[..peak].windows(2).all(|w| w[1] > w[0]));
        // It settles on the sustain level and stays there.
        assert!((held[frames - 1] - 0.5).abs() < 1e-4);
        assert!(held.iter().all(|&x| (0.0..=1.0).contains(&x)));

        let released = run(&mut h, frames, 0.0);
        assert!(released.windows(2).all(|w| w[1] <= w[0]));
        // Ten milliseconds is its 60 dB time: -60 dB of 0.5 is 5e-4.
        assert!(released[480] < 1e-3, "{}", released[480]);
        assert_eq!(released[frames - 1], 0.0);
    }

    #[test]
    fn stays_silent_without_a_gate() {
        let mut h = harness(64, Shape::MONO);
        assert!(run(&mut h, 64, 0.0).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn retriggering_climbs_from_the_current_level() {
        let frames = 2_400;
        let mut h = harness(frames, Shape::MONO);
        set_times(&mut h, 0.05, 0.05, 0.5, 0.2);
        run(&mut h, frames, 1.0);
        let releasing = run(&mut h, 480, 0.0);
        let before = *releasing.last().unwrap();
        assert!(before > 0.1, "release should still be going: {before}");
        let next = run(&mut h, 480, 1.0);
        // The first sample after the new gate continues upwards, no jump.
        assert!(next[0] > before);
        assert!(next[0] - before < 0.01);
    }

    #[test]
    fn each_voice_has_its_own_envelope() {
        let poly = Shape::new(2, 1);
        let mut h = harness(64, poly);
        set_times(&mut h, 0.001, 0.01, 0.5, 0.01);
        {
            let mut gate = h.input(GATE, 64);
            gate.lane_mut(0, 0).fill(1.0);
            gate.lane_mut(1, 0).fill(0.0);
        }
        h.run(64).unwrap();
        let out = h.output(OUT);
        assert_eq!(out.shape(), poly);
        assert!(out.lane(0, 0)[63] > 0.5);
        assert!(out.lane(1, 0).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn modulated_times_match_constant_times() {
        let frames = 1_000;
        let go = |modulated: bool| {
            let connected: Vec<_> = if modulated {
                vec![
                    (GATE, Shape::MONO),
                    (ATTACK, Shape::MONO),
                    (RELEASE, Shape::MONO),
                ]
            } else {
                vec![(GATE, Shape::MONO)]
            };
            let mut h = Harness::new(&Adsr, &Config::new(), &connected, RATE, frames).unwrap();
            h.set(DECAY, 0.02);
            h.set(SUSTAIN, 0.4);
            if modulated {
                h.input(ATTACK, frames).fill(0.005);
                h.input(RELEASE, frames).fill(0.02);
            } else {
                h.set(ATTACK, 0.005);
                h.set(RELEASE, 0.02);
            }
            let mut out = run(&mut h, frames, 1.0);
            out.extend(run(&mut h, frames, 0.0));
            out
        };
        assert_eq!(go(false), go(true));
    }

    #[test]
    fn every_time_can_be_the_only_one_modulated() {
        let frames = 1_000;
        let times = [0.005, 0.02, 0.4, 0.02];
        let go = |modulated: Option<usize>| {
            let mut connected = vec![(GATE, Shape::MONO)];
            connected.extend(modulated.map(|port| (port, Shape::MONO)));
            let mut h = Harness::new(&Adsr, &Config::new(), &connected, RATE, frames).unwrap();
            for (i, port) in [ATTACK, DECAY, SUSTAIN, RELEASE].into_iter().enumerate() {
                if modulated == Some(port) {
                    h.input(port, frames).fill(times[i]);
                } else {
                    h.set(port, times[i]);
                }
            }
            let mut out = run(&mut h, frames, 1.0);
            out.extend(run(&mut h, frames, 0.0));
            out
        };
        let expected = go(None);
        for port in [ATTACK, DECAY, SUSTAIN, RELEASE] {
            assert_eq!(go(Some(port)), expected, "port {port}");
        }
    }

    #[test]
    fn survives_a_non_finite_gate_time() {
        let mut h = Harness::new(
            &Adsr,
            &Config::new(),
            &[(GATE, Shape::MONO), (ATTACK, Shape::MONO)],
            RATE,
            64,
        )
        .unwrap();
        h.input(ATTACK, 64).fill(f32::NAN);
        run(&mut h, 64, 1.0);
        h.input(ATTACK, 64).fill(0.001);
        let out = run(&mut h, 64, 1.0);
        assert!(out.iter().all(|x| x.is_finite()));
    }
}
