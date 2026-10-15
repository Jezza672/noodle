//! The CI gate on the probes (M8).
//!
//! Noodle's warp modes are run on the probes and compared with the reference
//! stretchers' numbers, committed in `baseline/borrowed.json`. The rule is the
//! design's: on each cell (probe, tempo case, mode, metric) Noodle must be no
//! worse than the worst reference stretcher on the same material.
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

/// A metric, where it applies, and how far a worse-than-references cell may
/// move from its listed value: the larger of a quarter of it and `slack`.
struct Gated {
    metric: &'static str,
    probes: &'static [Probe],
    slack: f64,
}

const GATED: [Gated; 5] = [
    // Where the onsets land: only probes that have onsets.
    Gated {
        metric: "median_abs_offset_ms",
        probes: &[Probe::ClickTrain, Probe::LoneKick, Probe::NoiseBursts],
        slack: 6.0,
    },
    Gated {
        metric: "missed_onsets",
        probes: &[Probe::ClickTrain, Probe::LoneKick, Probe::NoiseBursts],
        slack: 0.15,
    },
    Gated {
        metric: "drift_ms",
        probes: &[Probe::ClickTrain, Probe::LoneKick, Probe::NoiseBursts],
        slack: 8.0,
    },
    // Whether the pitch is kept: only the probes with a single voice or sweep.
    Gated {
        metric: "pitch_median_abs_cents",
        probes: &[Probe::HeldChord, Probe::Sweep],
        slack: 10.0,
    },
    Gated {
        metric: "lufs_abs_delta",
        probes: &Probe::ALL,
        slack: 0.3,
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

/// Every gated cell where Noodle is worse than the references now, by key.
fn worse_cells() -> BTreeMap<String, f64> {
    let baseline = Baseline::from_json(include_str!("../baseline/borrowed.json")).unwrap();
    let cache = tempfile::tempdir().unwrap();
    // One thread to a probe: scoring is most of the time.
    let found: Vec<BTreeMap<String, f64>> = std::thread::scope(|scope| {
        let threads: Vec<_> = Probe::ALL
            .into_iter()
            .map(|probe| {
                let (baseline, cache) = (&baseline, cache.path());
                scope.spawn(move || worse_cells_of(probe, baseline, cache))
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    found.into_iter().flatten().collect()
}

fn worse_cells_of(probe: Probe, baseline: &Baseline, cache: &Path) -> BTreeMap<String, f64> {
    let mut worse = BTreeMap::new();
    {
        let options = Options {
            cases: CASES.iter().filter_map(|c| TempoCase::parse(c)).collect(),
            methods: vec![Method::Auto, Method::Grain, Method::Beats],
            references: vec![],
            cache: RefCache::new(cache),
            compensate_latency: true,
            export: None,
        };
        let (rows, _) = borrowed::run(&Piece::probe(probe), &options).unwrap();
        for row in rows.iter().filter(|r| r.view == "native") {
            for gated in GATED.iter().filter(|g| g.probes.contains(&probe)) {
                let Some(value) = row.metric(gated.metric).map(f64::abs) else {
                    continue;
                };
                let refs: Vec<f64> = baseline
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
                    .collect();
                // A metric the references could not measure (the drift of a
                // lone kick) has nothing to be worse than.
                if verdict(value, &refs) == Some(Verdict::Worse) {
                    worse.insert(
                        format!(
                            "{}/{}/{}/{}",
                            row.piece, row.case, row.contender, gated.metric
                        ),
                        (value * 1e3).round() / 1e3,
                    );
                }
            }
        }
    }
    worse
}

#[test]
fn noodle_is_no_worse_than_the_references_on_the_probes() {
    let now = worse_cells();
    if std::env::var_os("UPDATE_GATE").is_some() {
        let mut text = serde_json::to_string_pretty(&now).unwrap();
        text.push('\n');
        std::fs::write(path(), text).unwrap();
        return;
    }
    let listed: BTreeMap<String, f64> =
        serde_json::from_str(&std::fs::read_to_string(path()).unwrap()).unwrap();
    let mut failures = Vec::new();
    for (key, value) in &now {
        let metric = key.rsplit('/').next().unwrap();
        let slack = GATED.iter().find(|g| g.metric == metric).unwrap().slack;
        match listed.get(key) {
            None => failures.push(format!("{key}: {value} is worse than every reference")),
            Some(was) if *value > was + (0.25 * was).max(slack) => {
                failures.push(format!("{key}: {value}, was {was}"))
            }
            Some(_) => {}
        }
    }
    assert!(
        failures.is_empty(),
        "worse than the references or than the list:\n{}",
        failures.join("\n")
    );
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
