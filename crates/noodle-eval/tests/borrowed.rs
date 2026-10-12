//! Borrowed truth: the pipeline around the reference stretchers, with a
//! stand-in stretcher written here so no program has to be installed.
//!
//! The stand-in resamples along the plan's map (so it changes pitch, which
//! these tests do not care about) and delays its output by a known amount.
//! What is checked is the plumbing: the delay is measured and removed, a
//! stretcher that cannot follow a map is left out of those cases, the cache is
//! used, and a stretch of the wrong length is refused. How good the real
//! stretchers are is not tested; the numbers are in the baseline.

use noodle_eval::TimeMap;
use noodle_eval::borrowed::{self, Options, Piece, RATE, Verdict};
use noodle_eval::harness::Method;
use noodle_eval::probe::Probe;
use noodle_eval::refs::{
    Plan, RefCache, Stretcher, align, calibration, latency, to_stereo, write_pcm16,
};
use noodle_eval::tempo::TempoCase;
use noodle_io::{decode_file, write_wav};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fake {
    name: &'static str,
    map: bool,
    delay_seconds: f64,
    /// Scales the output's length, to be wrong on purpose.
    length_factor: f64,
    calls: Arc<AtomicUsize>,
}

impl Fake {
    fn new(name: &'static str, delay_seconds: f64) -> Self {
        Self {
            name,
            map: true,
            delay_seconds,
            length_factor: 1.0,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Stretcher for Fake {
    fn name(&self) -> String {
        self.name.into()
    }

    fn supports_map(&self) -> bool {
        self.map
    }

    fn fingerprint(&self) -> String {
        "fake".into()
    }

    fn stretch(&self, input: &Path, output: &Path, plan: &Plan) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let audio = decode_file(input).map_err(|e| e.to_string())?;
        let stereo = to_stereo(&audio);
        let frames = stereo.len() / 2;
        let rate = f64::from(plan.rate);
        let out_frames = (plan.output_frames() as f64 * self.length_factor) as usize;
        let delay = (self.delay_seconds * rate) as i64;
        let mut out = vec![0.0f32; out_frames * 2];
        for i in 0..out_frames {
            // The output at frame i plays what the map says, `delay` later.
            let at = i as i64 - delay;
            if at < 0 {
                continue;
            }
            let source = plan.map.to_source(at as f64 / rate) * rate;
            let (lo, frac) = (source.floor() as usize, source.fract() as f32);
            if lo + 1 >= frames {
                continue;
            }
            for c in 0..2 {
                out[i * 2 + c] =
                    stereo[lo * 2 + c] * (1.0 - frac) + stereo[(lo + 1) * 2 + c] * frac;
            }
        }
        write_wav(output, &out, 2, plan.rate).map_err(|e| e.to_string())
    }
}

fn options(cache: &Path, references: Vec<Box<dyn Stretcher>>, compensate: bool) -> Options {
    Options {
        cases: vec![TempoCase::Ratio(1.5), TempoCase::Step],
        methods: vec![Method::Grain],
        references,
        cache: RefCache::new(cache),
        compensate_latency: compensate,
        export: None,
    }
}

fn offset_ms(rows: &[borrowed::Row], contender: &str, case: &str) -> f64 {
    rows.iter()
        .find(|r| r.contender == contender && r.case == case && r.view == "source")
        .and_then(|r| r.metric("median_abs_offset_ms"))
        .unwrap()
}

#[test]
fn a_measured_delay_is_found_for_a_constant_ratio_and_a_changing_map() {
    let cal = calibration(8.0, RATE);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("cal.wav");
    write_pcm16(&input, &cal.samples, RATE).unwrap();
    let fake = Fake::new("fake", 0.037);
    for map in [
        TimeMap::Speed(1.5),
        TimeMap::Speed(0.5),
        TimeMap::Points(vec![(0.0, 0.0), (4.0, 3.2), (6.0, 7.2), (6.5, 8.0)]),
    ] {
        let out_seconds = map.to_output(8.0);
        let plan = Plan {
            rate: RATE,
            source_seconds: 8.0,
            output_seconds: out_seconds,
            map: map.clone(),
        };
        let output = dir.path().join("out.wav");
        fake.stretch(&input, &output, &plan).unwrap();
        let audio = decode_file(&output).unwrap();
        let measured = latency(&to_stereo(&audio), RATE, &map, &cal.times).unwrap();
        assert!(
            (measured.seconds - 0.037).abs() < 0.003,
            "{map:?}: measured {measured:?}"
        );
        assert!(measured.spread_seconds < 0.01, "{measured:?}");
        // Removing it puts the bursts where the map says.
        let aligned = align(
            &to_stereo(&audio),
            measured.seconds,
            RATE,
            plan.output_frames(),
        );
        let after = latency(&aligned, RATE, &map, &cal.times).unwrap();
        assert!(after.seconds.abs() < 0.002, "{after:?}");
    }
}

#[test]
fn the_delay_is_removed_before_scoring_and_costs_timing_when_it_is_not() {
    let cache = tempfile::tempdir().unwrap();
    let piece = Piece::probe(Probe::ClickTrain);
    let fake = || -> Vec<Box<dyn Stretcher>> { vec![Box::new(Fake::new("fake", 0.04))] };

    let (rows, _) = borrowed::run(&piece, &options(cache.path(), fake(), true)).unwrap();
    for case in ["x1.5", "step"] {
        assert!(
            offset_ms(&rows, "fake", case) < 5.0,
            "{case}: {}",
            offset_ms(&rows, "fake", case)
        );
    }

    // The same stretcher with the delay left in is scored 40 ms late: this is
    // what the correction is worth, and it shows the check can fail.
    let (late, _) = borrowed::run(&piece, &options(cache.path(), fake(), false)).unwrap();
    assert!(offset_ms(&late, "fake", "x1.5") > 30.0);
    assert!(offset_ms(&late, "fake", "step") > 30.0);
}

#[test]
fn rows_cover_every_contender_in_both_views_for_a_probe() {
    let cache = tempfile::tempdir().unwrap();
    let piece = Piece::probe(Probe::ClickTrain);
    let refs: Vec<Box<dyn Stretcher>> = vec![Box::new(Fake::new("fake", 0.0))];
    let (rows, notes) = borrowed::run(&piece, &options(cache.path(), refs, true)).unwrap();
    for case in ["x1.5", "step"] {
        for contender in ["noodle-grain", "fake"] {
            for view in ["source", "native"] {
                assert!(
                    rows.iter()
                        .any(|r| r.case == case && r.contender == contender && r.view == view),
                    "{case} {contender} {view}"
                );
            }
        }
    }
    assert!(
        rows.iter()
            .filter(|r| r.reference)
            .all(|r| r.contender == "fake")
    );
    assert!(notes.iter().all(|n| n.delay_ms.is_some()), "{notes:?}");
    // The baseline keys name piece, case, contender and view.
    let b = borrowed::baseline(&rows);
    assert!(b.0.contains_key("click-train/x1.5/fake/native"));
}

#[test]
fn a_constant_ratio_stretcher_sits_out_the_changing_cases() {
    let cache = tempfile::tempdir().unwrap();
    let piece = Piece::probe(Probe::LoneKick);
    let mut constant = Fake::new("constant-only", 0.0);
    constant.map = false;
    let refs: Vec<Box<dyn Stretcher>> = vec![Box::new(constant)];
    let (rows, notes) = borrowed::run(&piece, &options(cache.path(), refs, true)).unwrap();
    assert!(
        rows.iter()
            .any(|r| r.contender == "constant-only" && r.case == "x1.5")
    );
    assert!(
        !rows
            .iter()
            .any(|r| r.contender == "constant-only" && r.case == "step")
    );
    assert!(
        notes
            .iter()
            .any(|n| n.case == "step" && n.text.contains("constant ratio"))
    );
}

#[test]
fn a_second_run_reuses_the_cached_stretches() {
    let cache = tempfile::tempdir().unwrap();
    let piece = Piece::probe(Probe::LoneKick);
    let fake = Fake::new("fake", 0.0);
    let calls = fake.calls.clone();
    let mut opts = options(cache.path(), vec![Box::new(fake)], true);
    opts.methods = vec![];
    borrowed::run(&piece, &opts).unwrap();
    // Two cases, each the clip and its calibration.
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    borrowed::run(&piece, &opts).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[test]
fn a_stretch_of_the_wrong_length_is_refused_and_reported() {
    let cache = tempfile::tempdir().unwrap();
    let piece = Piece::probe(Probe::LoneKick);
    let mut short = Fake::new("short", 0.0);
    short.length_factor = 0.8;
    let refs: Vec<Box<dyn Stretcher>> = vec![Box::new(short), Box::new(Fake::new("good", 0.0))];
    let (rows, notes) = borrowed::run(&piece, &options(cache.path(), refs, true)).unwrap();
    assert!(!rows.iter().any(|r| r.contender == "short"));
    assert!(rows.iter().any(|r| r.contender == "good"));
    assert!(
        notes
            .iter()
            .any(|n| n.contender == "short" && n.text.starts_with("failed:"))
    );
}

#[test]
fn the_standings_count_a_noodle_mode_against_the_references_spread() {
    let cache = tempfile::tempdir().unwrap();
    let piece = Piece::probe(Probe::ClickTrain);
    let refs: Vec<Box<dyn Stretcher>> =
        vec![Box::new(Fake::new("a", 0.0)), Box::new(Fake::new("b", 0.0))];
    let mut opts = options(cache.path(), refs, true);
    opts.methods = vec![Method::Grain, Method::Repitch];
    let (rows, _) = borrowed::run(&piece, &opts).unwrap();
    let standings = borrowed::standings(&rows);
    let tally = standings["SSMAD"]["noodle-grain"];
    // Two cases, two views.
    assert_eq!(tally.total(), 4);
    // The floor is shown in tables but never counted.
    assert!(!standings["SSMAD"].contains_key("noodle-repitch"));
    let text = borrowed::format_standings(&rows);
    assert!(
        text.contains("noodle-grain") && text.contains("SSMAD"),
        "{text}"
    );
    assert!(borrowed::format_tables(&rows).contains("click-train at x1.5 against the native"));
    assert_eq!(borrowed::verdict(9.0, &[1.0, 2.0]), Some(Verdict::Worse));
}

/// With Rubber Band, SoundTouch or Signalsmith installed, the real programs
/// follow the plan: the right length, and bursts where the map says once
/// their delay is removed. Skipped when none are installed (CI has none).
#[test]
fn installed_stretchers_follow_the_plan() {
    use noodle_eval::refs::Tool;
    let bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/cache/bin");
    let cal = calibration(8.0, RATE);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("cal.wav");
    write_pcm16(&input, &cal.samples, RATE).unwrap();
    for tool in Tool::ALL {
        let Some(found) = tool.find(std::slice::from_ref(&bin)) else {
            eprintln!("{} is not installed; skipped", tool.name());
            continue;
        };
        let cases = [TimeMap::Speed(1.5), TimeMap::Speed(0.75)]
            .into_iter()
            .chain(
                found
                    .supports_map()
                    .then(|| TimeMap::Points(vec![(0.0, 0.0), (5.0, 4.0), (7.0, 8.0)])),
            );
        for map in cases {
            let plan = Plan {
                rate: RATE,
                source_seconds: 8.0,
                output_seconds: map.to_output(8.0),
                map: map.clone(),
            };
            let output = dir.path().join("out.wav");
            found.stretch(&input, &output, &plan).unwrap();
            let audio = decode_file(&output).unwrap();
            let frames = audio.samples.len() / audio.channels;
            assert!(
                (frames as f64 / f64::from(RATE) - plan.output_seconds).abs() < 0.2,
                "{} {map:?}: {frames} frames",
                tool.name()
            );
            let measured = latency(&to_stereo(&audio), RATE, &map, &cal.times).unwrap();
            let aligned = align(
                &to_stereo(&audio),
                measured.seconds,
                RATE,
                plan.output_frames(),
            );
            let after = latency(&aligned, RATE, &map, &cal.times).unwrap();
            assert!(
                after.seconds.abs() < 0.003,
                "{} {map:?}: {measured:?} then {after:?}",
                tool.name()
            );
        }
    }
}

#[test]
fn a_clip_file_becomes_a_stereo_piece_at_the_scoring_rate() {
    let dir = tempfile::tempdir().unwrap();
    // 3 s of mono at 44.1 kHz, plus a clip too short to warp and a non-audio file.
    let tone: Vec<f32> = (0..44_100 * 3)
        .map(|i| 0.5 * (i as f32 * 0.05).sin())
        .collect();
    write_wav(&dir.path().join("tone.wav"), &tone, 1, 44_100).unwrap();
    write_wav(&dir.path().join("blip.wav"), &tone[..4_000], 1, 44_100).unwrap();
    std::fs::write(dir.path().join("notes.txt"), "not audio").unwrap();

    let found = borrowed::find_clips(&[dir.path().to_path_buf(), dir.path().join("missing")]);
    let names: Vec<_> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(names, ["blip.wav", "tone.wav"]);

    let piece = borrowed::load_clip(&dir.path().join("tone.wav"), 30.0).unwrap();
    assert_eq!(piece.name, "tone");
    assert_eq!(piece.source.len(), RATE as usize * 3 * 2);
    // Mono is played on both sides, rounded to what a 16-bit file holds.
    assert!(piece.source.as_chunks::<2>().0.iter().all(|f| f[0] == f[1]));
    assert_eq!(piece.source, noodle_eval::refs::quantise(&piece.source));
    // --seconds cuts it.
    let cut = borrowed::load_clip(&dir.path().join("tone.wav"), 2.0).unwrap();
    assert_eq!(cut.source.len(), RATE as usize * 2 * 2);

    let err = borrowed::load_clip(&dir.path().join("blip.wav"), 30.0)
        .err()
        .unwrap();
    assert!(err.contains("too short"), "{err}");
}

#[test]
fn a_clip_has_no_native_render_so_it_is_scored_against_the_source_alone() {
    let dir = tempfile::tempdir().unwrap();
    // Four seconds of a tone with a click every half second.
    let signal: Vec<f32> = (0..RATE as usize * 4)
        .map(|i| {
            let click = if i % (RATE as usize / 2) < 200 {
                0.6
            } else {
                0.0
            };
            click + 0.1 * (i as f32 * 0.03).sin()
        })
        .collect();
    write_wav(&dir.path().join("clicks.wav"), &signal, 1, RATE).unwrap();
    let piece = borrowed::load_clip(&dir.path().join("clicks.wav"), 30.0).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let refs: Vec<Box<dyn Stretcher>> = vec![Box::new(Fake::new("fake", 0.02))];
    let mut opts = options(cache.path(), refs, true);
    opts.cases = vec![TempoCase::Ratio(2.0)];
    let (rows, _) = borrowed::run(&piece, &opts).unwrap();
    assert!(rows.iter().all(|r| r.view == "source"));
    assert_eq!(rows.len(), 2, "grain and the fake");
    // Twice as fast: half as long, with the clicks where the map says.
    assert!(offset_ms(&rows, "fake", "x2") < 5.0);
    assert!(offset_ms(&rows, "noodle-grain", "x2") < 25.0);
}

#[test]
fn a_changed_plan_is_stretched_again_not_replayed_from_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let stereo = vec![0.1f32; RATE as usize * 4 * 2];
    write_pcm16(&input, &stereo, RATE).unwrap();
    let fake = Fake::new("fake", 0.0);
    let cache = RefCache::new(dir.path().join("cache"));
    let plan = |points: Vec<(f64, f64)>| Plan {
        rate: RATE,
        source_seconds: 4.0,
        output_seconds: 4.0,
        map: TimeMap::Points(points),
    };
    let a = plan(vec![(0.0, 0.0), (2.0, 2.0), (4.0, 4.0)]);
    let b = plan(vec![(0.0, 0.0), (1.0, 2.0), (4.0, 4.0)]);
    for p in [&a, &a, &b] {
        cache.stretched(&fake, "k", &input, "wobble", p).unwrap();
    }
    // The same case name and total length, but a different map.
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
}
