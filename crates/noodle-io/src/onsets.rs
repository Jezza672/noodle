//! Finding the transients (onsets) in a piece of audio, for the warp engine's
//! Beats mode and, later, the clip editor's transient ticks.

/// How far apart two onsets must be, in seconds. Of two closer ones the
/// stronger stays.
const MIN_GAP: f64 = 0.05;
/// How much louder, in decibels, a short stretch of audio must be than the
/// quiet just before it to count as an attack.
const MIN_RISE_DB: f32 = 5.0;
/// Anything quieter than this (in decibels relative to full scale) is silence,
/// whatever it rose from.
const FLOOR_DB: f32 = -60.0;

/// The frames at which a transient starts in the bass of `mono` (below about
/// 150 Hz): kicks and the start of bass notes, which a hi-hat over a held bass
/// note doesn't count as.
pub fn detect_bass_onsets(mono: &[f32], rate: u32) -> Vec<u64> {
    let a = 1.0 - (-std::f32::consts::TAU * 150.0 / rate as f32).exp();
    let (mut p, mut q) = (0.0f32, 0.0f32);
    // The level of the bass, rising at once and falling over 40 ms: the swing
    // of a 40 Hz tone would otherwise look like an attack every cycle.
    let release = (-1.0 / (0.04 * rate as f32)).exp();
    let mut level = 0.0f32;
    let bass: Vec<f32> = mono
        .iter()
        .map(|x| {
            p += a * (x - p);
            q += a * (p - q);
            level = q.abs().max(level * release);
            level
        })
        .collect();
    detect_onsets(&bass, rate)
}

/// The frames at which a transient starts in `mono`, at `rate`, in order.
///
/// An onset is where the short-time level jumps by at least 5 dB over the
/// quietest of the two blocks before it, is the largest jump nearby, and
/// isn't silent. The reported frame is the first one of the jump that is
/// a tenth of the block's peak and clear of what was sounding before, so it
/// lands on the attack and not on the block that contains it.
pub fn detect_onsets(mono: &[f32], rate: u32) -> Vec<u64> {
    let hop = (rate as usize / 200).max(16);
    let blocks = mono.len() / hop;
    if blocks < 3 {
        return Vec::new();
    }
    let level: Vec<f32> = (0..blocks)
        .map(|b| {
            let block = &mono[b * hop..(b + 1) * hop];
            let power = block.iter().map(|x| x * x).sum::<f32>() / hop as f32;
            10.0 * (power + 1e-12).log10()
        })
        .collect();
    // How much each block rises over the quieter of the two before it.
    let rise: Vec<f32> = (0..blocks)
        .map(|b| {
            let before = level[b.saturating_sub(2)..b]
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min);
            if before.is_finite() {
                level[b] - before
            } else {
                0.0
            }
        })
        .collect();
    let near = ((MIN_GAP * f64::from(rate)) / hop as f64).ceil() as usize;
    let mut found: Vec<(usize, f32)> = Vec::new();
    for b in 1..blocks {
        if rise[b] < MIN_RISE_DB || level[b] < FLOOR_DB {
            continue;
        }
        // The strongest rise within the minimum gap either side.
        let lo = b.saturating_sub(near);
        let hi = (b + near).min(blocks - 1);
        let peak = (lo..=hi).all(|o| rise[o] < rise[b] || (rise[o] == rise[b] && o >= b));
        if peak {
            found.push((b, rise[b]));
        }
    }
    let mut onsets: Vec<u64> = Vec::with_capacity(found.len());
    for (b, _) in found {
        // Look in the block of the jump and the one before it for the first
        // frame that stands out from what was sounding before, and from the
        // attack's own peak.
        let from = (b - 1) * hop;
        let to = ((b + 1) * hop).min(mono.len());
        let peak = mono[from..to].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let before = &mono[from.saturating_sub(2 * hop)..from];
        let background = before.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let mut threshold = (peak * 0.1).max(background * 1.5);
        if threshold >= peak {
            threshold = peak * 0.5;
        }
        let at = mono[from..to]
            .iter()
            .position(|x| x.abs() >= threshold)
            .unwrap_or(0);
        let frame = (from + at) as u64;
        if onsets
            .last()
            .is_none_or(|last| frame >= last + (MIN_GAP * f64::from(rate)) as u64)
        {
            onsets.push(frame);
        }
    }
    onsets
}

/// The tempo (in beats per minute, 80 to 160) the onsets at `times` (seconds)
/// most nearly sit on the beat of, if there are enough of them to tell.
///
/// Each candidate tempo is scored by how well the onsets line up in phase:
/// with the beat period as the unit, onsets on the beat all land on the same
/// phase of the cycle, and the length of the sum of their phases is the
/// score. Onsets on the half beat still count, through a second sum on a
/// grid of eighth notes. A tempo of half or twice the true one is kept out
/// by the range.
pub fn estimate_tempo(times: &[f64]) -> Option<f64> {
    if times.len() < 4 {
        return None;
    }
    // Onsets on the beat and on the half beat both count: the second sum
    // is of the onsets' phases on a grid of eighth notes.
    let score = |bpm: f64| {
        let mut total = 0.0;
        for grid in [1.0, 2.0] {
            let (mut re, mut im) = (0.0, 0.0);
            for &t in times {
                let phase = std::f64::consts::TAU * t * bpm / 60.0 * grid;
                re += phase.cos();
                im += phase.sin();
            }
            total += re.hypot(im);
        }
        total / 2.0
    };
    let mut best = (80.0, 0.0);
    let mut bpm = 80.0;
    while bpm <= 160.0 {
        let s = score(bpm);
        if s > best.1 {
            best = (bpm, s);
        }
        bpm += 0.1;
    }
    // Refine around the best.
    let mut refined = best;
    let mut bpm = best.0 - 0.1;
    while bpm <= best.0 + 0.1 {
        let s = score(bpm);
        if s > refined.1 {
            refined = (bpm, s);
        }
        bpm += 0.005;
    }
    // Weak agreement is no tempo at all.
    (refined.1 / times.len() as f64 > 0.5).then_some((refined.0 * 100.0).round() / 100.0)
}

/// What the clip editor learns from a file's audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    /// Where transients start, in seconds from the start of the file.
    pub onsets: Vec<f64>,
    /// The tempo they sit on the beat of, if there's a clear one.
    pub tempo: Option<f64>,
    /// Whether the file is long enough, with transients close enough, that
    /// its beats are worth keeping whole when it is stretched.
    pub percussive: bool,
}

impl Analysis {
    pub fn of(mono: &[f32], rate: u32) -> Self {
        let onsets: Vec<f64> = detect_onsets(mono, rate)
            .into_iter()
            .map(|frame| frame as f64 / f64::from(rate))
            .collect();
        let seconds = mono.len() as f64 / f64::from(rate);
        Self {
            tempo: estimate_tempo(&onsets),
            percussive: seconds > 0.0 && onsets.len() as f64 / seconds >= 1.0,
            onsets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    #[test]
    fn bass_onsets_are_kicks_not_hats_or_the_swing_of_a_low_tone() {
        let tau = std::f32::consts::TAU;
        let mut seed = 3u32;
        let mut x: Vec<f32> = (0..4 * RATE as usize)
            .map(|i| {
                // A held 45 Hz note under a stream of hats.
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let hat = ((seed >> 8) as f32 / (1 << 23) as f32 - 1.0)
                    * if (i / 6_000) % 2 == 0 { 0.2 } else { 0.0 };
                (tau * 45.0 * i as f32 / RATE as f32).sin() * 0.1 + hat
            })
            .collect();
        for start in [RATE as usize, 3 * RATE as usize] {
            for i in 0..12_000 {
                let t = i as f32 / 12_000.0;
                x[start + i] +=
                    (tau * 60.0 * i as f32 / RATE as f32).sin() * 0.9 * (-t * 6.0).exp();
            }
        }
        let mut found = detect_bass_onsets(&x, RATE);
        // The note itself starts from silence.
        found.retain(|f| *f > 1_000);
        assert_eq!(found.len(), 2, "{found:?}");
        for (f, want) in found.iter().zip([RATE as u64, 3 * RATE as u64]) {
            assert!(f.abs_diff(want) < 600, "{f} for {want}");
        }
    }

    #[test]
    fn the_tempo_is_where_the_onsets_line_up() {
        let beats = |bpm: f64, n: usize| -> Vec<f64> {
            (0..n).map(|i| 0.1 + i as f64 * 60.0 / bpm).collect()
        };
        for bpm in [90.0, 120.0, 133.5, 150.0] {
            let found = estimate_tempo(&beats(bpm, 16)).unwrap();
            assert!((found - bpm).abs() < 0.3, "{found} for {bpm}");
        }
        // Hits on the off-beats too, and a missing beat.
        let mut times = beats(120.0, 16);
        times.extend((0..8).map(|i| 0.35 + i as f64));
        times.remove(5);
        times.sort_by(f64::total_cmp);
        let found = estimate_tempo(&times).unwrap();
        assert!((found - 120.0).abs() < 0.5, "{found}");
    }

    #[test]
    fn scattered_onsets_have_no_tempo() {
        assert_eq!(estimate_tempo(&[0.1, 0.4]), None);
        let mut seed = 5u32;
        let scattered: Vec<f64> = (0..20)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                i as f64 * 0.9 + (seed >> 16) as f64 / 65_536.0 * 0.7
            })
            .collect();
        assert_eq!(estimate_tempo(&scattered), None);
    }

    /// A decaying 200 Hz click at each of `at` (in seconds), over `seconds`.
    fn clicks(at: &[f64], seconds: f64) -> Vec<f32> {
        let mut out = vec![0.0f32; (seconds * f64::from(RATE)) as usize];
        for &t in at {
            let start = (t * f64::from(RATE)) as usize;
            for i in 0..12_000.min(out.len() - start) {
                let x = i as f32 / RATE as f32;
                out[start + i] += (x * 200.0 * std::f32::consts::TAU).sin() * (-x * 25.0).exp();
            }
        }
        out
    }

    #[test]
    fn finds_each_click_within_a_few_milliseconds() {
        let times = [0.3, 0.8, 1.1, 1.9];
        let found = detect_onsets(&clicks(&times, 2.5), RATE);
        assert_eq!(found.len(), times.len(), "{found:?}");
        for (f, t) in found.iter().zip(times) {
            let want = (t * f64::from(RATE)) as i64;
            assert!((*f as i64 - want).abs() < 480, "{f} vs {want}");
        }
    }

    #[test]
    fn silence_and_steady_tones_have_none() {
        assert!(detect_onsets(&vec![0.0; 48_000], RATE).is_empty());
        let tone: Vec<f32> = (0..96_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        // The very start of a tone is an attack; nothing after it is.
        let found = detect_onsets(&tone, RATE);
        assert!(found.len() <= 1, "{found:?}");
    }

    #[test]
    fn a_decay_does_not_make_onsets() {
        let found = detect_onsets(&clicks(&[0.2], 2.0), RATE);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn onsets_closer_than_the_gap_merge() {
        let found = detect_onsets(&clicks(&[0.3, 0.32], 1.0), RATE);
        assert_eq!(found.len(), 1, "{found:?}");
    }
}
