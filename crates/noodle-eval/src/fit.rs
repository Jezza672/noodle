//! A score fitted to listeners from the metrics.
//!
//! No single metric tracks the listening-test scores closely (the paper says
//! as much: "lack of individual features highly correlated with the SMOS"),
//! but the paper's predictor is a model over many features at once. This fits
//! the same kind of thing, as a ridge regression over every metric, and
//! checks it the honest way: each prediction comes from a model that never
//! saw that pair's *reference clip* (grouped k-fold), so a model cannot pass
//! by memorising a signal. The fit uses the same pairs as
//! [`validate`](crate::validate).
//!
//! Choices: features are standardised; a metric that could not be measured
//! (`NaN`) counts as its mean, so it adds nothing; predictions are clamped to
//! the scale of the scores, 1 to 5; and the penalty is picked by the same
//! cross-validation it is reported on, which flatters the figure slightly
//! (the grid is six values, so not by much).

use crate::validate::{Features, Row, pearson, spearman};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// One scored pair: what the metrics said and what listeners said.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// What cross-validation keeps together: the reference clip.
    pub group: String,
    pub method: String,
    /// How much the test was sped up; agreement is also reported within each
    /// speed, because several metrics track the speed alone.
    pub speed: f64,
    pub mos: f64,
    pub features: Vec<f64>,
}

/// Samples from measured rows. A metric that is `NaN` for every pair (the
/// stereo ones on a mono corpus) is dropped.
pub fn samples(rows: &[Row], measured: &[Features]) -> (Vec<String>, Vec<Sample>) {
    let Some(first) = measured.first() else {
        return (Vec::new(), Vec::new());
    };
    let keep: Vec<usize> = (0..first.len())
        .filter(|&j| measured.iter().any(|m| m[j].1.is_finite()))
        .collect();
    let names = keep.iter().map(|&j| first[j].0.to_string()).collect();
    let samples = rows
        .iter()
        .zip(measured)
        .map(|(row, m)| Sample {
            group: row.reference.display().to_string(),
            method: row.method.clone(),
            speed: row.speed,
            mos: row.mos,
            features: keep.iter().map(|&j| m[j].1).collect(),
        })
        .collect();
    (names, samples)
}

/// The samples without the named metrics, to see what a group of them adds.
pub fn without(
    names: &[String],
    samples: &[Sample],
    drop: &[&str],
) -> Result<(Vec<String>, Vec<Sample>), String> {
    let drop: Vec<&str> = drop.iter().copied().filter(|d| !d.is_empty()).collect();
    if let Some(bad) = drop.iter().find(|d| !names.iter().any(|n| n == *d)) {
        return Err(format!("no metric called {bad}"));
    }
    let keep: Vec<usize> = (0..names.len())
        .filter(|&j| !drop.contains(&names[j].as_str()))
        .collect();
    Ok((
        keep.iter().map(|&j| names[j].clone()).collect(),
        samples
            .iter()
            .map(|s| Sample {
                features: keep.iter().map(|&j| s.features[j]).collect(),
                ..s.clone()
            })
            .collect(),
    ))
}

/// What a cache was measured from. A cache is only reused when both match
/// the current manifest and the current build's metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    /// Pairs in the manifest.
    pub pairs: usize,
    /// Every metric this build measures, in order.
    pub metrics: Vec<String>,
}

/// Saves measured samples so a fit can be repeated without re-measuring.
pub fn save_cache(
    path: &Path,
    key: &CacheKey,
    names: &[String],
    samples: &[Sample],
) -> Result<(), String> {
    let plain = |s: &str| !s.contains(',') && !s.contains('\n');
    let mut text = format!("# pairs={} metrics={}\n", key.pairs, key.metrics.join(";"));
    text += &format!("group,method,speed,mos,{}\n", names.join(","));
    for s in samples {
        if !plain(&s.group) || !plain(&s.method) {
            return Err(format!(
                "cannot cache {}: commas in paths and method names are not supported",
                s.group
            ));
        }
        let values: Vec<String> = s.features.iter().map(|v| format!("{v}")).collect();
        let _ = writeln!(
            text,
            "{},{},{},{},{}",
            s.group,
            s.method,
            s.speed,
            s.mos,
            values.join(",")
        );
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

pub type Cache = (CacheKey, Vec<String>, Vec<Sample>);

pub fn load_cache(path: &Path) -> Result<Cache, String> {
    let bad = |what: &str| format!("{}: {what}", path.display());
    let text = std::fs::read_to_string(path).map_err(|e| bad(&e.to_string()))?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().ok_or_else(|| bad("empty cache"))?;
    let key = first
        .strip_prefix("# pairs=")
        .and_then(|rest| rest.split_once(" metrics="))
        .and_then(|(n, m)| {
            Some(CacheKey {
                pairs: n.parse().ok()?,
                metrics: m.split(';').map(String::from).collect(),
            })
        })
        .ok_or_else(|| bad("not a feature cache (or one from an older build)"))?;
    let header = lines.next().ok_or_else(|| bad("no header"))?;
    let cols: Vec<&str> = header.split(',').collect();
    if cols.len() < 5 || cols[..4] != ["group", "method", "speed", "mos"] {
        return Err(bad("not a feature cache"));
    }
    let names: Vec<String> = cols[4..].iter().map(|c| c.to_string()).collect();
    let mut out = Vec::new();
    for (i, line) in lines.enumerate() {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() != cols.len() {
            return Err(bad(&format!("line {}: wrong field count", i + 3)));
        }
        let num = |s: &str| {
            s.parse::<f64>()
                .map_err(|e| bad(&format!("line {}: {s}: {e}", i + 3)))
        };
        out.push(Sample {
            group: f[0].to_string(),
            method: f[1].to_string(),
            speed: num(f[2])?,
            mos: num(f[3])?,
            features: f[4..].iter().map(|v| num(v)).collect::<Result<_, _>>()?,
        });
    }
    Ok((key, names, out))
}

/// A fitted score: standardise, weigh, add the intercept.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub names: Vec<String>,
    pub mean: Vec<f64>,
    pub std: Vec<f64>,
    pub weights: Vec<f64>,
    pub intercept: f64,
    pub lambda: f64,
}

impl Model {
    pub fn fit(samples: &[Sample], names: &[String], lambda: f64) -> Model {
        let p = names.len();
        let (mut mean, mut std) = (vec![0.0; p], vec![1.0; p]);
        for j in 0..p {
            let v: Vec<f64> = samples
                .iter()
                .map(|s| s.features[j])
                .filter(|x| x.is_finite())
                .collect();
            if v.is_empty() {
                continue;
            }
            let m = v.iter().sum::<f64>() / v.len() as f64;
            let sd = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
            mean[j] = m;
            std[j] = if sd > 1e-12 { sd } else { 1.0 };
        }
        let n = samples.len().max(1) as f64;
        let intercept = samples.iter().map(|s| s.mos).sum::<f64>() / n;
        let mut gram = vec![vec![0.0; p]; p];
        let mut rhs = vec![0.0; p];
        let mut z = vec![0.0; p];
        for s in samples {
            standardise(&s.features, &mean, &std, &mut z);
            for a in 0..p {
                rhs[a] += z[a] * (s.mos - intercept);
                for b in 0..p {
                    gram[a][b] += z[a] * z[b];
                }
            }
        }
        for (a, row) in gram.iter_mut().enumerate() {
            row[a] += lambda;
        }
        Model {
            names: names.to_vec(),
            mean,
            std,
            weights: solve(gram, rhs),
            intercept,
            lambda,
        }
    }

    /// The predicted score, clamped to 1 to 5. `features` are in
    /// [`Model::names`] order.
    pub fn predict(&self, features: &[f64]) -> f64 {
        let mut z = vec![0.0; self.weights.len()];
        standardise(features, &self.mean, &self.std, &mut z);
        let raw = self.intercept + z.iter().zip(&self.weights).map(|(a, w)| a * w).sum::<f64>();
        raw.clamp(1.0, 5.0)
    }
}

/// A missing metric (`NaN`) becomes 0, which is the mean.
fn standardise(x: &[f64], mean: &[f64], std: &[f64], out: &mut [f64]) {
    for j in 0..out.len() {
        out[j] = if x[j].is_finite() {
            (x[j] - mean[j]) / std[j]
        } else {
            0.0
        };
    }
}

/// Gaussian elimination with partial pivoting; the ridge term keeps the
/// system well conditioned.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Vec<f64> {
    let n = b.len();
    for col in 0..n {
        let pivot = (col..n)
            .max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))
            .unwrap_or(col);
        a.swap(col, pivot);
        b.swap(col, pivot);
        let d = a[col][col];
        if d.abs() < 1e-12 {
            continue;
        }
        let (top, bottom) = a.split_at_mut(col + 1);
        for (offset, row) in bottom.iter_mut().enumerate() {
            let f = row[col] / d;
            for (x, p) in row[col..].iter_mut().zip(&top[col][col..]) {
                *x -= f * p;
            }
            b[col + 1 + offset] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let tail: f64 = (r + 1..n).map(|c| a[r][c] * x[c]).sum();
        x[r] = if a[r][r].abs() < 1e-12 {
            0.0
        } else {
            (b[r] - tail) / a[r][r]
        };
    }
    x
}

/// Predictions made by models that never saw the pair's reference clip: the
/// clips are dealt into `folds` groups in sorted order.
pub fn cross_validate(samples: &[Sample], names: &[String], lambda: f64, folds: usize) -> Vec<f64> {
    let mut groups: Vec<&str> = samples.iter().map(|s| s.group.as_str()).collect();
    groups.sort_unstable();
    groups.dedup();
    let fold_of: BTreeMap<&str, usize> = groups
        .iter()
        .enumerate()
        .map(|(i, g)| (*g, i % folds))
        .collect();
    let mut out = vec![f64::NAN; samples.len()];
    for fold in 0..folds {
        let train: Vec<Sample> = samples
            .iter()
            .filter(|s| fold_of[s.group.as_str()] != fold)
            .cloned()
            .collect();
        if train.is_empty() {
            continue;
        }
        let model = Model::fit(&train, names, lambda);
        for (i, s) in samples.iter().enumerate() {
            if fold_of[s.group.as_str()] == fold {
                out[i] = model.predict(&s.features);
            }
        }
    }
    out
}

/// How well predictions track the listeners.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Agreement {
    pub pearson: f64,
    pub spearman: f64,
    /// Spearman computed within each speed and averaged, as `validate` does:
    /// a pooled figure can come from the speed alone.
    pub within_speed_spearman: f64,
    pub rmse: f64,
}

/// `(predicted, actual, speed)` per pair; pairs without a prediction are left
/// out.
pub fn agreement(points: &[(f64, f64, f64)]) -> Agreement {
    let kept: Vec<_> = points.iter().filter(|p| p.0.is_finite()).collect();
    let pairs: Vec<(f64, f64)> = kept.iter().map(|p| (p.0, p.1)).collect();
    let n = pairs.len().max(1) as f64;
    let mut by_speed: BTreeMap<u64, Vec<(f64, f64)>> = BTreeMap::new();
    for p in &kept {
        by_speed.entry(p.2.to_bits()).or_default().push((p.0, p.1));
    }
    let within: Vec<f64> = by_speed
        .values()
        .filter(|v| v.len() >= 3)
        .map(|v| spearman(v))
        .filter(|c| c.is_finite())
        .collect();
    Agreement {
        pearson: pearson(&pairs),
        spearman: spearman(&pairs),
        within_speed_spearman: if within.is_empty() {
            f64::NAN
        } else {
            within.iter().sum::<f64>() / within.len() as f64
        },
        rmse: (pairs.iter().map(|p| (p.0 - p.1).powi(2)).sum::<f64>() / n).sqrt(),
    }
}

fn points(samples: &[Sample], predicted: &[f64]) -> Vec<(f64, f64, f64)> {
    samples
        .iter()
        .zip(predicted)
        .map(|(s, &p)| (p, s.mos, s.speed))
        .collect()
}

pub const LAMBDAS: [f64; 6] = [0.1, 1.0, 10.0, 100.0, 1000.0, 10_000.0];
pub const FOLDS: usize = 5;

/// The result of a fit.
#[derive(Debug, Clone)]
pub struct Fit {
    pub model: Model,
    /// Cross-validated, with the chosen penalty.
    pub overall: Agreement,
    /// The same agreement with a constant prediction (the mean), for scale.
    pub baseline_rmse: f64,
    /// (lambda, RMSE) over the grid.
    pub grid: Vec<(f64, f64)>,
    /// Per method: pairs, and the Spearman of the cross-validated predictions
    /// pooled and within speed.
    pub per_method: Vec<(String, usize, f64, f64)>,
}

pub fn fit(samples: &[Sample], names: &[String]) -> Result<Fit, String> {
    let groups = samples
        .iter()
        .map(|s| s.group.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if groups < FOLDS || names.is_empty() {
        return Err(format!(
            "need at least {FOLDS} different reference clips and one metric (have {groups} and {})",
            names.len()
        ));
    }
    let actual: Vec<f64> = samples.iter().map(|s| s.mos).collect();
    let mut best: Option<(f64, Vec<f64>, f64)> = None;
    let mut grid = Vec::new();
    for lambda in LAMBDAS {
        let predicted = cross_validate(samples, names, lambda, FOLDS);
        let rmse = agreement(&points(samples, &predicted)).rmse;
        grid.push((lambda, rmse));
        if best.as_ref().is_none_or(|b| rmse < b.2) {
            best = Some((lambda, predicted, rmse));
        }
    }
    let (lambda, predicted, _) = best.expect("the grid is not empty");
    let mean = actual.iter().sum::<f64>() / actual.len() as f64;
    let baseline_rmse =
        (actual.iter().map(|a| (a - mean).powi(2)).sum::<f64>() / actual.len() as f64).sqrt();
    let mut by_method: BTreeMap<&str, Vec<(f64, f64, f64)>> = BTreeMap::new();
    for (s, p) in samples.iter().zip(&predicted) {
        by_method
            .entry(&s.method)
            .or_default()
            .push((*p, s.mos, s.speed));
    }
    let per_method = by_method
        .into_iter()
        .map(|(m, v)| {
            let a = agreement(&v);
            (m.to_string(), v.len(), a.spearman, a.within_speed_spearman)
        })
        .collect();
    Ok(Fit {
        model: Model::fit(samples, names, lambda),
        overall: agreement(&points(samples, &predicted)),
        baseline_rmse,
        grid,
        per_method,
    })
}

impl Fit {
    pub fn format(&self, pairs: usize) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{pairs} pairs, {FOLDS}-fold cross-validation by reference clip"
        );
        let _ = writeln!(
            out,
            "penalty {} (RMSE by penalty: {})",
            self.model.lambda,
            self.grid
                .iter()
                .map(|(l, r)| format!("{l}: {r:.3}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let _ = writeln!(
            out,
            "held-out agreement: Pearson {:.3}, Spearman {:.3} (within speed {:.3}), RMSE {:.3} (guessing the mean: {:.3})",
            self.overall.pearson,
            self.overall.spearman,
            self.overall.within_speed_spearman,
            self.overall.rmse,
            self.baseline_rmse
        );
        let _ = writeln!(
            out,
            "\n{:<16} {:>6} {:>9} {:>9}",
            "method", "pairs", "Spearman", "in-speed"
        );
        for (m, n, s, w) in &self.per_method {
            let _ = writeln!(out, "{m:<16} {n:>6} {s:>9.3} {w:>9.3}");
        }
        let mut w: Vec<_> = self.model.names.iter().zip(&self.model.weights).collect();
        w.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
        let _ = writeln!(out, "\nweights (per standard deviation of the metric):");
        for (n, w) in w {
            let _ = writeln!(out, "  {n:<32} {w:>8.3}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 clips x 5 samples; `mos` is a known mix of two features, a third
    /// is noise, and one is missing now and then.
    fn synthetic(shuffle: bool) -> (Vec<String>, Vec<Sample>) {
        let mut seed = 12345u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f64::from(seed >> 8) / f64::from(1u32 << 24)
        };
        let names = ["a", "b", "noise"].map(String::from).to_vec();
        let mut samples = Vec::new();
        for g in 0..20 {
            for i in 0..5 {
                let (a, b, n) = (rnd() * 4.0, rnd() * 10.0, rnd());
                let mos = 4.5 - 0.5 * a - 0.1 * b;
                samples.push(Sample {
                    group: format!("clip{g}"),
                    method: if i % 2 == 0 { "x" } else { "y" }.into(),
                    speed: 1.0 + f64::from(i % 3),
                    mos,
                    features: vec![a, if i == 3 { f64::NAN } else { b }, n],
                });
            }
        }
        if shuffle {
            let moses: Vec<f64> = samples.iter().map(|s| s.mos).collect();
            for (i, s) in samples.iter_mut().enumerate() {
                s.mos = moses[(i * 37 + 11) % moses.len()];
            }
        }
        (names, samples)
    }

    #[test]
    fn a_known_mix_is_recovered_on_held_out_clips() {
        let (names, samples) = synthetic(false);
        let fit = fit(&samples, &names).unwrap();
        assert!(fit.overall.pearson > 0.9, "{:?}", fit.overall);
        assert!(fit.overall.rmse < 0.5 * fit.baseline_rmse);
        let w = |n: &str| {
            let j = names.iter().position(|x| x == n).unwrap();
            fit.model.weights[j].abs()
        };
        assert!(w("a") > 5.0 * w("noise"), "{:?}", fit.model.weights);
    }

    #[test]
    fn scores_unrelated_to_the_metrics_do_not_validate() {
        // The check can fail: shuffle the scores and the held-out agreement
        // collapses.
        let (names, samples) = synthetic(true);
        let fit = fit(&samples, &names).unwrap();
        assert!(fit.overall.pearson.abs() < 0.3, "{:?}", fit.overall);
    }

    #[test]
    fn many_noise_metrics_cannot_pass_by_memorising_the_training_clips() {
        // 60 noise metrics for 100 pairs: a model can fit the scores it was
        // shown almost exactly, so only a held-out check keeps this near
        // zero. (Testing on the training pairs makes it near 1.)
        let mut seed = 99u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f64::from(seed >> 8) / f64::from(1u32 << 24)
        };
        let names: Vec<String> = (0..60).map(|j| format!("n{j}")).collect();
        let samples: Vec<Sample> = (0..100)
            .map(|i| Sample {
                group: format!("clip{}", i / 5),
                method: "x".into(),
                speed: 1.0,
                mos: 1.0 + 4.0 * rnd(),
                features: (0..60).map(|_| rnd()).collect(),
            })
            .collect();
        let fit = fit(&samples, &names).unwrap();
        assert!(fit.overall.pearson.abs() < 0.3, "{:?}", fit.overall);
        let on_training = agreement(&points(
            &samples,
            &samples
                .iter()
                .map(|s| fit.model.predict(&s.features))
                .collect::<Vec<_>>(),
        ));
        assert!(on_training.pearson > 0.5, "{on_training:?}");
    }

    #[test]
    fn the_cache_round_trips_and_the_model_predicts_in_range() {
        let (names, samples) = synthetic(false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("features.csv");
        let key = CacheKey {
            pairs: 100,
            metrics: vec!["a".into(), "b".into(), "noise".into()],
        };
        save_cache(&path, &key, &names, &samples).unwrap();
        let (k2, n2, s2) = load_cache(&path).unwrap();
        assert_eq!(k2, key);
        assert_eq!(n2, names);
        assert_eq!(s2.len(), samples.len());
        assert!(s2[3].features[1].is_nan());
        let model = Model::fit(&samples, &names, 1.0);
        for s in &samples {
            let p = model.predict(&s.features);
            assert!((1.0..=5.0).contains(&p));
        }
        let json = serde_json::to_string(&model).unwrap();
        let back: Model = serde_json::from_str(&json).unwrap();
        assert_eq!(back.names, model.names);
    }

    #[test]
    fn paths_with_commas_cannot_be_cached_and_old_caches_are_refused() {
        let (names, mut samples) = synthetic(false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("features.csv");
        let key = CacheKey {
            pairs: 1,
            metrics: vec![],
        };
        samples[0].group = "a,b.wav".into();
        assert!(save_cache(&path, &key, &names, &samples).is_err());
        std::fs::write(&path, "group,method,mos,a\nx,y,1,2\n").unwrap();
        assert!(load_cache(&path).is_err());
    }

    #[test]
    fn agreement_within_speed_ignores_a_speed_only_relationship() {
        // Predictions and scores both fall with speed but are unrelated
        // within each speed: pooled looks strong, within-speed does not.
        let mut pts = Vec::new();
        let mut seed = 3u32;
        for speed in [1.0, 2.0, 3.0] {
            for _ in 0..30 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let a = f64::from(seed >> 8) / f64::from(1u32 << 24);
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let b = f64::from(seed >> 8) / f64::from(1u32 << 24);
                pts.push((5.0 - speed + 0.3 * a, 5.0 - speed + 0.3 * b, speed));
            }
        }
        let a = agreement(&pts);
        assert!(a.spearman > 0.7, "{a:?}");
        assert!(a.within_speed_spearman.abs() < 0.4, "{a:?}");
    }

    #[test]
    fn dropping_metrics_removes_their_columns_and_rejects_typos() {
        let (names, samples) = synthetic(false);
        let (n, s) = without(&names, &samples, &["noise"]).unwrap();
        assert_eq!(n, ["a", "b"]);
        assert_eq!(s[0].features.len(), 2);
        assert!(without(&names, &samples, &["nosie"]).is_err());
        assert_eq!(without(&names, &samples, &[""]).unwrap().0.len(), 3);
    }

    #[test]
    fn too_few_clips_is_an_error_not_a_number() {
        let (names, mut samples) = synthetic(false);
        samples.retain(|s| s.group == "clip0" || s.group == "clip1");
        assert!(fit(&samples, &names).is_err());
    }
}
