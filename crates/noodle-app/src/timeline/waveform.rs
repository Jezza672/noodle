//! A clip's waveform, drawn from the file's peaks.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, Painter, Pos2, Rect, Stroke, vec2};
use noodle_core::{AudioClip, ClipId};
use noodle_io::{Peak, Peaks};

/// The strip along the top of a clip that its name goes in.
const LABEL: f32 = 15.0;
/// At most this many columns, however wide the clip is zoomed.
const MAX_COLUMNS: usize = 4096;

/// The columns last worked out for each clip, so they're only worked out
/// again when the zoom, the scroll, the clip or the file changes.
#[derive(Default)]
pub struct Cache {
    columns: HashMap<ClipId, (Key, Vec<Peak>)>,
    #[cfg(test)]
    computed: usize,
}

/// What a clip's columns were worked out from.
struct Key {
    /// The peaks, by identity: a reloaded file has new ones. Held, so their
    /// address can't be reused by the next ones while this is compared.
    peaks: Arc<Peaks>,
    start: u64,
    end: u64,
    columns: usize,
}

impl Key {
    fn matches(&self, other: &Key) -> bool {
        Arc::ptr_eq(&self.peaks, &other.peaks)
            && (self.start, self.end, self.columns) == (other.start, other.end, other.columns)
    }
}

impl Cache {
    fn columns(
        &mut self,
        id: ClipId,
        peaks: &Arc<Peaks>,
        start: u64,
        end: u64,
        n: usize,
    ) -> &[Peak] {
        let key = Key {
            peaks: peaks.clone(),
            start,
            end,
            columns: n,
        };
        let entry = self.columns.entry(id).or_insert_with(|| {
            (
                Key {
                    peaks: peaks.clone(),
                    start: 0,
                    end: 0,
                    columns: 0,
                },
                Vec::new(),
            )
        });
        if !entry.0.matches(&key) {
            *entry = (key, peaks.columns(None, start, end, n));
            #[cfg(test)]
            {
                self.computed += 1;
            }
        }
        &entry.1
    }

    /// Forgets clips that are gone.
    pub fn retain(&mut self, keep: impl Fn(ClipId) -> bool) {
        self.columns.retain(|&id, _| keep(id));
    }

    #[cfg(test)]
    pub fn computed(&self) -> usize {
        self.computed
    }
}

/// The waveform's colour on a clip of `clip_colour`.
fn ink(clip_colour: Color32) -> Color32 {
    clip_colour.gamma_multiply(0.45)
}

/// The clip a waveform belongs to.
#[derive(Clone, Copy)]
pub struct Clip<'a> {
    pub id: ClipId,
    pub peaks: &'a Arc<Peaks>,
    pub audio: &'a AudioClip,
    /// The clip's colour; the waveform is a darker shade of it.
    pub colour: Color32,
}

/// Draws the part of the clip that's in view, `visible` of `full`, one pixel
/// column per peak. Says whether it drew anything.
pub fn draw(
    painter: &Painter,
    cache: &mut Cache,
    full: Rect,
    visible: Rect,
    clip: &Clip<'_>,
) -> bool {
    let Clip {
        id,
        peaks,
        audio,
        colour: clip_colour,
    } = *clip;
    let area = Rect::from_min_max(full.min + vec2(0.0, LABEL), full.max);
    if area.height() < 4.0 || full.width() <= 0.0 {
        return false;
    }
    // The visible part of the clip as a part of its audio.
    let from = f64::from((visible.left() - full.left()) / full.width());
    let to = f64::from((visible.right() - full.left()) / full.width());
    let length = audio.length as f64;
    let start = audio.offset + (length * from).round() as u64;
    let end = audio.offset + (length * to).round() as u64;
    let columns = (visible.width().ceil() as usize).clamp(1, MAX_COLUMNS);
    let stroke = Stroke::new(visible.width() / columns as f32, ink(clip_colour));
    let middle = area.center().y;
    let half = area.height() / 2.0 - 1.0;
    for (i, &peak) in cache
        .columns(id, peaks, start, end, columns)
        .iter()
        .enumerate()
    {
        let Peak { min, max } = scaled(peak, audio.gain);
        let x = visible.left() + (i as f32 + 0.5) * visible.width() / columns as f32;
        // At least a hairline, so silence still shows the clip has audio.
        let top = middle - max * half - 0.5;
        let bottom = middle - min * half + 0.5;
        painter.line_segment([Pos2::new(x, top), Pos2::new(x, bottom)], stroke);
    }
    true
}

/// The peak at the clip's gain, kept on the screen.
fn scaled(peak: Peak, gain: f32) -> Peak {
    Peak {
        min: (peak.min * gain).clamp(-1.0, 1.0),
        max: (peak.max * gain).clamp(-1.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clips_gain_scales_a_peak_but_not_off_the_screen() {
        let p = scaled(
            Peak {
                min: -0.25,
                max: 0.5,
            },
            2.0,
        );
        assert_eq!((p.min, p.max), (-0.5, 1.0));
        let p = scaled(
            Peak {
                min: -0.9,
                max: 0.9,
            },
            4.0,
        );
        assert_eq!((p.min, p.max), (-1.0, 1.0));
    }

    #[test]
    fn a_reloaded_file_is_not_mistaken_for_the_old_one() {
        let audio = noodle_io::Audio {
            samples: vec![0.5; 4096],
            channels: 1,
            sample_rate: 48_000,
        };
        let (old, new) = (
            Arc::new(Peaks::from_audio(&audio)),
            Arc::new(Peaks::from_audio(&audio)),
        );
        let mut cache = Cache::default();
        cache.columns(ClipId(1), &old, 0, 4096, 8);
        cache.columns(ClipId(1), &old, 0, 4096, 8);
        assert_eq!(cache.computed(), 1);
        // Identical contents and range, but a different load.
        cache.columns(ClipId(1), &new, 0, 4096, 8);
        assert_eq!(cache.computed(), 2);
    }
}
