//! The CI gate on the probes (M8).
//!
//! Noodle's warp modes are run on the probes and compared with the reference
//! stretchers' numbers, committed in `baseline/borrowed.json`. The rule is the
//! design's: on each cell (probe, tempo case, mode, metric) Noodle must be no
//! worse than the worst reference stretcher on the same material (on the
//! sweep's and lone kick's step case that is Rubber Band R2 alone, the only
//! reference that follows a changing tempo).
//!
//! Noodle is not there yet on every cell, so the cells that are worse today
//! are listed in `baseline/gate.json` with the value they have. Such a cell
//! may not get worse than its listed value (plus a slack for how the numbers
//! differ between platforms), and any other cell may not be worse than the
//! references at all. The list is a debt to pay down, not a threshold:
//! regenerate it with `UPDATE_GATE=1 cargo test -p noodle-eval --test gate`
//! when a change makes cells better, and never to excuse a regression.
//!
//! Only metrics that measure something physical gate: where the onsets land,
//! the pitch, the loudness. The Roberts and Paliwal features stay out until
//! `noodle-eval validate` has been run against the listening-test data.

use noodle_eval::borrowed::{self, Options, Piece, Verdict, verdict};
use noodle_eval::harness::Method;
use noodle_eval::probe::Probe;
use noodle_eval::refs::RefCache;
use noodle_eval::table::Baseline;
use noodle_eval::tempo::TempoCase;
use std::collections::BTreeMap;
use std::path::Path;

/// A metric, where it applies, and its measurement `step`: the smallest move
/// that is a different measurement, not platform noise (offsets and drift move
/// a whole analysis hop, 2.667 ms, at a time; the others are counts or are
/// stable to well under the step). A cell may sit one step over the worst
/// reference, and a listed cell may move one step plus a tenth from its value.
struct Gated {
    metric: &'static str,
    probes: &'static [Probe],
    step: f64,
}

const GATED: [Gated; 5] = [
    // Where the onsets land: only probes that have onsets.
    Gated {
        metric: "median_abs_offset_ms",
        probes: &[Probe::ClickTrain, Probe::LoneKick, Probe::NoiseBursts],
        step: 2.7,
    },
    Gated {
        metric: "missed_onsets",
        probes: &[Probe::ClickTrain, Probe::LoneKick, Probe::NoiseBursts],
        step: 0.0,
    },
    Gated {
        metric: "drift_ms",
        probes: &[Probe::ClickTrain, Probe::LoneKick, Probe::NoiseBursts],
        step: 2.7,
    },
    // Whether the pitch is kept: only the probes with a single voice or sweep.
    Gated {
        metric: "pitch_median_abs_cents",
        probes: &[Probe::HeldChord, Probe::Sweep],
        step: 1.0,
    },
    Gated {
        metric: "lufs_abs_delta",
        probes: &Probe::ALL,
        step: 0.1,
    },
];

/// A slow and a fast constant ratio and a step: the grid's three corners, as the
/// whole grid takes minutes. `noodle-eval borrowed` runs the rest.
const CASES: [&str; 3] = ["x0.75", "x1.5", "step"];

fn is_reference(contender: &str) -> bool {
    contender.starts_with("rubberband") || contender == "soundtouch" || contender == "signalsmith"
}

fn path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline/gate.json")
}

/// One measured cell: Noodle's value and the worst reference's.
#[derive(Clone, Copy, Debug)]
struct Cell {
    value: f64,
    worst_reference: f64,
}

/// Every gated cell that has reference numbers, by key.
fn measure() -> BTreeMap<String, Cell> {
    let baseline = Baseline::from_json(include_str!("../baseline/borrowed.json")).unwrap();
    let cache = tempfile::tempdir().unwrap();
    // One thread to a probe: scoring is most of the time.
    let found: Vec<BTreeMap<String, Cell>> = std::thread::scope(|scope| {
        let threads: Vec<_> = Probe::ALL
            .into_iter()
            .map(|probe| {
                let (baseline, cache) = (&baseline, cache.path());
                scope.spawn(move || measure_probe(probe, baseline, cache))
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    found.into_iter().flatten().collect()
}

fn measure_probe(probe: Probe, baseline: &Baseline, cache: &Path) -> BTreeMap<String, Cell> {
    let options = Options {
        cases: CASES
            .iter()
            .map(|c| TempoCase::parse(c).expect(c))
            .collect(),
        methods: vec![Method::Auto, Method::Grain, Method::Beats],
        references: vec![],
        cache: RefCache::new(cache),
        compensate_latency: true,
        export: None,
    };
    let (rows, _) = borrowed::run(&Piece::probe(probe), &options).unwrap();
    let mut cells = BTreeMap::new();
    for row in rows.iter().filter(|r| r.view == "native") {
        for gated in GATED.iter().filter(|g| g.probes.contains(&probe)) {
            let Some(value) = row.metric(gated.metric).map(f64::abs) else {
                continue;
            };
            let worst = baseline
                .0
                .iter()
                .filter_map(|(key, metrics)| {
                    let part: Vec<&str> = key.split('/').collect();
                    (part[0] == row.piece
                        && part[1] == row.case
                        && part[3] == "native"
                        && is_reference(part[2]))
                    .then(|| metrics.get(gated.metric).copied().flatten())
                    .flatten()
                })
                .map(f64::abs)
                .reduce(f64::max);
            // A metric the references could not measure (the drift of a lone
            // kick) has nothing to be worse than.
            let Some(worst_reference) = worst else {
                continue;
            };
            cells.insert(
                format!(
                    "{}/{}/{}/{}",
                    row.piece, row.case, row.contender, gated.metric
                ),
                Cell {
                    value: (value * 1e3).round() / 1e3,
                    worst_reference,
                },
            );
        }
    }
    cells
}

fn step_of(key: &str) -> f64 {
    let metric = key.rsplit('/').next().unwrap();
    GATED.iter().find(|g| g.metric == metric).unwrap().step
}

/// The cells to list: worse than the references, or within a step of it.
fn debt(now: &BTreeMap<String, Cell>) -> BTreeMap<String, f64> {
    now.iter()
        .filter(|(key, c)| c.value >= c.worst_reference - step_of(key))
        .map(|(key, c)| (key.clone(), c.value))
        .collect()
}

/// What fails: an unlisted cell worse than the references by more than a
/// step, and a listed cell that moved past its value.
fn judge(now: &BTreeMap<String, Cell>, listed: &BTreeMap<String, f64>) -> Vec<String> {
    let mut failures = Vec::new();
    for (key, cell) in now {
        let step = step_of(key);
        match listed.get(key) {
            None => {
                if verdict(cell.value, &[cell.worst_reference]) == Some(Verdict::Worse)
                    && cell.value > cell.worst_reference + step
                {
                    failures.push(format!(
                        "{key}: {} is worse than the worst reference, {}",
                        cell.value, cell.worst_reference
                    ));
                }
            }
            Some(was) if cell.value > was * 1.1 + step => {
                failures.push(format!("{key}: {}, was {was}", cell.value));
            }
            Some(_) => {}
        }
    }
    failures
}

#[test]
fn noodle_is_no_worse_than_the_references_on_the_probes() {
    let now = measure();
    if std::env::var_os("UPDATE_GATE").is_some() {
        let mut text = serde_json::to_string_pretty(&debt(&now)).unwrap();
        text.push('\n');
        std::fs::write(path(), text).unwrap();
        return;
    }
    let listed: BTreeMap<String, f64> =
        serde_json::from_str(&std::fs::read_to_string(path()).unwrap()).unwrap();
    let failures = judge(&now, &listed);
    assert!(
        failures.is_empty(),
        "worse than the references or than the list:\n{}",
        failures.join("\n")
    );
    // The list should not outlive the debt.
    for key in listed.keys() {
        let paid = now
            .get(key)
            .is_none_or(|c| c.value < c.worst_reference - step_of(key));
        if paid {
            println!("note: {key} is inside the references now; UPDATE_GATE=1 drops it");
        }
    }
}

#[test]
fn the_decision_fails_what_it_should() {
    let cell = |value, worst_reference| Cell {
        value,
        worst_reference,
    };
    let key = "click-train/x1.5/noodle-grain/median_abs_offset_ms".to_string();
    let judged = |value, listed: &BTreeMap<String, f64>| {
        judge(&BTreeMap::from([(key.clone(), cell(value, 10.0))]), listed)
    };
    // Unlisted: inside, a step over (platform noise), and well over.
    let none = BTreeMap::new();
    assert!(judged(8.0, &none).is_empty());
    assert!(judged(12.0, &none).is_empty());
    assert_eq!(judged(20.0, &none).len(), 1);
    // Listed at 20: the same passes, a tenth and a step more passes, past fails.
    let listed = BTreeMap::from([(key.clone(), 20.0)]);
    assert!(judged(20.0, &listed).is_empty());
    assert!(judged(24.0, &listed).is_empty());
    assert_eq!(judged(30.0, &listed).len(), 1);
    // Counts have no step: one more missed onset fails.
    let missed = "lone-kick/x1.5/noodle-grain/missed_onsets".to_string();
    let one = |value| {
        judge(
            &BTreeMap::from([(missed.clone(), cell(value, 0.5))]),
            &BTreeMap::new(),
        )
    };
    assert!(one(0.5).is_empty());
    assert_eq!(one(1.0).len(), 1);
}

#[test]
fn the_gate_fails_a_warp_that_is_broken() {
    // A gate that never fails is no gate: a Noodle row shifted late by 100 ms
    // has to be worse than the references on the click train.
    let baseline = Baseline::from_json(include_str!("../baseline/borrowed.json")).unwrap();
    let refs: Vec<f64> = baseline
        .0
        .iter()
        .filter(|(key, _)| key.starts_with("click-train/x1.5/") && key.ends_with("/native"))
        .filter(|(key, _)| is_reference(key.split('/').nth(2).unwrap()))
        .filter_map(|(_, m)| m.get("median_abs_offset_ms").copied().flatten())
        .collect();
    assert!(refs.len() >= 3, "{refs:?}");
    assert_eq!(verdict(100.0, &refs), Some(Verdict::Worse));
}
