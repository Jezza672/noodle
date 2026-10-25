//! The perfect-stem ceiling (ROADMAP M10, batch 1): what Stem warp could
//! score if the separator were flawless, before any model exists.
//!
//! A generated song knows which instrument played what, so its stems can be
//! rendered exactly: [`stem_of`] maps each part to one of the four stems an
//! HT-Demucs model gives (drums, bass, vocals, other), and the song is
//! rendered with only that stem's parts playing, through the same master
//! chain as the mix. What the stems do not add up to (a limiter, a reverb
//! tail shared between parts) is the residual. The generated songs have no
//! vocals, so the vocals stem is always empty here.
//!
//! Two things stand in for a real separator's flaws: [`leak`], which bleeds a
//! fraction of every other stem into each one, and [`hpss`], the non-AI
//! baseline, which splits the mix by median filtering into harmonic and
//! percussive parts and needs no model at all.

use crate::stft::{FFT_SIZE, Frames, HOP, Spectrogram};
use crate::transient::medians;
use noodle_core::WarpMode;

/// One of the parts a separator splits a clip into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stem {
    Drums,
    Bass,
    Vocals,
    Other,
}

impl Stem {
    pub const ALL: [Stem; 4] = [Stem::Drums, Stem::Bass, Stem::Vocals, Stem::Other];

    pub fn name(self) -> &'static str {
        match self {
            Self::Drums => "drums",
            Self::Bass => "bass",
            Self::Vocals => "vocals",
            Self::Other => "other",
        }
    }

    /// The warp mode Stem warp gives this stem: Beats for the drums, which
    /// are all transient, and Grain for the rest.
    pub fn mode(self) -> WarpMode {
        match self {
            Self::Drums => WarpMode::Beats,
            _ => WarpMode::Grain,
        }
    }
}

/// The stem a song part belongs to. The generated songs' parts are single
/// instruments; the drums arrive as one `Drums` rack in the committed songs
/// and as separate drum voices if a song is written that way. There are no
/// vocals, so the sung lines (`Lead`) fall to `Other`, as a model that finds
/// no voice in them would put them.
pub fn stem_of(part: &str) -> Stem {
    match part {
        "Drums" | "Kick" | "Snare" | "Clap" | "Hat" | "Open Hat" | "Tom" | "Perc" => Stem::Drums,
        "Bass" | "Sub" => Stem::Bass,
        _ => Stem::Other,
    }
}

/// Spreads a fraction `amount` of each stem evenly over the other stems.
/// Every stem keeps `1 - amount` of itself, so the stems still add up to the
/// same mix: only where the sound sits is wrong, which is what a real
/// separator's leakage does. With one stem there is nothing to leak into.
pub fn leak(stems: &[Vec<f32>], amount: f32) -> Vec<Vec<f32>> {
    let n = stems.len();
    if n < 2 {
        return stems.to_vec();
    }
    let share = amount / (n - 1) as f32;
    (0..n)
        .map(|i| {
            let mut out: Vec<f32> = stems[i].iter().map(|x| x * (1.0 - amount)).collect();
            for (j, other) in stems.iter().enumerate().filter(|(j, _)| *j != i) {
                debug_assert_eq!(other.len(), out.len(), "stem {j}");
                for (o, x) in out.iter_mut().zip(other) {
                    *o += x * share;
                }
            }
            out
        })
        .collect()
}

/// Amplitude fraction for a leak given in decibels below the stem.
pub fn leak_amount(db: f32) -> f32 {
    10f32.powf(-db / 20.0)
}

/// Splits interleaved stereo into its harmonic and percussive parts by
/// Fitzgerald's median-filter separation with soft masks, channel by channel,
/// masks from the channel's own spectrogram. The two parts add up to the
/// input.
pub fn hpss(stereo: &[f32], rate: u32) -> (Vec<f32>, Vec<f32>) {
    let frames = stereo.len() / 2;
    let mut harmonic = vec![0.0f32; stereo.len()];
    let mut percussive = vec![0.0f32; stereo.len()];
    for channel in 0..2 {
        let x: Vec<f32> = stereo.iter().skip(channel).step_by(2).copied().collect();
        let spectrum = Frames::analyse(&x, rate, FFT_SIZE, HOP);
        let mags: Spectrogram = spectrum.magnitudes();
        let (h, p) = medians(&mags);
        let (mut mask_h, mut mask_p) = (vec![0.0f32; h.len()], vec![0.0f32; h.len()]);
        for i in 0..h.len() {
            let (hh, pp) = (h[i] * h[i], p[i] * p[i]);
            let total = hh + pp;
            if total > 0.0 {
                mask_h[i] = hh / total;
                mask_p[i] = pp / total;
            } else {
                mask_h[i] = 0.5;
                mask_p[i] = 0.5;
            }
        }
        let h_out = spectrum.masked(&mask_h).synthesise(frames, FFT_SIZE);
        let p_out = spectrum.masked(&mask_p).synthesise(frames, FFT_SIZE);
        for n in 0..frames {
            harmonic[2 * n + channel] = h_out[n];
            percussive[2 * n + channel] = p_out[n];
        }
    }
    (harmonic, percussive)
}

/// Sample-by-sample difference `a - b`.
pub fn minus(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

/// Sum of equally long signals.
pub fn sum(signals: &[Vec<f32>]) -> Vec<f32> {
    let mut out = vec![0.0f32; signals.first().map_or(0, Vec::len)];
    for s in signals {
        for (o, x) in out.iter_mut().zip(s) {
            *o += x;
        }
    }
    out
}

pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

pub fn rms_db(x: &[f32]) -> f64 {
    let e = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64;
    10.0 * e.max(1e-20).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal() -> Vec<f32> {
        (0..24_000)
            .flat_map(|n| {
                let t = n as f32 / 48_000.0;
                let tone = (t * 440.0 * std::f32::consts::TAU).sin() * 0.3;
                let click = if n % 6000 < 8 { 0.8 } else { 0.0 };
                [tone + click, tone - click]
            })
            .collect()
    }

    #[test]
    fn drum_voices_bass_and_the_rest_map_to_their_stems() {
        assert_eq!(stem_of("Drums"), Stem::Drums);
        assert_eq!(stem_of("Kick"), Stem::Drums);
        assert_eq!(stem_of("Sub"), Stem::Bass);
        assert_eq!(stem_of("Pad"), Stem::Other);
        assert_eq!(stem_of("Lead"), Stem::Other);
    }

    #[test]
    fn leakage_keeps_the_sum_and_moves_sound_between_stems() {
        let a = vec![1.0, 0.0, 1.0, 0.0];
        let b = vec![0.0, 1.0, 0.0, 1.0];
        let c = vec![0.0; 4];
        let out = leak(&[a.clone(), b.clone(), c.clone()], 0.2);
        let before = sum(&[a.clone(), b.clone(), c.clone()]);
        let after = sum(&out);
        for (x, y) in before.iter().zip(&after) {
            assert!((x - y).abs() < 1e-6);
        }
        // The empty stem now holds a tenth of each of the others, a lost
        // fifth of the first stem is gone from it.
        assert!((out[2][0] - 0.1).abs() < 1e-6 && (out[2][1] - 0.1).abs() < 1e-6);
        assert!((out[0][0] - 0.8).abs() < 1e-6);
        assert_eq!(leak(std::slice::from_ref(&a), 0.5), vec![a]);
        assert!((leak_amount(20.0) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn harmonic_and_percussive_parts_add_back_up_and_split_tone_from_click() {
        let x = signal();
        let (h, p) = hpss(&x, 48_000);
        let back = sum(&[h.clone(), p.clone()]);
        let err = minus(&back, &x);
        assert!(peak(&err) < 1e-3, "residual {}", peak(&err));
        // The tone is mostly in the harmonic part, the clicks in the other.
        assert!(
            rms_db(&h) > rms_db(&p) + 3.0,
            "{} {}",
            rms_db(&h),
            rms_db(&p)
        );
        let click_at = 6000 * 2;
        let window = click_at - 400..click_at + 400;
        let local = |s: &[f32]| rms_db(&s[window.clone()]);
        let tone_only = 3000 * 2 - 400..3000 * 2 + 400;
        let quiet = rms_db(&p[tone_only]);
        assert!(local(&p) > quiet + 6.0, "{} vs {quiet}", local(&p));
    }
}
