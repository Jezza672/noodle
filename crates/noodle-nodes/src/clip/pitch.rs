//! Moving a clip's pitch without moving its timing, where the amount can
//! change along the clip (a transposition that follows the timeline's key).
//!
//! The clip is stretched as slowly or as quickly as the pitch change needs
//! (a rise of `r` is rendered `r` times as long), and then read back at `r`
//! frames per output frame: the speed change cancels, the pitch stays moved.
//! A [`PitchPath`] says what `r` is at each output frame; [`Pitcher`] does
//! the second half over a [`Stretcher`].

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use super::formant::{BLOCK, FormantStage, LATENCY, WARMUP};
use super::stretch::{Source, Stretcher};
use super::warp::cubic;

/// How far apart, in semitones, two shifts must be to count as different.
const SAME: f64 = 1e-6;

/// The pitch ratio at each stretch of a clip, in output frames from the
/// clip's start: `(first frame, ratio)`, sorted, the first at 0. A ratio
/// holds until the next step.
#[derive(Clone, Debug, PartialEq)]
pub struct PitchPath {
    steps: Vec<(u64, f64)>,
    /// Whether the formants are carried back to where they were (see
    /// `formant`) after the pitch is moved.
    formants: bool,
}

impl PitchPath {
    /// A path through `shifts`, (first frame, semitones). `None` if it moves
    /// nothing anywhere.
    pub fn from_semitones(shifts: &[(u64, f64)]) -> Option<Self> {
        let mut steps: Vec<(u64, f64)> = Vec::new();
        for &(at, semitones) in shifts {
            let ratio = (semitones / 12.0).exp2();
            match steps.last() {
                Some(&(_, last)) if (last.log2() * 12.0 - semitones).abs() < SAME => {}
                _ => steps.push((at, ratio)),
            }
        }
        if steps.is_empty() || steps.iter().all(|s| (s.1 - 1.0).abs() < SAME) {
            return None;
        }
        steps[0].0 = 0;
        Some(Self {
            steps,
            formants: false,
        })
    }

    /// This path, with the formants kept in place or moved with the pitch.
    pub fn keeping_formants(mut self, keep: bool) -> Self {
        self.formants = keep;
        self
    }

    /// Whether the formants are kept in place.
    pub fn formants(&self) -> bool {
        self.formants
    }

    /// The pitch ratio at output frame `out`.
    pub fn ratio_at(&self, out: u64) -> f64 {
        let at = self.steps.partition_point(|s| s.0 <= out).saturating_sub(1);
        self.steps[at].1
    }

    pub fn steps(&self) -> &[(u64, f64)] {
        &self.steps
    }

    /// Where, in the stretched audio, output frame `out` is read.
    pub fn inter(&self, out: f64) -> f64 {
        let mut total = 0.0;
        for (i, &(at, ratio)) in self.steps.iter().enumerate() {
            let end = self.steps.get(i + 1).map_or(f64::INFINITY, |s| s.0 as f64);
            if out <= end {
                return total + (out - at as f64) * ratio;
            }
            total += (end - at as f64) * ratio;
        }
        total
    }

    /// This path for a clip that starts `head` frames earlier.
    pub fn shifted(&self, head: u64) -> Self {
        let mut steps: Vec<(u64, f64)> = self.steps.iter().map(|&(a, r)| (a + head, r)).collect();
        steps.insert(0, (0, self.steps[0].1));
        steps.dedup_by(|b, a| (a.1 - b.1).abs() < SAME);
        Self {
            steps,
            formants: self.formants,
        }
    }

    /// The part of this path from output frame `from` for `len` frames, from
    /// 0 again; `None` if it moves nothing there.
    pub fn slice(&self, from: u64, len: u64) -> Option<Self> {
        let mut shifts: Vec<(u64, f64)> = Vec::new();
        let at = self
            .steps
            .partition_point(|s| s.0 <= from)
            .saturating_sub(1);
        shifts.push((0, self.steps[at].1.log2() * 12.0));
        for &(start, ratio) in &self.steps[at + 1..] {
            if start >= from + len {
                break;
            }
            shifts.push((start - from, ratio.log2() * 12.0));
        }
        Self::from_semitones(&shifts).map(|p| p.keeping_formants(self.formants))
    }
}

impl Hash for PitchPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for (at, ratio) in &self.steps {
            at.hash(state);
            ratio.to_bits().hash(state);
        }
        self.formants.hash(state);
    }
}

/// A [`Stretcher`] whose output is read back at the pitch path's rate.
pub struct Pitcher<S> {
    stretcher: Stretcher<S>,
    path: Option<Arc<PitchPath>>,
    /// The next output frame.
    pos: u64,
    /// Stretched frames (stereo, interleaved) from `start`.
    buffer: Vec<f32>,
    start: i64,
    ended: bool,
    scratch: Vec<f32>,
    /// Puts the formants back after the shift, if the path asks for it.
    formants: Option<Formants>,
}

/// The formant stage and where it is in its stream.
struct Formants {
    stage: FormantStage,
    /// Output frames still to drop: the stage's delay, and after a seek the
    /// warm-up before the position asked for.
    skip: usize,
    /// Zeros still to feed after the audio ends, so its tail comes out.
    flush: usize,
    done: bool,
    block: Vec<f32>,
}

const CHUNK: usize = 2048;

impl<S: Source> Pitcher<S> {
    pub fn new(stretcher: Stretcher<S>, path: Option<Arc<PitchPath>>) -> Self {
        let formants = path.as_ref().filter(|p| p.formants()).map(|_| Formants {
            stage: FormantStage::new(),
            skip: LATENCY,
            flush: LATENCY,
            done: false,
            block: vec![0.0; BLOCK * 2],
        });
        Self {
            formants,
            stretcher,
            path,
            pos: 0,
            buffer: Vec::new(),
            start: 0,
            ended: false,
            scratch: vec![0.0; CHUNK * 2],
        }
    }

    /// Moves to output frame `pos`.
    pub fn seek(&mut self, pos: u64) {
        if self.formants.is_some() {
            // Start early enough for the stage's window and delay to settle,
            // and drop what comes out before `pos`.
            let from = pos.saturating_sub(WARMUP as u64);
            self.seek_raw(from);
            if let Some(f) = &mut self.formants {
                f.stage.reset();
                f.skip = (pos - from) as usize + LATENCY;
                f.flush = LATENCY;
                f.done = false;
            }
            return;
        }
        self.seek_raw(pos);
    }

    fn seek_raw(&mut self, pos: u64) {
        let Some(path) = &self.path else {
            self.stretcher.seek(pos);
            return;
        };
        let target = (path.inter(pos as f64).floor() as i64 - 2).max(0);
        self.stretcher.seek(target as u64);
        self.pos = pos;
        self.start = target;
        self.buffer.clear();
        self.ended = false;
    }

    /// Fills `out` (stereo, interleaved) with the next frames; returns how
    /// many it wrote.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        if self.formants.is_none() {
            return self.read_raw(out);
        }
        let want = out.len() / 2;
        let mut made = 0;
        while made < want {
            let n = (want - made).min(BLOCK);
            let mut block = std::mem::take(&mut self.formants.as_mut().unwrap().block);
            let start = self.pos;
            let got = if self.formants.as_ref().unwrap().done {
                0
            } else {
                self.read_raw(&mut block[..n * 2])
            };
            let f = self.formants.as_mut().unwrap();
            if got < n {
                f.done = true;
            }
            let pad = (n - got).min(f.flush);
            f.flush -= pad;
            block[got * 2..(got + pad) * 2].fill(0.0);
            let fed = got + pad;
            let path = self.path.clone().unwrap();
            f.stage.process(&mut block[..fed * 2], |t| {
                (1.0 / path.ratio_at(start + u64::from(t))) as f32
            });
            let drop = f.skip.min(fed);
            f.skip -= drop;
            let keep = fed - drop;
            out[made * 2..(made + keep) * 2].copy_from_slice(&block[drop * 2..fed * 2]);
            made += keep;
            f.block = block;
            if fed < n {
                break;
            }
        }
        made
    }

    fn read_raw(&mut self, out: &mut [f32]) -> usize {
        let Some(path) = self.path.clone() else {
            return self.stretcher.read(out);
        };
        let frames = out.len() / 2;
        for k in 0..frames {
            let m = path.inter((self.pos + k as u64) as f64);
            let base = m.floor() as i64;
            let need = base + 3 - self.start;
            while !self.ended && (self.buffer.len() / 2) < need.max(0) as usize {
                let made = self.stretcher.read(&mut self.scratch);
                self.buffer.extend_from_slice(&self.scratch[..made * 2]);
                if made < CHUNK {
                    self.ended = true;
                }
            }
            let t = (m - base as f64) as f32;
            let at = |i: i64| -> usize { (i - self.start).max(0) as usize };
            for c in 0..2 {
                let get = |i: i64| -> f32 {
                    self.buffer
                        .get(at(i) * 2 + c)
                        .copied()
                        .filter(|_| i >= self.start)
                        .unwrap_or(0.0)
                };
                out[k * 2 + c] = cubic(get(base - 1), get(base), get(base + 1), get(base + 2), t);
            }
        }
        self.pos += frames as u64;
        // Forget what is behind the reading.
        let keep = (path.inter(self.pos as f64).floor() as i64 - 2).max(self.start);
        let drop = (keep - self.start) as usize;
        if drop > 4096 {
            self.buffer.drain(..drop * 2);
            self.start = keep;
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_counts_stretched_frames() {
        let path = PitchPath::from_semitones(&[(0, 12.0), (100, 0.0)]).unwrap();
        assert!((path.inter(50.0) - 100.0).abs() < 1e-9);
        assert!((path.inter(100.0) - 200.0).abs() < 1e-9);
        assert!((path.inter(150.0) - 250.0).abs() < 1e-9);
    }

    #[test]
    fn no_shift_is_no_path() {
        assert!(PitchPath::from_semitones(&[(0, 0.0), (10, 0.0)]).is_none());
    }

    #[test]
    fn slices_start_again_from_zero() {
        let path = PitchPath::from_semitones(&[(0, 0.0), (100, 12.0)]).unwrap();
        let tail = path.slice(150, 100).unwrap();
        assert_eq!(tail.steps().len(), 1);
        assert!((tail.inter(10.0) - 20.0).abs() < 1e-9);
        assert!(path.slice(0, 100).is_none());
    }

    #[test]
    fn shifting_a_path_keeps_its_first_ratio_in_the_head() {
        let path = PitchPath::from_semitones(&[(0, 12.0)]).unwrap();
        let moved = path.shifted(50);
        assert_eq!(moved.steps().len(), 1);
    }
}
