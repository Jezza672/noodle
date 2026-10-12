//! Borrowed truth: scoring warps of audio we cannot generate.
//!
//! The probes and songs have a native render, a true answer. Speech, a solo
//! instrument or a recorded mix has none, so Noodle's warps are measured
//! against what established stretchers ([`crate::refs`]) make of the same
//! audio, and against the source.
//!
//! There is no true time map either, so the map comes from the tempo case: the
//! piece is declared to be at the harness's 110 bpm, the target tempo map
//! follows from the case, and every contender (Noodle's modes and each
//! reference) is asked for exactly that. Noodle's `auto` still picks its own
//! tempo from the audio and so can be wrong, which is scored.
//!
//! Each warp is scored against the source through the map (`source` view), and
//! for probes also against the native render (`native`). Numbers are read two
//! ways:
//!
//! - **Relative to the references' spread.** For each metric the references'
//!   lowest and highest value is the range a good stretcher falls in; a Noodle
//!   mode is better than, inside, or worse than it ([`Verdict`]). With one
//!   reference there is no spread, only a comparison.
//! - **Checks that need no reference stretcher**: onsets land where the map
//!   says, the pitch is the source's, the loudness is kept. They are in the same
//!   rows, printed beside the references' own.
//!
//! Hard thresholds come only from the known-truth regime (ADR 0003): nothing
//! here fails because a number is bad.

use crate::clip::Clip;
use crate::harness::{Harness, Method, analyse};
use crate::probe::Probe;
use crate::refs::{
    self, Plan, RefCache, Stretcher, align, calibration, latency, to_stereo, write_pcm16,
};
use crate::report::evaluate;
use crate::table::{Baseline, cell};
use crate::tempo::TempoCase;
use crate::timemap::TimeMap;
use noodle_core::Tick;
use noodle_io::{decode_file, resample, write_wav};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

pub const RATE: u32 = 48_000;
/// The tempo every piece is declared to be at.
pub const SOURCE_BPM: f64 = 110.0;
/// Clips are cut to this long unless told otherwise.
pub const DEFAULT_SECONDS: f64 = 30.0;
/// The shortest clip worth scoring: a warp needs room for a few grains.
const MIN_SECONDS: f64 = 2.0;

/// Audio to warp: a stereo source at [`RATE`], already rounded to 16 bits so
/// every contender is given exactly the same samples.
pub struct Piece {
    pub name: String,
    pub source: Vec<f32>,
    /// A probe, whose native render at any tempo is known.
    pub native: Option<Probe>,
}

impl Piece {
    pub fn seconds(&self) -> f64 {
        self.source.len() as f64 / 2.0 / f64::from(RATE)
    }

    pub fn probe(probe: Probe) -> Self {
        let harness = Harness::new(RATE, SOURCE_BPM).expect("a temp dir");
        Self {
            name: probe.name().into(),
            source: refs::quantise(&harness.source(probe)),
            native: Some(probe),
        }
    }
}

/// Audio files in `dirs`, by name. A file that will not decode is an error;
/// one with an extension no decoder takes is skipped quietly.
pub fn find_clips(dirs: &[PathBuf]) -> Vec<PathBuf> {
    const EXTENSIONS: [&str; 6] = ["wav", "flac", "mp3", "ogg", "m4a", "aif"];
    let mut found: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    found.sort();
    found
}

/// Decodes `path` into a [`Piece`]: stereo, at [`RATE`], the first `seconds`.
pub fn load_clip(path: &Path, seconds: f64) -> Result<Piece, String> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("a file with no name")?
        .to_string();
    let decoded = decode_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let audio = resample(&decoded, RATE).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut stereo = to_stereo(&audio);
    stereo.truncate((seconds * f64::from(RATE)) as usize * 2);
    let piece = Piece {
        name,
        source: refs::quantise(&stereo),
        native: None,
    };
    if piece.seconds() < MIN_SECONDS {
        return Err(format!(
            "{}: {:.1} s is too short to warp (need {MIN_SECONDS} s)",
            path.display(),
            piece.seconds()
        ));
    }
    Ok(piece)
}

/// The contenders' name for a Noodle mode.
pub fn noodle_name(method: Method) -> String {
    format!("noodle-{}", method.name())
}

/// One scored warp.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub piece: String,
    pub case: String,
    pub contender: String,
    /// A reference stretcher, as opposed to a Noodle mode.
    pub reference: bool,
    /// `source`, or `native` for a probe.
    pub view: &'static str,
    pub metrics: Vec<(String, f64)>,
}

impl Row {
    pub fn key(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.piece, self.case, self.contender, self.view
        )
    }

    pub fn metric(&self, name: &str) -> Option<f64> {
        self.metrics
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
    }

    /// Noodle's floor (plain resampling) is shown but is not a contender.
    pub fn is_floor(&self) -> bool {
        self.contender == noodle_name(Method::Repitch)
    }
}

pub struct Options {
    pub cases: Vec<TempoCase>,
    pub methods: Vec<Method>,
    pub references: Vec<Box<dyn Stretcher>>,
    pub cache: RefCache,
    /// Remove each stretcher's measured delay before scoring. Off only to show
    /// what the delay would cost.
    pub compensate_latency: bool,
    /// Write every contender's audio here to listen to.
    pub export: Option<PathBuf>,
}

/// Why a stretcher was not run on a case, or how far its delay measurement
/// can be trusted, for the report.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub piece: String,
    pub case: String,
    pub contender: String,
    pub text: String,
    /// The delay that was removed from this stretcher's output, when this is
    /// the note that reports it.
    pub delay_ms: Option<f64>,
}

/// Scores every contender on `piece` for every case.
pub fn run(piece: &Piece, options: &Options) -> Result<(Vec<Row>, Vec<Note>), String> {
    let beats = piece.seconds() * SOURCE_BPM / 60.0;
    let harness = Harness::new(RATE, SOURCE_BPM)
        .map_err(|e| e.to_string())?
        .with_beats(beats);
    let analysis = analyse(&piece.source, RATE);
    let key = format!("{}-{:016x}", piece.name, fingerprint(&piece.source));
    // What the stretchers read, kept with the cache so a run can reuse it.
    let input = options.cache.root.join("inputs").join(format!("{key}.wav"));
    std::fs::create_dir_all(input.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    let held = write_pcm16(&input, &piece.source, RATE)?;
    if held != piece.source {
        return Err(format!("{}: source is not 16-bit exact", piece.name));
    }
    let calibration = calibration(piece.seconds(), RATE);
    let calibration_key = format!("calibration-{}", (piece.seconds() * 1000.0).round() as u64);
    let calibration_input = options
        .cache
        .root
        .join("inputs")
        .join(format!("{calibration_key}.wav"));
    write_pcm16(&calibration_input, &calibration.samples, RATE)?;

    let mut rows = Vec::new();
    let mut notes = Vec::new();
    for &case in &options.cases {
        let name = case.name();
        let target = harness.target(case);
        let map = harness.source_map(case, &target);
        let native = piece.native.map(|p| p.render(&target, RATE));
        let output_seconds = target.seconds_at(Tick::from_quarters(beats));
        let frames = native.as_ref().map_or_else(
            || (output_seconds * f64::from(RATE)).round() as usize,
            |n| n.len() / 2,
        );
        let plan = Plan {
            rate: RATE,
            source_seconds: piece.seconds(),
            output_seconds: frames as f64 / f64::from(RATE),
            map: map.clone(),
        };
        let mut contenders: Vec<(String, bool, Vec<f32>)> = Vec::new();
        for &method in &options.methods {
            let (tempo, mode) = harness.choice(method, &analysis, &target);
            let warped = harness
                .warp(&piece.source, tempo, mode, &target, frames)
                .map_err(|e| format!("{} {name} {}: {e}", piece.name, method.name()))?;
            contenders.push((noodle_name(method), false, warped));
        }
        for stretcher in &options.references {
            let id = stretcher.name();
            if plan.speed().is_none() && !stretcher.supports_map() {
                notes.push(Note {
                    piece: piece.name.clone(),
                    case: name.clone(),
                    contender: id,
                    text: "takes one constant ratio only".into(),
                    delay_ms: None,
                });
                continue;
            }
            let stretched =
                match options
                    .cache
                    .stretched(stretcher.as_ref(), &key, &input, &name, &plan)
                {
                    Ok(audio) => audio,
                    Err(e) => {
                        notes.push(Note {
                            piece: piece.name.clone(),
                            case: name.clone(),
                            contender: id,
                            text: format!("failed: {e}"),
                            delay_ms: None,
                        });
                        continue;
                    }
                };
            let mut samples = to_stereo(&stretched);
            if options.compensate_latency {
                let probe = options
                    .cache
                    .stretched(
                        stretcher.as_ref(),
                        &calibration_key,
                        &calibration_input,
                        &name,
                        &plan,
                    )
                    .map_err(|e| format!("{} {name} {id} calibration: {e}", piece.name))?;
                match latency(&to_stereo(&probe), RATE, &map, &calibration.times) {
                    Some(l) => {
                        if l.spread_seconds > MAX_LATENCY_SPREAD {
                            notes.push(Note {
                                piece: piece.name.clone(),
                                case: name.clone(),
                                contender: id.clone(),
                                text: format!(
                                    "delay of {:.1} ms is not constant (spread {:.1} ms)",
                                    l.seconds * 1e3,
                                    l.spread_seconds * 1e3
                                ),
                                delay_ms: None,
                            });
                        }
                        notes.push(Note {
                            piece: piece.name.clone(),
                            case: name.clone(),
                            contender: id.clone(),
                            text: String::new(),
                            delay_ms: Some(l.seconds * 1e3),
                        });
                        samples = align(&samples, l.seconds, RATE, frames);
                    }
                    None => notes.push(Note {
                        piece: piece.name.clone(),
                        case: name.clone(),
                        contender: id.clone(),
                        text: "delay could not be measured; scored as returned".into(),
                        delay_ms: None,
                    }),
                }
            }
            samples.resize(frames * 2, 0.0);
            contenders.push((id, true, samples));
        }
        for (contender, reference, audio) in &contenders {
            export(options, &piece.name, &name, contender, audio)?;
            let mut views: Vec<(&'static str, &[f32], TimeMap)> =
                vec![("source", &piece.source, map.clone())];
            if let Some(n) = &native {
                views.push(("native", n, TimeMap::Identity));
            }
            for (view, reference_audio, view_map) in views {
                let report = evaluate(
                    Clip::new(reference_audio, 2, RATE),
                    Clip::new(audio, 2, RATE),
                    &view_map,
                );
                let mut metrics: Vec<(String, f64)> = report
                    .features()
                    .into_iter()
                    .map(|(n, v)| (n.to_string(), v))
                    .collect();
                if contender == &noodle_name(Method::Auto) {
                    metrics.push(("detected_bpm".into(), analysis.tempo.unwrap_or(f64::NAN)));
                }
                rows.push(Row {
                    piece: piece.name.clone(),
                    case: name.clone(),
                    contender: contender.clone(),
                    reference: *reference,
                    view,
                    metrics,
                });
            }
        }
    }
    export_source(options, piece)?;
    Ok((rows, notes))
}

/// A measured delay that varies more than this over the bursts is flagged.
const MAX_LATENCY_SPREAD: f64 = 0.02;

fn export(
    options: &Options,
    piece: &str,
    case: &str,
    contender: &str,
    audio: &[f32],
) -> Result<(), String> {
    let Some(dir) = &options.export else {
        return Ok(());
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    write_wav(
        &dir.join(format!("{piece}_{case}_{contender}.wav")),
        audio,
        2,
        RATE,
    )
    .map_err(|e| e.to_string())
}

fn export_source(options: &Options, piece: &Piece) -> Result<(), String> {
    let Some(dir) = &options.export else {
        return Ok(());
    };
    write_wav(
        &dir.join(format!("{}_source.wav", piece.name)),
        &piece.source,
        2,
        RATE,
    )
    .map_err(|e| e.to_string())
}

/// FNV-1a over the samples' bits: a cache key, not a security measure.
fn fingerprint(samples: &[f32]) -> u64 {
    samples.iter().fold(0xcbf2_9ce4_8422_2325, |h, s| {
        (h ^ u64::from(s.to_bits())).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A baseline of the rows' numbers.
pub fn baseline(rows: &[Row]) -> Baseline {
    Baseline::of_entries(rows.iter().map(|r| (r.key(), r.metrics.as_slice())))
}

/// The metrics scored against the references' spread: the paper's features
/// and the spectral distance.
pub const RELATIVE: [&str; 6] = [
    "SSMAD",
    "SPhMW",
    "DeltaP_abs",
    "TrRat_abs_ln",
    "HPSTrRat_abs_ln",
    "log_distance_db",
];

/// The checks that need no reference stretcher: where the onsets land, whether
/// the pitch is kept (single voices only), whether the loudness is.
pub const CHECKS: [&str; 5] = [
    "median_abs_offset_ms",
    "missed_onsets",
    "drift_ms",
    "pitch_median_abs_cents",
    "lufs_abs_delta",
];

/// Where a value falls against the references' range, lower being better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Beyond the best reference.
    Better,
    /// Within the range of the references.
    Inside,
    /// Beyond the worst reference.
    Worse,
}

impl Verdict {
    pub fn mark(self) -> char {
        match self {
            Self::Better => '<',
            Self::Inside => '=',
            Self::Worse => '>',
        }
    }
}

/// How far outside the references' range a value may sit and still count as
/// inside it, as a fraction of the range's largest magnitude: numbers that
/// agree to a couple of percent are not different.
const TOLERANCE: f64 = 0.02;

/// `value` against `references`, all as magnitudes (lower is better). `None`
/// with no reference to compare to or no value.
pub fn verdict(value: f64, references: &[f64]) -> Option<Verdict> {
    let refs: Vec<f64> = references
        .iter()
        .copied()
        .filter(|r| r.is_finite())
        .collect();
    if refs.is_empty() || !value.is_finite() {
        return None;
    }
    let low = refs.iter().copied().fold(f64::INFINITY, f64::min);
    let high = refs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let tolerance = TOLERANCE * high.abs().max(low.abs()).max(1e-9);
    Some(if value < low - tolerance {
        Verdict::Better
    } else if value > high + tolerance {
        Verdict::Worse
    } else {
        Verdict::Inside
    })
}

/// Rows of one piece, tempo case and view.
fn groups(rows: &[Row]) -> BTreeMap<(&str, &str, &str), Vec<&Row>> {
    let mut out: BTreeMap<(&str, &str, &str), Vec<&Row>> = BTreeMap::new();
    for row in rows {
        out.entry((&row.piece, &row.case, row.view))
            .or_default()
            .push(row);
    }
    out
}

fn magnitude(row: &Row, metric: &str) -> Option<f64> {
    row.metric(metric).map(f64::abs).filter(|v| v.is_finite())
}

/// How many times a Noodle contender was better than, inside, and worse than
/// the references, per metric.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub better: usize,
    pub inside: usize,
    pub worse: usize,
}

impl Tally {
    fn add(&mut self, v: Verdict) {
        match v {
            Verdict::Better => self.better += 1,
            Verdict::Inside => self.inside += 1,
            Verdict::Worse => self.worse += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.better + self.inside + self.worse
    }
}

/// The standing of each Noodle contender, per metric, over every group that
/// has a reference.
pub fn standings(rows: &[Row]) -> BTreeMap<&'static str, BTreeMap<String, Tally>> {
    let mut out: BTreeMap<&'static str, BTreeMap<String, Tally>> = BTreeMap::new();
    for group in groups(rows).values() {
        for metric in RELATIVE.iter().chain(CHECKS.iter()) {
            let refs: Vec<f64> = group
                .iter()
                .filter(|r| r.reference)
                .filter_map(|r| magnitude(r, metric))
                .collect();
            for row in group.iter().filter(|r| !r.reference && !r.is_floor()) {
                if let Some(v) = magnitude(row, metric).and_then(|v| verdict(v, &refs)) {
                    out.entry(metric)
                        .or_default()
                        .entry(row.contender.clone())
                        .or_default()
                        .add(v);
                }
            }
        }
    }
    out
}

/// The standings as a table: for each metric and Noodle mode, how many times
/// it was better than / inside / worse than the references.
pub fn format_standings(rows: &[Row]) -> String {
    let standings = standings(rows);
    if standings.is_empty() {
        return "no reference stretchers ran, so there is no spread to compare with\n".into();
    }
    let contenders: Vec<&String> = {
        let mut c: Vec<&String> = standings.values().flat_map(BTreeMap::keys).collect();
        c.sort();
        c.dedup();
        c
    };
    let mut out =
        String::from("against the references' spread: better / inside / worse (lower is better)\n");
    let _ = write!(out, "{:<26}", "metric");
    for c in &contenders {
        let _ = write!(out, " {c:>16}");
    }
    out.push('\n');
    let mut total: BTreeMap<&String, Tally> = BTreeMap::new();
    for metric in RELATIVE.iter().chain(CHECKS.iter()) {
        let Some(per) = standings.get(metric) else {
            continue;
        };
        let _ = write!(out, "{metric:<26}");
        for c in &contenders {
            match per.get(*c) {
                Some(t) => {
                    let _ = write!(
                        out,
                        " {:>16}",
                        format!("{}/{}/{}", t.better, t.inside, t.worse)
                    );
                    let sum = total.entry(c).or_default();
                    sum.better += t.better;
                    sum.inside += t.inside;
                    sum.worse += t.worse;
                }
                None => {
                    let _ = write!(out, " {:>16}", "-");
                }
            }
        }
        out.push('\n');
    }
    let _ = write!(out, "{:<26}", "all");
    for c in &contenders {
        let t = total.get(c).copied().unwrap_or_default();
        let _ = write!(
            out,
            " {:>16}",
            format!("{}/{}/{}", t.better, t.inside, t.worse)
        );
    }
    out.push('\n');
    out
}

/// Every group as a table: the references' range, then each Noodle mode's
/// value marked `<`, `=` or `>`; then the checks for every contender.
pub fn format_tables(rows: &[Row]) -> String {
    let mut out = String::new();
    for ((piece, case, view), group) in groups(rows) {
        let references: Vec<&&Row> = group.iter().filter(|r| r.reference).collect();
        let noodle: Vec<&&Row> = group.iter().filter(|r| !r.reference).collect();
        let _ = writeln!(
            out,
            "\n{piece} at {case} against the {view} ({} reference{})",
            references.len(),
            if references.len() == 1 { "" } else { "s" }
        );
        let _ = write!(out, "{:<24} {:>17}", "metric", "references");
        for r in &noodle {
            let _ = write!(out, " {:>16}", r.contender.trim_start_matches("noodle-"));
        }
        out.push('\n');
        for metric in RELATIVE {
            let refs: Vec<f64> = references
                .iter()
                .filter_map(|r| magnitude(r, metric))
                .collect();
            let range = if refs.is_empty() {
                "-".into()
            } else {
                let low = refs.iter().copied().fold(f64::INFINITY, f64::min);
                let high = refs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                format!("{}..{}", cell(Some(low)), cell(Some(high)))
            };
            let _ = write!(out, "{metric:<24} {range:>17}");
            for r in &noodle {
                let value = magnitude(r, metric);
                let mark = value
                    .and_then(|v| verdict(v, &refs))
                    .map_or(' ', Verdict::mark);
                let _ = write!(out, " {:>14} {mark}", cell(value));
            }
            out.push('\n');
        }
        let _ = writeln!(out, "checks (no reference stretcher needed):");
        let _ = write!(out, "{:<24}", "contender");
        for check in CHECKS {
            let _ = write!(out, " {:>14}", check.trim_end_matches("_ms"));
        }
        out.push('\n');
        for r in group.iter() {
            let _ = write!(out, "{:<24}", r.contender);
            for check in CHECKS {
                let _ = write!(out, " {:>14}", cell(magnitude(r, check)));
            }
            out.push('\n');
        }
    }
    out
}

/// The delays removed, one row per stretcher and piece.
pub fn format_delays(notes: &[Note]) -> String {
    let mut by: BTreeMap<(&str, &str), Vec<(&str, f64)>> = BTreeMap::new();
    for n in notes {
        if let Some(ms) = n.delay_ms {
            by.entry((&n.contender, &n.piece))
                .or_default()
                .push((&n.case, ms));
        }
    }
    let mut out = String::new();
    if !by.is_empty() {
        out.push_str(
            "delay removed from each stretcher, in ms (measured on a calibration signal)\n",
        );
    }
    for ((contender, piece), cases) in by {
        let _ = write!(out, "  {contender:<14} {piece:<18}");
        for (case, ms) in cases {
            let _ = write!(out, " {case}:{ms:.1}");
        }
        out.push('\n');
    }
    out
}

pub fn format_notes(notes: &[Note]) -> String {
    let mut by_text: BTreeMap<(&str, &str, &str), Vec<&str>> = BTreeMap::new();
    for n in notes.iter().filter(|n| n.delay_ms.is_none()) {
        by_text
            .entry((&n.piece, &n.contender, &n.text))
            .or_default()
            .push(&n.case);
    }
    by_text
        .into_iter()
        .map(|((piece, contender, text), cases)| {
            format!("note: {piece} {contender} {}: {text}\n", cases.join(","))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_judged_against_the_range_with_a_little_slack() {
        let refs = [0.2, 0.4];
        assert_eq!(verdict(0.1, &refs), Some(Verdict::Better));
        assert_eq!(verdict(0.3, &refs), Some(Verdict::Inside));
        assert_eq!(verdict(0.2, &refs), Some(Verdict::Inside));
        assert_eq!(verdict(0.405, &refs), Some(Verdict::Inside));
        assert_eq!(verdict(0.5, &refs), Some(Verdict::Worse));
    }

    #[test]
    fn one_reference_is_a_point_and_none_is_no_verdict() {
        assert_eq!(verdict(0.3, &[0.3]), Some(Verdict::Inside));
        assert_eq!(verdict(0.9, &[0.3]), Some(Verdict::Worse));
        assert_eq!(verdict(0.9, &[]), None);
        assert_eq!(verdict(f64::NAN, &[0.3]), None);
        assert_eq!(verdict(0.9, &[f64::NAN]), None);
    }
}
