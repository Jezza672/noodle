//! Checking the metrics against human listeners.
//!
//! A *manifest* is a CSV with the header `reference,test,method,speed,mos`:
//! paths (relative to the manifest), the algorithm that made the test, the
//! speed it was stretched to (output length is reference length over speed),
//! and the listeners' mean opinion score. For each metric the correlation with
//! the scores is reported pooled, and within each speed (averaged), because a
//! pooled correlation can come from the speed alone.

use crate::clip::Clip;
use crate::report::evaluate;
use crate::timemap::TimeMap;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Row {
    pub reference: PathBuf,
    pub test: PathBuf,
    pub method: String,
    pub speed: f64,
    pub mos: f64,
}

pub fn parse_manifest(path: &Path) -> Result<Vec<Row>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next().ok_or("empty manifest")?;
    if header.trim() != "reference,test,method,speed,mos" {
        return Err(format!(
            "expected header reference,test,method,speed,mos, got {header}"
        ));
    }
    lines
        .enumerate()
        .map(|(i, line)| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() != 5 {
                return Err(format!("line {}: expected 5 fields", i + 2));
            }
            let num = |s: &str| {
                s.parse::<f64>()
                    .map_err(|e| format!("line {}: {s}: {e}", i + 2))
            };
            Ok(Row {
                reference: dir.join(f[0]),
                test: dir.join(f[1]),
                method: f[2].to_string(),
                speed: num(f[3])?,
                mos: num(f[4])?,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Correlation {
    pub metric: &'static str,
    pub pooled_spearman: f64,
    pub within_speed_spearman: f64,
    pub pairs: usize,
}

/// Metrics need at least this much rank correlation (absolute) with the
/// listeners, pooled and within speed, to be allowed to gate CI.
pub const GATE_MIN_ABS_SPEARMAN: f64 = 0.5;

impl Correlation {
    pub fn usable(&self) -> bool {
        self.pooled_spearman.abs() >= GATE_MIN_ABS_SPEARMAN
            && self.within_speed_spearman.abs() >= GATE_MIN_ABS_SPEARMAN
    }
}

/// The metrics of one pair, in [`Report::features`](crate::Report::features)
/// order.
pub type Features = Vec<(&'static str, f64)>;

/// Scores every pair, spread over the available cores (decoding and the
/// metrics dominate the time of a validation run). Results are in row order.
pub fn measure(rows: &[Row]) -> Result<Vec<Features>, String> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(rows.len().max(1));
    let chunk = rows.len().div_ceil(threads).max(1);
    let parts: Vec<Result<Vec<Features>, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = rows
            .chunks(chunk)
            .map(|part| scope.spawn(move || part.iter().map(measure_one).collect()))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a measuring thread panicked"))
            .collect()
    });
    let mut out = Vec::with_capacity(rows.len());
    for part in parts {
        out.extend(part?);
    }
    Ok(out)
}

fn measure_one(row: &Row) -> Result<Features, String> {
    let r = noodle_io::decode_file(&row.reference)
        .map_err(|e| format!("{}: {e}", row.reference.display()))?;
    let t =
        noodle_io::decode_file(&row.test).map_err(|e| format!("{}: {e}", row.test.display()))?;
    if r.sample_rate != t.sample_rate {
        return Err(format!(
            "{}: sample rate differs from its reference",
            row.test.display()
        ));
    }
    Ok(evaluate(
        Clip::from_audio(&r),
        Clip::from_audio(&t),
        &TimeMap::Speed(row.speed),
    )
    .features())
}

pub fn run(rows: &[Row]) -> Result<Vec<Correlation>, String> {
    let mut per_metric: BTreeMap<&'static str, Vec<(f64, f64, f64)>> = BTreeMap::new();
    let mut order: Vec<&'static str> = Vec::new();
    for (row, features) in rows.iter().zip(measure(rows)?) {
        for (name, value) in features {
            if !order.contains(&name) {
                order.push(name);
            }
            per_metric
                .entry(name)
                .or_default()
                .push((value, row.mos, row.speed));
        }
    }
    Ok(order
        .into_iter()
        .map(|metric| {
            let points: Vec<_> = per_metric[metric]
                .iter()
                .filter(|p| p.0.is_finite())
                .copied()
                .collect();
            let pooled = spearman(&points.iter().map(|p| (p.0, p.1)).collect::<Vec<_>>());
            let mut by_speed: BTreeMap<u64, Vec<(f64, f64)>> = BTreeMap::new();
            for p in &points {
                by_speed.entry(p.2.to_bits()).or_default().push((p.0, p.1));
            }
            let within: Vec<f64> = by_speed
                .values()
                .map(|v| spearman(v))
                .filter(|c| c.is_finite())
                .collect();
            Correlation {
                metric,
                pooled_spearman: pooled,
                within_speed_spearman: if within.is_empty() {
                    f64::NAN
                } else {
                    within.iter().sum::<f64>() / within.len() as f64
                },
                pairs: points.len(),
            }
        })
        .collect())
}

fn ranks(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
    let mut out = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] {
            j += 1;
        }
        let rank = (i + j) as f64 / 2.0;
        for &k in &idx[i..=j] {
            out[k] = rank;
        }
        i = j + 1;
    }
    out
}

pub fn pearson(pairs: &[(f64, f64)]) -> f64 {
    let n = pairs.len() as f64;
    if pairs.len() < 3 {
        return f64::NAN;
    }
    let (mx, my) = (
        pairs.iter().map(|p| p.0).sum::<f64>() / n,
        pairs.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for p in pairs {
        sxy += (p.0 - mx) * (p.1 - my);
        sxx += (p.0 - mx).powi(2);
        syy += (p.1 - my).powi(2);
    }
    sxy / (sxx * syy).sqrt()
}

pub fn spearman(pairs: &[(f64, f64)]) -> f64 {
    let rx = ranks(&pairs.iter().map(|p| p.0).collect::<Vec<_>>());
    let ry = ranks(&pairs.iter().map(|p| p.1).collect::<Vec<_>>());
    pearson(&rx.into_iter().zip(ry).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spearman_is_rank_based_and_handles_ties() {
        let up: Vec<_> = (0..10).map(|i| (f64::from(i), f64::from(i * i))).collect();
        assert!((spearman(&up) - 1.0).abs() < 1e-12);
        let down: Vec<_> = (0..10).map(|i| (f64::from(i), -f64::from(i))).collect();
        assert!((spearman(&down) + 1.0).abs() < 1e-12);
        let tied = [(1.0, 1.0), (1.0, 2.0), (2.0, 3.0), (2.0, 4.0)];
        assert!(spearman(&tied) > 0.0 && spearman(&tied) < 1.0);
    }
}
