use std::f32::consts::TAU;

use noodle_engine::{
    Cancelled, Config, Context, Instance, Io, Layout, NodeError, NodeInfo, NodeType, OfflineNode,
    ParamInfo, Ports, Progress, Setup, Unit,
};

/// Changes how long its input takes without changing its pitch.
///
/// A `ratio` of 2 plays the input twice as slowly, 0.5 twice as fast. The
/// output is as long as the range, as for every node: slowed down, the
/// input's end falls off the range; sped up, the output ends in silence.
/// Chain it after a clip that is shorter than the range, or set the range
/// with the stretch in mind.
///
/// It uses waveform-similarity overlap-add (WSOLA): overlapping grains are
/// taken from the input at the speed the ratio asks for, each shifted a
/// little to line up with the one before it, then faded together. That keeps
/// tonal sound intact and smears sharp transients a little; a longer `grain`
/// suits slow tonal material, a shorter one drums.
///
/// The ratio can be modulated, and moves the speed along the range. All
/// channels of a voice use the same grain positions, so a stereo image
/// holds.
pub struct TimeStretch;

pub const TIME_STRETCH_ID: &str = "noodle.offline.time_stretch";

#[derive(Ports)]
#[ports(offline)]
struct StretchPorts {
    #[input("in", "In")]
    input: (),
    #[param(
        "ratio",
        "Ratio",
        ParamInfo::new(0.25, 4.0, 1.0).log().offset()
    )]
    ratio: (),
    #[param(
        "grain",
        "Grain",
        ParamInfo::new(0.01, 0.1, 0.04).unit(Unit::Seconds)
    )]
    grain: (),
    #[output("out", "Out")]
    out: (),
}

const IN: usize = StretchPorts::INPUT;
const RATIO: usize = StretchPorts::RATIO;
const GRAIN: usize = StretchPorts::GRAIN;
const OUT: usize = StretchPorts::OUT;

static INFO: NodeInfo = NodeInfo {
    id: TIME_STRETCH_ID,
    version: 1,
    name: "Time Stretch",
    category: "Offline",
};

impl NodeType for TimeStretch {
    fn info(&self) -> &NodeInfo {
        &INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(StretchPorts::layout())
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::offline(StretchNode))
    }
}

struct StretchNode;

/// The correlation search looks at every this-many-th sample.
const DECIMATE: usize = 4;
/// Ratios outside this are clamped, so a wild modulation can't stall the
/// render or skip the whole input.
const RATIO_RANGE: (f32, f32) = (0.05, 20.0);

impl OfflineNode for StretchNode {
    fn render(
        &mut self,
        ctx: &Context,
        io: Io<'_, '_>,
        progress: &Progress,
    ) -> Result<(), Cancelled> {
        let input = io.inputs[IN];
        let ratio = io.inputs[RATIO];
        let grain = io.inputs[GRAIN].lane(0, 0).first().copied().unwrap_or(0.04);
        let out = &mut io.outputs[OUT];
        let shape = out.shape();
        // An even window of about `grain` seconds, never tiny.
        let window = ((grain.clamp(0.005, 0.5) * ctx.sample_rate) as usize / 2 * 2).max(64);

        for voice in 0..shape.voices {
            let lanes: Vec<&[f32]> = (0..shape.channels)
                .map(|channel| input.lane(voice, channel))
                .collect();
            let mut outs = vec![vec![0.0f32; ctx.frames]; shape.channels];
            stretch_voice(&lanes, ratio.lane(voice, 0), window, &mut outs, &|done| {
                progress.report((voice as f32 + done) / shape.voices as f32)
            })?;
            for (channel, data) in outs.iter().enumerate() {
                out.lane_mut(voice, channel).copy_from_slice(data);
            }
        }
        Ok(())
    }
}

/// WSOLA over one voice. `inputs` and `outputs` have one slice per channel,
/// all as long as `ratio`.
fn stretch_voice(
    inputs: &[&[f32]],
    ratio: &[f32],
    window: usize,
    outputs: &mut [Vec<f32>],
    report: &dyn Fn(f32) -> Result<(), Cancelled>,
) -> Result<(), Cancelled> {
    let len = ratio.len();
    let hop = window / 2;
    let reach = window / 4;
    if len == 0 {
        return Ok(());
    }
    // What the search compares: all channels summed.
    let mono: Vec<f32> = (0..len)
        .map(|i| inputs.iter().map(|lane| lane[i]).sum())
        .collect();
    let at = |signal: &[f32], i: isize| -> f32 {
        usize::try_from(i)
            .ok()
            .and_then(|i| signal.get(i))
            .copied()
            .unwrap_or(0.0)
    };
    // A Hann window that is never exactly zero, so the first and last
    // samples of the range still count in the weights.
    let fade: Vec<f32> = (0..window)
        .map(|i| 0.5 - 0.5 * (TAU * (i as f32 + 0.5) / window as f32).cos())
        .collect();
    let mut weight = vec![0.0f32; len];

    let mut nominal = 0.0f64;
    let mut previous: Option<isize> = None;
    let mut start = 0usize;
    let mut frames_done = 0usize;
    while start < len && nominal < len as f64 {
        let near = nominal.round() as isize;
        let chosen = match previous {
            None => near,
            Some(previous) => {
                // The audio that would follow the last grain, unstretched,
                // is what this one should join onto.
                let wanted = previous + hop as isize;
                let mut best = (f32::MIN, near);
                for shift in 0..=2 * reach as isize {
                    // Nearest the nominal position first, so ties favour it.
                    let offset = if shift % 2 == 0 {
                        shift / 2
                    } else {
                        -(shift + 1) / 2
                    };
                    let candidate = near + offset;
                    if candidate < 0 {
                        continue;
                    }
                    let (mut dot, mut energy) = (0.0f32, 1e-9f32);
                    for k in (0..hop).step_by(DECIMATE) {
                        let a = at(&mono, candidate + k as isize);
                        dot += a * at(&mono, wanted + k as isize);
                        energy += a * a;
                    }
                    let score = dot / energy.sqrt();
                    if score > best.0 {
                        best = (score, candidate);
                    }
                }
                best.1
            }
        };
        for (channel, input) in inputs.iter().enumerate() {
            let out = &mut outputs[channel];
            for (i, gain) in fade.iter().enumerate() {
                let Some(slot) = out.get_mut(start + i) else {
                    break;
                };
                *slot += at(input, chosen + i as isize) * gain;
            }
        }
        for (i, gain) in fade.iter().enumerate() {
            let Some(slot) = weight.get_mut(start + i) else {
                break;
            };
            *slot += gain;
        }
        previous = Some(chosen);
        let speed = ratio[start.min(len - 1)];
        let speed = if speed.is_finite() {
            speed.clamp(RATIO_RANGE.0, RATIO_RANGE.1)
        } else {
            1.0
        };
        nominal += hop as f64 / f64::from(speed);
        start += hop;
        frames_done += 1;
        if frames_done.is_multiple_of(64) {
            report(start as f32 / len as f32)?;
        }
    }
    for out in outputs.iter_mut() {
        for (sample, &w) in out.iter_mut().zip(&weight) {
            if w > 1e-6 {
                *sample /= w;
            } else {
                *sample = 0.0;
            }
        }
    }
    report(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_engine::Shape;
    use noodle_engine::testing::Harness;

    const RATE: f32 = 48_000.0;
    const FRAMES: usize = 48_000;

    fn sine(frequency: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (TAU * frequency * i as f32 / RATE).sin())
            .collect()
    }

    fn run(ratio: f32, input: &[f32]) -> Vec<f32> {
        let mut h = Harness::new(
            &TimeStretch,
            &Config::new(),
            &[(IN, Shape::MONO)],
            RATE,
            input.len(),
        )
        .unwrap();
        h.set(RATIO, ratio);
        h.input(IN, input.len())
            .lane_mut(0, 0)
            .copy_from_slice(input);
        h.run(input.len()).unwrap();
        h.output(OUT).lane(0, 0).to_vec()
    }

    fn crossings(signal: &[f32]) -> usize {
        signal
            .windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count()
    }

    #[test]
    fn a_ratio_of_one_changes_nothing() {
        let input = sine(220.0, FRAMES);
        let out = run(1.0, &input);
        let worst = input
            .iter()
            .zip(&out)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "{worst}");
    }

    #[test]
    fn stretching_keeps_the_pitch_and_the_level() {
        let input = sine(440.0, FRAMES);
        for ratio in [2.0, 0.5] {
            let out = run(ratio, &input);
            // Away from the ends, and for ratio 0.5 before the input runs out.
            let middle = &out[4_800..FRAMES / 2 - 4_800];
            let seconds = middle.len() as f32 / RATE;
            let hz = crossings(middle) as f32 / seconds;
            assert!((hz - 440.0).abs() < 8.0, "ratio {ratio}: {hz} Hz");
            let peak = middle.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            assert!((0.9..1.1).contains(&peak), "ratio {ratio}: peak {peak}");
        }
    }

    #[test]
    fn it_really_changes_the_duration() {
        // A burst in the first quarter of the input lands at twice the time.
        let mut input = vec![0.0; FRAMES];
        input[..FRAMES / 4].copy_from_slice(&sine(440.0, FRAMES / 4));
        let out = run(2.0, &input);
        let last_loud = out.iter().rposition(|x| x.abs() > 0.3).unwrap();
        let expected = FRAMES / 2;
        assert!(
            last_loud.abs_diff(expected) < 2_048,
            "ends at {last_loud}, expected near {expected}"
        );
        // Sped up it ends early and the rest is silent.
        let out = run(0.5, &input);
        let last_loud = out.iter().rposition(|x| x.abs() > 0.3).unwrap();
        assert!(last_loud.abs_diff(FRAMES / 8) < 2_048, "{last_loud}");
        assert!(out[FRAMES / 2..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn stereo_channels_keep_their_balance() {
        let mut h = Harness::new(
            &TimeStretch,
            &Config::new(),
            &[(IN, Shape::STEREO)],
            RATE,
            FRAMES,
        )
        .unwrap();
        h.set(RATIO, 1.5);
        let tone = sine(300.0, FRAMES);
        let mut input = h.input(IN, FRAMES);
        input.lane_mut(0, 0).copy_from_slice(&tone);
        for (out, x) in input.lane_mut(0, 1).iter_mut().zip(&tone) {
            *out = x * 0.25;
        }
        h.run(FRAMES).unwrap();
        let (left, right) = (h.output(OUT).lane(0, 0), h.output(OUT).lane(0, 1));
        for i in (2_000..30_000).step_by(97) {
            assert!((right[i] - left[i] * 0.25).abs() < 1e-4, "frame {i}");
        }
    }

    #[test]
    fn a_wired_ratio_changes_the_speed_along_the_range() {
        let input = sine(440.0, FRAMES);
        let mut h = Harness::new(
            &TimeStretch,
            &Config::new(),
            &[(IN, Shape::MONO), (RATIO, Shape::MONO)],
            RATE,
            FRAMES,
        )
        .unwrap();
        h.input(IN, FRAMES).lane_mut(0, 0).copy_from_slice(&input);
        let ratio: Vec<f32> = (0..FRAMES)
            .map(|i| if i < FRAMES / 2 { 1.0 } else { 2.0 })
            .collect();
        h.input(RATIO, FRAMES)
            .lane_mut(0, 0)
            .copy_from_slice(&ratio);
        h.run(FRAMES).unwrap();
        let out = h.output(OUT).lane(0, 0);
        // Still the same pitch in both halves.
        for part in [&out[4_800..19_200], &out[28_800..43_200]] {
            let hz = crossings(part) as f32 / (part.len() as f32 / RATE);
            assert!((hz - 440.0).abs() < 8.0, "{hz}");
        }
    }

    #[test]
    fn cancelling_stops_the_render() {
        let node_type = TimeStretch;
        let progress = Progress::new();
        progress.cancel();
        let inputs = [noodle_engine::OfflineInput::Signal {
            shape: Shape::MONO,
            data: sine(440.0, FRAMES),
        }];
        let result = noodle_engine::render_offline_node(
            &node_type,
            &Config::new(),
            noodle_engine::NodeId(1),
            &[
                inputs.into_iter().next().unwrap(),
                noodle_engine::OfflineInput::Constant(1.0),
                noodle_engine::OfflineInput::Constant(0.04),
            ],
            FRAMES,
            RATE,
            &progress,
        );
        assert!(matches!(
            result,
            Err(noodle_engine::OfflineError::Cancelled)
        ));
    }
}
