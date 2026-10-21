//! Finding a sample's pitch from its first moments, to set a Sampler's root.

/// How much of the start of a sample is listened to, in seconds: the attack
/// and early sustain, before a long note drifts or a decay goes noisy.
pub const LISTEN_SECONDS: f32 = 0.3;

/// The lowest and highest pitch looked for, in Hz.
const MIN_HZ: f32 = 40.0;
const MAX_HZ: f32 = 2_000.0;
/// The YIN threshold: a dip in the difference function counts as a period
/// when it is below this.
const THRESHOLD: f32 = 0.15;
/// A frame quieter than this fraction of the loudest is not asked for a pitch.
const QUIET: f32 = 0.1;
/// The share of the loud frames that must find a pitch, and agree on it
/// (within a quarter tone of the median), for the sample to have one.
const AGREEMENT: f32 = 0.5;

/// The fundamental of `left` and `right` (mixed to mono) in Hz, from the first
/// [`LISTEN_SECONDS`], or `None` if there isn't one: noise, a drum, silence.
///
/// This is YIN on frames across the window, with the median of the frames
/// that find a pitch, accepted when at least half of the loud frames agree.
pub fn fundamental(left: &[f32], right: &[f32], sample_rate: u32) -> Option<f32> {
    let rate = sample_rate as f32;
    let tau_max = (rate / MIN_HZ).ceil() as usize;
    let tau_min = ((rate / MAX_HZ).floor() as usize).max(2);
    // YIN integrates over `tau_max` samples for each lag up to `tau_max`.
    let frame = 2 * tau_max;
    let listen = ((LISTEN_SECONDS * rate) as usize).min(left.len().min(right.len()));
    if listen < frame {
        return None;
    }
    let mono: Vec<f32> = (0..listen).map(|i| 0.5 * (left[i] + right[i])).collect();
    let hop = (tau_max / 2).max(1);
    let starts: Vec<usize> = (0..=listen - frame).step_by(hop).collect();
    let rms = |at: usize| {
        let x = &mono[at..at + frame];
        (x.iter().map(|s| s * s).sum::<f32>() / frame as f32).sqrt()
    };
    let loudest = starts.iter().map(|&at| rms(at)).fold(0.0, f32::max);
    if loudest < 1e-4 {
        return None;
    }
    let mut loud = 0;
    let mut found = Vec::new();
    for &at in &starts {
        if rms(at) < QUIET * loudest {
            continue;
        }
        loud += 1;
        if let Some(hz) = yin(&mono[at..at + frame], tau_min, tau_max, rate) {
            found.push(hz);
        }
    }
    if loud == 0 || (found.len() as f32) < AGREEMENT * loud as f32 {
        return None;
    }
    found.sort_by(f32::total_cmp);
    let median = found[found.len() / 2];
    let agree = found
        .iter()
        .filter(|&&hz| (hz / median).log2().abs() * 12.0 < 0.5)
        .count();
    ((agree as f32) >= AGREEMENT * loud as f32).then_some(median)
}

/// The MIDI key number (fractional) of a frequency, `None` outside 0 to 127.
pub fn key_of(hz: f32) -> Option<f32> {
    let key = 69.0 + 12.0 * (hz / 440.0).log2();
    (0.0..=127.0).contains(&key).then_some(key)
}

fn yin(x: &[f32], tau_min: usize, tau_max: usize, rate: f32) -> Option<f32> {
    let window = x.len() - tau_max;
    // The difference function, then its cumulative mean normalisation.
    let mut d = vec![0.0f32; tau_max + 1];
    for (tau, d) in d.iter_mut().enumerate().skip(1) {
        *d = (0..window)
            .map(|j| {
                let delta = x[j] - x[j + tau];
                delta * delta
            })
            .sum();
    }
    let mut running = 0.0;
    let mut cmnd = vec![1.0f32; tau_max + 1];
    for tau in 1..=tau_max {
        running += d[tau];
        cmnd[tau] = if running > 0.0 {
            d[tau] * tau as f32 / running
        } else {
            1.0
        };
    }
    // The first dip below the threshold, followed down to its bottom.
    let mut tau = tau_min;
    while tau < tau_max {
        if cmnd[tau] < THRESHOLD {
            while tau + 1 < tau_max && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            // Parabolic interpolation between the neighbours.
            let (a, b, c) = (cmnd[tau - 1], cmnd[tau], cmnd[tau + 1]);
            let denominator = a - 2.0 * b + c;
            let shift = if denominator.abs() > 1e-9 {
                0.5 * (a - c) / denominator
            } else {
                0.0
            };
            return Some(rate / (tau as f32 + shift));
        }
        tau += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: u32 = 48_000;

    fn tone(hz: f32, seconds: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|i| 0.5 * (TAU * hz * i as f32 / RATE as f32).sin())
            .collect()
    }

    fn pluck(hz: f32) -> Vec<f32> {
        // A few harmonics, decaying, with the fundamental weaker than the second.
        tone(hz, 1.0)
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let t = i as f32 / RATE as f32;
                let partial = |n: f32, a: f32| a * (TAU * hz * n * t).sin();
                (partial(1.0, 0.3) + partial(2.0, 0.5) + partial(3.0, 0.25)) * (-3.0 * t).exp()
            })
            .collect()
    }

    #[test]
    fn sine_tones_are_found_within_a_few_cents() {
        for hz in [55.0, 110.0, 220.0, 261.63, 440.0, 880.0, 1760.0] {
            let x = tone(hz, 1.0);
            let found = fundamental(&x, &x, RATE).unwrap_or_else(|| panic!("{hz} Hz"));
            let cents = 1200.0 * (found / hz).log2();
            assert!(
                cents.abs() < 5.0,
                "{hz} Hz found as {found} ({cents} cents)"
            );
        }
    }

    #[test]
    fn a_pluck_with_a_weak_fundamental_is_found_at_the_fundamental() {
        let x = pluck(196.0);
        let found = fundamental(&x, &x, RATE).unwrap();
        assert!((1200.0 * (found / 196.0).log2()).abs() < 10.0, "{found}");
    }

    #[test]
    fn only_the_first_part_is_listened_to() {
        // 220 Hz for the first 0.3 s, then a very different pitch.
        let mut x = tone(220.0, 0.3);
        x.extend(tone(777.0, 1.0));
        let found = fundamental(&x, &x, RATE).unwrap();
        assert!((found - 220.0).abs() < 2.0, "{found}");
    }

    #[test]
    fn noise_and_silence_have_no_pitch() {
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..RATE)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
            })
            .collect();
        assert_eq!(fundamental(&noise, &noise, RATE), None);
        let silence = vec![0.0; RATE as usize];
        assert_eq!(fundamental(&silence, &silence, RATE), None);
    }

    #[test]
    fn a_sample_shorter_than_two_periods_of_the_lowest_pitch_has_none() {
        let x = tone(440.0, 0.01);
        assert_eq!(fundamental(&x, &x, RATE), None);
    }

    #[test]
    fn keys_follow_a440() {
        assert!((key_of(440.0).unwrap() - 69.0).abs() < 1e-4);
        assert!((key_of(261.6256).unwrap() - 60.0).abs() < 1e-3);
        assert_eq!(key_of(1.0), None);
    }
}
