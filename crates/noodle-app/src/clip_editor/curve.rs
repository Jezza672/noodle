//! The waveform as the samples themselves, once zoomed in far enough that a
//! pixel column covers less than a frame: a line through the samples, and a
//! dot on each at the deepest zoom. The peaks only go down to a block of
//! frames, so the editor reads the visible stretch of the file itself.

use std::path::{Path, PathBuf};

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke};
use noodle_io::Decoder;

/// Pixels per frame from which the curve replaces the peak columns.
pub(super) const CURVE_AT: f32 = 1.5;
/// Pixels per frame from which each sample gets a dot.
const DOTS_AT: f32 = 4.0;

/// A stretch of a file, mixed down to mono, starting at frame `start`.
pub(super) struct Samples {
    path: PathBuf,
    start: u64,
    mono: Vec<f32>,
}

impl Samples {
    fn covers(&self, path: &Path, from: u64, to: u64) -> bool {
        self.path == path && self.start <= from && to <= self.start + self.mono.len() as u64
    }

    /// The sample at `frame`, if the stretch has it.
    fn at(&self, frame: u64) -> Option<f32> {
        let i = usize::try_from(frame.checked_sub(self.start)?).ok()?;
        self.mono.get(i).copied()
    }
}

/// The cached stretch, replaced by one around `from..to` (frames) of `path`
/// when it doesn't cover them. The new one reaches a view's width either side,
/// so panning doesn't read again every frame.
pub(super) fn window<'a>(
    cache: &'a mut Option<Samples>,
    path: &Path,
    from: u64,
    to: u64,
) -> Option<&'a Samples> {
    if !cache.as_ref().is_some_and(|c| c.covers(path, from, to)) {
        let margin = to.saturating_sub(from);
        *cache = read(path, from.saturating_sub(margin), to.saturating_add(margin));
    }
    cache.as_ref().filter(|c| c.covers(path, from, to))
}

fn read(path: &Path, from: u64, to: u64) -> Option<Samples> {
    let mut decoder = Decoder::open(path).ok()?;
    let channels = decoder.info().channels.max(1);
    let mut at = decoder.seek(from).ok()?;
    let mut mono = Vec::new();
    let mut start = from;
    let mut chunk = Vec::new();
    while at < to {
        let Some(frames) = decoder.read_chunk(&mut chunk).ok()? else {
            break;
        };
        for (i, frame) in chunk.chunks_exact(channels).take(frames).enumerate() {
            let here = at + i as u64;
            // A compressed seek lands early: skip up to where the view starts.
            if here < from {
                continue;
            }
            if here >= to {
                break;
            }
            if mono.is_empty() {
                start = here;
            }
            mono.push(frame.iter().sum::<f32>() / channels as f32);
        }
        at += frames as u64;
    }
    Some(Samples {
        path: path.to_owned(),
        start,
        mono,
    })
}

/// Draws the frames `from..to` of `samples` along `wave`, frame `f` at
/// `x_of(f)`.
pub(super) fn draw(
    painter: &Painter,
    wave: Rect,
    samples: &Samples,
    (from, to): (u64, u64),
    gain: f32,
    ink: Color32,
    x_of: impl Fn(f64) -> f32,
) {
    let middle = wave.center().y;
    let half = wave.height() / 2.0 - 2.0;
    let dots = to > from && (x_of((from + 1) as f64) - x_of(from as f64)).abs() >= DOTS_AT;
    // One frame past each end, so the line runs off the edge of the view.
    let points: Vec<Pos2> = (from.saturating_sub(1)..=to + 1)
        .filter_map(|f| {
            let s = (samples.at(f)? * gain).clamp(-1.0, 1.0);
            Some(Pos2::new(x_of(f as f64), middle - s * half))
        })
        .collect();
    let line = Stroke::new(1.5, ink);
    painter.add(Shape::line(points.clone(), line));
    if dots {
        for p in points.iter().filter(|p| wave.x_range().contains(p.x)) {
            painter.circle_filled(*p, 2.5, ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_samples_of_a_stretch_and_keeps_them_while_it_covers_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ramp.wav");
        let ramp: Vec<f32> = (0..20_000).map(|i| i as f32 / 20_000.0).collect();
        noodle_io::write_wav(&path, &ramp, 1, 48_000).unwrap();
        let mut cache = None;
        let samples = window(&mut cache, &path, 5_000, 5_100).unwrap();
        assert!((samples.at(5_050).unwrap() - 5_050.0 / 20_000.0).abs() < 1e-3);
        // Past the end of what was read there is nothing to draw.
        assert!(samples.at(19_999 + 10).is_none());
        let start = cache.as_ref().unwrap().start;
        window(&mut cache, &path, 5_010, 5_090).unwrap();
        assert_eq!(cache.as_ref().unwrap().start, start);
    }
}
