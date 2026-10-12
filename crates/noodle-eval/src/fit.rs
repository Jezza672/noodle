//! Fitting our own score from the features.
//!
//! The paper finds no single feature that tracks listeners well and predicts
//! the score from many of them together. This does the same with a ridge
//! regression: [`extract`] computes every feature of every pair once (in
//! parallel, since it is the slow part) and caches them as a CSV, and [`cv`]
//! scores a model on pairs whose reference clip it never saw, so a clip's
//! own character cannot be what it learned.

use crate::clip::Clip;
use crate::report::evaluate;
use crate::timemap::TimeMap;
use crate::validate::{Row, pearson, spearman};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The speed is known wherever a warp is scored, so it is a feature too.
pub const SPEED_FEATURE: &str = "abs_ln_speed";

#[derive(Debug, Clone)]
pub struct TableRow {
    pub reference: String,
    pub test: String,
    pub method: String,
    pub speed: f64,
    pub mos: f64,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub names: Vec<String>,
    pub rows: Vec<TableRow>,
}

/// Feature names and values of one pair.
type Scored = (Vec<&'static str>, Vec<f64>);

/// Every feature of every pair, on `threads` threads.
pub fn extract(rows: &[Row], threads: usize) -> Result<Table, String> {
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<Scored>>> = Mutex::new(vec![None; rows.len()]);
    let failure: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= rows.len() || failure.lock().unwrap().is_some() {
                        return;
                    }
                    match one(&rows[i]) {
                        Ok(v) => out.lock().unwrap()[i] = Some(v),
                        Err(e) => *failure.lock().unwrap() = Some(e),
                    }
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(200) {
                        eprintln!("{n}/{}", rows.len());
                    }
                }
            });
        }
    });
    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    let out = out.into_inner().unwrap();
    let names: Vec<String> = out[0]
        .as_ref()
        .map(|(n, _)| n.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let rows = rows
        .iter()
        .zip(out)
        .map(|(r, o)| TableRow {
            reference: r.reference.display().to_string(),
            test: r.test.display().to_string(),
            method: r.method.clone(),
            speed: r.speed,
            mos: r.mos,
            values: o.expect("every row was scored").1,
        })
        .collect();
    Ok(Table { names, rows })
}

fn one(row: &Row) -> Result<Scored, String> {
    let load = |p: &Path| noodle_io::decode_file(p).map_err(|e| format!("{}: {e}", p.display()));
    let (r, t) = (load(&row.reference)?, load(&row.test)?);
    if r.sample_rate != t.sample_rate {
        return Err(format!(
            "{}: sample rate differs from its reference",
            row.test.display()
        ));
    }
    let report = evaluate(
        Clip::from_audio(&r),
        Clip::from_audio(&t),
        &TimeMap::Speed(row.speed),
    );
    let (mut names, mut values): (Vec<_>, Vec<_>) = report
        .features()
        .into_iter()
        .chain(report.extra.features())
        .unzip();
    names.push(SPEED_FEATURE);
    values.push(row.speed.ln().abs());
    Ok((names, values))
}

impl Table {
    pub fn write_csv(&self, path: &Path) -> Result<(), String> {
        let mut text = format!("reference,test,method,speed,mos,{}\n", self.names.join(","));
        for r in &self.rows {
            let vals: Vec<String> = r.values.iter().map(|v| format!("{v}")).collect();
            text += &format!(
                "{},{},{},{},{},{}\n",
                r.reference,
                r.test,
                r.method,
                r.speed,
                r.mos,
                vals.join(",")
            );
        }
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn read_csv(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header: Vec<&str> = lines
            .next()
            .ok_or("empty features file")?
            .split(',')
            .collect();
        if header.len() < 6 || header[..5] != ["reference", "test", "method", "speed", "mos"] {
            return Err("not a features file (see `noodle-eval features`)".into());
        }
        let names = header[5..].iter().map(|s| s.to_string()).collect();
        let num = |s: &str| s.parse::<f64>().map_err(|e| format!("{s}: {e}"));
        let rows = lines
            .map(|l| {
                let f: Vec<&str> = l.split(',').collect();
                if f.len() != header.len() {
                    return Err(format!("expected {} fields: {l}", header.len()));
                }
                Ok(TableRow {
                    reference: f[0].into(),
                    test: f[1].into(),
                    method: f[2].into(),
                    speed: num(f[3])?,
                    mos: num(f[4])?,
                    values: f[5..].iter().map(|s| num(s)).collect::<Result<_, _>>()?,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { names, rows })
    }
}

/// One input of the model and how to prepare it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    /// Replaces a value that could not be measured.
    pub median: f64,
    /// For a non-negative feature, `ln(1 + x / log_scale)` tames its tail.
    pub log_scale: Option<f64>,
    pub mean: f64,
    pub std: f64,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub lambda: f64,
    pub intercept: f64,
    pub columns: Vec<Column>,
}

impl Column {
    fn prepare(&self, x: f64) -> f64 {
        let x = if x.is_finite() { x } else { self.median };
        let x = self.log_scale.map_or(x, |m| (1.0 + x.max(0.0) / m).ln());
        (x - self.mean) / self.std
    }
}

impl Model {
    /// Predicted mean opinion score for feature values in the table's column
    /// order for `names`.
    pub fn predict(&self, names: &[String], values: &[f64]) -> f64 {
        self.columns.iter().fold(self.intercept, |acc, c| {
            let i = names
                .iter()
                .position(|n| *n == c.name)
                .expect("feature present");
            acc + c.weight * c.prepare(values[i])
        })
    }

    /// Fits to `rows`, using only the features named in `only` when given.
    pub fn fit(names: &[String], rows: &[&TableRow], lambda: f64, only: Option<&[&str]>) -> Self {
        let n = rows.len() as f64;
        let mut columns = Vec::new();
        let mut cols: Vec<Vec<f64>> = Vec::new();
        for (j, name) in names.iter().enumerate() {
            if only.is_some_and(|o| !o.contains(&name.as_str())) {
                continue;
            }
            let finite: Vec<f64> = rows
                .iter()
                .map(|r| r.values[j])
                .filter(|v| v.is_finite())
                .collect();
            if (finite.len() as f64) < 0.5 * n {
                continue;
            }
            let median = quantile(&finite, 0.5);
            let log_scale = if finite.iter().all(|&v| v >= 0.0) {
                let pos: Vec<f64> = finite.iter().copied().filter(|&v| v > 0.0).collect();
                Some(if pos.is_empty() {
                    1.0
                } else {
                    quantile(&pos, 0.5)
                })
            } else {
                None
            };
            let mut c = Column {
                name: name.clone(),
                median,
                log_scale,
                mean: 0.0,
                std: 1.0,
                weight: 0.0,
            };
            let raw: Vec<f64> = rows.iter().map(|r| c.prepare(r.values[j])).collect();
            let mean = raw.iter().sum::<f64>() / n;
            let std = (raw.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
            if std < 1e-12 {
                continue;
            }
            c.mean = mean;
            c.std = std;
            cols.push(raw.iter().map(|v| (v - mean) / std).collect());
            columns.push(c);
        }
        let d = columns.len();
        let ymean = rows.iter().map(|r| r.mos).sum::<f64>() / n;
        let mut a = vec![vec![0.0; d + 1]; d];
        for i in 0..d {
            for j in 0..d {
                a[i][j] = cols[i].iter().zip(&cols[j]).map(|(x, y)| x * y).sum();
            }
            a[i][i] += lambda;
            a[i][d] = cols[i]
                .iter()
                .zip(rows)
                .map(|(x, r)| x * (r.mos - ymean))
                .sum();
        }
        for (c, w) in columns.iter_mut().zip(solve(a)) {
            c.weight = w;
        }
        Self {
            lambda,
            intercept: ymean,
            columns,
        }
    }

    fn predict_row(&self, names: &[String], r: &TableRow) -> f64 {
        self.predict(names, &r.values)
    }
}

fn quantile(v: &[f64], q: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[((s.len() - 1) as f64 * q).round() as usize]
}

/// Solves the augmented `d x (d + 1)` system by Gaussian elimination.
fn solve(mut a: Vec<Vec<f64>>) -> Vec<f64> {
    let d = a.len();
    for col in 0..d {
        let pivot = (col..d)
            .max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))
            .unwrap();
        a.swap(col, pivot);
        let p = a[col][col];
        for cell in &mut a[col][col..=d] {
            *cell /= p;
        }
        for r in (0..d).filter(|&r| r != col) {
            let f = a[r][col];
            let pivot_row = a[col].clone();
            for (cell, &pv) in a[r][col..=d].iter_mut().zip(&pivot_row[col..=d]) {
                *cell -= f * pv;
            }
        }
    }
    a.iter().map(|row| row[d]).collect()
}

/// The paper's nine features, as named in the table.
pub const PAPER: [&str; 9] = [
    "MPhMW",
    "SPhMW",
    "MPhNW",
    "SPhNW",
    "SSMAD",
    "SSMD_abs",
    "DeltaP_abs",
    "TrRat_abs_ln",
    "HPSTrRat_abs_ln",
];

/// The perceptual features of [`crate::extra`].
pub const EXTRA: [&str; 12] = [
    "loud_added",
    "loud_added_rms",
    "loud_missing",
    "nmr_db",
    "nmr_over",
    "mfcc_dist",
    "centroid_abs_ln",
    "rolloff_abs_ln",
    "flatness_abs_ln",
    "chroma_dist",
    "stationarity_delta",
    "mod_abs_ln",
];

pub const FOLDS: usize = 5;
pub const LAMBDAS: [f64; 6] = [0.1, 1.0, 10.0, 100.0, 1000.0, 10000.0];

#[derive(Debug, Clone)]
pub struct Score {
    pub label: String,
    pub lambda: f64,
    pub pearson: f64,
    pub spearman: f64,
    pub in_speed_spearman: f64,
    pub rmse: f64,
    pub by_method: Vec<(String, f64)>,
    pub predictions: Vec<f64>,
}

/// Cross-validated predictions with whole reference clips held out, for the
/// ridge strength that predicts best.
pub fn cv(table: &Table, label: &str, only: Option<&[&str]>) -> Score {
    let mut refs: Vec<&str> = table.rows.iter().map(|r| r.reference.as_str()).collect();
    refs.sort_unstable();
    refs.dedup();
    let fold_of = |r: &TableRow| refs.iter().position(|x| *x == r.reference).unwrap() % FOLDS;
    let folds: Vec<usize> = table.rows.iter().map(fold_of).collect();
    let mut best: Option<(f64, f64, Vec<f64>)> = None;
    for &lambda in &LAMBDAS {
        let mut pred = vec![0.0; table.rows.len()];
        for f in 0..FOLDS {
            let train: Vec<&TableRow> = table
                .rows
                .iter()
                .zip(&folds)
                .filter(|&(_, &g)| g != f)
                .map(|(r, _)| r)
                .collect();
            let model = Model::fit(&table.names, &train, lambda, only);
            for (i, r) in table.rows.iter().enumerate() {
                if folds[i] == f {
                    pred[i] = model.predict_row(&table.names, r);
                }
            }
        }
        let mse = pred
            .iter()
            .zip(&table.rows)
            .map(|(p, r)| (p - r.mos).powi(2))
            .sum::<f64>()
            / pred.len() as f64;
        if best.as_ref().is_none_or(|b| mse < b.1) {
            best = Some((lambda, mse, pred));
        }
    }
    let (lambda, mse, pred) = best.expect("lambdas");
    let pairs: Vec<(f64, f64)> = pred
        .iter()
        .zip(&table.rows)
        .map(|(p, r)| (*p, r.mos))
        .collect();
    let mut by_speed: BTreeMap<u64, Vec<(f64, f64)>> = BTreeMap::new();
    let mut by_method: BTreeMap<&str, Vec<(f64, f64)>> = BTreeMap::new();
    for (pair, r) in pairs.iter().zip(&table.rows) {
        by_speed.entry(r.speed.to_bits()).or_default().push(*pair);
        by_method.entry(&r.method).or_default().push(*pair);
    }
    let within: Vec<f64> = by_speed
        .values()
        .map(|v| spearman(v))
        .filter(|c| c.is_finite())
        .collect();
    Score {
        label: label.into(),
        lambda,
        pearson: pearson(&pairs),
        spearman: spearman(&pairs),
        in_speed_spearman: within.iter().sum::<f64>() / within.len().max(1) as f64,
        rmse: mse.sqrt(),
        by_method: by_method
            .into_iter()
            .map(|(m, v)| (m.to_string(), spearman(&v)))
            .collect(),
        predictions: pred,
    }
}

/// The standard deviation of the scores: the error of always guessing the mean.
pub fn baseline_rmse(table: &Table) -> f64 {
    let n = table.rows.len() as f64;
    let m = table.rows.iter().map(|r| r.mos).sum::<f64>() / n;
    (table.rows.iter().map(|r| (r.mos - m).powi(2)).sum::<f64>() / n).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scores that depend on two of three features, plus a little noise from a
    /// fixed sequence.
    fn synthetic() -> Table {
        let mut rows = Vec::new();
        for i in 0..200u32 {
            let f = |k: u32| f64::from((i * (7 + k) + k * 13) % 31) / 31.0;
            let (a, b, c) = (f(0), f(1), f(2));
            rows.push(TableRow {
                reference: format!("ref{}", i % 10),
                test: format!("t{i}"),
                method: "m".into(),
                speed: 1.0 + f64::from(i % 3),
                mos: 4.0 - 2.0 * a + 0.5 * b + 0.01 * f64::from(i % 5),
                values: vec![a, b, c],
            });
        }
        Table {
            names: ["a", "b", "c"].map(String::from).to_vec(),
            rows,
        }
    }

    #[test]
    fn ridge_recovers_the_features_that_matter_on_unseen_references() {
        let t = synthetic();
        let s = cv(&t, "all", None);
        assert!(s.pearson > 0.95, "held-out correlation {}", s.pearson);
        assert!(s.rmse < 0.2 * baseline_rmse(&t));
        // A feature that carries nothing predicts nothing.
        let only_c = cv(&t, "c", Some(&["c"]));
        assert!(only_c.pearson.abs() < 0.5, "{}", only_c.pearson);
    }

    #[test]
    fn solve_handles_a_small_system() {
        // 2x + y = 5, x + 3y = 10.
        let x = solve(vec![vec![2.0, 1.0, 5.0], vec![1.0, 3.0, 10.0]]);
        assert!((x[0] - 1.0).abs() < 1e-12 && (x[1] - 3.0).abs() < 1e-12);
    }

    #[test]
    fn a_features_file_round_trips() {
        let t = synthetic();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.csv");
        t.write_csv(&p).unwrap();
        let back = Table::read_csv(&p).unwrap();
        assert_eq!(back.names, t.names);
        assert_eq!(back.rows[7].values, t.rows[7].values);
        assert_eq!(back.rows[7].mos, t.rows[7].mos);
    }
}
