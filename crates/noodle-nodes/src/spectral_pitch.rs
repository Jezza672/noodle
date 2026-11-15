//! Pitch Shift and Time Stretch on spectral frames.
//!
//! Both are phase vocoders that lock the phase of a partial's whole region of
//! bins to its peak ("identity phase locking", Laroche and Dolson 1999), so a
//! partial stays one clean sine instead of smearing into a chorus. A frame
//! is cut into regions, one round each peak of the magnitude; each region's
//! peak has a true frequency, worked out from how its phase moved since the
//! last frame; the region is moved to where the new frequency falls, and its
//! phase is carried on from where the same peak left off.
//!
//! They only use what a spectral wire gives a node: one frame out for each
//! frame in, at the same time. Pitch Shift changes where the partials are and
//! nothing about when. Time Stretch reads a store of recent frames at a speed
//! of its own, so it changes when and nothing about where.

use noodle_engine::{
    Bin, Config, Context, Instance, Io, Layout, Node, NodeError, NodeInfo, NodeType, ParamInfo,
    Ports, Setup, Unit, fft_bins,
};

use crate::spectral::{map_frames, param, spectral_shape, wrap_phase};

use std::f32::consts::FRAC_PI_2;

/// One partial: the bin at its peak and the bins of its region, from valley
/// to valley, both ends included.
#[derive(Clone, Copy, Debug)]
struct Peak {
    bin: usize,
    lo: usize,
    hi: usize,
}

/// Moves the partials of a frame, carrying each one's phase on from frame to
/// frame. All its room is allocated by [`PhaseLock::new`].
struct PhaseLock {
    magnitude: Vec<f32>,
    peaks: Vec<Peak>,
    /// Where each peak of the last frame landed in the output, and the phase
    /// its synthesis had there.
    last: Vec<(usize, f32)>,
    next: Vec<(usize, f32)>,
}

/// Peaks quieter than this (about -140 dB) are silence, not partials.
const PEAK_FLOOR: f32 = 1e-7;
/// Peaks this far under the frame's loudest bin (-90 dB) are rounding noise
/// of the transform, not partials. Their phases are arbitrary and differ
/// between processors, so following them would make the sound depend on them.
const PEAK_RELATIVE_FLOOR: f32 = 3e-5;

impl PhaseLock {
    fn new(bins: usize) -> Self {
        Self {
            magnitude: vec![0.0; bins],
            peaks: Vec::with_capacity(bins / 2 + 2),
            last: Vec::with_capacity(bins / 2 + 2),
            next: Vec::with_capacity(bins / 2 + 2),
        }
    }

    fn forget(&mut self) {
        self.last.clear();
    }

    /// Finds the partials of `frame`: the bins that are the loudest of the
    /// five around them (which leaves out a window's side lobes), and the
    /// valleys between them.
    fn find_peaks(&mut self, frame: &[Bin]) {
        let bins = frame.len();
        for (m, bin) in self.magnitude.iter_mut().zip(frame) {
            *m = bin.magnitude();
        }
        let m = &self.magnitude;
        let floor = PEAK_FLOOR.max(m.iter().fold(0.0f32, |a, &b| a.max(b)) * PEAK_RELATIVE_FLOOR);
        self.peaks.clear();
        for k in 0..bins {
            if m[k] <= floor {
                continue;
            }
            let lo = k.saturating_sub(2);
            let hi = (k + 2).min(bins - 1);
            // Ties go to the lower bin, so a flat top is one peak.
            if (lo..k).all(|j| m[j] < m[k]) && (k + 1..=hi).all(|j| m[j] <= m[k]) {
                self.peaks.push(Peak {
                    bin: k,
                    lo: 0,
                    hi: bins - 1,
                });
            }
        }
        // Region limits: the quietest bin between two peaks ends the lower
        // one's region.
        for i in 1..self.peaks.len() {
            let (a, b) = (self.peaks[i - 1].bin, self.peaks[i].bin);
            let valley = (a..=b).min_by(|&x, &y| m[x].total_cmp(&m[y])).unwrap_or(a);
            self.peaks[i - 1].hi = valley;
            self.peaks[i].lo = (valley + 1).min(bins - 1);
        }
    }

    /// Writes the frame `x` to `out` with every partial moved to `ratio` times
    /// its frequency. `freq(bin)` is the true frequency of the partial at a
    /// peak bin, in bins (fractional).
    fn apply(
        &mut self,
        x: &[Bin],
        out: &mut [Bin],
        ratio: f32,
        mut freq: impl FnMut(usize) -> f32,
    ) {
        let bins = x.len();
        out.fill(Bin::ZERO);
        self.find_peaks(x);
        self.next.clear();
        let mut near = 0;
        for index in 0..self.peaks.len() {
            let Peak { bin, lo, hi } = self.peaks[index];
            let f = freq(bin);
            let target = f * ratio;
            let q = (target.round().max(0.0) as usize).min(bins - 1);
            // The same partial in the last frame, if there was one nearby.
            while near < self.last.len() && self.last[near].0 + 2 < q {
                near += 1;
            }
            let matched = self.last[near..]
                .iter()
                .take_while(|(b, _)| *b <= q + 2)
                .min_by_key(|(b, _)| b.abs_diff(q))
                .map(|&(_, phase)| phase);
            let live = x[bin].phase();
            let phase = match matched {
                Some(phase) => wrap_phase(phase + FRAC_PI_2 * target),
                None => live,
            };
            // The region's own phases, relative to its peak, turned to where
            // the peak now is.
            let (sin, cos) = (phase - live).sin_cos();
            let shift = q as isize - bin as isize;
            for (k, bin) in x.iter().enumerate().take(hi + 1).skip(lo) {
                let to = k as isize + shift;
                if !(0..bins as isize).contains(&to) {
                    continue;
                }
                let v = Bin::new(bin.re * cos - bin.im * sin, bin.re * sin + bin.im * cos);
                let slot = &mut out[to as usize];
                if v.power() > slot.power() {
                    *slot = v;
                }
            }
            self.next.push((q, phase));
        }
        std::mem::swap(&mut self.last, &mut self.next);
    }
}

/// The true frequency, in bins, of the partial whose peak is at `bin`, from
/// how far its phase moved between two frames a hop (a quarter of a frame)
/// apart. A partial at a bin's centre advances `bin / 4` turns.
fn instantaneous(now: &[Bin], before: &[Bin], bin: usize) -> f32 {
    let expected = FRAC_PI_2 * bin as f32;
    let deviation = wrap_phase(now[bin].phase() - before[bin].phase() - expected);
    bin as f32 + deviation * 2.0 / std::f32::consts::PI
}

// ---------------------------------------------------------------------------
// Pitch Shift
// ---------------------------------------------------------------------------

/// Changes the pitch of a spectrum by `shift` semitones without changing its
/// timing: every partial goes to `2^(shift/12)` times its frequency, its
/// phase carried on so it stays a clean tone. The shape of the spectrum moves
/// with the pitch, so a voice shifted up turns small and chipmunk-like; follow
/// it with a Formant Shift of the opposite amount to keep the character (the
/// Pitch Shifter group does).
///
/// It works from the true frequency of each peak, so it is at its best on
/// tones and sung or played notes, and at its softest on noise and sharp
/// attacks, which come out slightly washed. A smaller frame (Analyze's
/// `size`) keeps attacks tighter and blurs low notes; a larger one is cleaner
/// on bass and chords.
pub struct SpectralPitch;

pub const SPECTRAL_PITCH_ID: &str = "noodle.spectral.pitch";

#[derive(Ports)]
struct PitchPorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[param(
        "shift",
        "Shift",
        ParamInfo::new(-24.0, 24.0, 0.0)
            .unit(Unit::Semitones)
            .offset()
    )]
    shift: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static PITCH_INFO: NodeInfo = NodeInfo {
    id: SPECTRAL_PITCH_ID,
    version: 1,
    name: "Spectral Pitch",
    category: "Spectral",
};

impl NodeType for SpectralPitch {
    fn info(&self) -> &NodeInfo {
        &PITCH_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(PitchPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let lanes = spectral_shape(setup, PitchPorts::OUTPUTS).lanes();
        let bins = fft_bins(setup.spectral_size);
        Ok(Instance::realtime(PitchNode {
            lanes: (0..lanes).map(|_| PitchLane::new(bins)).collect(),
        }))
    }
}

struct PitchLane {
    lock: PhaseLock,
    /// The frame before the one being worked on.
    before: Vec<Bin>,
}

impl PitchLane {
    fn new(bins: usize) -> Self {
        Self {
            lock: PhaseLock::new(bins),
            before: vec![Bin::ZERO; bins],
        }
    }
}

struct PitchNode {
    lanes: Vec<PitchLane>,
}

impl Node for PitchNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let inputs = io.inputs;
        let lanes = &mut self.lanes;
        map_frames(io, |time, voice, lane, input, out| {
            let state = &mut lanes[lane];
            let semitones = param(inputs, PitchPorts::SHIFT, voice, time);
            let ratio = (semitones / 12.0).exp2();
            if (ratio - 1.0).abs() < 1e-6 {
                out.copy_from_slice(input);
                state.lock.forget();
            } else {
                let before = &state.before;
                state
                    .lock
                    .apply(input, out, ratio, |bin| instantaneous(input, before, bin));
            }
            state.before.copy_from_slice(input);
        });
    }

    fn reset(&mut self) {
        for lane in &mut self.lanes {
            lane.lock.forget();
            lane.before.fill(Bin::ZERO);
        }
    }
}

// ---------------------------------------------------------------------------
// Time Stretch
// ---------------------------------------------------------------------------

/// The most frames' worth of time a Time Stretch holds, in seconds.
pub const MAX_LENGTH: f32 = 8.0;

/// Plays the spectrum back at `rate` times the speed without changing its
/// pitch: 0.5 is half speed and stays in tune, 2 is double speed.
///
/// A wire is live, so it can't be stretched for ever. Time Stretch keeps the
/// last `length` seconds and moves a read point through them at `rate`. Slower
/// than 1, the read point falls behind the live edge and, when it is `length`
/// seconds behind, jumps up to the live edge again (so a sustained sound
/// stretches and a run of notes is slowed and then skips ahead). Faster than
/// 1, it catches the live edge and jumps back `length` seconds. Set `length`
/// to the length of the part you want slowed: put a loop through it, or
/// freeze a moment with a very slow rate for a long wash of sound.
///
/// It adds no delay of its own: a frame comes out for each frame that goes
/// in. Memory: `length` is at most 8 seconds of frames for each lane.
pub struct SpectralStretch;

pub const SPECTRAL_STRETCH_ID: &str = "noodle.spectral.stretch";

#[derive(Ports)]
struct StretchPorts {
    #[spectral_input("spectrum", "Spectrum")]
    spectrum: (),
    #[param("rate", "Rate", ParamInfo::new(0.05, 4.0, 1.0).log().offset())]
    rate: (),
    #[param(
        "length",
        "Length",
        ParamInfo::new(0.1, MAX_LENGTH, 2.0)
            .unit(Unit::Seconds)
            .log()
    )]
    length: (),
    #[spectral_output("out", "Out")]
    out: (),
}

static STRETCH_INFO: NodeInfo = NodeInfo {
    id: SPECTRAL_STRETCH_ID,
    version: 1,
    name: "Spectral Stretch",
    category: "Spectral",
};

impl NodeType for SpectralStretch {
    fn info(&self) -> &NodeInfo {
        &STRETCH_INFO
    }

    fn layout(&self, _config: &Config) -> Result<Layout, NodeError> {
        Ok(StretchPorts::layout())
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let lanes = spectral_shape(setup, StretchPorts::OUTPUTS).lanes();
        let bins = fft_bins(setup.spectral_size);
        let hop = noodle_engine::fft_hop(setup.spectral_size);
        // Enough frames for the longest length, and the two the read point
        // needs either side of it.
        let capacity = (MAX_LENGTH * setup.sample_rate / hop as f32).ceil() as usize + 3;
        Ok(Instance::realtime(StretchNode {
            bins,
            capacity,
            frames_per_second: setup.sample_rate / hop as f32,
            lanes: (0..lanes)
                .map(|_| StretchLane::new(bins, capacity))
                .collect(),
        }))
    }
}

struct StretchLane {
    lock: PhaseLock,
    ring: Vec<Bin>,
    /// Frames written so far.
    written: u64,
    /// The read point, as a frame number (fractional) in the same count.
    head: f64,
}

impl StretchLane {
    fn new(bins: usize, capacity: usize) -> Self {
        Self {
            lock: PhaseLock::new(bins),
            ring: vec![Bin::ZERO; bins * capacity],
            written: 0,
            head: 0.0,
        }
    }

    fn clear(&mut self) {
        self.lock.forget();
        self.written = 0;
        self.head = 0.0;
    }
}

struct StretchNode {
    bins: usize,
    capacity: usize,
    frames_per_second: f32,
    lanes: Vec<StretchLane>,
}

impl Node for StretchNode {
    fn process(&mut self, _ctx: &Context, io: Io<'_, '_>) {
        let inputs = io.inputs;
        let (bins, capacity, per_second) = (self.bins, self.capacity, self.frames_per_second);
        let lanes = &mut self.lanes;
        map_frames(io, |time, voice, lane, input, out| {
            let StretchLane {
                lock,
                ring,
                written,
                head,
            } = &mut lanes[lane];
            let rate = f64::from(param(inputs, StretchPorts::RATE, voice, time)).clamp(0.05, 4.0);
            let length = param(inputs, StretchPorts::LENGTH, voice, time).clamp(0.1, MAX_LENGTH);
            let span = f64::from((length * per_second).min((capacity - 3) as f32).max(2.0));

            // Store the new frame.
            let slot = (*written % capacity as u64) as usize;
            ring[slot * bins..(slot + 1) * bins].copy_from_slice(input);
            *written += 1;
            let live = (*written - 1) as f64;
            let oldest = written.saturating_sub(capacity as u64 - 1) as f64;

            // Keep the read point inside what is held, and inside `length`.
            if *written == 1 {
                *head = 0.0;
            }
            if *head > live {
                *head = (live - span + (*head - live)).max(oldest + 1.0).min(live);
            } else if *head < (live - span).max(oldest + 1.0) {
                *head = live;
            }

            let frame_at = |n: u64| {
                let slot = (n % capacity as u64) as usize;
                &ring[slot * bins..(slot + 1) * bins]
            };
            let now = head.floor() as u64;
            if (now as f64) < oldest + 1.0 {
                // Nothing before it to measure a frequency against.
                out.copy_from_slice(frame_at(now));
                lock.forget();
            } else {
                let (current, before) = (frame_at(now), frame_at(now - 1));
                lock.apply(current, out, 1.0, |bin| instantaneous(current, before, bin));
            }
            *head += rate;
        });
    }

    fn reset(&mut self) {
        for lane in &mut self.lanes {
            lane.clear();
        }
    }
}
