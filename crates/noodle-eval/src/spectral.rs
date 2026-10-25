//! Spectral similarity between the reference and the test.

use crate::stft::Spectrogram;
use crate::timemap::TimeMap;
use serde::Serialize;

/// Quiet below this, relative to the loudest magnitude in either signal, is
/// not worth comparing.
const FLOOR_DB: f32 = -80.0;
/// Smoothing, in frames, applied to band levels before comparing them.
const SMOOTH_FRAMES: usize = 5;
/// Bands are this many per octave.
const BANDS_PER_OCTAVE: f64 = 3.0;
const LOWEST_HZ: f64 = 50.0;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Spectral {
    /// Root-mean-square difference of the log magnitudes over the frames both
    /// signals have, in decibels. Full resolution, so sensitive to fine
    /// structure such as smearing.
    pub log_distance_db: f64,
    /// Mean absolute difference of the signals' band levels (a third of an
    /// octave wide) smoothed over about 60 ms, in decibels: how far the tonal
    /// balance moves over time.
    pub smoothed_band_distance_db: f64,
    /// Frobenius norm of the magnitude error over that of the reference.
    pub convergence: f64,
    /// Fraction of the test's frames that have a counterpart in the reference.
    pub covered: f64,
}

/// For each test frame, the reference row at the time `map` says it plays at,
/// or `None` past the reference's end.
pub fn align(reference: &Spectrogram, test: &Spectrogram, map: &TimeMap) -> (Vec<f32>, Vec<bool>) {
    let mut rows = vec![0.0; test.mag.len()];
    let mut valid = vec![false; test.frames];
    for f in 0..test.frames {
        let t = f as f64 * test.hop_seconds;
        let src = map.to_source(t);
        valid[f] = reference.row_at(src, &mut rows[f * test.bins..(f + 1) * test.bins]);
    }
    (rows, valid)
}

fn band_edges(s: &Spectrogram) -> Vec<usize> {
    let nyquist = s.hz(s.bins - 1);
    let mut edges = vec![];
    let mut hz = LOWEST_HZ;
    while hz < nyquist {
        let bin = (hz / s.hz(1)).round() as usize;
        if edges.last() != Some(&bin) {
            edges.push(bin);
        }
        hz *= 2f64.powf(1.0 / BANDS_PER_OCTAVE);
    }
    edges.push(s.bins);
    edges
}

pub fn compare(reference: &Spectrogram, test: &Spectrogram, map: &TimeMap) -> Spectral {
    let (rows, valid) = align(reference, test, map);
    let covered = valid.iter().filter(|&&v| v).count();
    if covered == 0 {
        return Spectral {
            log_distance_db: f64::NAN,
            smoothed_band_distance_db: f64::NAN,
            convergence: f64::NAN,
            covered: 0.0,
        };
    }
    let bins = test.bins;
    let peak = rows
        .iter()
        .chain(&test.mag)
        .copied()
        .fold(0.0, f32::max)
        .max(1e-9);
    let floor = peak * 10f32.powf(FLOOR_DB / 20.0);
    let db = |m: f32| 20.0 * m.max(floor).log10();

    let edges = band_edges(test);
    let nb = edges.len() - 1;
    let mut band_ref = vec![vec![0.0f32; nb]; test.frames];
    let mut band_test = vec![vec![0.0f32; nb]; test.frames];
    let (mut sq, mut n, mut err2, mut ref2) = (0.0f64, 0usize, 0.0f64, 0.0f64);
    for f in (0..test.frames).filter(|&f| valid[f]) {
        let (r, t) = (&rows[f * bins..(f + 1) * bins], test.row(f));
        for k in 0..bins {
            let d = f64::from(db(t[k]) - db(r[k]));
            sq += d * d;
            n += 1;
            let e = f64::from(t[k] - r[k]);
            err2 += e * e;
            ref2 += f64::from(r[k]) * f64::from(r[k]);
        }
        for b in 0..nb {
            let energy = |x: &[f32]| x[edges[b]..edges[b + 1]].iter().map(|v| v * v).sum::<f32>();
            band_ref[f][b] = energy(r);
            band_test[f][b] = energy(t);
        }
    }
    // Band levels in decibels, smoothed over time on valid frames.
    let bfloor = floor * floor;
    let smooth = |bands: &[Vec<f32>], f: usize, b: usize| -> f32 {
        let lo = f.saturating_sub(SMOOTH_FRAMES / 2);
        let hi = (f + SMOOTH_FRAMES / 2).min(test.frames - 1);
        let (mut s, mut c) = (0.0, 0);
        for g in (lo..=hi).filter(|&g| valid[g]) {
            s += bands[g][b];
            c += 1;
        }
        10.0 * (s / c as f32).max(bfloor).log10()
    };
    let (mut bsum, mut bn) = (0.0f64, 0usize);
    for f in (0..test.frames).filter(|&f| valid[f]) {
        for b in 0..nb {
            bsum += f64::from((smooth(&band_test, f, b) - smooth(&band_ref, f, b)).abs());
            bn += 1;
        }
    }
    Spectral {
        log_distance_db: (sq / n as f64).sqrt(),
        smoothed_band_distance_db: bsum / bn as f64,
        convergence: (err2 / ref2.max(1e-30)).sqrt(),
        covered: covered as f64 / test.frames as f64,
    }
}
