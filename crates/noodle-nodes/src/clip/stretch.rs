//! Pitch-keeping time stretching for warped clips: the Grain and Beats modes.
//!
//! A [`Stretcher`] turns a clip's [`WarpMap`] (for every output frame, where in
//! the source to read) into audio by waveform-similarity overlap-add (WSOLA):
//! overlapping Hann-windowed grains are taken from the source at the speed the
//! map asks for, each nudged a little so that it carries on the grain before
//! it, and added together. The pitch stays, because every grain is read at
//! natural speed.
//!
//! Beats mode cuts the clip at its transients first. Each transient starts a
//! *segment*; the first stretch of a segment (the attack) is copied from the
//! source at natural speed, so the hit isn't smeared, and the rest is stretched
//! to fill the room up to the next transient. Segments are joined with a short
//! crossfade that ends exactly on the transient. Grain mode is the same thing
//! with one segment and no attack.
//!
//! The stretcher does no I/O of its own: it reads the source through a
//! [`Source`], and is meant to run on a worker thread ahead of the playhead
//! (see `ahead.rs`). It allocates freely and is not real-time safe.

use std::f32::consts::{FRAC_PI_2, PI};
use std::sync::Arc;

use super::warp::WarpMap;

/// Where the stretcher reads the audio it stretches.
pub trait Source {
    fn channels(&self) -> usize;

    /// Fills `out` (whole frames, interleaved) with the source's frames from
    /// `start`. Frames before the first and after the last are silent.
    fn read(&mut self, start: i64, out: &mut [f32]);
}

/// How long a grain is, in seconds. A hop is half of it.
const GRAIN_SECONDS: f64 = 0.04;
/// How far a grain may be moved from where the map puts it, as a fraction of
/// the hop, to line up with the one before.
const SEARCH: f64 = 0.5;
/// The correlation search first looks at every this-many-th offset (and
/// every second sample), then refines around the best.
const COARSE_STEP: i64 = 4;
/// Hops of output rendered and thrown away after a seek, so the grains have
/// settled by the time the audio starts.
const PREROLL: i64 = 3;
/// A transient's attack, played at natural speed, in seconds.
const ATTACK_SECONDS: f64 = 0.025;
/// The crossfade into a segment, ending on its transient, in seconds.
const LEAD_SECONDS: f64 = 0.0015;
/// Transients closer than this (in output) to the last one, or to the end,
/// aren't cut at.
const MIN_SEGMENT_SECONDS: f64 = 0.04;

/// One stretch of the clip between two transients (or the whole clip).
#[derive(Clone, Debug, PartialEq)]
struct Segment {
    /// Where the segment's transient falls in the output and in the source.
    o0: i64,
    s0: f64,
    /// Where the segment ends in the output and in the source. Grains stop
    /// reading the source there, so the next transient isn't heard early.
    o1: i64,
    s1: i64,
    /// Output frames at natural speed after `o0`.
    attack: i64,
    /// The crossfade before `o0`; the segment is rendered from `o0 - lead`.
    lead: i64,
    /// How far the body is moved to join the attack to the map.
    join: f64,
    /// Whether another segment follows, whose lead overlaps the end of this.
    has_next: bool,
}

impl Segment {
    fn first(&self) -> i64 {
        self.o0 - self.lead
    }

    /// Where the output stops being final for this segment alone: the next
    /// segment's crossfade starts there.
    fn limit(&self, fade: i64) -> i64 {
        if self.has_next {
            self.o1 - fade
        } else {
            self.o1
        }
    }
}

pub struct Stretcher<S> {
    source: S,
    channels: usize,
    map: Arc<WarpMap>,
    segments: Vec<Segment>,
    total: i64,
    hop: i64,
    win: usize,
    radius: i64,
    fade: i64,
    hann: Vec<f32>,
    // Where the render is: the segment, the grain in it, where the last
    // grain was taken from, and the output summed so far from `base`.
    seg: usize,
    grain: i64,
    prev: Option<i64>,
    acc: Vec<f32>,
    base: i64,
    /// Finished frames (stereo, interleaved), `ready_at` being the first not
    /// yet handed out.
    ready: Vec<f32>,
    ready_at: usize,
    /// Frames before this are rendered and dropped (after a seek).
    skip_until: i64,
    // Scratch.
    region: Vec<f32>,
    mono: Vec<f32>,
    target: Vec<f32>,
    target_mono: Vec<f32>,
    frames: Vec<f32>,
}

impl<S: Source> Stretcher<S> {
    /// A stretcher for the section `map` describes, `total` output frames
    /// long. `onsets` are the source's transients, in source frames (Beats
    /// mode), or empty (Grain). The stretcher starts at output frame 0.
    pub fn new(source: S, map: Arc<WarpMap>, onsets: &[u64], total: u64, rate: f32) -> Self {
        let rate = f64::from(rate);
        let hop = ((GRAIN_SECONDS * rate / 2.0).round() as i64).max(8);
        let win = hop as usize * 2;
        let radius = ((hop as f64 * SEARCH).round() as i64).max(1);
        let fade = ((LEAD_SECONDS * rate).round() as i64).max(2);
        let channels = source.channels().max(1);
        let total = total as i64;
        let segments = plan(&map, onsets, total, rate, fade);
        let hann = (0..win)
            .map(|j| (PI * j as f32 / win as f32).sin().powi(2))
            .collect();
        let mut stretcher = Self {
            source,
            channels,
            map,
            segments,
            total,
            hop,
            win,
            radius,
            fade,
            hann,
            seg: 0,
            grain: 0,
            prev: None,
            acc: Vec::new(),
            base: 0,
            ready: Vec::new(),
            ready_at: 0,
            skip_until: 0,
            region: Vec::new(),
            mono: Vec::new(),
            target: Vec::new(),
            target_mono: Vec::new(),
            frames: Vec::new(),
        };
        stretcher.seek(0);
        stretcher
    }

    /// Moves to output frame `pos`. The grains in the few hops before it are
    /// rendered and dropped, so that what follows has settled.
    pub fn seek(&mut self, pos: u64) {
        let pos = (pos as i64).min(self.total);
        self.ready.clear();
        self.ready_at = 0;
        self.acc.clear();
        self.prev = None;
        self.skip_until = pos;
        if pos >= self.total {
            self.seg = self.segments.len();
            return;
        }
        self.seg = self.segments.iter().rposition(|s| s.o0 <= pos).unwrap_or(0);
        let first = self.segments[self.seg].first();
        self.grain = ((pos - first) / self.hop - PREROLL).max(0);
        self.base = first + self.grain * self.hop;
    }

    /// Fills `out` (stereo, interleaved) with the next frames and returns how
    /// many it wrote: fewer than asked for only at the end of the clip.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        let want = out.len() / 2;
        let mut done = 0;
        while done < want {
            let have = (self.ready.len() - self.ready_at) / 2;
            if have == 0 {
                if self.seg >= self.segments.len() {
                    break;
                }
                self.step();
                continue;
            }
            let n = have.min(want - done);
            out[done * 2..(done + n) * 2]
                .copy_from_slice(&self.ready[self.ready_at..self.ready_at + n * 2]);
            self.ready_at += n * 2;
            done += n;
            if self.ready_at == self.ready.len() {
                self.ready.clear();
                self.ready_at = 0;
            }
        }
        done
    }

    /// Where the source is read for the output frame `o` of `seg`.
    fn source_at(&self, seg: &Segment, o: i64) -> f64 {
        if seg.attack > 0 && o <= seg.o0 + seg.attack {
            return seg.s0 + (o - seg.o0) as f64;
        }
        let mapped = self.map.source_at(o.max(0) as f64);
        if seg.join == 0.0 {
            return mapped;
        }
        let body = (seg.o1 - seg.o0 - seg.attack).max(1) as f64;
        mapped + seg.join * ((seg.o1 - o) as f64 / body).clamp(0.0, 1.0)
    }

    /// The gain that fades the segment in and out at its ends.
    fn envelope(&self, seg: &Segment, o: i64) -> f32 {
        let mut gain = 1.0;
        if o < seg.o0 {
            let x = (o - seg.first()) as f32 + 0.5;
            gain *= (FRAC_PI_2 * x / seg.lead as f32).sin().powi(2);
        }
        if seg.has_next && o >= seg.o1 - self.fade {
            let x = (o - (seg.o1 - self.fade)) as f32 + 0.5;
            gain *= (FRAC_PI_2 * x / self.fade as f32).cos().powi(2);
        }
        if o >= seg.o1 { 0.0 } else { gain.max(0.0) }
    }

    /// Renders one grain and hands over the frames it finishes.
    fn step(&mut self) {
        let seg = self.segments[self.seg].clone();
        let a = seg.first() + self.grain * self.hop;
        if a >= seg.o1 {
            // On to the next segment, which starts where this one stopped
            // being final.
            self.seg += 1;
            self.grain = 0;
            self.prev = None;
            return;
        }
        let channels = self.channels;
        let win = self.win;
        let hop = self.hop as usize;
        let mut nominal = self.source_at(&seg, a).round() as i64;
        if a >= seg.o0 + seg.attack {
            // Slowed down, the grains would run past the end of their part of
            // the source before the output gets there; they repeat the last
            // stretch of it instead, and so keep sounding up to the cut.
            let latest = seg.s1 - win as i64;
            if latest >= seg.s0 as i64 {
                nominal = nominal.min(latest);
            }
        }
        // The first grain after a seek or at a segment's start has nothing to
        // line up with, and its window starts flat.
        let flat = self.prev.is_none();
        let start = if !flat && a >= seg.o0 + seg.attack {
            self.search(nominal)
        } else {
            nominal
        };
        self.frames.resize(win * channels, 0.0);
        self.source.read(start, &mut self.frames);
        if seg.has_next {
            let keep = (seg.s1 - start).clamp(0, win as i64) as usize;
            self.frames[keep * channels..].fill(0.0);
        }
        self.prev = Some(start);

        // Sum the windowed grain into the output.
        let needed = ((a + win as i64 - self.base) * 2) as usize;
        if self.acc.len() < needed {
            self.acc.resize(needed, 0.0);
        }
        for j in 0..win {
            let o = a + j as i64;
            let w = if flat && j < hop { 1.0 } else { self.hann[j] };
            let gain = w * self.envelope(&seg, o);
            if gain == 0.0 {
                continue;
            }
            let frame = &self.frames[j * channels..(j + 1) * channels];
            let (l, r) = if channels == 1 {
                (frame[0], frame[0])
            } else {
                (frame[0], frame[1])
            };
            let at = ((o - self.base) * 2) as usize;
            self.acc[at] += l * gain;
            self.acc[at + 1] += r * gain;
        }
        self.grain += 1;
        let limit = (a + self.hop).min(seg.limit(self.fade));
        self.finish(limit);
    }

    /// Hands over the summed output before `limit`.
    fn finish(&mut self, limit: i64) {
        if limit <= self.base {
            return;
        }
        let n = ((limit - self.base) as usize).min(self.acc.len() / 2);
        for f in 0..n {
            if self.base + f as i64 >= self.skip_until {
                self.ready.extend_from_slice(&self.acc[f * 2..f * 2 + 2]);
            }
        }
        self.acc.drain(..n * 2);
        self.base = limit;
    }

    /// The source frame the next grain, which the map puts at `nominal`,
    /// starts at: within the search radius, the one whose start best matches
    /// the source that follows the previous grain.
    fn search(&mut self, nominal: i64) -> i64 {
        let prev = self.prev.expect("only searched with a grain before");
        let (hop, radius, channels) = (self.hop, self.radius, self.channels);
        self.target.resize(hop as usize * channels, 0.0);
        self.source.read(prev + hop, &mut self.target);
        mix_down(&self.target, channels, &mut self.target_mono);
        let energy =
            |stride: usize| -> f32 { self.target_mono.iter().step_by(stride).map(|x| x * x).sum() };
        let (energy1, energy2) = (energy(1), energy(2));
        if energy1 < 1e-9 {
            return nominal;
        }
        let length = (2 * radius + self.win as i64) as usize;
        self.region.resize(length * channels, 0.0);
        self.source.read(nominal - radius, &mut self.region);
        mix_down(&self.region, channels, &mut self.mono);

        let hop = hop as usize;
        let score = |d: i64, stride: usize, target_energy: f32| -> f32 {
            let at = (d + radius) as usize;
            let candidate = &self.mono[at..at + hop];
            let (mut dot, mut power) = (0.0f32, 0.0f32);
            for i in (0..hop).step_by(stride) {
                dot += candidate[i] * self.target_mono[i];
                power += candidate[i] * candidate[i];
            }
            dot / (power * target_energy + 1e-12).sqrt()
        };
        // Offsets nearest the map's own choice are tried first, and only a
        // clearly better one replaces it.
        let mut best = (0, f32::MIN);
        let mut d = 0;
        loop {
            for d in if d == 0 { vec![0] } else { vec![d, -d] } {
                let s = score(d, 2, energy2);
                if s > best.1 + 1e-6 {
                    best = (d, s);
                }
            }
            d += COARSE_STEP;
            if d > radius {
                break;
            }
        }
        let around = best.0;
        let mut fine: Vec<i64> = (around - COARSE_STEP + 1..around + COARSE_STEP)
            .filter(|d| d.abs() <= radius && *d != around)
            .collect();
        fine.sort_by_key(|d| d.abs());
        let mut best = (around, score(around, 1, energy1));
        for d in fine {
            let s = score(d, 1, energy1);
            if s > best.1 + 1e-6 {
                best = (d, s);
            }
        }
        nominal + best.0
    }
}

fn mix_down(frames: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    out.extend(
        frames
            .chunks_exact(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32),
    );
}

/// Cuts the section at its transients: one segment each, the first starting
/// at the section's start. A transient that would leave a segment too short
/// isn't cut at.
fn plan(map: &WarpMap, onsets: &[u64], total: i64, rate: f64, fade: i64) -> Vec<Segment> {
    if total <= 0 {
        return Vec::new();
    }
    let shortest = (MIN_SEGMENT_SECONDS * rate) as i64;
    let mut anchors: Vec<(i64, f64)> = vec![(0, 0.0)];
    for &onset in onsets {
        let source = onset as f64;
        if source <= 0.0 || source >= map.source_len() {
            continue;
        }
        let at = map.out_at(source).round() as i64;
        if at - anchors[anchors.len() - 1].0 >= shortest && total - at >= shortest {
            anchors.push((at, source));
        }
    }
    (0..anchors.len())
        .map(|i| {
            let (o0, s0) = anchors[i];
            let (o1, s1) = anchors
                .get(i + 1)
                .map_or((total, map.source_len()), |a| (a.0, a.1));
            let cut = i > 0;
            let attack = if cut {
                ((ATTACK_SECONDS * rate) as i64).min((o1 - o0) / 2)
            } else {
                0
            };
            let join = if attack > 0 {
                (s0 + attack as f64) - map.source_at((o0 + attack) as f64)
            } else {
                0.0
            };
            Segment {
                o0,
                s0,
                o1,
                s1: s1 as i64,
                attack,
                lead: if cut { fade } else { 0 },
                join,
                has_next: i + 1 < anchors.len(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    /// Audio held in memory, interleaved.
    struct Memory {
        data: Vec<f32>,
        channels: usize,
    }

    impl Source for Memory {
        fn channels(&self) -> usize {
            self.channels
        }

        fn read(&mut self, start: i64, out: &mut [f32]) {
            let frames = (self.data.len() / self.channels) as i64;
            for (i, frame) in out.chunks_exact_mut(self.channels).enumerate() {
                let at = start + i as i64;
                if (0..frames).contains(&at) {
                    let at = at as usize * self.channels;
                    frame.copy_from_slice(&self.data[at..at + self.channels]);
                } else {
                    frame.fill(0.0);
                }
            }
        }
    }

    fn sine(hz: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (i as f32 / RATE * hz * std::f32::consts::TAU).sin() * 0.5)
            .collect()
    }

    /// Source `frames` long played over `out` frames at a steady speed.
    fn steady(frames: u64, out: u64) -> Arc<WarpMap> {
        Arc::new(WarpMap::from_knots(vec![
            (0.0, 0.0),
            (out as f64, frames as f64),
        ]))
    }

    fn render(data: Vec<f32>, channels: usize, map: Arc<WarpMap>, onsets: &[u64]) -> Vec<f32> {
        let total = map.out_len().round() as u64;
        let mut stretcher = Stretcher::new(Memory { data, channels }, map, onsets, total, RATE);
        let mut out = vec![0.0; total as usize * 2];
        assert_eq!(stretcher.read(&mut out), total as usize);
        out
    }

    fn crossings(left: &[f32]) -> usize {
        left.windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count()
    }

    fn rms(left: &[f32]) -> f32 {
        (left.iter().map(|x| x * x).sum::<f32>() / left.len() as f32).sqrt()
    }

    fn left(stereo: &[f32]) -> Vec<f32> {
        stereo.iter().step_by(2).copied().collect()
    }

    #[test]
    fn natural_speed_gives_the_source_back() {
        let data = sine(440.0, 48_000);
        let out = render(data.clone(), 1, steady(48_000, 48_000), &[]);
        // Up to the last grain, which can't reach past the end of the source.
        for (i, x) in data.iter().enumerate().take(48_000 - 2_000) {
            assert!((out[i * 2] - x).abs() < 1e-3, "frame {i}");
            assert_eq!(out[i * 2], out[i * 2 + 1]);
        }
    }

    #[test]
    fn slowing_down_keeps_the_pitch_and_the_level() {
        let data = sine(440.0, 48_000);
        let out = left(&render(data, 1, steady(48_000, 96_000), &[]));
        assert_eq!(out.len(), 96_000);
        // 440 Hz for 2 s, not 1 s.
        let c = crossings(&out) as i64;
        // The last few milliseconds are lost to the source running out under
        // the final grain.
        assert!((c - 880).abs() <= 12, "{c} cycles");
        let level = rms(&out[4_000..92_000]);
        assert!((level - 0.5 / 2f32.sqrt()).abs() < 0.02, "{level}");
    }

    #[test]
    fn speeding_up_keeps_the_pitch_and_the_level() {
        let data = sine(440.0, 96_000);
        let out = left(&render(data, 1, steady(96_000, 48_000), &[]));
        let c = crossings(&out) as i64;
        assert!((c - 440).abs() <= 3, "{c} cycles");
        let level = rms(&out[4_000..44_000]);
        assert!((level - 0.5 / 2f32.sqrt()).abs() < 0.02, "{level}");
    }

    #[test]
    fn the_speed_follows_the_map() {
        // First half at half speed, the second at double.
        let data = sine(300.0, 96_000);
        let map = Arc::new(WarpMap::from_knots(vec![
            (0.0, 0.0),
            (48_000.0, 24_000.0),
            (72_000.0, 72_000.0),
        ]));
        let out = left(&render(data, 1, map, &[]));
        let first = crossings(&out[..48_000]) as i64;
        assert!((first - 300).abs() <= 3, "{first}");
        let second = crossings(&out[48_000..]) as i64;
        // 300 Hz holds, for the half second this part lasts.
        assert!((second - 150).abs() <= 4, "{second}");
    }

    #[test]
    fn stereo_channels_keep_their_balance() {
        let tone = sine(440.0, 48_000);
        let data: Vec<f32> = tone.iter().flat_map(|x| [*x, *x * 0.25]).collect();
        let out = render(data, 2, steady(48_000, 72_000), &[]);
        let l = rms(&left(&out)[4_000..68_000]);
        let r = rms(&out.iter().skip(1).step_by(2).copied().collect::<Vec<_>>()[4_000..68_000]);
        assert!((r / l - 0.25).abs() < 0.01, "{}", r / l);
    }

    #[test]
    fn a_seek_lands_on_steady_audio() {
        let data = sine(440.0, 96_000);
        let map = steady(96_000, 192_000);
        let mut stretcher = Stretcher::new(Memory { data, channels: 1 }, map, &[], 192_000, RATE);
        stretcher.seek(100_000);
        let mut out = vec![0.0; 24_000];
        assert_eq!(stretcher.read(&mut out), 12_000);
        let l = left(&out);
        assert!((rms(&l) - 0.5 / 2f32.sqrt()).abs() < 0.02, "{}", rms(&l));
        // No jump bigger than the tone's own steepest step.
        let step = l
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(step < 0.07, "{step}");
        // And it ends where the clip does.
        stretcher.seek(191_000);
        assert_eq!(stretcher.read(&mut vec![0.0; 4_000]), 1_000);
    }

    #[test]
    fn a_seek_lands_on_the_right_part_of_the_clip() {
        // 440 Hz for the first 1.2 s of the source, then an octave up.
        let data: Vec<f32> = (0..96_000)
            .map(|i| {
                let phase = if i < 57_600 {
                    i as f32 * 440.0
                } else {
                    57_600.0 * 440.0 + (i - 57_600) as f32 * 880.0
                };
                (phase / RATE * std::f32::consts::TAU).sin() * 0.5
            })
            .collect();
        let map = steady(96_000, 192_000);
        let mut stretcher = Stretcher::new(Memory { data, channels: 1 }, map, &[], 192_000, RATE);
        // Output frame 70 000 is source frame 35 000: still 440 Hz. Output
        // frame 150 000 is source 75 000: 880 Hz.
        for (pos, hz) in [(150_000, 880), (70_000, 440), (150_000, 880)] {
            stretcher.seek(pos);
            let mut out = vec![0.0; 2 * 24_000];
            assert_eq!(stretcher.read(&mut out), 24_000);
            let c = crossings(&left(&out)) as i64;
            let want = 24_000 * hz / 48_000;
            assert!((c - want).abs() <= 3, "at {pos}: {c} cycles, wanted {want}");
        }
    }

    /// A noise burst with a sharp attack and a tail, at each of `at`.
    fn hits(at: &[usize], frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames];
        let mut seed = 12345u32;
        for &t in at {
            for i in 0..9_000.min(frames - t) {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f32 / (1 << 23) as f32 - 1.0;
                out[t + i] += noise * (-(i as f32) / 1_500.0).exp() * 0.8;
            }
        }
        out
    }

    #[test]
    fn beats_keeps_each_attack_whole_and_on_time() {
        let at = [0usize, 24_000, 48_000, 72_000];
        let data = hits(&at, 96_000);
        let onsets: Vec<u64> = at.iter().skip(1).map(|t| *t as u64).collect();
        // Twice as slow: each hit lands on twice its frame.
        let out = left(&render(data.clone(), 1, steady(96_000, 192_000), &onsets));
        for t in at.iter().skip(1) {
            // The attack is the source, frame for frame.
            for i in 0..1_500 {
                let (got, want) = (out[t * 2 + i], data[t + i]);
                assert!(
                    (got - want).abs() < 1e-4,
                    "hit {t}, frame {i}: {got} vs {want}"
                );
            }
            // Nothing from the hit before leaks past the cut.
            let before = &out[t * 2 - 600..t * 2 - 100];
            let tail = data[t - 300..t - 100]
                .iter()
                .fold(0.0, |m: f32, x| m.max(x.abs()));
            let heard = before.iter().fold(0.0, |m: f32, x| m.max(x.abs()));
            assert!(heard < tail * 1.1 + 0.02, "{heard} vs {tail}");
        }
        // Grain mode smears the same hit.
        let grain = left(&render(data.clone(), 1, steady(96_000, 192_000), &[]));
        let sharp = |out: &[f32]| {
            out[48_000..48_000 + 400]
                .iter()
                .fold(0.0f32, |m, x| m.max(x.abs()))
        };
        assert!(sharp(&out) > 0.7, "{}", sharp(&out));
        let smeared = grain[48_000..48_000 + 400]
            .iter()
            .fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(smeared < sharp(&out), "{smeared}");
    }

    #[test]
    fn beats_fills_the_gap_after_a_hit() {
        let at = [0usize, 24_000];
        let data = hits(&at, 48_000);
        let out = left(&render(data, 1, steady(48_000, 144_000), &[24_000]));
        // The tail of the first hit is stretched over the longer gap: it is
        // still sounding well after the source's tail would have died.
        let late = rms(&out[24_000..26_000]);
        assert!(late > 0.0005, "{late}");
    }

    #[test]
    fn transients_too_close_are_not_cut_at() {
        let map = steady(96_000, 96_000);
        let segments = plan(
            &map,
            &[10, 500, 24_000, 24_400, 95_500],
            96_000,
            48_000.0,
            72,
        );
        let cuts: Vec<i64> = segments.iter().map(|s| s.o0).collect();
        assert_eq!(cuts, vec![0, 24_000]);
        assert!(!segments[1].has_next);
    }
}
