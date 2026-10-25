//! Reading the harness's rows: a table, a rank summary, and a diff against a
//! committed baseline.
//!
//! The baseline holds metric numbers only (no audio), keyed by
//! `probe/case/method/view`. It shows a before and after for a change to a
//! warp method. It is not a threshold (ADR 0003): nothing here fails because a
//! number moved.

use crate::harness::{Method, Row};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write;

/// The metrics in the printed table, the paper's and the ones that say most
/// about a probe's failure.
pub const HEADLINE: [&str; 8] = [
    "SSMAD",
    "SPhMW",
    "DeltaP_abs",
    "TrRat_abs_ln",
    "HPSTrRat_abs_ln",
    "median_abs_offset_ms",
    "pitch_median_abs_cents",
    "log_distance_db",
];

/// Metrics left out of the ranking: loudness is gain-matched away before
/// scoring, so these only measure the matching, and the detected tempo is not
/// a measure of the sound.
const UNRANKED: [&str; 3] = ["lufs_abs_delta", "peak_abs_delta_db", "detected_bpm"];

/// Metric numbers by row key, then metric name. `null` is a value that could
/// not be measured.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Baseline(pub BTreeMap<String, BTreeMap<String, Option<f64>>>);

impl Baseline {
    pub fn of(rows: &[Row]) -> Self {
        Self::of_entries(rows.iter().map(|row| (row.key(), row.metrics.as_slice())))
    }

    /// A baseline from (row key, metrics) pairs, for rows that are not the
    /// harness's.
    pub fn of_entries<'a>(entries: impl Iterator<Item = (String, &'a [(String, f64)])>) -> Self {
        Self(
            entries
                .map(|(key, metrics)| {
                    let metrics = metrics
                        .iter()
                        .map(|(n, v)| {
                            let rounded = (v * 1e6).round() / 1e6;
                            (n.clone(), rounded.is_finite().then_some(rounded))
                        })
                        .collect();
                    (key, metrics)
                })
                .collect(),
        )
    }

    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("numbers serialise");
        text.push('\n');
        text
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// Replaces the rows `fresh` has and keeps the rest, so a run of part of
    /// the grid can update its part of the baseline.
    pub fn merged(mut self, fresh: Baseline) -> Self {
        self.0.extend(fresh.0);
        self
    }
}

/// A metric that moved against the baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub key: String,
    pub metric: String,
    pub before: Option<f64>,
    pub after: Option<f64>,
}

/// A change smaller than this, relative to the larger number and with an
/// absolute floor, is rounding and not reported.
const RELATIVE: f64 = 0.01;
const ABSOLUTE: f64 = 1e-4;

/// How `fresh` differs from `old`, for the rows in `fresh`. A row or metric
/// that the baseline lacks shows as a change from nothing.
pub fn diff(old: &Baseline, fresh: &Baseline) -> Vec<Change> {
    let mut changes = Vec::new();
    for (key, metrics) in &fresh.0 {
        for (metric, after) in metrics {
            let before = old
                .0
                .get(key)
                .and_then(|m| m.get(metric))
                .copied()
                .flatten();
            let moved = match (before, *after) {
                (None, None) => false,
                (Some(a), Some(b)) => (a - b).abs() > ABSOLUTE.max(RELATIVE * a.abs().max(b.abs())),
                _ => true,
            };
            if moved {
                changes.push(Change {
                    key: key.clone(),
                    metric: metric.clone(),
                    before,
                    after: *after,
                });
            }
        }
    }
    changes
}

/// Each contender's mean rank (1 is best) over every probe, tempo and view,
/// and every metric that all contenders have a number for. Ties share the
/// average of their places. Repitch, the floor, is not ranked.
///
/// Every metric counts once, so the many diagnostics weigh as much as the
/// paper's features; the summary says who is better on the whole, and the
/// table says on what.
pub fn mean_ranks(rows: &[Row]) -> BTreeMap<Method, f64> {
    let mut groups: BTreeMap<(&str, &str, &str), Vec<&Row>> = BTreeMap::new();
    for row in rows.iter().filter(|r| r.method.is_contender()) {
        groups
            .entry((&row.probe, &row.case, row.view.name()))
            .or_default()
            .push(row);
    }
    let mut sums: BTreeMap<Method, (f64, usize)> = BTreeMap::new();
    for group in groups.values().filter(|g| g.len() > 1) {
        for (name, _) in &group[0].metrics {
            if UNRANKED.contains(&name.as_str()) {
                continue;
            }
            let values: Vec<Option<f64>> = group
                .iter()
                .map(|r| r.metric(name).filter(|v| v.is_finite()).map(f64::abs))
                .collect();
            if values.iter().any(Option::is_none) {
                continue;
            }
            for (row, rank) in group.iter().zip(ranks(&values)) {
                let entry = sums.entry(row.method).or_default();
                entry.0 += rank;
                entry.1 += 1;
            }
        }
    }
    sums.into_iter()
        .map(|(m, (sum, n))| (m, sum / n as f64))
        .collect()
}

/// Places from 1, lowest value first, ties averaged.
fn ranks(values: &[Option<f64>]) -> Vec<f64> {
    let v: Vec<f64> = values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
    v.iter()
        .map(|x| {
            let below = v.iter().filter(|y| **y < *x - 1e-12).count() as f64;
            let same = v.iter().filter(|y| (**y - *x).abs() <= 1e-12).count() as f64;
            below + (same + 1.0) / 2.0
        })
        .collect()
}

pub fn cell(v: Option<f64>) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:.3}"),
        _ => "-".into(),
    }
}

/// The rows as a text table of the [`HEADLINE`] metrics.
pub fn format_rows(rows: &[Row]) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "{:<13} {:<7} {:<8} {:<10}",
        "probe", "tempo", "method", "view"
    );
    for h in HEADLINE {
        let _ = write!(out, " {:>12}", short(h));
    }
    out.push('\n');
    for row in rows {
        let _ = write!(
            out,
            "{:<13} {:<7} {:<8} {:<10}",
            row.probe,
            row.case,
            row.method.name(),
            row.view.name()
        );
        for h in HEADLINE {
            let _ = write!(out, " {:>12}", cell(row.metric(h)));
        }
        out.push('\n');
    }
    out
}

fn short(name: &str) -> &str {
    match name {
        "median_abs_offset_ms" => "offset_ms",
        "pitch_median_abs_cents" => "pitch_cents",
        "HPSTrRat_abs_ln" => "HPSTrRat_ln",
        other => other,
    }
}

pub fn format_ranks(ranks: &BTreeMap<Method, f64>) -> String {
    let mut sorted: Vec<_> = ranks.iter().collect();
    sorted.sort_by(|a, b| a.1.total_cmp(b.1));
    if ranks.is_empty() {
        return String::new();
    }
    let mut out = String::from("mean rank over probes, tempos, views and metrics (1 is best)\n");
    for (m, r) in sorted {
        let _ = writeln!(out, "  {:<8} {r:.2}", m.name());
    }
    out.push_str("  (repitch is the floor and is not ranked)\n");
    out
}

pub fn format_changes(changes: &[Change]) -> String {
    // Numbers the baseline never had are new, not moved.
    let (new, moved): (Vec<&Change>, Vec<&Change>) =
        changes.iter().partition(|c| c.before.is_none());
    let mut out = String::new();
    if !new.is_empty() {
        let _ = writeln!(
            out,
            "{} metrics have no baseline yet (--update adds them)",
            new.len()
        );
    }
    if moved.is_empty() {
        out.push_str("no metric moved against the baseline\n");
        return out;
    }
    let _ = writeln!(out, "{} metrics moved against the baseline", moved.len());
    for c in moved {
        let _ = writeln!(
            out,
            "  {:<44} {:<28} {:>10} -> {:>10}",
            c.key,
            c.metric,
            cell(c.before),
            cell(c.after)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::View;

    fn row(method: Method, value: f64) -> Row {
        Row {
            probe: "p".into(),
            case: "x2".into(),
            method,
            view: View::Native,
            metrics: vec![("SSMAD".into(), value), ("SPhMW".into(), value * 2.0)],
        }
    }

    #[test]
    fn the_better_method_ranks_first_and_the_floor_is_left_out() {
        let rows = [
            row(Method::Grain, 0.2),
            row(Method::Beats, 0.1),
            row(Method::Repitch, 5.0),
        ];
        let ranks = mean_ranks(&rows);
        assert_eq!(ranks[&Method::Beats], 1.0);
        assert_eq!(ranks[&Method::Grain], 2.0);
        assert!(!ranks.contains_key(&Method::Repitch));
    }

    #[test]
    fn ties_share_a_place() {
        let rows = [row(Method::Grain, 0.2), row(Method::Beats, 0.2)];
        let ranks = mean_ranks(&rows);
        assert_eq!(ranks[&Method::Grain], 1.5);
        assert_eq!(ranks[&Method::Beats], 1.5);
    }

    #[test]
    fn a_baseline_round_trips_and_reports_only_what_moved() {
        let old = Baseline::of(&[row(Method::Grain, 0.2), row(Method::Beats, f64::NAN)]);
        let again = Baseline::from_json(&old.to_json()).unwrap();
        assert_eq!(again, old);
        assert!(again.0["p/x2/beats/native"]["SSMAD"].is_none());

        let fresh = Baseline::of(&[row(Method::Grain, 0.2004), row(Method::Beats, 0.3)]);
        let changes = diff(&old, &fresh);
        // Grain moved by rounding; Beats gained numbers where it had none.
        assert!(
            changes.iter().all(|c| c.key.contains("beats")),
            "{changes:?}"
        );
        assert_eq!(changes.len(), 2);
        let worse = Baseline::of(&[row(Method::Grain, 0.4)]);
        assert_eq!(diff(&old, &worse).len(), 2);
    }

    #[test]
    fn a_part_run_updates_only_its_part() {
        let old = Baseline::of(&[row(Method::Grain, 0.2), row(Method::Beats, 0.1)]);
        let merged = old.merged(Baseline::of(&[row(Method::Grain, 0.5)]));
        assert_eq!(merged.0["p/x2/grain/native"]["SSMAD"], Some(0.5));
        assert_eq!(merged.0["p/x2/beats/native"]["SSMAD"], Some(0.1));
    }
}
