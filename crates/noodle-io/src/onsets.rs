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

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

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
