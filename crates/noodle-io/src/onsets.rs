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

/// What the onsets of a clip say about its beat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoEstimate {
    /// Beats per minute, 80 to 160.
    pub bpm: f64,
    /// How well the onsets agree with it: 0 for no agreement, 1 for onsets
    /// that are all exactly on the beat. Above [`CLEAR_TEMPO`] it is a clear
    /// tempo; below, a guess.
    pub confidence: f64,
    /// Where the beat falls against the onsets, in beats from the start of
    /// the audio, in `[-0.5, 0.5)`: the onsets sit near the whole beats of a
    /// grid slid this far (`n + phase`).
    pub phase: f64,
}

/// The confidence from which a tempo is clear and not a guess.
pub const CLEAR_TEMPO: f64 = 0.5;
/// Below this confidence there is nothing worth guessing.
const MIN_CONFIDENCE: f64 = 0.2;

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
    estimate_grid(times)
        .filter(|e| e.confidence > CLEAR_TEMPO)
        .map(|e| e.bpm)
}

/// Like [`estimate_tempo`], but also says where the beat falls, and gives a
/// weak guess where [`estimate_tempo`] would find nothing (its
/// [`confidence`](TempoEstimate::confidence) says which). `None` only if
/// there are too few onsets or they agree with no tempo at all.
pub fn estimate_grid(times: &[f64]) -> Option<TempoEstimate> {
    if times.len() < 4 {
        return None;
    }
    // Onsets on the beat and on the half beat both count: the second sum
    // is of the onsets' phases on a grid of eighth notes.
    let sums = |bpm: f64, grid: f64| {
        let (mut re, mut im) = (0.0, 0.0);
        for &t in times {
            let phase = std::f64::consts::TAU * t * bpm / 60.0 * grid;
            re += phase.cos();
            im += phase.sin();
        }
        (re, im)
    };
    let score = |bpm: f64| {
        [1.0, 2.0]
            .into_iter()
            .map(|grid| {
                let (re, im) = sums(bpm, grid);
                re.hypot(im)
            })
            .sum::<f64>()
            / 2.0
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
    let confidence = refined.1 / times.len() as f64;
    if confidence < MIN_CONFIDENCE {
        return None;
    }
    // The onsets' average position in the beat. A beat is more often hit
    // on its own than on its half, so the first sum decides.
    let (re, im) = sums(refined.0, 1.0);
    let phase = im.atan2(re) / std::f64::consts::TAU;
    Some(TempoEstimate {
        bpm: (refined.0 * 100.0).round() / 100.0,
        confidence,
        phase: phase - (phase + 0.5).floor(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

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
    fn the_grid_phase_says_where_the_beat_falls() {
        // Beats of 120 bpm starting 0.1 s in: a fifth of a beat after 0.
        let times: Vec<f64> = (0..16).map(|i| 0.1 + i as f64 * 0.5).collect();
        let found = estimate_grid(&times).unwrap();
        assert!((found.bpm - 120.0).abs() < 0.3, "{found:?}");
        assert!(found.confidence > CLEAR_TEMPO);
        assert!((found.phase - 0.2).abs() < 0.02, "{found:?}");
        // Starting just before a beat is a small negative phase.
        let early: Vec<f64> = (0..16).map(|i| 0.45 + i as f64 * 0.5).collect();
        let found = estimate_grid(&early).unwrap();
        assert!((found.phase + 0.1).abs() < 0.02, "{found:?}");
    }

    #[test]
    fn loose_onsets_still_give_a_guess_that_says_it_is_one() {
        // A steady 100 bpm with every onset up to 0.12 s off.
        let mut seed = 11u32;
        let times: Vec<f64> = (0..24)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let jitter = ((seed >> 16) as f64 / 65_536.0 - 0.5) * 0.24;
                0.3 + i as f64 * 0.6 + jitter
            })
            .collect();
        let guess = estimate_grid(&times).expect("a guess");
        assert!((guess.bpm - 100.0).abs() < 2.0, "{guess:?}");
        // Too few onsets, or nothing agreeing, is still no tempo.
        assert_eq!(estimate_grid(&[0.1, 0.4, 0.9]), None);
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
