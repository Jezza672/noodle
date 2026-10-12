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
use std::sync::{Arc, Mutex};

use super::warp::WarpMap;

/// Where the stretcher reads the audio it stretches.
pub trait Source {
    fn channels(&self) -> usize;

    /// Fills `out` (whole frames, interleaved) with the source's frames from
    /// `start`. Frames before the first and after the last are silent.
    fn read(&mut self, start: i64, out: &mut [f32]);
}

/// The crossover between the two bands, in Hz.
const SPLIT_HZ: f64 = 150.0;
/// Taps of the linear-phase filter that splits the bands (odd).
const SPLIT_TAPS: usize = 2047;
/// Frames of filtered source kept at a time.
const SPLIT_BLOCK: usize = 8192;
/// The mids and highs: a grain this long (a hop is half of it), moved at most
/// this far from where the map puts it, to line up with the one before.
const HIGH_GRAIN_SECONDS: f64 = 0.04;
const HIGH_SEARCH_SECONDS: f64 = 0.010;
/// The bass: a grain of several periods of the lowest notes, free to move
/// half a period of a 40 Hz note. Aligned on its own, so a bass line isn't
/// knocked out of phase by whatever the mids need.
const LOW_GRAIN_SECONDS: f64 = 0.1;
const LOW_SEARCH_SECONDS: f64 = 0.0125;
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

struct Band<S> {
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

/// Both bands of a clip, stretched on their own and added: the bass (below
/// [`SPLIT_HZ`]) with long grains that line up on the bass, everything else
/// with short ones. One set of grains can't do both: the shift that lines up
/// a vocal's period is a random phase jump for the bass under it, which
/// detunes and pumps the bass line.
pub struct Stretcher<S> {
    low: Band<LowSource<S>>,
    high: Band<HighSource<S>>,
    scratch: Vec<f32>,
}

impl<S: Source> Stretcher<S> {
    /// A stretcher for the section `map` describes, `total` output frames
    /// long. `onsets` are the source's transients, in source frames (Beats
    /// mode), or empty (Grain). The stretcher starts at output frame 0.
    pub fn new(source: S, map: Arc<WarpMap>, onsets: &[u64], total: u64, rate: f32) -> Self {
        let split = Arc::new(Mutex::new(Split::new(source, rate)));
        let low = Band::new(
            LowSource(split.clone()),
            map.clone(),
            onsets,
            total,
            rate,
            (LOW_GRAIN_SECONDS, LOW_SEARCH_SECONDS),
        );
        let high = Band::new(
            HighSource(split),
            map,
            onsets,
            total,
            rate,
            (HIGH_GRAIN_SECONDS, HIGH_SEARCH_SECONDS),
        );
        Self {
            low,
            high,
            scratch: Vec::new(),
        }
    }

    /// Moves to output frame `pos`. The grains in the few hops before it are
    /// rendered and dropped, so that what follows has settled.
    pub fn seek(&mut self, pos: u64) {
        self.low.seek(pos);
        self.high.seek(pos);
    }

    /// Fills `out` (stereo, interleaved) with the next frames and returns how
    /// many it wrote: fewer than asked for only at the end of the clip.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        let n = self.low.read(out);
        self.scratch.resize(out.len(), 0.0);
        let m = self.high.read(&mut self.scratch);
        debug_assert_eq!(n, m);
        let n = n.min(m);
        for (o, h) in out[..n * 2].iter_mut().zip(&self.scratch) {
            *o += h;
        }
        n
    }
}

/// The source and its bass, shared by the two bands. The bass is a
/// linear-phase low-pass, filtered a block at a time and kept, since the
/// grains read the same audio again and again.
struct Split<S> {
    source: S,
    channels: usize,
    /// Half the filter, centre first.
    kernel: Vec<f32>,
    /// Filtered blocks (interleaved), oldest first.
    blocks: Vec<(i64, Vec<f32>)>,
    raw: Vec<f32>,
    lane: Vec<f32>,
}

impl<S: Source> Split<S> {
    fn new(source: S, rate: f32) -> Self {
        let channels = source.channels().max(1);
        let half = SPLIT_TAPS / 2;
        let cutoff = SPLIT_HZ / f64::from(rate);
        let mut kernel: Vec<f64> = (0..=half)
            .map(|k| {
                let x = k as f64;
                let sinc = if k == 0 {
                    2.0 * cutoff
                } else {
                    (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                };
                let t = std::f64::consts::PI * (x + half as f64) / half as f64;
                // Blackman window.
                sinc * (0.42 - 0.5 * t.cos() + 0.08 * (2.0 * t).cos())
            })
            .collect();
        let sum = kernel[0] + 2.0 * kernel[1..].iter().sum::<f64>();
        kernel.iter_mut().for_each(|k| *k /= sum);
        Self {
            source,
            channels,
            kernel: kernel.into_iter().map(|k| k as f32).collect(),
            blocks: Vec::new(),
            raw: Vec::new(),
            lane: Vec::new(),
        }
    }

    fn block(&mut self, index: i64) -> usize {
        if let Some(at) = self.blocks.iter().position(|(i, _)| *i == index) {
            return at;
        }
        let half = SPLIT_TAPS / 2;
        let channels = self.channels;
        let first = index * SPLIT_BLOCK as i64;
        self.raw.resize((SPLIT_BLOCK + 2 * half) * channels, 0.0);
        self.source.read(first - half as i64, &mut self.raw);
        let mut filtered = vec![0.0f32; SPLIT_BLOCK * channels];
        for channel in 0..channels {
            self.lane.clear();
            self.lane
                .extend(self.raw.iter().skip(channel).step_by(channels));
            for n in 0..SPLIT_BLOCK {
                let centre = n + half;
                let mut sum = self.kernel[0] * self.lane[centre];
                let before = &self.lane[n..centre];
                let after = &self.lane[centre + 1..=centre + half];
                for ((k, b), a) in self.kernel[1..].iter().zip(before.iter().rev()).zip(after) {
                    sum += k * (b + a);
                }
                filtered[n * channels + channel] = sum;
            }
        }
        if self.blocks.len() >= 6 {
            self.blocks.remove(0);
        }
        self.blocks.push((index, filtered));
        self.blocks.len() - 1
    }

    fn low(&mut self, start: i64, out: &mut [f32]) {
        let channels = self.channels;
        let mut done = 0;
        let frames = out.len() / channels;
        while done < frames {
            let at = start + done as i64;
            let index = at.div_euclid(SPLIT_BLOCK as i64);
            let offset = at.rem_euclid(SPLIT_BLOCK as i64) as usize;
            let n = (SPLIT_BLOCK - offset).min(frames - done);
            let block = self.block(index);
            out[done * channels..(done + n) * channels]
                .copy_from_slice(&self.blocks[block].1[offset * channels..(offset + n) * channels]);
            done += n;
        }
    }
}

struct LowSource<S>(Arc<Mutex<Split<S>>>);
struct HighSource<S>(Arc<Mutex<Split<S>>>);

impl<S: Source> Source for LowSource<S> {
    fn channels(&self) -> usize {
        self.0.lock().unwrap().channels
    }

    fn read(&mut self, start: i64, out: &mut [f32]) {
        self.0.lock().unwrap().low(start, out);
    }
}

impl<S: Source> Source for HighSource<S> {
    fn channels(&self) -> usize {
        self.0.lock().unwrap().channels
    }

    fn read(&mut self, start: i64, out: &mut [f32]) {
        let mut split = self.0.lock().unwrap();
        split.source.read(start, out);
        let mut low = std::mem::take(&mut split.lane);
        low.resize(out.len(), 0.0);
        split.low(start, &mut low);
        for (o, l) in out.iter_mut().zip(&low) {
            *o -= l;
        }
        split.lane = low;
    }
}

impl<S: Source> Band<S> {
    /// A stretcher for the section `map` describes, `total` output frames
    /// long, with grains of `grain` seconds that may move `search` seconds.
    /// `onsets` are the source's transients, in source frames (Beats
    /// mode), or empty (Grain). The stretcher starts at output frame 0.
    fn new(
        source: S,
        map: Arc<WarpMap>,
        onsets: &[u64],
        total: u64,
        rate: f32,
        (grain, search): (f64, f64),
    ) -> Self {
        let rate = f64::from(rate);
        let hop = ((grain * rate / 2.0).round() as i64).max(8);
        let win = hop as usize * 2;
        let radius = ((search * rate).round() as i64).max(1);
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
    fn seek(&mut self, pos: u64) {
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
    fn read(&mut self, out: &mut [f32]) -> usize {
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

#[cfg(test)]
mod mix_tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: f32 = 48_000.0;

    struct Memory(Vec<f32>);
    impl Source for Memory {
        fn channels(&self) -> usize {
            1
        }
        fn read(&mut self, start: i64, out: &mut [f32]) {
            for (i, x) in out.iter_mut().enumerate() {
                let at = start + i as i64;
                *x = if at >= 0 {
                    self.0.get(at as usize).copied().unwrap_or(0.0)
                } else {
                    0.0
                };
            }
        }
    }

    /// A full-mix stand-in: bass notes (with a second harmonic) changing every
    /// half second under louder, harmonically rich mids with vibrato, and
    /// noisy hats.
    fn mix(frames: usize) -> Vec<f32> {
        const NOTES: [f32; 4] = [46.25, 55.0, 61.74, 41.2];
        let mut seed = 7u32;
        let (mut bass_phase, mut mid_phase) = (0.0f32, 0.0f32);
        (0..frames)
            .map(|i| {
                let t = i as f32 / RATE;
                let note = NOTES[(i / 24_000) % NOTES.len()];
                bass_phase = (bass_phase + TAU * note / RATE) % TAU;
                let bass = (bass_phase.sin() + 0.4 * (2.0 * bass_phase).sin()) * 0.35;
                let f = 310.0 * (1.0 + 0.01 * (TAU * 5.0 * t).sin());
                mid_phase = (mid_phase + TAU * f / RATE) % TAU;
                let mids: f32 = (1..=8)
                    .map(|h| (mid_phase * h as f32).sin() / h as f32 * 0.25)
                    .sum();
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f32 / (1 << 23) as f32 - 1.0;
                let hat = noise * 0.1 * if (i / 6_000) % 2 == 0 { 1.0 } else { 0.2 };
                bass + mids + hat
            })
            .collect()
    }

    /// Everything above ~120 Hz taken out, leaving the bass fundamental.
    fn low(x: &[f32]) -> Vec<f32> {
        let a = 1.0 - (-TAU * 90.0 / RATE).exp();
        let (mut p, mut q) = (0.0f32, 0.0f32);
        x.iter()
            .map(|v| {
                p += a * (v - p);
                q += a * (p - q);
                q
            })
            .collect()
    }

    fn stretched(data: Vec<f32>, speed: f64) -> Vec<f32> {
        let src = data.len() as u64;
        let total = (src as f64 / speed) as u64;
        let map = Arc::new(WarpMap::from_knots(vec![
            (0.0, 0.0),
            (total as f64, src as f64),
        ]));
        let mut s = Stretcher::new(Memory(data), map, &[], total, RATE);
        let mut out = vec![0.0; total as usize * 2];
        assert_eq!(s.read(&mut out), total as usize);
        out.iter().step_by(2).copied().collect()
    }

    /// The strongest frequency within 8% of `near`, by a fine scan.
    fn hz(x: &[f32], near: f32) -> f32 {
        let power = |f: f32| -> f32 {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, v) in x.iter().enumerate() {
                let w = 0.5 - 0.5 * (TAU * i as f32 / x.len() as f32).cos();
                let a = TAU * f * i as f32 / RATE;
                re += v * w * a.cos();
                im += v * w * a.sin();
            }
            re * re + im * im
        };
        (-40..=40)
            .map(|k| near * (1.0 + k as f32 * 0.002))
            .max_by(|a, b| power(*a).total_cmp(&power(*b)))
            .unwrap()
    }

    /// Spread of the bass level over 25 ms windows, relative to its mean.
    fn wobble(x: &[f32]) -> f32 {
        let levels: Vec<f32> = x
            .chunks(1_200)
            .filter(|c| c.len() == 1_200)
            .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / 1_200.0).sqrt())
            .collect();
        let mean = levels.iter().sum::<f32>() / levels.len() as f32;
        let var = levels.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / levels.len() as f32;
        var.sqrt() / mean
    }

    const NOTES: [f32; 4] = [46.25, 55.0, 61.74, 41.2];

    /// The bass of note `n` in the output of a clip played at `speed`, away
    /// from the note's ends.
    fn note(bass: &[f32], n: usize, speed: f64) -> &[f32] {
        let from = (n as f64 * 24_000.0 / speed) as usize + 3_000;
        &bass[from..from + (24_000.0 / speed) as usize - 6_000]
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Sped up or slowed down, each bass note keeps its pitch to within a few
    /// cents and its level and steadiness, under mids that would pull a
    /// single set of grains out of step with it.
    #[test]
    fn a_bass_line_under_a_full_mix_keeps_its_pitch_and_level() {
        let input = mix(8 * 48_000);
        let before = low(&input);
        for speed in [0.8f64, 1.1, 1.25, 1.5] {
            let out = stretched(input.clone(), speed);
            let after = low(&out);
            let notes = (after.len() as f64 * speed / 24_000.0) as usize - 2;
            for n in 1..notes {
                let want = NOTES[n % NOTES.len()];
                let heard = note(&after, n, speed);
                let cents = 1200.0 * (hz(heard, want) / want).log2();
                assert!(
                    cents.abs() < 6.0,
                    "speed {speed}, note {n}: {cents:.1} cents"
                );
                let reference = &before[n * 24_000 + 3_000..(n + 1) * 24_000 - 3_000];
                let level = rms(heard) / rms(reference);
                assert!(
                    (0.85..1.15).contains(&level),
                    "speed {speed}, note {n}: level {level:.2}"
                );
                let (wobble, ideal) = (wobble(heard), wobble(reference));
                assert!(
                    wobble < ideal + 0.02,
                    "speed {speed}, note {n}: wobble {wobble:.3} (source {ideal:.3})"
                );
            }
        }
    }
}
