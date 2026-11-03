//! Best-effort key detection: a 12-class pitch profile of the whole clip,
//! matched against the Krumhansl-Schmuckler major and minor profiles.

use noodle_core::{Key, Mode};

const MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

/// Seconds of audio analysed, from the start (long clips are not read whole).
const WINDOW_SECONDS: f64 = 60.0;

/// The pitch-class energy of `mono` (C2 to B6, one resonator per note), in
/// windows across the clip.
fn profile(mono: &[f32], rate: u32) -> [f32; 12] {
    let step = (rate / 6000).max(1) as usize;
    let low_rate = rate as f32 / step as f32;
    let take = ((WINDOW_SECONDS * f64::from(rate)) as usize).min(mono.len());
    let low: Vec<f32> = mono[..take]
        .chunks(step)
        .map(|c| c.iter().sum::<f32>() / c.len() as f32)
        .collect();
    let window = (0.25 * low_rate) as usize;
    let mut chroma = [0.0f32; 12];
    if window == 0 || low.len() < window {
        return chroma;
    }
    let notes: Vec<(f32, usize)> = (36..96)
        .map(|n| (440.0 * 2f32.powf((n as f32 - 69.0) / 12.0), n % 12))
        .collect();
    let hann: Vec<f32> = (0..window)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / window as f32).cos())
        .collect();
    for chunk in low.chunks_exact(window) {
        let mut frame = [0.0f32; 12];
        for &(hz, class) in &notes {
            let coeff = 2.0 * (std::f32::consts::TAU * hz / low_rate).cos();
            let (mut s1, mut s2) = (0.0f32, 0.0f32);
            for (x, h) in chunk.iter().zip(&hann) {
                let s0 = x * h + coeff * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            frame[class] += (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt();
        }
        // Each window counts alike, so a loud passage doesn't swamp the rest.
        let norm = frame.iter().sum::<f32>();
        if norm > 1e-9 {
            for (c, f) in chroma.iter_mut().zip(frame) {
                *c += f / norm;
            }
        }
    }
    chroma
}

fn correlation(a: &[f32; 12], b: impl Fn(usize) -> f32) -> f32 {
    let mean_a = a.iter().sum::<f32>() / 12.0;
    let mean_b = (0..12).map(&b).sum::<f32>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for (i, &x) in a.iter().enumerate() {
        let (x, y) = (x - mean_a, b(i) - mean_b);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 0.0 || db <= 0.0 {
        return 0.0;
    }
    num / (da.sqrt() * db.sqrt())
}

/// The most likely key of `mono`, or `None` for silence or noise with no
/// clear tonal centre.
pub fn detect_key(mono: &[f32], rate: u32) -> Option<Key> {
    let chroma = profile(mono, rate);
    if chroma.iter().sum::<f32>() <= 0.0 {
        return None;
    }
    let mut best: Option<(f32, Key)> = None;
    for tonic in 0..12u8 {
        for (mode, table) in [(Mode::Major, &MAJOR), (Mode::Minor, &MINOR)] {
            let score = correlation(&chroma, |i| table[(i + 12 - usize::from(tonic)) % 12]);
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, Key::new(tonic, mode)));
            }
        }
    }
    best.filter(|(score, _)| *score > 0.3).map(|(_, key)| key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(notes: &[(u32, f32)], rate: u32, seconds: f32) -> Vec<f32> {
        // `notes` are MIDI note numbers with a weight; a slow arpeggio.
        let n = (rate as f32 * seconds) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let which = (t * 4.0) as usize % notes.len();
                let (note, w) = notes[which];
                let hz = 440.0 * 2f32.powf((note as f32 - 69.0) / 12.0);
                w * (std::f32::consts::TAU * hz * t).sin() * 0.3
            })
            .collect()
    }

    #[test]
    fn finds_c_major_and_a_minor() {
        let c = tone(
            &[
                (60, 2.0),
                (64, 1.0),
                (67, 1.5),
                (62, 0.5),
                (65, 0.5),
                (59, 0.4),
            ],
            24000,
            12.0,
        );
        assert_eq!(detect_key(&c, 24000), Some(Key::new(0, Mode::Major)));
        let a = tone(
            &[
                (57, 2.0),
                (60, 1.0),
                (64, 1.5),
                (59, 0.5),
                (62, 0.5),
                (56, 0.5),
            ],
            24000,
            12.0,
        );
        assert_eq!(detect_key(&a, 24000), Some(Key::new(9, Mode::Minor)));
    }

    #[test]
    fn silence_has_no_key() {
        assert_eq!(detect_key(&vec![0.0; 48000], 24000), None);
    }
}
