//! Reference stretchers: established tools whose output Noodle's warps are
//! measured against where we have no native render (ADR 0003, "borrowed
//! truth").
//!
//! Four are run as programs: Rubber Band's R3 engine and its R2 engine
//! (`rubberband`), SoundTouch (`soundstretch`) and Signalsmith Stretch (built by
//! `eval/build-signalsmith.sh`, since nobody packages it). Whichever are
//! installed are used; a missing one is skipped, not an error.
//!
//! None of them returns audio that starts where the input does: each has an
//! algorithmic delay. [`latency`] measures it for every tool and tempo case
//! from a calibration signal of bursts at known times, stretched under the
//! same plan, and [`align`] removes it, so the delay is never scored as bad
//! timing. The stretches are cached outside git ([`RefCache`]); only metric
//! numbers are committed.
//!
//! Rubber Band takes a time map (`--timemap`), so it can handle the ramp, step
//! and wobble cases, but its R3 engine cannot: in 3.3.0 offline R3 with a time
//! map warns of a NaN time ratio and returns the wrong length (checked here
//! with a two-point map). R3 is therefore asked for constant ratios only, and
//! R2 is the one reference for changing tempo. SoundTouch and Signalsmith's
//! command-line tools take one constant ratio, so they sit those out.

use crate::timemap::TimeMap;
use noodle_io::{Audio, decode_file};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What to ask of a stretcher: the map from output time to source time that
/// the caller's tempo case defines, and the lengths on each side.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub rate: u32,
    pub source_seconds: f64,
    pub output_seconds: f64,
    /// Output seconds to source seconds, as [`crate::harness::Harness::source_map`]
    /// builds it for the tempo case: a constant [`TimeMap::Speed`] or
    /// [`TimeMap::Points`] ending at the two lengths.
    pub map: TimeMap,
}

impl Plan {
    /// How many frames the stretch should have.
    pub fn output_frames(&self) -> usize {
        (self.output_seconds * f64::from(self.rate)).round() as usize
    }

    /// The constant speed, if there is one.
    pub fn speed(&self) -> Option<f64> {
        match &self.map {
            TimeMap::Speed(s) => Some(*s),
            TimeMap::Identity => Some(1.0),
            TimeMap::Points(_) => None,
        }
    }

    /// A hash of everything in the plan, for cache names.
    pub fn fingerprint(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut eat = |v: u64| h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
        eat(u64::from(self.rate));
        eat(self.source_seconds.to_bits());
        eat(self.output_seconds.to_bits());
        for (s, o) in self.frame_pairs() {
            eat(s);
            eat(o);
        }
        h
    }

    /// The map as (source frame, output frame) pairs, which is how Rubber
    /// Band's time map file reads.
    pub fn frame_pairs(&self) -> Vec<(u64, u64)> {
        let rate = f64::from(self.rate);
        let to_frames = |s: f64| (s * rate).round().max(0.0) as u64;
        match &self.map {
            TimeMap::Points(points) => {
                let mut out: Vec<(u64, u64)> = Vec::new();
                for &(output, source) in points {
                    let pair = (to_frames(source), to_frames(output));
                    // Rounding can make neighbours equal; the tool wants
                    // strictly increasing pairs.
                    if out.last().is_none_or(|l| pair.0 > l.0 && pair.1 > l.1) {
                        out.push(pair);
                    }
                }
                out
            }
            _ => vec![
                (0, 0),
                (to_frames(self.source_seconds), self.output_frames() as u64),
            ],
        }
    }
}

/// A program that time-stretches a WAV file.
pub trait Stretcher {
    /// The name rows and cache folders use.
    fn name(&self) -> String;

    /// Whether it follows a changing tempo map. If not, it is asked for
    /// constant ratios only.
    fn supports_map(&self) -> bool;

    /// Changes when the program does, so a stale cache is not replayed.
    fn fingerprint(&self) -> String;

    /// Reads the 16-bit WAV `input` and writes the stretched audio to
    /// `output`.
    fn stretch(&self, input: &Path, output: &Path, plan: &Plan) -> Result<(), String>;
}

/// The stretchers that run as programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// The R3 (finer) engine, for constant ratios.
    RubberBand,
    /// The R2 engine, which also follows a time map.
    RubberBandR2,
    SoundTouch,
    Signalsmith,
}

impl Tool {
    pub const ALL: [Tool; 4] = [
        Tool::RubberBand,
        Tool::RubberBandR2,
        Tool::SoundTouch,
        Tool::Signalsmith,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::RubberBand => "rubberband-r3",
            Self::RubberBandR2 => "rubberband-r2",
            Self::SoundTouch => "soundtouch",
            Self::Signalsmith => "signalsmith",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Self::RubberBand | Self::RubberBandR2 => "rubberband",
            Self::SoundTouch => "soundstretch",
            Self::Signalsmith => "signalsmith-stretch",
        }
    }

    /// Where to get it, for the message about a missing one.
    pub fn install_hint(self) -> &'static str {
        match self {
            Self::RubberBand | Self::RubberBandR2 => {
                "apt install rubberband-cli (3.0 or later), or brew install rubberband"
            }
            Self::SoundTouch => "apt install soundstretch, or brew install sound-touch",
            Self::Signalsmith => "eval/build-signalsmith.sh",
        }
    }

    /// The installed program: in one of `dirs` first, then on `PATH`.
    pub fn find(self, dirs: &[PathBuf]) -> Option<CommandStretcher> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        dirs.iter()
            .cloned()
            .chain(std::env::split_paths(&path))
            .map(|d| d.join(self.program()))
            .find(|p| p.is_file())
            .map(|binary| CommandStretcher { tool: self, binary })
    }
}

/// A [`Tool`] and where it is installed.
#[derive(Debug, Clone)]
pub struct CommandStretcher {
    pub tool: Tool,
    pub binary: PathBuf,
}

impl Stretcher for CommandStretcher {
    fn name(&self) -> String {
        self.tool.name().into()
    }

    fn supports_map(&self) -> bool {
        self.tool == Tool::RubberBandR2
    }

    fn fingerprint(&self) -> String {
        let meta = std::fs::metadata(&self.binary).ok();
        let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
        let seconds = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        // Bump when the arguments `stretch` passes change.
        format!("{size:x}-{seconds:x}-args1")
    }

    fn stretch(&self, input: &Path, output: &Path, plan: &Plan) -> Result<(), String> {
        let mut command = Command::new(&self.binary);
        let mut map_file = None;
        match self.tool {
            Tool::RubberBand | Tool::RubberBandR2 => {
                command.args([
                    if self.tool == Tool::RubberBand {
                        "-3"
                    } else {
                        "-2"
                    },
                    "-q",
                ]);
                match plan.speed() {
                    Some(speed) => {
                        command.arg("-t").arg(format!("{}", 1.0 / speed));
                    }
                    None => {
                        let file = output.with_extension("timemap");
                        let text: String = plan
                            .frame_pairs()
                            .iter()
                            .map(|(s, o)| format!("{s} {o}\n"))
                            .collect();
                        std::fs::write(&file, text)
                            .map_err(|e| format!("{}: {e}", file.display()))?;
                        command.arg("-M").arg(&file);
                        command
                            .arg("-t")
                            .arg(format!("{}", plan.output_seconds / plan.source_seconds));
                        map_file = Some(file);
                    }
                }
                command.arg(input).arg(output);
            }
            Tool::SoundTouch => {
                let speed = plan.speed().ok_or("soundtouch takes one constant ratio")?;
                command
                    .arg(input)
                    .arg(output)
                    .arg(format!("-tempo={}", (speed - 1.0) * 100.0));
            }
            Tool::Signalsmith => {
                let speed = plan.speed().ok_or("signalsmith takes one constant ratio")?;
                command
                    .arg(input)
                    .arg(output)
                    .arg(format!("--time={}", 1.0 / speed));
            }
        }
        let result = command
            .output()
            .map_err(|e| format!("{}: {e}", self.binary.display()))?;
        if let Some(file) = map_file {
            let _ = std::fs::remove_file(file);
        }
        if !result.status.success() || !output.is_file() {
            return Err(format!(
                "{} failed: {}",
                self.tool.name(),
                String::from_utf8_lossy(&result.stderr).trim()
            ));
        }
        Ok(())
    }
}

/// `samples` rounded to what a 16-bit WAV holds.
pub fn quantise(samples: &[f32]) -> Vec<f32> {
    samples
        .iter()
        .map(|s| f32::from((s.clamp(-1.0, 1.0) * 32767.0).round() as i16) / 32767.0)
        .collect()
}

/// Writes `stereo` as the 16-bit WAV every tool reads (Signalsmith's reads no
/// other). Returns the samples as that file holds them, so the caller can
/// score against exactly what the stretchers were given.
pub fn write_pcm16(path: &Path, stereo: &[f32], rate: u32) -> Result<Vec<f32>, String> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(path, spec).map_err(|e| format!("{}: {e}", path.display()))?;
    for &s in stereo {
        let q = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        writer.write_sample(q).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(quantise(stereo))
}

/// Interleaved stereo from any file's channels.
pub fn to_stereo(audio: &Audio) -> Vec<f32> {
    let c = audio.channels;
    audio
        .samples
        .chunks_exact(c)
        .flat_map(|f| [f[0], f[c.min(2) - 1]])
        .collect()
}

/// Calibration bursts: stereo, silent except for a short burst of noise at
/// the start of every second after the first half.
pub struct Calibration {
    pub samples: Vec<f32>,
    /// The centre of each burst, in seconds.
    pub times: Vec<f64>,
}

const BURST_SECONDS: f64 = 0.008;

pub fn calibration(seconds: f64, rate: u32) -> Calibration {
    let frames = (seconds * f64::from(rate)).round() as usize;
    let mut samples = vec![0.0f32; frames * 2];
    let mut times = Vec::new();
    let width = (BURST_SECONDS * f64::from(rate)) as usize;
    let mut seed = 0x0ca1_1b8eu32;
    let mut t = 0.5;
    // Leave a second at the end for the stretcher's tail.
    while t + 1.0 < seconds {
        let start = (t * f64::from(rate)) as usize;
        for i in 0..width {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (f64::from(seed >> 8) / f64::from(1u32 << 23) - 1.0) as f32;
            let window = (std::f64::consts::PI * i as f64 / width as f64)
                .sin()
                .powi(2) as f32;
            let v = 0.8 * noise * window;
            samples[(start + i) * 2] = v;
            samples[(start + i) * 2 + 1] = v;
        }
        times.push(t + BURST_SECONDS / 2.0);
        t += 1.0;
    }
    Calibration { samples, times }
}

/// How late a stretcher's output is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Latency {
    /// Median over the bursts of where each landed minus where the map says
    /// it should: positive is late.
    pub seconds: f64,
    /// Largest minus smallest over the bursts. A real latency is constant, so
    /// a large spread means the measurement (or the tool) is not trustworthy.
    pub spread_seconds: f64,
}

/// Measures the delay from the stretched calibration, `stereo`, whose bursts
/// were at `times` in the source. `map` takes output time to source time.
/// `None` if no burst could be found. Delays must stay under about 0.45 of the
/// bursts' spacing in the output (0.22 s at twice speed, where they are 0.5 s
/// apart); real tools are tens of milliseconds.
pub fn latency(stereo: &[f32], rate: u32, map: &TimeMap, times: &[f64]) -> Option<Latency> {
    let rate_f = f64::from(rate);
    let mono: Vec<f32> = stereo
        .as_chunks::<2>()
        .0
        .iter()
        .map(|f| (f[0] + f[1]) / 2.0)
        .collect();
    // A 5 ms moving average of the magnitude finds the burst's peak; the
    // energy centroid around it then places it more finely.
    let smooth = (0.005 * rate_f) as usize;
    let mut offsets = Vec::new();
    for &t in times {
        let expect = map.to_output(t);
        // Search less than half the spacing to the neighbouring bursts, so a
        // long delay cannot lock onto the wrong one.
        let spacing = (map.to_output(t + 1.0) - expect).abs();
        let half = (0.45 * spacing).min(0.3);
        let lo = ((expect - half) * rate_f).max(0.0) as usize;
        let hi = (((expect + half) * rate_f) as usize).min(mono.len());
        if hi <= lo + 2 * smooth {
            continue;
        }
        let window = &mono[lo..hi];
        let mut running: f32 = window[..smooth].iter().map(|x| x.abs()).sum();
        let (mut best, mut at) = (running, smooth / 2);
        for i in smooth..window.len() {
            running += window[i].abs() - window[i - smooth].abs();
            if running > best {
                (best, at) = (running, i - smooth / 2);
            }
        }
        if best < 1e-4 * smooth as f32 {
            continue;
        }
        let half = (0.015 * rate_f) as usize;
        let (a, b) = (at.saturating_sub(half), (at + half).min(window.len()));
        let (mut weight, mut moment) = (0.0f64, 0.0f64);
        for (i, x) in window[a..b].iter().enumerate() {
            let e = f64::from(*x) * f64::from(*x);
            weight += e;
            moment += e * (a + i) as f64;
        }
        if weight > 0.0 {
            offsets.push((lo as f64 + moment / weight) / rate_f - expect);
        }
    }
    if offsets.is_empty() {
        return None;
    }
    offsets.sort_by(f64::total_cmp);
    Some(Latency {
        seconds: offsets[offsets.len() / 2],
        spread_seconds: offsets[offsets.len() - 1] - offsets[0],
    })
}

/// `stereo` moved earlier by `latency_seconds` (later if negative), cut or
/// padded with silence to `frames`.
pub fn align(stereo: &[f32], latency_seconds: f64, rate: u32, frames: usize) -> Vec<f32> {
    let shift = (latency_seconds * f64::from(rate)).round() as i64;
    (0..frames as i64 * 2)
        .map(|i| {
            let (frame, channel) = (i / 2, i % 2);
            let from = frame + shift;
            if from < 0 {
                return 0.0;
            }
            stereo
                .get((from * 2 + channel) as usize)
                .copied()
                .unwrap_or(0.0)
        })
        .collect()
}

/// A stretch may differ from the plan's length by this much plus a fraction
/// of it: tools round, and some pad their tails.
const LENGTH_SLACK_SECONDS: f64 = 0.1;
const LENGTH_SLACK_FRACTION: f64 = 0.01;

/// Stretches kept outside git, so a rerun reuses them. Each is keyed by the
/// stretcher (and its fingerprint), the audio it was given and the tempo
/// case.
#[derive(Debug, Clone)]
pub struct RefCache {
    pub root: PathBuf,
}

impl RefCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The stretched audio for `input` (a 16-bit WAV that `key` names) under
    /// `plan`, made now if the cache lacks it. `case` names the plan.
    pub fn stretched(
        &self,
        stretcher: &dyn Stretcher,
        key: &str,
        input: &Path,
        case: &str,
        plan: &Plan,
    ) -> Result<Audio, String> {
        let dir = self
            .root
            .join(format!("{}-{}", stretcher.name(), stretcher.fingerprint()))
            .join(key);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // The plan is part of the name, so a changed tempo curve or map is
        // stretched again instead of replaying the old one.
        let file = dir.join(format!("{case}-{:016x}.wav", plan.fingerprint()));
        if !file.is_file() {
            // Written beside the final name, so an interrupted run leaves
            // nothing that a later one would take for a finished stretch.
            let partial = dir.join(format!("{case}-{:016x}.partial.wav", plan.fingerprint()));
            stretcher.stretch(input, &partial, plan)?;
            std::fs::rename(&partial, &file).map_err(|e| e.to_string())?;
        }
        let audio = decode_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let frames = audio.samples.len() / audio.channels.max(1);
        let off = (frames as f64 - plan.output_frames() as f64).abs() / f64::from(plan.rate);
        if off > LENGTH_SLACK_SECONDS + LENGTH_SLACK_FRACTION * plan.output_seconds {
            // A stretch of the wrong length has the wrong map; do not keep it.
            let _ = std::fs::remove_file(&file);
            return Err(format!(
                "{} returned {:.2} s where the plan has {:.2} s",
                stretcher.name(),
                frames as f64 / f64::from(plan.rate),
                plan.output_seconds
            ));
        }
        if audio.sample_rate != plan.rate {
            return Err(format!(
                "{} returned {} Hz for a {} Hz file",
                stretcher.name(),
                audio.sample_rate,
                plan.rate
            ));
        }
        Ok(audio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_pairs_of_a_constant_plan_run_corner_to_corner() {
        let plan = Plan {
            rate: 1000,
            source_seconds: 4.0,
            output_seconds: 2.0,
            map: TimeMap::Speed(2.0),
        };
        assert_eq!(plan.frame_pairs(), vec![(0, 0), (4000, 2000)]);
        assert_eq!(plan.speed(), Some(2.0));
    }

    #[test]
    fn frame_pairs_of_a_changing_plan_are_source_then_output() {
        let plan = Plan {
            rate: 1000,
            source_seconds: 6.0,
            output_seconds: 4.0,
            map: TimeMap::Points(vec![(0.0, 0.0), (2.0, 4.0), (4.0, 6.0)]),
        };
        assert_eq!(plan.frame_pairs(), vec![(0, 0), (4000, 2000), (6000, 4000)]);
        assert_eq!(plan.speed(), None);
    }

    #[test]
    fn calibration_bursts_sit_where_it_says() {
        let cal = calibration(5.0, 8000);
        assert_eq!(cal.times.len(), 4);
        let at = (cal.times[1] * 8000.0) as usize;
        assert!(cal.samples[at * 2].abs() + cal.samples[at * 2 + 2].abs() > 0.0);
        assert_eq!(cal.samples[(at + 4000) * 2], 0.0);
    }

    #[test]
    fn align_moves_earlier_and_pads() {
        let x: Vec<f32> = (0..8).map(|i| i as f32).collect(); // 4 stereo frames
        assert_eq!(
            align(&x, 0.001, 1000, 4),
            [2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 0.0, 0.0]
        );
        assert_eq!(align(&x, -0.001, 1000, 3), [0.0, 0.0, 0.0, 1.0, 2.0, 3.0]);
    }
}
