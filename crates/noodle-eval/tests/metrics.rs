//! The metrics on signals whose answer is known.

use noodle_eval::{Clip, TimeMap, evaluate, phasiness::phasiness};

const RATE: u32 = 22_050;

fn noise(n: usize, seed: &mut u32) -> Vec<f32> {
    (0..n)
        .map(|_| {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (*seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        })
        .collect()
}

/// Noise bursts with a fast exponential decay at the given times.
fn hits(times: &[f64], seconds: f64) -> Vec<f32> {
    let mut out = vec![0.0; (seconds * f64::from(RATE)) as usize];
    let mut seed = 7;
    for &t in times {
        let start = (t * f64::from(RATE)) as usize;
        let burst = noise(2000, &mut seed);
        for (i, b) in burst.iter().enumerate() {
            if let Some(o) = out.get_mut(start + i) {
                *o += b * (-(i as f32) / 250.0).exp() * 0.8;
            }
        }
    }
    out
}

fn tone(hz: f64, seconds: f64) -> Vec<f32> {
    (0..(seconds * f64::from(RATE)) as usize)
        .map(|n| (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(RATE)).sin() as f32 * 0.5)
        .collect()
}

fn clip(x: &[f32]) -> Clip<'_> {
    Clip::new(x, 1, RATE)
}

fn grid(step: f64, count: usize) -> Vec<f64> {
    (1..=count).map(|i| i as f64 * step).collect()
}

#[test]
fn a_signal_scores_perfectly_against_itself() {
    let x = hits(&grid(0.5, 8), 5.0);
    let r = evaluate(clip(&x), clip(&x), &TimeMap::Identity);
    assert!(r.spectral.log_distance_db < 1e-6);
    assert!(r.spectral.convergence < 1e-6);
    assert_eq!(r.timing.matched, r.timing.expected);
    assert!(r.timing.expected >= 7);
    assert_eq!(r.timing.max_abs_offset_ms, 0.0);
    assert_eq!(r.timing.drift_ms, 0.0);
    assert_eq!(r.vertical_phasiness_delta, 0.0);
    assert!((r.transients.flux_peakiness_ratio - 1.0).abs() < 1e-9);
    assert!((r.transients.percussive_share_ratio - 1.0).abs() < 1e-9);
    assert!(r.transients.flux_peak_change.abs() < 1e-9);
    let p = r.paper;
    assert!(
        p.mph_mw < 1e-9 && p.sph_mw < 1e-9 && p.mph_nw < 1e-9,
        "{p:?}"
    );
    assert!(p.ssmad < 1e-9 && p.ssmd.abs() < 1e-9, "{p:?}");
    assert_eq!(p.delta_p, 0.0);
    assert!(
        (p.tr_rat - 1.0).abs() < 1e-9 && (p.hps_tr_rat - 1.0).abs() < 1e-9,
        "{p:?}"
    );
    assert_eq!(r.level.lufs_delta, 0.0);
    assert!(r.stereo.is_none());
}

#[test]
fn a_late_signal_reports_its_delay() {
    let reference = hits(&grid(0.5, 8), 5.0);
    let test = hits(
        &grid(0.5, 8).iter().map(|t| t + 0.04).collect::<Vec<_>>(),
        5.0,
    );
    let r = evaluate(clip(&reference), clip(&test), &TimeMap::Identity);
    assert_eq!(r.timing.matched, r.timing.expected);
    assert!(
        (r.timing.median_abs_offset_ms - 40.0).abs() < 12.0,
        "{:?}",
        r.timing
    );
    assert!(r.timing.mean_offset_ms > 0.0, "late is positive");
}

#[test]
fn the_time_map_decides_where_onsets_should_land() {
    // Hits every 0.5 s, stretched to every 1 s: right under Speed(0.5), and
    // wrong under the identity map.
    let reference = hits(&grid(0.5, 8), 5.0);
    let test = hits(&grid(1.0, 8), 10.0);
    let right = evaluate(clip(&reference), clip(&test), &TimeMap::Speed(0.5));
    assert_eq!(right.timing.matched, right.timing.expected);
    assert!(
        right.timing.median_abs_offset_ms < 12.0,
        "{:?}",
        right.timing
    );
    let wrong = evaluate(clip(&reference), clip(&test), &TimeMap::Identity);
    assert!(
        wrong.timing.matched <= wrong.timing.expected / 2,
        "{:?}",
        wrong.timing
    );
}

#[test]
fn a_tempo_step_is_followed_with_a_points_map() {
    // Source hits every 0.5 s; the first two seconds are played at half speed.
    let source = grid(0.5, 8);
    let out: Vec<f64> = source
        .iter()
        .map(|&t| if t <= 2.0 { t * 2.0 } else { t + 2.0 })
        .collect();
    let reference = hits(&source, 5.0);
    let test = hits(&out, 7.0);
    let map = TimeMap::Points(vec![(0.0, 0.0), (4.0, 2.0), (7.0, 5.0)]);
    let r = evaluate(clip(&reference), clip(&test), &map);
    assert_eq!(r.timing.matched, r.timing.expected, "{:?}", r.timing);
    assert!(r.timing.max_abs_offset_ms < 15.0, "{:?}", r.timing);
}

#[test]
fn a_tempo_ramp_is_followed_with_a_densely_sampled_points_map() {
    // Output time runs ahead of source time quadratically: a smooth slowdown.
    let out_of = |s: f64| s + 0.05 * s * s;
    let source = grid(0.5, 8);
    let reference = hits(&source, 5.0);
    let test = hits(
        &source.iter().map(|&s| out_of(s)).collect::<Vec<_>>(),
        out_of(5.0) + 0.5,
    );
    let points: Vec<(f64, f64)> = (0..=110)
        .map(|i| (out_of(f64::from(i) * 0.05), f64::from(i) * 0.05))
        .collect();
    let r = evaluate(clip(&reference), clip(&test), &TimeMap::Points(points));
    assert_eq!(r.timing.matched, r.timing.expected, "{:?}", r.timing);
    assert!(r.timing.max_abs_offset_ms < 15.0, "{:?}", r.timing);
}

#[test]
fn slow_drift_shows_even_when_every_hit_still_matches() {
    // Each hit lands 1% later in the piece than it should: the last one is
    // 60 ms late, inside the match window, so only the drift shows it.
    let reference = hits(&grid(0.5, 12), 7.0);
    let drifted: Vec<f64> = grid(0.5, 12).iter().map(|t| t * 1.01).collect();
    let test = hits(&drifted, 7.0);
    let r = evaluate(clip(&reference), clip(&test), &TimeMap::Identity);
    assert_eq!(r.timing.matched, r.timing.expected, "{:?}", r.timing);
    assert!(r.timing.drift_ms > 25.0, "{:?}", r.timing);
}

#[test]
fn a_constant_delay_is_not_drift() {
    let reference = hits(&grid(0.5, 12), 7.0);
    let test = hits(
        &grid(0.5, 12).iter().map(|t| t + 0.04).collect::<Vec<_>>(),
        7.0,
    );
    let r = evaluate(clip(&reference), clip(&test), &TimeMap::Identity);
    assert!(r.timing.drift_ms < 12.0, "{:?}", r.timing);
    assert!(r.timing.mean_offset_ms > 25.0, "{:?}", r.timing);
}

#[test]
fn a_clip_too_short_to_measure_drift_says_so() {
    let x = hits(&[0.3], 1.5);
    let r = evaluate(clip(&x), clip(&x), &TimeMap::Identity);
    assert!(r.timing.drift_ms.is_nan());
}

#[test]
fn a_pure_gain_change_moves_nothing_but_the_level() {
    let reference = hits(&grid(0.5, 8), 5.0);
    let quiet: Vec<f32> = reference.iter().map(|s| s * 0.25).collect(); // -12 dB
    let r = evaluate(clip(&reference), clip(&quiet), &TimeMap::Identity);
    assert!((r.level.lufs_delta + 12.04).abs() < 0.1, "{:?}", r.level);
    assert!(r.spectral.log_distance_db < 0.5, "{:?}", r.spectral);
    assert!(
        r.spectral.smoothed_band_distance_db < 0.5,
        "{:?}",
        r.spectral
    );
    assert!(r.spectral.convergence < 0.06, "{:?}", r.spectral);
    assert!(
        r.transients.flux_peak_change.abs() < 0.05,
        "{:?}",
        r.transients
    );
    assert!(
        (r.transients.flux_peakiness_ratio - 1.0).abs() < 0.05,
        "{:?}",
        r.transients
    );
    assert!(
        (r.transients.percussive_share_ratio - 1.0).abs() < 0.05,
        "{:?}",
        r.transients
    );
    assert_eq!(r.timing.matched, r.timing.expected);
    let p = r.paper;
    assert!(p.ssmad < 0.01 && p.ssmd.abs() < 0.01, "{p:?}");
    assert!(p.delta_p.abs() < 0.3, "{p:?}");
    assert!(
        (p.tr_rat - 1.0).abs() < 0.05 && (p.hps_tr_rat - 1.0).abs() < 0.05,
        "{p:?}"
    );
}

#[test]
fn smeared_attacks_lose_sharpness() {
    let reference = hits(&grid(0.5, 8), 5.0);
    // Averaging the signal with its own delayed copies blurs each attack.
    let blur = 4000;
    let test: Vec<f32> = (0..reference.len())
        .map(|i| {
            (0..blur)
                .step_by(20)
                .map(|d| if i >= d { reference[i - d] } else { 0.0 })
                .sum::<f32>()
                / 30.0
        })
        .collect();
    let r = evaluate(clip(&reference), clip(&test), &TimeMap::Identity);
    assert!(r.transients.flux_peak_change < -0.2, "{:?}", r.transients);
    assert!(
        r.transients.flux_peakiness_ratio < 0.9,
        "{:?}",
        r.transients
    );
    assert!(
        r.transients.percussive_share_ratio < 0.9,
        "{:?}",
        r.transients
    );
    // The paper's measures: the reference's attacks are taller and its
    // transient signal louder than the smeared test's (ratios above 1).
    assert!(r.paper.tr_rat > 1.1, "{:?}", r.paper);
    assert!(r.paper.hps_tr_rat > 1.1, "{:?}", r.paper);
}

#[test]
fn a_pitch_shift_is_measured_in_cents() {
    let reference = tone(440.0, 3.0);
    let test = tone(440.0 * 2f64.powf(1.0 / 12.0), 3.0);
    let r = evaluate(clip(&reference), clip(&test), &TimeMap::Identity);
    assert!((r.pitch.median_cents - 100.0).abs() < 15.0, "{:?}", r.pitch);
    assert!(r.pitch.agreement > 0.9, "{:?}", r.pitch);
    let same = evaluate(clip(&reference), clip(&reference), &TimeMap::Identity);
    assert!(same.pitch.median_abs_cents < 1.0, "{:?}", same.pitch);
}

#[test]
fn pitch_is_compared_across_a_stretch() {
    // The same note, twice as long, is not a pitch change.
    let r = evaluate(
        clip(&tone(330.0, 2.0)),
        clip(&tone(330.0, 4.0)),
        &TimeMap::Speed(0.5),
    );
    assert!(r.pitch.median_abs_cents < 5.0, "{:?}", r.pitch);
}

#[test]
fn phase_jumps_raise_phasiness() {
    let clean = tone(1000.0, 3.0);
    let mut seed = 3;
    let jumpy: Vec<f32> = {
        let mut out = Vec::new();
        while out.len() < clean.len() {
            let phase = f64::from(noise(1, &mut seed)[0]) * std::f64::consts::PI;
            let n0 = out.len();
            out.extend((0..300).map(|n| {
                (2.0 * std::f64::consts::PI * 1000.0 * (n0 + n) as f64 / f64::from(RATE) + phase)
                    .sin() as f32
                    * 0.5
            }));
        }
        out.truncate(clean.len());
        out
    };
    let (a, b) = (phasiness(&clean, RATE), phasiness(&jumpy, RATE));
    assert!(a < 0.05, "a steady sinusoid is coherent: {a}");
    assert!(b > a + 0.1, "{a} vs {b}");
}

#[test]
fn a_dulled_signal_is_spectrally_further_than_a_louder_one() {
    let reference = hits(&grid(0.5, 8), 5.0);
    let dull: Vec<f32> = (0..reference.len())
        .map(|i| {
            (0..8)
                .map(|d| if i >= d { reference[i - d] } else { 0.0 })
                .sum::<f32>()
                / 8.0
        })
        .collect();
    let loud: Vec<f32> = reference.iter().map(|s| s * 1.1).collect();
    let d = evaluate(clip(&reference), clip(&dull), &TimeMap::Identity);
    let l = evaluate(clip(&reference), clip(&loud), &TimeMap::Identity);
    assert!(
        d.spectral.log_distance_db > 3.0 * l.spectral.log_distance_db,
        "{:?} {:?}",
        d.spectral,
        l.spectral
    );
    assert!(d.spectral.smoothed_band_distance_db > 3.0 * l.spectral.smoothed_band_distance_db);
}

#[test]
fn collapsing_the_stereo_image_shows() {
    let n = 40_000;
    let mut seed = 11;
    let (l, r) = (noise(n, &mut seed), noise(n, &mut seed));
    let wide: Vec<f32> = l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect();
    let narrow: Vec<f32> = l
        .iter()
        .zip(&r)
        .flat_map(|(a, b)| [(a + b) * 0.5, (a + b) * 0.5])
        .collect();
    let rep = evaluate(
        Clip::new(&wide, 2, RATE),
        Clip::new(&narrow, 2, RATE),
        &TimeMap::Identity,
    );
    let s = rep.stereo.unwrap();
    assert!(s.correlation_delta > 0.9, "{s:?}");
    assert!(s.width_delta_db < -20.0, "{s:?}");
}

#[test]
fn the_papers_phasiness_rises_when_a_stretch_loses_phase_continuity() {
    // A steady tone stretched to twice the length is the ideal result; the
    // same with a random phase jump every 300 samples is a bad one.
    let reference = tone(1000.0, 2.0);
    let ideal = tone(1000.0, 4.0);
    let mut seed = 9;
    let mut jumpy = Vec::new();
    while jumpy.len() < ideal.len() {
        let phase = f64::from(noise(1, &mut seed)[0]) * std::f64::consts::PI;
        let n0 = jumpy.len();
        jumpy.extend((0..300).map(|n| {
            (2.0 * std::f64::consts::PI * 1000.0 * (n0 + n) as f64 / f64::from(RATE) + phase).sin()
                as f32
                * 0.5
        }));
    }
    jumpy.truncate(ideal.len());
    let good = evaluate(clip(&reference), clip(&ideal), &TimeMap::Speed(0.5)).paper;
    let bad = evaluate(clip(&reference), clip(&jumpy), &TimeMap::Speed(0.5)).paper;
    assert!(bad.mph_nw > 2.0 * good.mph_nw, "{good:?} {bad:?}");
    assert!(bad.mph_mw > 2.0 * good.mph_mw, "{good:?} {bad:?}");
}

#[test]
fn peak_delta_counts_hits_the_test_added_or_lost() {
    let sparse = hits(&grid(0.5, 8), 5.0);
    let dense = hits(&grid(0.25, 16), 5.0);
    let more = evaluate(clip(&sparse), clip(&dense), &TimeMap::Identity).paper;
    let fewer = evaluate(clip(&dense), clip(&sparse), &TimeMap::Identity).paper;
    assert!(more.delta_p > 1.0, "{more:?}");
    assert!(fewer.delta_p < -1.0, "{fewer:?}");
}
