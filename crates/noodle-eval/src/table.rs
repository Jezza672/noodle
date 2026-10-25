//! Reading the harness's rows: a table, a rank summary, and a diff against a
//! committed baseline.
//!
//! The baseline holds metric numbers only (no audio), keyed by
//! `probe/case/method/view`. It shows a before and after for a change to a
//! warp method. It is not a threshold (ADR 0003): nothing here fails because a
//! number moved.

use crate::harness::{Method, Row, View};
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

    /// The rows this baseline holds, so a verdict can be read again without
    /// rerunning the grid. Keys that do not parse (a method or view this
    /// version does not know) are skipped, and a number that could not be
    /// measured is `NaN`.
    pub fn rows(&self) -> Vec<Row> {
        self.0
            .iter()
            .filter_map(|(key, metrics)| {
                let mut parts = key.rsplitn(4, '/');
                let last = parts.next()?;
                let view = View::ALL.into_iter().find(|v| v.name() == last)?;
                let method = Method::parse(parts.next()?)?;
                let case = parts.next()?.to_string();
                let probe = parts.next()?.to_string();
                Some(Row {
                    probe,
                    case,
                    method,
                    view,
                    metrics: metrics
                        .iter()
                        .map(|(n, v)| (n.clone(), v.unwrap_or(f64::NAN)))
                        .collect(),
                })
            })
            .collect()
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

/// One split contender against the best whole-mix mode.
#[derive(Debug, Clone, PartialEq)]
pub struct Versus {
    /// Metric cells compared.
    pub cells: usize,
    /// Cells where the contender's error is lower than every whole-mix mode's.
    pub wins: usize,
    /// Median of the contender's error over the best whole-mix mode's: below
    /// 1 is better.
    pub median_ratio: f64,
}

/// Compares each contender that is not a whole-mix mode with the best
/// whole-mix mode (Auto, Grain or Beats, whichever scores lowest on that
/// metric of that song, tempo and view). Only metrics every method has a
/// number for count, as in [`mean_ranks`]. `pick` limits the rows, such as
/// to one view or one song.
pub fn versus_best_whole_mix(
    rows: &[Row],
    pick: impl Fn(&Row) -> bool,
) -> BTreeMap<Method, Versus> {
    versus(rows, pick, &Method::WHOLE_MIX)
}

/// [`versus_best_whole_mix`] with any set of yardstick methods: each other
/// contender is compared with the best of `yardstick` per metric.
pub fn versus(
    rows: &[Row],
    pick: impl Fn(&Row) -> bool,
    yardstick: &[Method],
) -> BTreeMap<Method, Versus> {
    versus_of(rows, pick, yardstick, |_| true)
}

/// [`versus`] over only the metrics `wanted` picks.
pub fn versus_of(
    rows: &[Row],
    pick: impl Fn(&Row) -> bool,
    yardstick: &[Method],
    wanted: impl Fn(&str) -> bool,
) -> BTreeMap<Method, Versus> {
    let mut groups: BTreeMap<(&str, &str, &str), Vec<&Row>> = BTreeMap::new();
    for row in rows.iter().filter(|r| pick(r)) {
        groups
            .entry((&row.probe, &row.case, row.view.name()))
            .or_default()
            .push(row);
    }
    let mut ratios: BTreeMap<Method, (usize, Vec<f64>)> = BTreeMap::new();
    for group in groups.values() {
        let whole: Vec<&&Row> = group
            .iter()
            .filter(|r| yardstick.contains(&r.method))
            .collect();
        if whole.is_empty() {
            continue;
        }
        for row in group
            .iter()
            .filter(|r| !Method::WHOLE_MIX.contains(&r.method))
        {
            for (name, _) in &row.metrics {
                if UNRANKED.contains(&name.as_str()) || !wanted(name) {
                    continue;
                }
                let value = |r: &Row| r.metric(name).filter(|v| v.is_finite()).map(f64::abs);
                let Some(mine) = value(row) else { continue };
                let best = whole
                    .iter()
                    .map(|r| value(r))
                    .collect::<Option<Vec<f64>>>()
                    .and_then(|v| v.into_iter().reduce(f64::min));
                let Some(best) = best else { continue };
                let entry = ratios.entry(row.method).or_default();
                if mine < best {
                    entry.0 += 1;
                }
                if best > 1e-9 {
                    entry.1.push(mine / best);
                } else if mine <= 1e-9 {
                    entry.1.push(1.0);
                }
            }
        }
    }
    ratios
        .into_iter()
        .map(|(m, (wins, mut r))| {
            r.sort_by(f64::total_cmp);
            let median_ratio = r.get(r.len() / 2).copied().unwrap_or(f64::NAN);
            (
                m,
                Versus {
                    cells: r.len(),
                    wins,
                    median_ratio,
                },
            )
        })
        .collect()
}

/// The ceiling experiment's verdict as text: per view, how often and by how
/// much each split contender beats the best whole-mix mode, and then the same
/// per song in the native view (the answer key).
pub fn format_ceiling(rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str(
        "split contenders against the best whole-mix mode (auto, grain or beats) per metric:\n\
         wins = share of metric cells with a lower error; ratio = median error over the best whole-mix error (below 1 is better)\n",
    );
    let mut table = |title: &str, pick: &dyn Fn(&Row) -> bool, yard: &[Method]| {
        let versus = versus(rows, pick, yard);
        if versus.is_empty() {
            return;
        }
        let _ = writeln!(out, "\n{title}");
        for (m, v) in versus {
            let _ = writeln!(
                out,
                "  {:<10} wins {:>5.1}% of {:>5}   ratio {:>6.3}",
                m.name(),
                100.0 * v.wins as f64 / v.cells.max(1) as f64,
                v.cells,
                v.median_ratio
            );
        }
    };
    for view in View::ALL {
        table(
            &format!("all songs, {} view", view.name()),
            &|r| r.view == view,
            &Method::WHOLE_MIX,
        );
    }
    for yard in Method::WHOLE_MIX {
        table(
            &format!("all songs, native view, against {} alone", yard.name()),
            &|r| r.view == View::Native,
            &[yard],
        );
    }
    let mut songs: Vec<&str> = rows.iter().map(|r| r.probe.as_str()).collect();
    songs.sort_unstable();
    songs.dedup();
    for song in songs {
        table(
            &format!("{song}, native view"),
            &|r| r.probe == song && r.view == View::Native,
            &Method::WHOLE_MIX,
        );
    }
    out.push_str(&format_by_metric(rows));
    let native: Vec<Row> = rows
        .iter()
        .filter(|r| r.view == View::Native)
        .cloned()
        .collect();
    out.push('\n');
    out.push_str(&format_ranks(&mean_ranks(&native)));
    out
}

/// The contenders in the per-metric block.
const BY_METRIC: [Method; 6] = [
    Method::Stems,
    Method::StemsLeak20,
    Method::StemsLeak10,
    Method::StemsBeats,
    Method::StemsGrain,
    Method::Hpss,
];

/// One line per metric: the median error of each split contender over Auto's,
/// in the native view, with the share of cells it wins in brackets. A pooled
/// median can net opposite effects to 1, as spectral gains against transient
/// losses did in the ceiling experiment, so this is the table to read.
pub fn format_by_metric(rows: &[Row]) -> String {
    let Some(first) = rows.iter().find(|r| r.method == Method::Auto) else {
        return String::new();
    };
    let mut out =
        String::from("\nper metric, native view, error over auto's (share of cells won):\n");
    let _ = write!(out, "  {:<30}", "");
    for m in BY_METRIC {
        let _ = write!(out, " {:>13}", m.name());
    }
    out.push('\n');
    for (name, _) in &first.metrics {
        if UNRANKED.contains(&name.as_str()) {
            continue;
        }
        let versus = versus_of(
            rows,
            |r| r.view == View::Native,
            &[Method::Auto],
            |n| n == name,
        );
        if versus.is_empty() {
            continue;
        }
        let _ = write!(out, "  {name:<30}");
        for m in BY_METRIC {
            match versus.get(&m) {
                Some(v) => {
                    let _ = write!(
                        out,
                        " {:>6.2} ({:>3.0}%)",
                        v.median_ratio,
                        100.0 * v.wins as f64 / v.cells.max(1) as f64
                    );
                }
                None => {
                    let _ = write!(out, " {:>13}", "-");
                }
            }
        }
        out.push('\n');
    }
    out
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

    #[test]
    fn a_split_contender_is_scored_against_the_best_whole_mix_mode() {
        let rows = [
            row(Method::Grain, 0.4),
            row(Method::Beats, 0.2),
            row(Method::Stems, 0.1),
            row(Method::Hpss, 0.3),
        ];
        let v = versus_best_whole_mix(&rows, |_| true);
        assert_eq!(
            v.len(),
            2,
            "the whole-mix modes are the yardstick, not rivals"
        );
        assert_eq!((v[&Method::Stems].wins, v[&Method::Stems].cells), (2, 2));
        assert_eq!(v[&Method::Stems].median_ratio, 0.5);
        assert_eq!(v[&Method::Hpss].wins, 0);
        assert!((v[&Method::Hpss].median_ratio - 1.5).abs() < 1e-9);
        // A pick that leaves out the yardstick leaves nothing to compare.
        assert!(versus_best_whole_mix(&rows, |r| r.method == Method::Stems).is_empty());
    }

    #[test]
    fn a_baseline_gives_its_rows_back() {
        let rows = [row(Method::Grain, 0.2), row(Method::StemsLeak20, f64::NAN)];
        let again = Baseline::of(&rows).rows();
        assert_eq!(again.len(), 2);
        let leaky = again
            .iter()
            .find(|r| r.method == Method::StemsLeak20)
            .unwrap();
        assert_eq!((leaky.probe.as_str(), leaky.case.as_str()), ("p", "x2"));
        assert!(leaky.metric("SSMAD").unwrap().is_nan());
        assert_eq!(Baseline::of(&again).rows().len(), 2);
    }
}
