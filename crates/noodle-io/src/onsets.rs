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

/// The frames at which a transient starts in the treble of `mono` (above about
/// 1.5 kHz): bells and plucks over a dark pad, which the full-band level
/// can't see through.
pub fn detect_treble_onsets(mono: &[f32], rate: u32) -> Vec<u64> {
    let a = 1.0 - (-std::f32::consts::TAU * 1_500.0 / rate as f32).exp();
    let (mut p, mut q) = (0.0f32, 0.0f32);
    let treble: Vec<f32> = mono
        .iter()
        .map(|x| {
            p += a * (x - p);
            q += a * (p - q);
            x - q
        })
        .collect();
    detect_onsets(&treble, rate)
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
    // How much each block rises over the average power of the 30 ms before
    // it. The quietest of a couple of short blocks would do for noise, but a
    // low held tone is about one cycle per block and its block power swings
    // from one to the next, so a held chord looked like an attack every few
    // blocks; the average of several blocks is steady for a held sound and
    // still far below a real attack.
    const BEFORE: usize = 6;
    let power: Vec<f32> = level.iter().map(|db| 10f32.powf(db / 10.0)).collect();
    let rise: Vec<f32> = (0..blocks)
        .map(|b| {
            if b == 0 {
                return 0.0;
            }
            let before = &power[b.saturating_sub(BEFORE)..b];
            let mean = before.iter().sum::<f32>() / before.len() as f32;
            level[b] - 10.0 * mean.max(1e-12).log10()
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
    /// How far the best tempo's score stands above the middle of the scores
    /// of all tempos tried (the best over their median). Onsets that agree
    /// with no tempo score about 2; a beat under a lot of off-beat hits can
    /// have a low `confidence` and still stand out here.
    pub prominence: f64,
    /// How many onsets it was found from.
    pub onsets: usize,
    /// Where the beat falls against the onsets, in beats from the start of
    /// the audio, in `[-0.5, 0.5)`: the onsets sit near the whole beats of a
    /// grid slid this far (`n + phase`).
    pub phase: f64,
}

/// The confidence from which a tempo is clear and not a guess.
pub const CLEAR_TEMPO: f64 = 0.5;
/// The prominence from which a tempo is clear whatever its confidence, given
/// at least [`PROMINENT_ONSETS`] onsets.
pub const CLEAR_PROMINENCE: f64 = 3.5;
/// How many onsets a tempo needs to be clear by prominence alone.
pub const PROMINENT_ONSETS: usize = 8;

impl TempoEstimate {
    /// Whether this is a clear tempo and not a guess: onsets mostly on the
    /// beat, or a beat that stands out from every other tempo.
    pub fn is_clear(&self) -> bool {
        self.confidence > CLEAR_TEMPO
            || (self.prominence >= CLEAR_PROMINENCE && self.onsets >= PROMINENT_ONSETS)
    }
}
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
        .filter(TempoEstimate::is_clear)
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
    let mut scores = Vec::with_capacity(801);
    let mut bpm = 80.0;
    while bpm <= 160.0 {
        let s = score(bpm);
        scores.push(s);
        if s > best.1 {
            best = (bpm, s);
        }
        bpm += 0.1;
    }
    scores.sort_by(f64::total_cmp);
    let median = scores[scores.len() / 2];
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
    // Where the beat falls, and the tempo refined on the onsets that sit on
    // it. The phase is the most common position, not the average: a
    // syncopated kick or an off-beat bass line would pull an average off the
    // beat.
    let (bpm, phase) = fit_beat(times, refined.0);
    Some(TempoEstimate {
        bpm: (bpm * 100.0).round() / 100.0,
        confidence,
        prominence: if median > 0.0 {
            refined.1 / median
        } else {
            0.0
        },
        onsets: times.len(),
        phase: phase - (phase + 0.5).floor(),
    })
}

/// How close to a whole beat, in beats, an onset must be to count as on it.
const ON_BEAT: f64 = 0.06;

/// The beat's phase (in `[-0.5, 0.5)`) and a tempo near `bpm` fitted to the
/// onsets on it: the phase is the slide of the grid that puts the most
/// onsets within [`ON_BEAT`] of a whole beat, and the tempo is a straight
/// line fitted through the times of those onsets against their beat number,
/// which off-beat hits can't bias.
fn fit_beat(times: &[f64], bpm: f64) -> (f64, f64) {
    let wrap = |x: f64| x - (x + 0.5).floor();
    let mut bpm = bpm;
    let mut phase = 0.0;
    for _ in 0..3 {
        let beats: Vec<f64> = times.iter().map(|t| t * bpm / 60.0).collect();
        // The most onsets within a window of a slide, on a fine grid. Where
        // a bass line sits on the off-beat as often as on the beat (funk),
        // the two slides are close to a tie and the beat can come out half a
        // beat off: onsets alone can't say which is the downbeat.
        let best = (0..100)
            .map(|k| {
                let slide = k as f64 / 100.0 - 0.5;
                let n = beats
                    .iter()
                    .filter(|b| wrap(**b - slide).abs() < ON_BEAT)
                    .count();
                (slide, n)
            })
            .max_by_key(|c| c.1)
            .unwrap_or((0.0, 0));
        let near: Vec<f64> = beats
            .iter()
            .map(|b| wrap(b - best.0))
            .filter(|d| d.abs() < ON_BEAT)
            .collect();
        phase = wrap(best.0 + near.iter().sum::<f64>() / near.len().max(1) as f64);
        // Fit the on-beat onsets: time = start + number * period.
        let on: Vec<(f64, f64)> = times
            .iter()
            .zip(&beats)
            .filter(|(_, b)| wrap(**b - phase).abs() < ON_BEAT)
            .map(|(t, b)| ((*b - phase).round(), *t))
            .collect();
        if on.len() < 4 {
            break;
        }
        let n = on.len() as f64;
        let (sx, sy) = on.iter().fold((0.0, 0.0), |a, (x, y)| (a.0 + x, a.1 + y));
        let (sxx, sxy) = on
            .iter()
            .fold((0.0, 0.0), |a, (x, y)| (a.0 + x * x, a.1 + x * y));
        let spread = n * sxx - sx * sx;
        if spread <= 0.0 {
            break;
        }
        let period = (n * sxy - sx * sy) / spread;
        let fitted = 60.0 / period;
        // Only a small correction to what the sums found.
        if (80.0..=160.0).contains(&fitted) && (fitted - bpm).abs() < 0.02 * bpm {
            bpm = fitted;
        } else {
            break;
        }
    }
    (bpm, phase)
}

/// Seconds between the frames of [`harmony_change`].
const HARMONY_HOP: f64 = 0.05;
/// Seconds each side of a frame's boundary that [`harmony_change`] compares.
const HARMONY_WINDOW: f64 = 0.2;

/// The result of [`harmony_change`]: the frames and where they really fall,
/// since the hop and window are whole samples of the reduced rate and so
/// differ a little from their nominal seconds (and the error adds up over a
/// long file).
struct Harmony {
    change: Vec<f32>,
    /// Seconds between frames.
    hop: f64,
    /// Seconds from a frame's start to the boundary of its two windows.
    window: f64,
}

/// How much the harmony changes at each `HARMONY_HOP` seconds: one minus the
/// cosine of the pitch-class profiles of the window before and the window after.
/// A chord or a bass note changes on a strong beat, which a pattern of hits
/// can't tell from the off-beat.
fn harmony_change(mono: &[f32], rate: u32) -> Harmony {
    // 4 kHz is plenty for notes up to B6 and keeps the work small.
    let step = (rate as usize / 4_000).max(1);
    let low_rate = rate as f32 / step as f32;
    let low: Vec<f32> = mono
        .chunks(step)
        .map(|c| c.iter().sum::<f32>() / c.len() as f32)
        .collect();
    let half = (HARMONY_WINDOW * f64::from(low_rate)) as usize;
    let hop = ((HARMONY_HOP * f64::from(low_rate)) as usize).max(1);
    // One resonator per note from C2 to B6.
    let notes: Vec<(f32, usize)> = (36..96)
        .map(|n| (440.0 * 2f32.powf((n as f32 - 69.0) / 12.0), n % 12))
        .collect();
    let profile = |from: usize| -> [f32; 12] {
        let mut chroma = [0.0f32; 12];
        let window = &low[from..(from + half).min(low.len())];
        for &(hz, class) in &notes {
            let w = std::f32::consts::TAU * hz / low_rate;
            let coeff = 2.0 * w.cos();
            let (mut s1, mut s2) = (0.0f32, 0.0f32);
            for (i, x) in window.iter().enumerate() {
                // A Hann window keeps a neighbouring note's leak down.
                let h = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / half as f32).cos();
                let s0 = x * h + coeff * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
            chroma[class] += power.max(0.0).sqrt();
        }
        let norm = chroma.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 1e-9 {
            for v in &mut chroma {
                *v /= norm;
            }
        }
        chroma
    };
    let frames = low.len().saturating_sub(2 * half) / hop;
    let change = (0..frames)
        .map(|f| {
            let a = profile(f * hop);
            let b = profile(f * hop + half);
            1.0 - a.iter().zip(&b).map(|(x, y)| x * y).sum::<f32>()
        })
        .collect();
    Harmony {
        change,
        hop: hop as f64 / f64::from(low_rate),
        window: half as f64 / f64::from(low_rate),
    }
}

/// How much more the harmony must change on the other grid for it to take
/// the beat.
const DOWNBEAT_MARGIN: f64 = 1.2;
/// The least change (a cosine distance) that counts as a change of chord.
const MIN_CHANGE: f64 = 0.05;

/// The beat's phase (as in [`TempoEstimate::phase`]), told apart from the
/// off-beat half a beat away by the harmony: a chord or a bass note changes
/// on a strong beat (the first of a bar most often), and onsets alone can't
/// say, where a syncopated bass line sits on the off-beat as often as on the
/// beat. For each of the two grids, and each of the four beats a bar can
/// start on, the mean change at those beats is taken; `phase` is kept unless
/// the other grid's best is clearly higher. Audio with no change of harmony
/// (clicks, one held chord) keeps `phase`.
fn downbeat_phase(harmony: &Harmony, bpm: f64, phase: f64) -> f64 {
    let change = &harmony.change;
    let beat = 60.0 / bpm;
    let bar_start_change = |phase: f64| {
        let (mut sum, mut count) = ([0.0f64; 4], [0usize; 4]);
        let mut t = phase * beat;
        for k in 0usize.. {
            // A frame is centred on the boundary of its two windows.
            let frame = ((t - harmony.window) / harmony.hop).round();
            if frame >= change.len() as f64 {
                break;
            }
            if frame >= 0.0 {
                sum[k % 4] += f64::from(change[frame as usize]);
                count[k % 4] += 1;
            }
            t += beat;
        }
        (0..4)
            .map(|i| sum[i] / count[i].max(1) as f64)
            .fold(0.0, f64::max)
    };
    let other = phase + if phase < 0.0 { 0.5 } else { -0.5 };
    let (here, there) = (bar_start_change(phase), bar_start_change(other));
    if there > MIN_CHANGE && there > here * DOWNBEAT_MARGIN {
        other
    } else {
        phase
    }
}

/// How much of a file is listened to for transients: a long recording is
/// analysed from its start only, to keep the work and memory bounded.
pub const ANALYSIS_SECONDS: f64 = 120.0;

/// What the clip editor learns from a file's audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    /// Where transients start, in seconds from the start of the file.
    pub onsets: Vec<f64>,
    /// The tempo they sit on the beat of, if there's a clear one.
    pub tempo: Option<f64>,
    /// The tempo and where the beat falls, even when it is only a guess (see
    /// [`TempoEstimate::confidence`]). `None` if there are too few
    /// transients, or none agree on a tempo.
    pub grid: Option<TempoEstimate>,
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
        let bass: Vec<f64> = detect_bass_onsets(mono, rate)
            .into_iter()
            .map(|frame| frame as f64 / f64::from(rate))
            .collect();
        // A bass line that changes note every few bars has a few onsets that
        // fit some tempo by chance: it only speaks for the beat if it hits
        // on at least a quarter of the beats.
        let bass_grid =
            estimate_grid(&bass).filter(|b| bass.len() as f64 >= 0.25 * seconds * b.bpm / 60.0);
        let treble: Vec<f64> = detect_treble_onsets(mono, rate)
            .into_iter()
            .map(|frame| frame as f64 / f64::from(rate))
            .collect();
        let treble_grid =
            estimate_grid(&treble).filter(|t| treble.len() as f64 >= 0.25 * seconds * t.bpm / 60.0);
        // The treble speaks only when the full band and the bass found no
        // clear tempo: bells over a dark pad, where those see nothing.
        let found = best_grid(estimate_grid(&onsets), bass_grid);
        let mut grid = found
            .filter(TempoEstimate::is_clear)
            .or(treble_grid.filter(TempoEstimate::is_clear))
            .or(found);
        // Whether the beat or the off-beat between two beats is the beat:
        // onsets can tie (funk), the harmony says.
        if let Some(g) = grid.as_mut() {
            g.phase = downbeat_phase(&harmony_change(mono, rate), g.bpm, g.phase);
        }
        Self {
            tempo: grid.filter(TempoEstimate::is_clear).map(|g| g.bpm),
            grid,
            percussive: seconds > 0.0 && onsets.len() as f64 / seconds >= 1.0,
            onsets,
        }
    }
}

/// The better of the tempos found from all onsets and from the bass onsets
/// alone. A full mix has hats, plucks and arpeggios between the beats that
/// bury the beat in the full-band score, while the kick and bass line keep
/// it; so a full-band tempo is used only when its onsets mostly sit on the
/// beat, then a clear bass one, then the rest in order of confidence.
fn best_grid(full: Option<TempoEstimate>, bass: Option<TempoEstimate>) -> Option<TempoEstimate> {
    match (full, bass) {
        (Some(f), _) if f.confidence > CLEAR_TEMPO => Some(f),
        (_, Some(b)) if b.is_clear() => Some(b),
        (Some(f), _) if f.is_clear() => Some(f),
        (Some(f), Some(b)) => Some(if b.confidence > f.confidence { b } else { f }),
        (f, b) => f.or(b),
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
    fn a_held_chord_is_not_a_run_of_attacks() {
        // Low notes a few cents apart under harmonics that beat: block power
        // swings, but nothing is struck after the first moment.
        let tau = std::f64::consts::TAU;
        let x: Vec<f32> = (0..6 * RATE as usize)
            .map(|i| {
                let t = i as f64 / f64::from(RATE);
                let envelope = (t / 0.02).min(1.0);
                let tone: f64 = [220.0, 220.0 * 1.0017, 261.63, 329.63]
                    .iter()
                    .map(|f| {
                        (1..=6)
                            .map(|h| (tau * f * f64::from(h) * t).sin() / f64::from(h))
                            .sum::<f64>()
                    })
                    .sum();
                (0.09 * envelope * tone) as f32
            })
            .collect();
        let found = detect_onsets(&x, RATE);
        assert!(found.len() <= 2, "{found:?}");
    }

    #[test]
    fn bells_over_a_dark_pad_give_a_tempo_through_the_treble() {
        let tau = std::f32::consts::TAU;
        let beat = RATE as usize / 2; // 120 bpm
        let mut x: Vec<f32> = (0..16 * beat)
            .map(|i| (tau * 110.0 * i as f32 / RATE as f32).sin() * 0.3)
            .collect();
        for n in 0..16 {
            for i in 0..RATE as usize / 4 {
                let t = i as f32 / RATE as f32;
                x[n * beat + i] += (tau * 3_000.0 * t).sin() * 0.15 * (-t * 14.0).exp();
            }
        }
        let analysis = Analysis::of(&x, RATE);
        let tempo = analysis.tempo.expect("a tempo");
        assert!((tempo - 120.0).abs() < 0.5, "{tempo}");
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
    fn the_phase_is_the_most_common_position_not_the_average() {
        // Beats of 120 bpm on the whole beat, with a third as many hits a
        // sixteenth early and another third on the half beat: an average of
        // the positions would sit well off the beat.
        let mut times: Vec<f64> = (0..24).map(|i| i as f64 * 0.5).collect();
        times.extend((0..8).map(|i| 0.5 * (i * 3) as f64 - 0.125));
        times.extend((0..8).map(|i| 0.5 * (i * 3 + 1) as f64 + 0.25));
        times.retain(|t| *t >= 0.0);
        times.sort_by(f64::total_cmp);
        let found = estimate_grid(&times).unwrap();
        assert!((found.bpm - 120.0).abs() < 0.2, "{found:?}");
        assert!(found.phase.abs() < 0.03, "{found:?}");
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
    fn a_kick_pattern_under_off_beat_hats_is_a_clear_tempo_in_the_bass() {
        // Kicks on every beat of 126 bpm, with 3 hat-like clicks off the beat
        // for each: the full-band onsets agree with the beat only a quarter of
        // the time, but the bass onsets are the kicks.
        let beat = 60.0 / 126.0;
        let mut at: Vec<f64> = (0..32).map(|i| 0.2 + i as f64 * beat).collect();
        let mut seed = 9u32;
        for i in 0..32 {
            for _ in 0..3 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                at.push(0.2 + i as f64 * beat + 0.1 + (seed >> 16) as f64 / 65_536.0 * 0.25);
            }
        }
        let mut mono =
            vec![0.0f32; (at.len() as f64 * 0.0 + 32.0 * beat * 48_000.0) as usize + 48_000];
        let mut put = |t: f64, hz: f32, decay: f32, gain: f32| {
            let start = (t * 48_000.0) as usize;
            for i in 0..4_800.min(mono.len() - start) {
                let x = i as f32 / 48_000.0;
                mono[start + i] +=
                    (x * hz * std::f32::consts::TAU).sin() * (-x * decay).exp() * gain;
            }
        };
        for (n, t) in at.iter().enumerate() {
            if n < 32 {
                put(*t, 60.0, 30.0, 0.8);
            } else {
                put(*t, 6000.0, 150.0, 0.5);
            }
        }
        let analysis = Analysis::of(&mono, 48_000);
        let tempo = analysis.tempo.expect("a clear tempo");
        assert!((tempo - 126.0).abs() < 0.5, "{tempo}");
    }

    /// A funk-like bar at `bpm`: a kick and a bass line on a sub-bass tone,
    /// with a stream of hats on the off-beats, for `bars` bars. `kick` and
    /// `hats` scale their levels.
    fn mix(bpm: f64, bars: usize, kick: f32, hats: f32) -> Vec<f32> {
        let beat = 60.0 / bpm;
        let tau = std::f32::consts::TAU;
        let mut out = vec![0.0f32; ((bars * 4) as f64 * beat * f64::from(RATE)) as usize];
        let mut put = |t: f64, f: &dyn Fn(f32) -> f32, len: f32| {
            let start = (t * f64::from(RATE)) as usize;
            for i in 0..((len * RATE as f32) as usize).min(out.len().saturating_sub(start)) {
                let x = i as f32 / RATE as f32;
                out[start + i] += f(x);
            }
        };
        let mut seed = 1u32;
        for bar in 0..bars {
            let origin = (bar * 4) as f64 * beat;
            // Kick on 1 and the "and" of 2; bass on 1, 3 and the "and" of 4.
            for step in [0.0, 1.5] {
                put(
                    origin + step * beat,
                    &|x| {
                        (tau * (45.0 + 80.0 * (-x * 40.0).exp()) * x).sin()
                            * (-x * 14.0).exp()
                            * kick
                    },
                    0.4,
                );
            }
            for (step, hz) in [(0.0, 41.2), (2.0, 49.0), (3.5, 55.0)] {
                put(
                    origin + step * beat,
                    &|x| {
                        let env = (x / 0.005).min(1.0) * (-x * 5.0).exp();
                        ((tau * hz * x).sin() + 0.4 * (tau * 2.0 * hz * x).sin()) * 0.5 * env
                    },
                    0.5,
                );
            }
            for eighth in 0..8 {
                if eighth % 2 == 1 || eighth == 2 {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let level = 0.15 + 0.1 * ((seed >> 20) as f32 / 4096.0);
                    let hit = seed;
                    put(
                        origin + eighth as f64 * 0.5 * beat,
                        &|x| {
                            // White noise from a hash of the sample's time.
                            let h = ((x * RATE as f32) as u32 ^ hit)
                                .wrapping_mul(2_654_435_761)
                                .rotate_left(13)
                                .wrapping_mul(1_664_525);
                            ((h >> 8) as f32 / (1 << 23) as f32 - 1.0)
                                * (-x * 90.0).exp()
                                * level
                                * hats
                        },
                        0.1,
                    );
                }
            }
        }
        out
    }

    /// An RBJ high-pass at `hz`, Q 0.7.
    fn low_cut(x: &mut [f32], hz: f32) {
        let w = std::f32::consts::TAU * hz / RATE as f32;
        let alpha = w.sin() / (2.0 * 0.7);
        let c = w.cos();
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = ((1.0 + c) / 2.0 / a0, -(1.0 + c) / a0, (1.0 + c) / 2.0 / a0);
        let (a1, a2) = (-2.0 * c / a0, (1.0 - alpha) / a0);
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for v in x.iter_mut() {
            let y = b0 * *v + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, *v, y1, y);
            *v = y;
        }
    }

    /// A bus compressor: peak follower (`attack_ms`, 150 ms release), 4:1
    /// over `threshold_db`.
    fn compress(x: &mut [f32], threshold_db: f32, attack_ms: f32) {
        let coef = |ms: f32| (-1.0 / (ms * 0.001 * RATE as f32)).exp();
        let (attack, release) = (coef(attack_ms), coef(150.0));
        let mut level = 0.0f32;
        for v in x.iter_mut() {
            let a = v.abs();
            let k = if a > level { attack } else { release };
            level = a + k * (level - a);
            let db = 20.0 * level.max(1e-9).log10();
            let over = (db - threshold_db).max(0.0);
            *v *= 10f32.powf(-over * 0.75 / 20.0);
        }
    }

    /// Mixes are low-cut and bus-compressed, and neither moves where a bass
    /// line's onsets fall: the tempo and the beat stay put (#156).
    #[test]
    fn the_tempo_survives_a_low_cut_and_bus_compression() {
        let bpm = 110.0;
        for (kick, hats) in [(1.0, 1.0), (0.7, 1.0), (1.0, 0.7), (1.4, 1.4)] {
            for (cut, threshold, attack) in [
                (false, None, 0.0),
                (true, None, 0.0),
                (false, Some(-24.0), 3.0),
                (true, Some(-24.0), 3.0),
                (true, Some(-30.0), 1.0),
            ] {
                let mut x = mix(bpm, 16, kick, hats);
                if cut {
                    low_cut(&mut x, 30.0);
                }
                if let Some(t) = threshold {
                    compress(&mut x, t, attack);
                }
                let label = format!("kick {kick} hats {hats} cut {cut} comp {threshold:?}");
                let tempo = Analysis::of(&x, RATE)
                    .tempo
                    .unwrap_or_else(|| panic!("{label}: no tempo"));
                assert!((tempo - bpm).abs() / bpm < 0.002, "{label}: {tempo}");
            }
        }
    }

    /// Clicks on the off-beats only, over a pad whose chord changes every
    /// bar from `start` seconds on.
    fn off_beat_clicks(bpm: f64, bars: usize, chords: bool, rate: u32) -> Vec<f32> {
        let beat = 60.0 / bpm;
        let seconds = (bars * 4) as f64 * beat;
        let mut out = vec![0.0f32; (seconds * f64::from(rate)) as usize];
        for k in 0..bars * 4 {
            let start = ((k as f64 + 0.5) * beat * f64::from(rate)) as usize;
            for i in 0..(rate as usize / 4).min(out.len() - start) {
                let x = i as f32 / rate as f32;
                out[start + i] += (x * 200.0 * std::f32::consts::TAU).sin() * (-x * 25.0).exp();
            }
        }
        if chords {
            let roots = [196.0f32, 261.63, 233.08, 146.83];
            for (i, x) in out.iter_mut().enumerate() {
                let t = i as f64 / f64::from(rate);
                let root = roots[(t / (4.0 * beat)) as usize % 4];
                let tau = std::f32::consts::TAU * i as f32 / rate as f32;
                *x += 0.1 * ((tau * root).sin() + (tau * root * 1.498).sin());
            }
        }
        out
    }

    /// Onsets only on the off-beat put the beat half a beat off; a chord that
    /// changes on the bar says where the beat really is.
    #[test]
    fn the_harmony_tells_the_beat_from_the_off_beat() {
        let without = Analysis::of(&off_beat_clicks(110.0, 16, false, RATE), RATE);
        assert!(
            (without.grid.unwrap().phase + 0.5).abs() < 0.05,
            "{:?}",
            without.grid
        );
        let with = Analysis::of(&off_beat_clicks(110.0, 16, true, RATE), RATE);
        let phase = with.grid.unwrap().phase;
        assert!(phase.abs() < 0.05, "{phase}");
    }

    /// The frames' time is their real spacing: at 44.1 kHz the hop is 200
    /// samples of 4009 Hz, not 0.05 s, and a nominal reading would put a
    /// change at 60 s some 0.14 s early.
    #[test]
    fn harmony_frames_are_timed_by_their_real_hop() {
        let rate = 44_100u32;
        let tau = std::f32::consts::TAU;
        let x: Vec<f32> = (0..rate as usize * 80)
            .map(|i| {
                let hz = if i < rate as usize * 60 { 220.0 } else { 330.0 };
                (tau * hz * i as f32 / rate as f32).sin()
            })
            .collect();
        let harmony = harmony_change(&x, rate);
        let peak = harmony
            .change
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        let at = peak as f64 * harmony.hop + harmony.window;
        assert!((at - 60.0).abs() < 0.06, "{at}");
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
