//! Makes the wavetables the system library ships (`assets/samples/wt-*.wav`).
//!
//! They are generated, not recorded, so they are ours to licence as CC0: each
//! cycle is a sum of sine harmonics with amplitudes chosen by a formula, 2048
//! samples long, scaled to the same peak. A table is one file of 16 cycles one
//! after another; the Wavetable node cuts it every 2048 samples.
//!
//! `cargo run -p noodle-nodes --example wavetables` rewrites the files. Then
//! `UPDATE_SAMPLE_INDEX=1 cargo test -p noodle-nodes system_samples` rewrites
//! the index. The output is the same on every run.

use std::f64::consts::{PI, TAU};
use std::path::Path;

const N: usize = 2048;
const CYCLES: usize = 16;
const HARMONICS: usize = N / 2 - 1;
const PEAK: f64 = 0.9;

/// One cycle from the amplitude of each harmonic, with a phase for each (a
/// sine for zero, a cosine for a quarter), scaled to a peak of [`PEAK`].
fn cycle(amplitude: impl Fn(usize) -> f64, phase: impl Fn(usize) -> f64) -> Vec<f32> {
    let amps: Vec<f64> = (1..=HARMONICS).map(&amplitude).collect();
    let mut out: Vec<f64> = (0..N)
        .map(|i| {
            let t = i as f64 / N as f64;
            amps.iter()
                .enumerate()
                .filter(|(_, a)| **a != 0.0)
                .map(|(k, a)| a * (TAU * (k + 1) as f64 * t + phase(k + 1)).sin())
                .sum()
        })
        .collect();
    let peak = out.iter().fold(0.0f64, |m, x| m.max(x.abs())).max(1e-9);
    out.iter_mut().for_each(|x| *x *= PEAK / peak);
    out.into_iter().map(|x| x as f32).collect()
}

fn mix(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The harmonic amplitudes of the four basic shapes.
fn shape(which: usize, k: usize) -> f64 {
    let (odd, kf) = (k % 2 == 1, k as f64);
    match which {
        0 => f64::from(k == 1),
        // Triangle: odd harmonics at 1/k², alternating in sign.
        1 => {
            if odd {
                let sign = if (k / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
                sign / (kf * kf)
            } else {
                0.0
            }
        }
        // Saw: every harmonic at 1/k (the sign makes it rise).
        2 => -1.0 / kf,
        _ => {
            if odd {
                1.0 / kf
            } else {
                0.0
            }
        }
    }
}

/// Sine, triangle, saw, square, each melting into the next.
fn basic_shapes() -> Vec<Vec<f32>> {
    (0..CYCLES)
        .map(|c| {
            let at = c as f64 / (CYCLES - 1) as f64 * 3.0;
            let (from, t) = ((at.floor() as usize).min(2), at - at.floor().min(2.0));
            let t = if at >= 3.0 { 1.0 } else { t };
            cycle(|k| mix(shape(from, k), shape(from + 1, k), t), |_| 0.0)
        })
        .collect()
}

/// A pulse wave whose width narrows from a half to a twentieth.
fn pulse_width() -> Vec<Vec<f32>> {
    (0..CYCLES)
        .map(|c| {
            let duty = mix(0.5, 0.05, c as f64 / (CYCLES - 1) as f64);
            // A pulse of width `duty` is cosines at 2 sin(pi k d) / (pi k),
            // moved to centre it; the phase does both.
            cycle(
                |k| (PI * k as f64 * duty).sin() / k as f64,
                |k| PI / 2.0 - PI * k as f64 * duty,
            )
        })
        .collect()
}

/// A saw's harmonics added one at a time, one to sixteen.
fn harmonic_sweep() -> Vec<Vec<f32>> {
    (0..CYCLES)
        .map(|c| cycle(|k| if k <= c + 1 { 1.0 / k as f64 } else { 0.0 }, |_| 0.0))
        .collect()
}

/// A saw with one wide peak in its spectrum, which rises from the 2nd
/// harmonic to the 20th, so it sounds like a vowel opening up.
fn formant_sweep() -> Vec<Vec<f32>> {
    (0..CYCLES)
        .map(|c| {
            let centre = 2.0 * (10.0f64).powf(c as f64 / (CYCLES - 1) as f64);
            cycle(
                |k| {
                    let d = (k as f64 - centre) / (0.35 * centre + 0.8);
                    (0.03 + (-d * d).exp()) / k as f64
                },
                |_| 0.0,
            )
        })
        .collect()
}

/// Stepped, bit-crushed shapes: each cycle holds a few sines at whole ratios,
/// is quantised to fewer levels as the table goes on, and holds each value for
/// more samples.
fn digital() -> Vec<Vec<f32>> {
    (0..CYCLES)
        .map(|c| {
            let levels = (64.0 / (1.0 + c as f64 * 0.9)).round().max(3.0);
            let hold = 1 + c / 2;
            let a = 1 + c % 5;
            let b = 2 + (c * 3) % 7;
            let raw: Vec<f64> = (0..N / hold)
                .map(|i| {
                    let t = (i * hold) as f64 / N as f64;
                    (TAU * a as f64 * t).sin() + 0.6 * (TAU * b as f64 * t + 1.0).sin()
                })
                .collect();
            let peak = raw.iter().fold(0.0f64, |m, x| m.max(x.abs()));
            let mut out: Vec<f32> = (0..N)
                .map(|i| {
                    let x = raw[(i / hold).min(raw.len() - 1)] / peak;
                    ((x * levels / 2.0).round() / (levels / 2.0) * PEAK) as f32
                })
                .collect();
            // Zero mean, so a stack of these doesn't carry a DC step.
            let mean = out.iter().sum::<f32>() / N as f32;
            out.iter_mut().for_each(|x| *x -= mean);
            out
        })
        .collect()
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/samples");
    let tables: [(&str, Vec<Vec<f32>>); 5] = [
        ("wt-basic-shapes", basic_shapes()),
        ("wt-pulse-width", pulse_width()),
        ("wt-harmonic-sweep", harmonic_sweep()),
        ("wt-formant-sweep", formant_sweep()),
        ("wt-digital", digital()),
    ];
    for (name, cycles) in tables {
        let samples: Vec<f32> = cycles.into_iter().flatten().collect();
        let path = dir.join(format!("{name}.wav"));
        noodle_io::write_wav(&path, &samples, 1, 48_000).unwrap();
        println!("{}", path.display());
    }
}
