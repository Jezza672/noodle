//! The training and evaluation data for the drum hit classifier (issue #499).
//!
//! `noodle-eval drum-dataset build` turns a sample pack (one-shots with
//! licences and credits) into rows of the real detector's own output:
//!
//! * every one-shot, alone in a short silence, run through
//!   [`noodle_io::analyse_hits`], and
//! * synthetic loops: one-shots placed on a grid with timing jitter and level
//!   changes, overlapped, bled into each other, put in a room, EQ'd and mixed
//!   with noise, then cut by the same detector. Each detected hit is labelled
//!   from where the one-shot was placed.
//!
//! Features are what [`noodle_io::analyse_hits`] gives, so training sees what
//! deployment sees. Samples are split into train, validation and test by a
//! hash of their file name, and a loop only uses samples of its own split, so
//! nothing leaks. Eval songs are benchmarks and never come near this.
//!
//! The output is a `hits.csv` (features only, no audio), a `manifest.json`
//! that lists every sample with its licence and credit, and a `CREDITS.md`
//! that carries the CC BY credit. `noodle-eval drum-dataset baseline` scores
//! the current rule labeller ([`noodle_io::guess`]) on those rows.

use noodle_io::{DrumKind, Hit, HitFeatures, HitLabeller, analyse_hits, decode_file, resample};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The rate everything is rendered and analysed at.
pub const RATE: u32 = 44_100;
/// Bumped when the generator changes what it writes.
pub const DATASET_VERSION: u32 = 2;
/// The longest a one-shot is kept, in seconds (cymbals ring a long time).
const MAX_SAMPLE: f64 = 3.0;
/// A loop hit labelled from a placed hit mixed further down than this (dB) is
/// mostly something else's tail, so it is not scored or trained on.
pub const MIN_GAIN_DB: f32 = -35.0;
/// How near a detected onset must be to a placed hit to take its label.
const MATCH: f64 = 0.025;

/// What the classifier tells apart. Rides and crashes stay apart here; the app
/// has no ride, so [`Class::kind`] folds rides into `Crash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    Kick,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
    Crash,
    Ride,
    Tom,
    /// Percussion, body sounds, anything that should come out `Unknown`.
    Other,
}

impl Class {
    pub const ALL: [Class; 9] = [
        Class::Kick,
        Class::Snare,
        Class::Clap,
        Class::ClosedHat,
        Class::OpenHat,
        Class::Crash,
        Class::Ride,
        Class::Tom,
        Class::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Class::Kick => "kick",
            Class::Snare => "snare",
            Class::Clap => "clap",
            Class::ClosedHat => "closed_hat",
            Class::OpenHat => "open_hat",
            Class::Crash => "crash",
            Class::Ride => "ride",
            Class::Tom => "tom",
            Class::Other => "other",
        }
    }

    pub fn from_name(name: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The pack folder (and file name, for the two folders that mix) to class.
    pub fn of_sample(category: &str, file: &str) -> Class {
        let hat_open = file.contains("open");
        match category {
            "kick" | "m-kick" | "m-room-kick" => Class::Kick,
            "snare" | "m-snare" | "m-room-snare" | "snare-extra" => Class::Snare,
            // A cross-stick is a click, not a snare.
            "rim" if file.contains("crossstick") => Class::Other,
            "rim" => Class::Snare,
            "clap" => Class::Clap,
            "hihat-closed" | "hihat-pedal" => Class::ClosedHat,
            "hihat-open" | "hihat-half" => Class::OpenHat,
            "m-hat" if hat_open => Class::OpenHat,
            "m-hat" => Class::ClosedHat,
            "crash" | "m-crash" | "china" | "m-china" | "splash" => Class::Crash,
            "ride" | "m-ride" => Class::Ride,
            "tom" | "m-tom" | "m-room-tom" => Class::Tom,
            // percussion, cowbell, body, stomp, snap, m-stack
            _ => Class::Other,
        }
    }

    /// What the app would call it. `pitch_key` splits toms low from high at
    /// the same boundary the rule labeller uses.
    pub fn kind(self, pitch_key: Option<f32>) -> DrumKind {
        match self {
            Class::Kick => DrumKind::Kick,
            Class::Snare => DrumKind::Snare,
            Class::Clap => DrumKind::Clap,
            Class::ClosedHat => DrumKind::ClosedHat,
            Class::OpenHat => DrumKind::OpenHat,
            Class::Crash | Class::Ride => DrumKind::Crash,
            Class::Tom if pitch_key.is_some_and(|k| k >= TOM_SPLIT_KEY) => DrumKind::HighTom,
            Class::Tom => DrumKind::LowTom,
            Class::Other => DrumKind::Unknown,
        }
    }
}

/// The key number between a low tom and a high one (the rule labeller puts the
/// boundary between 49 and 55).
pub const TOM_SPLIT_KEY: f32 = 52.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    Train,
    Val,
    Test,
}

impl Split {
    pub fn name(self) -> &'static str {
        match self {
            Split::Train => "train",
            Split::Val => "val",
            Split::Test => "test",
        }
    }
}

/// One one-shot of the pack.
#[derive(Clone, Debug)]
pub struct Source {
    /// Path relative to the pack.
    pub file: String,
    pub category: String,
    pub library: String,
    pub class: Class,
    pub licence: String,
    pub credit: String,
    pub split: Split,
}

/// A small deterministic generator, so a dataset is reproducible by seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// 0 up to but not including 1.
    pub fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f()
    }
    pub fn chance(&mut self, p: f32) -> bool {
        self.f() < p
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

/// What close-mic, room and overhead versions of one hit share, so they land
/// in the same split: the library and the name without mic and folder words.
pub fn split_key(file: &str, library: &str) -> String {
    const MIC: [&str; 12] = [
        "room", "oh", "btm", "top", "cl", "close", "tip", "shank", "rcshank", "rctip", "m", "mj",
    ];
    let stem = file.rsplit('/').next().unwrap_or(file);
    let stem = stem
        .rsplit_once('.')
        .map_or(stem, |(s, _)| s)
        .to_lowercase();
    let kept: Vec<&str> = stem
        .split(['-', '_'])
        .filter(|t| !MIC.contains(t) && !t.is_empty())
        .collect();
    format!("{library}|{}", kept.join("-"))
}

/// Which split a sample is in. `holdout` (a library tag) goes entirely to
/// test, which asks "does it work on a kit it has never heard".
pub fn split_of(file: &str, library: &str, holdout: Option<&str>) -> Split {
    if holdout == Some(library) {
        return Split::Test;
    }
    match fnv(&split_key(file, library)) % 100 {
        0..=69 => Split::Train,
        70..=84 => Split::Val,
        _ => Split::Test,
    }
}

fn quoted<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(key)? + key.len();
    let rest = &line[at..];
    Some(&rest[..rest.find('"')?])
}

/// The samples of `pack` (a folder with a `pack.ron`), with licences, credits
/// and splits. A sample without a file or a licence is an error: a dataset
/// that cannot say where a sound came from is not releasable.
pub fn read_pack(pack: &Path, holdout: Option<&str>) -> Result<Vec<Source>, String> {
    let ron = std::fs::read_to_string(pack.join("pack.ron"))
        .map_err(|e| format!("{}: {e}", pack.join("pack.ron").display()))?;
    let mut out = Vec::new();
    for line in ron
        .lines()
        .filter(|l| l.contains("(hash:") && l.contains("file: Some(\"audio/"))
    {
        let file = quoted(line, "file: Some(\"").ok_or("a sample has no file")?;
        let licence =
            quoted(line, "licence: Some(spdx(\"").ok_or_else(|| format!("{file}: no licence"))?;
        let credit = quoted(line, "credit: Some(\"").unwrap_or("");
        let mut parts = file.split('/');
        let (_, category, name) = (
            parts.next(),
            parts.next().unwrap_or(""),
            parts.next_back().unwrap_or(""),
        );
        let tags = line.split("tags: [").nth(1).unwrap_or("");
        let library = tags
            .split('"')
            .filter(|t| !t.contains(',') && !t.contains(']') && !t.is_empty())
            .nth(1)
            .unwrap_or("unknown")
            .to_string();
        out.push(Source {
            file: file.to_string(),
            category: category.to_string(),
            class: Class::of_sample(category, name),
            split: split_of(file, &library, holdout),
            library,
            licence: licence.to_string(),
            credit: credit.to_string(),
        });
    }
    if out.is_empty() {
        return Err("the pack lists no samples".into());
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(out)
}

/// A one-shot as mono audio at [`RATE`], peak-normalised, capped in length.
fn load(pack: &Path, source: &Source) -> Result<Vec<f32>, String> {
    let path = pack.join(&source.file);
    let audio = decode_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let audio = resample(&audio, RATE).map_err(|e| format!("{}: {e}", path.display()))?;
    let ch = audio.channels.max(1);
    let mut mono: Vec<f32> = audio
        .samples
        .chunks(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    mono.truncate((MAX_SAMPLE * f64::from(RATE)) as usize);
    let fade = (0.02 * f64::from(RATE)) as usize;
    let n = mono.len();
    for i in 0..fade.min(n) {
        mono[n - 1 - i] *= i as f32 / fade as f32;
    }
    let peak = mono.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak < 1e-4 {
        return Err(format!("{}: silent", source.file));
    }
    mono.iter_mut().for_each(|s| *s /= peak);
    Ok(mono)
}

fn db_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A one-pole low-pass in place.
fn lowpass(x: &mut [f32], hz: f32) {
    let a = 1.0 - (-2.0 * std::f32::consts::PI * hz / RATE as f32).exp();
    let mut y = 0.0;
    for s in x {
        y += a * (*s - y);
        *s = y;
    }
}

/// A one-pole high-pass in place.
fn highpass(x: &mut [f32], hz: f32) {
    let mut low = x.to_vec();
    lowpass(&mut low, hz);
    for (s, l) in x.iter_mut().zip(low) {
        *s -= l;
    }
}

/// Convolves `x` with `ir` by FFT.
fn convolve(x: &[f32], ir: &[f32]) -> Vec<f32> {
    use realfft::RealFftPlanner;
    let n = (x.len() + ir.len()).next_power_of_two();
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let (mut a, mut b) = (vec![0.0; n], vec![0.0; n]);
    a[..x.len()].copy_from_slice(x);
    b[..ir.len()].copy_from_slice(ir);
    let (mut fa, mut fb) = (fwd.make_output_vec(), fwd.make_output_vec());
    fwd.process(&mut a, &mut fa).unwrap();
    fwd.process(&mut b, &mut fb).unwrap();
    for (p, q) in fa.iter_mut().zip(&fb) {
        *p *= *q;
    }
    let mut out = vec![0.0; n];
    inv.process(&mut fa, &mut out).unwrap();
    out.iter_mut().for_each(|s| *s /= n as f32);
    out.truncate(x.len());
    out
}

/// A synthetic room: noise with an exponential decay of `rt60` seconds.
fn room(rng: &mut Rng, rt60: f32) -> Vec<f32> {
    let len = (rt60 * RATE as f32) as usize;
    let k = 6.91 / (rt60 * RATE as f32);
    let mut ir: Vec<f32> = (0..len)
        .map(|i| (rng.f() * 2.0 - 1.0) * (-k * i as f32).exp())
        .collect();
    lowpass(&mut ir, rng.range(3_000.0, 10_000.0));
    let e = ir.iter().map(|s| s * s).sum::<f32>().sqrt().max(1e-9);
    ir.iter_mut().for_each(|s| *s /= e);
    ir
}

/// How pure a detection is as a kit sample. Only `Clean` may enter a bin of
/// single hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Purity {
    /// One hit that finishes ringing before the end of its window.
    Clean,
    /// Two or more hits of one drum (a flam, a roll, a doubled hit).
    Double,
    /// Still ringing at the end of its window, or cut by the next hit.
    Truncated,
    /// A real hit that is damaged: no attack, clipped, faded in.
    Malformed,
    /// Another drum sounds under it.
    Mixed,
    /// Not a hit.
    Spurious,
}

impl Purity {
    pub const ALL: [Purity; 6] = [
        Purity::Clean,
        Purity::Double,
        Purity::Truncated,
        Purity::Malformed,
        Purity::Mixed,
        Purity::Spurious,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Purity::Clean => "clean",
            Purity::Double => "double",
            Purity::Truncated => "truncated",
            Purity::Malformed => "malformed",
            Purity::Mixed => "mixed",
            Purity::Spurious => "spurious",
        }
    }

    pub fn from_name(name: &str) -> Option<Purity> {
        Purity::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// The envelope's block, in frames (2 ms).
const BLOCK: usize = (RATE as usize) / 500;
/// A hit is over once it is this far (dB) under its peak.
const OVER_DB: f32 = 30.0;
/// A predecessor still this close (dB) to a hit's peak at its start is under it.
const RING_DB: f32 = 25.0;
/// The detector's longest cut, in frames.
const MAX_CUT: usize = (1.5 * RATE as f32) as usize;

/// The level of each 2 ms block of `x`, in dB.
fn env_db(x: &[f32]) -> Vec<f32> {
    x.chunks(BLOCK)
        .map(|c| 10.0 * (c.iter().map(|s| s * s).sum::<f32>() / BLOCK as f32 + 1e-12).log10())
        .collect()
}

/// Whether `x` is still sounding (within [`OVER_DB`] of its peak) just before
/// `end` (the 20 ms before the last 20 ms, which the loader fades).
fn ringing_at(x: &[f32], end: usize) -> bool {
    let end = end.min(x.len());
    let env = env_db(x);
    let peak = env.iter().copied().fold(-200.0, f32::max);
    let a = end.saturating_sub(2 * 20 * RATE as usize / 1000) / BLOCK;
    let b = (end.saturating_sub(20 * RATE as usize / 1000) / BLOCK).max(a + 1);
    env[a.min(env.len() - 1)..b.min(env.len())]
        .iter()
        .any(|&l| l > peak - OVER_DB)
}

/// A hit placed in a loop.
#[derive(Clone, Debug)]
pub struct Placed {
    pub at: usize,
    pub class: Class,
    pub file: String,
    /// The loudness it was mixed at (dB, after bleed).
    pub gain_db: f32,
    /// Its own level over time as mixed, in dB per 2 ms block.
    pub env: Vec<f32>,
    /// Hits placed together on purpose as a double share a group.
    pub group: Option<u32>,
}

impl Placed {
    fn peak(&self) -> f32 {
        self.env.iter().copied().fold(-200.0, f32::max)
    }

    /// Its own level at `frame` (-200 before and after it).
    fn level_at(&self, frame: usize) -> f32 {
        frame
            .checked_sub(self.at)
            .and_then(|d| self.env.get(d / BLOCK))
            .copied()
            .unwrap_or(-200.0)
    }

    /// The frame where it has fallen [`OVER_DB`] under its peak.
    fn own_end(&self) -> usize {
        let peak = self.peak();
        let top = self.env.iter().position(|&l| l >= peak).unwrap_or(0);
        let b = (top + 1..self.env.len())
            .find(|&b| self.env[b] < peak - OVER_DB)
            .unwrap_or(self.env.len());
        self.at + (b * BLOCK).min(MAX_CUT)
    }
}

/// The purity of each placed hit, from what else is placed (ground truth):
/// another hit of the same drum, or placed with it as a double, on top of it
/// makes it `Double`; another drum's tail still strong under it makes it
/// `Mixed`; another hit starting before it has fallen away makes it
/// `Truncated`. Anything else is `Clean`.
pub fn purity_labels(placed: &[Placed]) -> Vec<Purity> {
    placed
        .iter()
        .enumerate()
        .map(|(k, p)| {
            let peak = p.peak();
            let (mut double, mut mixed, mut cut) = (false, false, false);
            for (j, q) in placed.iter().enumerate() {
                if j == k {
                    continue;
                }
                let same = q.class == p.class || (q.group.is_some() && q.group == p.group);
                if q.at >= p.at {
                    if q.at < p.own_end() && q.peak() >= p.level_at(q.at) - 12.0 {
                        if same {
                            double = true;
                        } else {
                            cut = true;
                        }
                    }
                } else if q.own_end() > p.at && q.level_at(p.at + 2 * BLOCK) > peak - RING_DB {
                    if same {
                        double = true;
                    } else {
                        mixed = true;
                    }
                }
            }
            if double {
                Purity::Double
            } else if mixed {
                Purity::Mixed
            } else if cut {
                Purity::Truncated
            } else {
                Purity::Clean
            }
        })
        .collect()
}

/// A rendered loop and where everything in it is.
pub struct Loop {
    pub audio: Vec<f32>,
    pub placed: Vec<Placed>,
}

struct Pool<'a> {
    by_class: BTreeMap<Class, Vec<(&'a Source, &'a Vec<f32>)>>,
}

impl<'a> Pool<'a> {
    fn pick(
        &self,
        rng: &mut Rng,
        class: Class,
        library: &str,
    ) -> Option<(&'a Source, &'a Vec<f32>)> {
        let all = self.by_class.get(&class)?;
        let mine: Vec<_> = all.iter().filter(|(s, _)| s.library == library).collect();
        if !mine.is_empty() && rng.chance(0.85) {
            let (s, a) = mine[rng.below(mine.len())];
            Some((*s, *a))
        } else {
            let (s, a) = all[rng.below(all.len())];
            Some((s, a))
        }
    }
}

/// How long after a hit its double comes, in frames: from a flam to most of a
/// beat, spread evenly in the logarithm.
fn double_gap(rng: &mut Rng) -> usize {
    let secs = (0.005f32.ln() + rng.f() * (0.6f32.ln() - 0.005f32.ln())).exp();
    (secs * RATE as f32) as usize
}

/// Renders one loop from `pool` (the samples of one split).
fn make_loop(pool: &Pool, rng: &mut Rng) -> Option<Loop> {
    let libs: Vec<&str> = pool
        .by_class
        .get(&Class::Kick)?
        .iter()
        .map(|(s, _)| s.library.as_str())
        .collect();
    let library = libs[rng.below(libs.len())].to_string();
    let bpm = rng.range(70.0, 170.0);
    let step = (60.0 / bpm / 4.0 * RATE as f32) as usize;
    let bars = 2 + rng.below(3);
    let steps = bars * 16;
    // The kit: one or two variants of each piece, so hits of a piece repeat.
    let mut kit: HashMap<Class, Vec<(&Source, &Vec<f32>)>> = HashMap::new();
    for class in Class::ALL {
        let n = if class == Class::Tom {
            3
        } else {
            1 + rng.below(2)
        };
        let v: Vec<_> = (0..n)
            .filter_map(|_| pool.pick(rng, class, &library))
            .collect();
        if !v.is_empty() {
            kit.insert(class, v);
        }
    }
    let mut events: Vec<(usize, Class, f32)> = Vec::new(); // step, class, gain dB
    let mode_ride = rng.chance(0.2);
    let clap_backbeat = rng.chance(0.25);
    let hat16 = rng.chance(0.4);
    for s in 0..steps {
        let in_bar = s % 16;
        let last_bar = s / 16 == bars - 1;
        if (in_bar % 8 == 0 && rng.chance(0.95))
            || rng.chance(if in_bar % 4 == 2 { 0.2 } else { 0.06 })
        {
            events.push((s, Class::Kick, rng.range(-8.0, 0.0)));
        }
        if in_bar % 8 == 4 && rng.chance(0.95) {
            let class = if clap_backbeat && rng.chance(0.7) {
                Class::Clap
            } else {
                Class::Snare
            };
            events.push((s, class, rng.range(-6.0, 0.0)));
        } else if rng.chance(0.04) {
            events.push((s, Class::Snare, rng.range(-24.0, -14.0)));
        }
        let timekeeper = if mode_ride {
            Class::Ride
        } else {
            Class::ClosedHat
        };
        if (in_bar % 2 == 0 || (hat16 && !mode_ride)) && rng.chance(0.92) {
            let open = !mode_ride && in_bar % 4 == 2 && rng.chance(0.12);
            let class = if open { Class::OpenHat } else { timekeeper };
            events.push((s, class, rng.range(-16.0, -4.0)));
        }
        if s == 0 && rng.chance(0.5) || (last_bar && in_bar == 0 && rng.chance(0.3)) {
            events.push((s, Class::Crash, rng.range(-8.0, -1.0)));
        }
        if last_bar && in_bar >= 12 && rng.chance(0.5) {
            events.push((s, Class::Tom, rng.range(-8.0, -1.0)));
        }
        if rng.chance(0.04) {
            events.push((s, Class::Other, rng.range(-14.0, -3.0)));
        }
    }
    // A stem-like loop: one piece up front, the rest bled in.
    let focus = if rng.chance(0.3) {
        Some(Class::ALL[rng.below(Class::ALL.len() - 1)])
    } else {
        None
    };
    let tail = (1.5 * RATE as f32) as usize;
    let total = steps * step + tail;
    let mut mix = vec![0.0f32; total];
    let mut placed = Vec::new();
    for (s, class, mut gain_db) in events {
        let Some(variants) = kit.get(&class) else {
            continue;
        };
        let (source, audio) = variants[rng.below(variants.len())];
        let jitter = (rng.range(-0.008, 0.008) * RATE as f32) as isize;
        let at = (s * step) as isize + jitter;
        if at < 0 {
            continue;
        }
        let at = at as usize;
        let mut hit = audio.to_vec();
        if focus.is_some_and(|f| f != class) {
            gain_db -= rng.range(12.0, 30.0);
            lowpass(&mut hit, rng.range(500.0, 3_000.0));
        }
        let g = db_gain(gain_db);
        for (i, v) in hit.iter().enumerate() {
            if let Some(m) = mix.get_mut(at + i) {
                *m += v * g;
            }
        }
        let scaled: Vec<f32> = hit.iter().map(|v| v * g).collect();
        placed.push(Placed {
            at,
            class,
            file: source.file.clone(),
            gain_db,
            env: env_db(&scaled),
            group: None,
        });
        // Sometimes the same piece again soon after, on purpose: a flam, a
        // roll or a doubled hit.
        if !focus.is_some_and(|f| f != class) && rng.chance(0.06) {
            let (source2, audio2) = if rng.chance(0.7) {
                variants[rng.below(variants.len())]
            } else {
                (source, audio)
            };
            let gap = double_gap(rng);
            let at2 = at + gap;
            let delta = rng.range(-8.0, 0.0);
            let g2 = db_gain(gain_db + delta);
            for (i, v) in audio2.iter().enumerate() {
                if let Some(m) = mix.get_mut(at2 + i) {
                    *m += v * g2;
                }
            }
            let scaled: Vec<f32> = audio2.iter().map(|v| v * g2).collect();
            let group = placed.len() as u32;
            placed.last_mut().unwrap().group = Some(group);
            placed.push(Placed {
                at: at2,
                class,
                file: source2.file.clone(),
                gain_db: gain_db + delta,
                env: env_db(&scaled),
                group: Some(group),
            });
        }
    }
    // The room, EQ and noise.
    if rng.chance(0.5) {
        let wet = db_gain(rng.range(-20.0, -6.0));
        let rt60 = rng.range(0.1, 0.7);
        let tail = convolve(&mix, &room(rng, rt60));
        mix.iter_mut().zip(tail).for_each(|(m, t)| *m += t * wet);
    }
    if rng.chance(0.4) {
        lowpass(&mut mix, rng.range(3_000.0, 16_000.0));
    }
    if rng.chance(0.3) {
        highpass(&mut mix, rng.range(30.0, 200.0));
    }
    if rng.chance(0.6) {
        let level = db_gain(rng.range(-65.0, -40.0));
        let mut noise: Vec<f32> = (0..mix.len()).map(|_| rng.f() * 2.0 - 1.0).collect();
        if rng.chance(0.5) {
            lowpass(&mut noise, rng.range(500.0, 6_000.0));
        }
        let e = (noise.iter().map(|s| s * s).sum::<f32>() / noise.len() as f32)
            .sqrt()
            .max(1e-9);
        mix.iter_mut()
            .zip(noise)
            .for_each(|(m, n)| *m += n / e * level);
    }
    let peak = mix.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6);
    mix.iter_mut().for_each(|s| *s *= 0.9 / peak);
    Some(Loop { audio: mix, placed })
}

/// One row of the dataset.
#[derive(Clone, Debug)]
pub struct Row {
    pub split: Split,
    /// `oneshot` or `loop`.
    pub source: &'static str,
    /// The loop number, or the one-shot's file.
    pub origin: String,
    /// A class name, or `spurious` for a detection that is no placed hit.
    pub label: String,
    pub onset_s: f32,
    pub level_db: f32,
    /// How loud the placed hit it is labelled from was mixed (dB; 0 for a
    /// one-shot). A hit labelled from a placed hit far under the loudest
    /// thing around is mostly something else's tail.
    pub gain_db: f32,
    pub mixed: bool,
    pub truncated: bool,
    /// How pure it is as a sample (ground truth from the generator).
    pub purity: Purity,
    /// `none`, or what was done to a damaged one-shot (`chop_start`, `clip`,
    /// `fade_in`, `gate`), or `double` for a one-shot with a second hit.
    pub damage: String,
    pub features: HitFeatures,
}

const CSV_HEAD: &str = "split,source,origin,label,onset_s,level_db,gain_db,mixed,truncated,centroid,flatness,low,mid,high,decay,pitched,pitch_key,attack_peaks";

fn csv_header() -> String {
    let mut s = CSV_HEAD.to_string();
    for b in 0..24 {
        write!(s, ",b{b}").unwrap();
    }
    s.push_str(",purity,damage");
    s
}

fn csv_row(r: &Row) -> String {
    let f = &r.features;
    let mut s = format!(
        "{},{},{},{},{:.4},{:.2},{:.1},{},{},{:.2},{:.5},{:.5},{:.5},{:.5},{:.4},{:.4},{},{}",
        r.split.name(),
        r.source,
        r.origin.replace(',', "_"),
        r.label,
        r.onset_s,
        r.level_db,
        r.gain_db,
        u8::from(r.mixed),
        u8::from(r.truncated),
        f.centroid,
        f.flatness,
        f.split[0],
        f.split[1],
        f.split[2],
        f.decay,
        f.pitched,
        f.pitch_key.map_or(String::new(), |k| format!("{k:.3}")),
        f.attack_peaks,
    );
    for b in f.bands {
        write!(s, ",{b:.3}").unwrap();
    }
    write!(s, ",{},{}", r.purity.name(), r.damage).unwrap();
    s
}

/// Reads `hits.csv` back.
pub fn read_rows(path: &Path) -> Result<Vec<Row>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate().skip(1) {
        let c: Vec<&str> = line.split(',').collect();
        if c.len() != 18 + 24 + 2 {
            return Err(format!("line {}: {} columns", n + 1, c.len()));
        }
        let num = |i: usize| -> Result<f32, String> {
            c[i].parse().map_err(|e| format!("line {}: {e}", n + 1))
        };
        let mut bands = [0.0; 24];
        for (b, band) in bands.iter_mut().enumerate() {
            *band = num(18 + b)?;
        }
        rows.push(Row {
            split: match c[0] {
                "train" => Split::Train,
                "val" => Split::Val,
                _ => Split::Test,
            },
            source: match c[1] {
                "loop" => "loop",
                "damaged" => "damaged",
                "double" => "double",
                _ => "oneshot",
            },
            origin: c[2].to_string(),
            label: c[3].to_string(),
            onset_s: num(4)?,
            level_db: num(5)?,
            gain_db: num(6)?,
            mixed: c[7] == "1",
            truncated: c[8] == "1",
            purity: Purity::from_name(c[42]).ok_or_else(|| format!("line {}: purity", n + 1))?,
            damage: c[43].to_string(),
            features: HitFeatures {
                bands,
                centroid: num(9)?,
                flatness: num(10)?,
                split: [num(11)?, num(12)?, num(13)?],
                decay: num(14)?,
                pitched: num(15)?,
                pitch_key: if c[16].is_empty() {
                    None
                } else {
                    Some(num(16)?)
                },
                attack_peaks: num(17)? as u8,
            },
        });
    }
    Ok(rows)
}

pub struct BuildOptions {
    pub pack: PathBuf,
    pub out: PathBuf,
    pub seed: u64,
    pub loops: usize,
    pub holdout_library: Option<String>,
}

fn detect(audio: &[f32]) -> Vec<Hit> {
    analyse_hits(audio, RATE, &mut |_| true).unwrap_or_default()
}

/// Builds the dataset into `opts.out`. Returns a summary to print.
pub fn build(opts: &BuildOptions) -> Result<String, String> {
    let sources = read_pack(&opts.pack, opts.holdout_library.as_deref())?;
    std::fs::create_dir_all(&opts.out).map_err(|e| e.to_string())?;
    let mut audio: HashMap<String, Vec<f32>> = HashMap::new();
    for s in &sources {
        audio.insert(s.file.clone(), load(&opts.pack, s)?);
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut undetected = 0;
    // One-shots alone, with a little silence before and after.
    let pad = (0.1 * RATE as f32) as usize;
    let alone = |x: &[f32]| {
        let mut y = vec![0.0; pad];
        y.extend_from_slice(x);
        y.extend(std::iter::repeat_n(0.0, pad * 4));
        let hits = detect(&y);
        let found = hits
            .iter()
            .position(|h| (h.onset as f64 / f64::from(RATE) - 0.1).abs() < 0.03);
        (hits, found)
    };
    for s in &sources {
        let (hits, found) = alone(&audio[&s.file]);
        let Some(j) = found else {
            undetected += 1;
            continue;
        };
        // The window is cut at the longest the detector cuts to; a hit that is
        // still ringing there is truncated.
        let purity = if ringing_at(&audio[&s.file], MAX_CUT.min(audio[&s.file].len())) {
            Purity::Truncated
        } else {
            Purity::Clean
        };
        rows.push(row_of(
            s.split,
            "oneshot",
            s.file.clone(),
            s.class.name().into(),
            0.0,
            &hits[j],
            purity,
            "none",
        ));
    }
    // The same one-shots damaged: attack chopped off, clipped, faded in, or
    // cut off while still sounding. Two kinds each, picked by the seed.
    let mut dmg = Rng::new(opts.seed ^ 0x00DA_3A6E);
    for s in &sources {
        let x = &audio[&s.file];
        let kinds = ["chop_start", "clip", "fade_in", "gate"];
        let first = dmg.below(kinds.len());
        let second = (first + 1 + dmg.below(kinds.len() - 1)) % kinds.len();
        for kind in [kinds[first], kinds[second]] {
            let Some((y, purity)) = damage(x, kind, &mut dmg) else {
                continue;
            };
            let (hits, found) = alone(&y);
            let Some(j) = found else {
                undetected += 1;
                continue;
            };
            rows.push(row_of(
                s.split,
                "damaged",
                s.file.clone(),
                s.class.name().into(),
                0.0,
                &hits[j],
                purity,
                kind,
            ));
        }
    }
    // Doubles: a one-shot and a second hit of the same piece (or, a third of
    // the time, another of the same split) a flam to most of a beat later.
    let mut dbl = Rng::new(opts.seed ^ 0x000D_0B1E);
    for split in [Split::Train, Split::Val, Split::Test] {
        let mine: Vec<&Source> = sources.iter().filter(|s| s.split == split).collect();
        for (n, s) in mine.iter().enumerate() {
            let other = if dbl.chance(0.35) {
                mine[dbl.below(mine.len())]
            } else {
                s
            };
            let gap = double_gap(&mut dbl);
            let delta = dbl.range(-8.0, 0.0);
            let (first, second) = (&audio[&s.file], &audio[&other.file]);
            let mut mix = vec![0.0f32; gap + second.len().max(first.len())];
            let mut placed = Vec::new();
            for (at, x, src, gain_db) in [(0, first, *s, 0.0), (gap, second, other, delta)] {
                let g = db_gain(gain_db);
                for (i, v) in x.iter().enumerate() {
                    mix[at + i] += v * g;
                }
                let scaled: Vec<f32> = x.iter().map(|v| v * g).collect();
                placed.push(Placed {
                    at: at + pad,
                    class: src.class,
                    file: src.file.clone(),
                    gain_db,
                    env: env_db(&scaled),
                    group: Some(n as u32),
                });
            }
            let peak = mix.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
            mix.iter_mut().for_each(|v| *v /= peak);
            let mut y = vec![0.0; pad];
            y.extend_from_slice(&mix);
            y.extend(std::iter::repeat_n(0.0, pad * 4));
            let hits = detect(&y);
            for (label, purity, gain, h) in label_detections(&hits, &placed) {
                if label != "spurious" {
                    rows.push(row_of(
                        split,
                        "double",
                        s.file.clone(),
                        label,
                        gain,
                        h,
                        purity,
                        "double",
                    ));
                }
            }
        }
    }
    // Loops.
    let mut spurious = 0usize;
    let mut missed = 0usize;
    let mut placed_total = 0usize;
    let mut rng = Rng::new(opts.seed);
    for split in [Split::Train, Split::Val, Split::Test] {
        let mut by_class: BTreeMap<Class, Vec<(&Source, &Vec<f32>)>> = BTreeMap::new();
        for s in sources.iter().filter(|s| s.split == split) {
            by_class
                .entry(s.class)
                .or_default()
                .push((s, &audio[&s.file]));
        }
        let pool = Pool { by_class };
        let n = match split {
            Split::Train => opts.loops * 7 / 10,
            Split::Val => opts.loops * 15 / 100,
            Split::Test => opts.loops * 15 / 100,
        };
        for i in 0..n {
            let Some(lp) = make_loop(&pool, &mut rng) else {
                continue;
            };
            let hits = detect(&lp.audio);
            placed_total += lp.placed.len();
            let labelled = label_detections(&hits, &lp.placed);
            let found: Vec<bool> = lp
                .placed
                .iter()
                .map(|p| {
                    labelled.iter().any(|(l, _, _, h)| {
                        *l != "spurious"
                            && h.onset.abs_diff(p.at as u64) as f64 <= MATCH * f64::from(RATE)
                    })
                })
                .collect();
            for (label, purity, gain, h) in labelled {
                if label == "spurious" {
                    spurious += 1;
                }
                rows.push(row_of(
                    split,
                    "loop",
                    format!("{}-{i}", split.name()),
                    label,
                    gain,
                    h,
                    purity,
                    "none",
                ));
            }
            missed += found.iter().filter(|f| !**f).count();
        }
    }
    let mut csv = csv_header();
    csv.push('\n');
    for r in &rows {
        csv.push_str(&csv_row(r));
        csv.push('\n');
    }
    std::fs::write(opts.out.join("hits.csv"), csv).map_err(|e| e.to_string())?;
    write_manifest(opts, &sources)?;
    let mut summary = format!(
        "{} rows ({} samples, {} loops); {undetected} one-shots undetected; \
         {placed_total} placed hits, {missed} never detected, {spurious} detections matched nothing\n",
        rows.len(),
        sources.len(),
        opts.loops
    );
    for split in [Split::Train, Split::Val, Split::Test] {
        let n = rows.iter().filter(|r| r.split == split).count();
        writeln!(summary, "  {}: {n} rows", split.name()).unwrap();
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn row_of(
    split: Split,
    source: &'static str,
    origin: String,
    label: String,
    gain_db: f32,
    h: &Hit,
    purity: Purity,
    damage: &str,
) -> Row {
    Row {
        split,
        source,
        origin,
        label,
        onset_s: (h.onset as f64 / f64::from(RATE)) as f32,
        level_db: h.level_db,
        gain_db,
        mixed: h.mixed,
        truncated: h.truncated,
        purity,
        damage: damage.to_string(),
        features: h.features.clone(),
    }
}

/// Labels each detection from the placed hits: the nearest detection within
/// [`MATCH`] of a placed hit takes its class and purity (a ring past the end of
/// its window makes it `Truncated`), and every other is `spurious`.
fn label_detections<'a>(hits: &'a [Hit], placed: &[Placed]) -> Vec<(String, Purity, f32, &'a Hit)> {
    let window = MATCH * f64::from(RATE);
    let nearest: Vec<Option<(usize, f64)>> = hits
        .iter()
        .map(|h| {
            placed
                .iter()
                .enumerate()
                .map(|(k, p)| (k, (p.at as f64 - h.onset as f64).abs()))
                .filter(|&(_, d)| d <= window)
                .min_by(|a, b| a.1.total_cmp(&b.1))
        })
        .collect();
    let mut owner: Vec<Option<usize>> = vec![None; placed.len()];
    for (j, n) in nearest.iter().enumerate() {
        if let Some((k, d)) = *n
            && owner[k].is_none_or(|o| d < nearest[o].map_or(f64::MAX, |x| x.1))
        {
            owner[k] = Some(j);
        }
    }
    let purity = purity_labels(placed);
    hits.iter()
        .enumerate()
        .map(|(j, h)| match nearest[j] {
            Some((k, _)) if owner[k] == Some(j) => {
                let p = &placed[k];
                let mut pur = purity[k];
                if pur == Purity::Clean
                    && p.level_at((h.end as usize).saturating_sub(2 * BLOCK)) > p.peak() - OVER_DB
                {
                    pur = Purity::Truncated;
                }
                (p.class.name().to_string(), pur, p.gain_db, h)
            }
            _ => ("spurious".to_string(), Purity::Spurious, 0.0, h),
        })
        .collect()
}

/// `x` damaged as `kind`, and how pure the result is.
fn damage(x: &[f32], kind: &str, rng: &mut Rng) -> Option<(Vec<f32>, Purity)> {
    let ms = |t: f32| (t / 1000.0 * RATE as f32) as usize;
    let mut y = x.to_vec();
    let purity = match kind {
        "chop_start" => {
            let n = ms(rng.range(10.0, 60.0));
            if n + ms(50.0) >= y.len() {
                return None;
            }
            y.drain(..n);
            Purity::Malformed
        }
        "clip" => {
            let g = rng.range(3.0, 12.0);
            y.iter_mut().for_each(|v| *v = (*v * g).clamp(-1.0, 1.0));
            Purity::Malformed
        }
        "fade_in" => {
            let n = ms(rng.range(40.0, 150.0)).min(y.len());
            for (i, v) in y.iter_mut().take(n).enumerate() {
                *v *= i as f32 / n as f32;
            }
            Purity::Malformed
        }
        _ => {
            // Cut dead, no fade, while it still sounds.
            let at = ms(rng.range(40.0, 400.0));
            if at + ms(60.0) >= y.len() || !ringing_at(&y, at + ms(20.0)) {
                return None;
            }
            y.truncate(at);
            Purity::Truncated
        }
    };
    let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    y.iter_mut().for_each(|v| *v /= peak);
    Some((y, purity))
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn write_manifest(opts: &BuildOptions, sources: &[Source]) -> Result<(), String> {
    let mut m = String::from("{\n");
    writeln!(m, "  \"dataset_version\": {DATASET_VERSION},").unwrap();
    writeln!(
        m,
        "  \"detector_version\": {},",
        noodle_io::DETECTOR_VERSION
    )
    .unwrap();
    writeln!(m, "  \"seed\": {},", opts.seed).unwrap();
    writeln!(m, "  \"loops\": {},", opts.loops).unwrap();
    writeln!(m, "  \"rate\": {RATE},").unwrap();
    writeln!(
        m,
        "  \"holdout_library\": {},",
        opts.holdout_library
            .as_ref()
            .map_or("null".into(), |l| format!("\"{}\"", esc(l)))
    )
    .unwrap();
    let classes: Vec<String> = Class::ALL
        .iter()
        .map(|c| format!("\"{}\"", c.name()))
        .collect();
    writeln!(m, "  \"classes\": [{}],", classes.join(", ")).unwrap();
    writeln!(m, "  \"tom_split_key\": {TOM_SPLIT_KEY},").unwrap();
    m.push_str("  \"samples\": [\n");
    for (i, s) in sources.iter().enumerate() {
        writeln!(
            m,
            "    {{\"file\": \"{}\", \"category\": \"{}\", \"library\": \"{}\", \"class\": \"{}\", \"split\": \"{}\", \"licence\": \"{}\", \"credit\": \"{}\"}}{}",
            esc(&s.file),
            esc(&s.category),
            esc(&s.library),
            s.class.name(),
            s.split.name(),
            esc(&s.licence),
            esc(&s.credit),
            if i + 1 < sources.len() { "," } else { "" }
        )
        .unwrap();
    }
    m.push_str("  ]\n}\n");
    std::fs::write(opts.out.join("manifest.json"), m).map_err(|e| e.to_string())?;

    let mut credits = String::from(
        "# Credits and licences\n\n\
         The rows in `hits.csv` are numbers measured from the samples below, mixed into synthetic loops.\n\
         No audio is in this dataset. The licence of each sample, and for CC BY the credit that must travel with\n\
         anything built from it, is in `manifest.json`.\n\n",
    );
    let mut by_licence: BTreeMap<&str, Vec<&Source>> = BTreeMap::new();
    for s in sources {
        by_licence.entry(&s.licence).or_default().push(s);
    }
    for (licence, list) in by_licence {
        writeln!(credits, "## {licence} ({} samples)\n", list.len()).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for s in list {
            // The credit names a library and an author, not each file.
            let line = s.credit.split(", ").next().unwrap_or("").to_string();
            if seen.insert(line.clone()) {
                writeln!(credits, "- {line}").unwrap();
            }
        }
        credits.push('\n');
    }
    std::fs::write(opts.out.join("CREDITS.md"), credits).map_err(|e| e.to_string())
}

/// Counts of (truth, predicted) and the numbers derived from them.
#[derive(Default)]
pub struct Confusion {
    /// truth class name to predicted name to count.
    pub cells: BTreeMap<String, BTreeMap<String, usize>>,
}

impl Confusion {
    pub fn add(&mut self, truth: &str, predicted: &str) {
        *self
            .cells
            .entry(truth.to_string())
            .or_default()
            .entry(predicted.to_string())
            .or_default() += 1;
    }

    pub fn total(&self) -> usize {
        self.cells.values().flat_map(|r| r.values()).sum()
    }

    /// Hits the guess got right. `other` is right when nothing was guessed.
    pub fn correct(&self) -> usize {
        self.cells
            .iter()
            .map(|(t, r)| {
                r.get(if t == "other" { "Unknown" } else { t })
                    .copied()
                    .unwrap_or(0)
            })
            .sum()
    }

    /// A markdown table with recall per row.
    pub fn table(&self) -> String {
        let mut cols: Vec<String> = Class::ALL.iter().map(|c| c.name().to_string()).collect();
        cols.push("Unknown".into());
        let mut s = String::from("| truth \\ guess |");
        for c in &cols {
            write!(s, " {c} |").unwrap();
        }
        s.push_str(" n | recall |\n|---|");
        s.push_str(&"---|".repeat(cols.len() + 2));
        s.push('\n');
        for c in Class::ALL {
            let Some(row) = self.cells.get(c.name()) else {
                continue;
            };
            let n: usize = row.values().sum();
            write!(s, "| {} |", c.name()).unwrap();
            for p in &cols {
                write!(s, " {} |", row.get(p).copied().unwrap_or(0)).unwrap();
            }
            let right = row
                .get(if c == Class::Other {
                    "Unknown"
                } else {
                    c.name()
                })
                .copied()
                .unwrap_or(0);
            writeln!(s, " {n} | {:.0}% |", 100.0 * right as f32 / n.max(1) as f32).unwrap();
        }
        s
    }
}

/// What the labeller said, as a class name (`Unknown` for no guess). The two
/// toms are both `tom`; a crash guess is `crash` and is also counted right for
/// a ride (the app has no ride).
fn hit_of(row: &Row) -> Hit {
    Hit {
        onset: 0,
        start: 0,
        end: 0,
        truncated: row.truncated,
        mixed: row.mixed,
        level_db: row.level_db,
        features: row.features.clone(),
    }
}

fn guess_class(labeller: &dyn HitLabeller, row: &Row) -> &'static str {
    let hit = hit_of(row);
    match labeller.label(&hit).0 {
        DrumKind::Kick => "kick",
        DrumKind::Snare => "snare",
        DrumKind::ClosedHat => "closed_hat",
        DrumKind::OpenHat => "open_hat",
        DrumKind::Clap => "clap",
        DrumKind::LowTom | DrumKind::HighTom => "tom",
        DrumKind::Crash => "crash",
        DrumKind::Unknown => "Unknown",
    }
}

/// The 95% Wilson interval of `k` in `n`, as percentages.
fn wilson(k: usize, n: usize) -> (f32, f32) {
    if n == 0 {
        return (0.0, 100.0);
    }
    let (k, n) = (k as f32, n as f32);
    let z = 1.96f32;
    let p = k / n;
    let d = 1.0 + z * z / n;
    let centre = (p + z * z / (2.0 * n)) / d;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / d;
    (
        100.0 * (centre - half).max(0.0),
        100.0 * (centre + half).min(1.0),
    )
}

/// The purity gate's report: how many hits that are not clean get into a clean
/// bin (the false-accept rate, the headline), per kind of impurity and per
/// kind of damage, and how many clean hits are turned away. `gate` is the
/// probability of `clean` a hit needs; a labeller with no purity output is
/// scored on the detector's `mixed` and `truncated` flags.
pub fn purity_report(rows: &[Row], split: Split, labeller: &dyn HitLabeller, gate: f32) -> String {
    let mut out = String::new();
    let used: Vec<&Row> = rows
        .iter()
        .filter(|r| r.split == split && r.level_db >= -45.0 && r.gain_db >= MIN_GAIN_DB)
        .collect();
    let probs: Vec<Option<Vec<f32>>> = used.iter().map(|r| labeller.purity(&hit_of(r))).collect();
    let has_model = probs.iter().any(Option::is_some);
    let accept = |i: usize, gate: f32| match &probs[i] {
        Some(p) => noodle_io::pure_enough(p, &hit_of(used[i]), gate),
        None => !used[i].mixed && !used[i].truncated,
    };
    writeln!(
        out,
        "## Purity gate ({} split)\n\n{}\n\n\
         False-accept rate: of the hits that are not clean, the share let into a clean bin. 95% Wilson interval in brackets.\n",
        split.name(),
        if has_model {
            format!("Accepted when `clean` is the likeliest class with probability at least {gate} and the detector flags neither `mixed` nor `truncated`.")
        } else {
            "No purity model: accepted when the detector flags neither `mixed` nor `truncated`.".into()
        }
    )
    .unwrap();
    let rate = |k: usize, n: usize| {
        let (lo, hi) = wilson(k, n);
        format!(
            "{:.1}% ({lo:.1} to {hi:.1})",
            100.0 * k as f32 / n.max(1) as f32
        )
    };
    // Headline: every real hit that is not clean (not a spurious detection).
    let bad: Vec<usize> = (0..used.len())
        .filter(|&i| !matches!(used[i].purity, Purity::Clean | Purity::Spurious))
        .collect();
    let bad_in = bad.iter().filter(|&&i| accept(i, gate)).count();
    let clean: Vec<usize> = (0..used.len())
        .filter(|&i| used[i].purity == Purity::Clean)
        .collect();
    let clean_in = clean.iter().filter(|&&i| accept(i, gate)).count();
    writeln!(
        out,
        "**False-accept rate: {}** ({bad_in} of {} not-clean hits). Clean hits kept: {} ({clean_in} of {}).\n",
        rate(bad_in, bad.len()),
        bad.len(),
        rate(clean_in, clean.len()),
        clean.len()
    )
    .unwrap();
    out.push_str("| truth | n | let into a clean bin |\n|---|---|---|\n");
    for pu in Purity::ALL {
        let ix: Vec<usize> = (0..used.len()).filter(|&i| used[i].purity == pu).collect();
        if ix.is_empty() {
            continue;
        }
        let k = ix.iter().filter(|&&i| accept(i, gate)).count();
        writeln!(
            out,
            "| {} | {} | {} |",
            pu.name(),
            ix.len(),
            rate(k, ix.len())
        )
        .unwrap();
    }
    out.push_str("\nBy source, and by damage for damaged one-shots (not-clean hits only):\n\n| source | n not clean | let in |\n|---|---|---|\n");
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &i in &bad {
        let r = used[i];
        let key = if r.source == "damaged" {
            format!("damaged: {}", r.damage)
        } else {
            r.source.to_string()
        };
        groups.entry(key).or_default().push(i);
    }
    for (key, ix) in groups {
        let k = ix.iter().filter(|&&i| accept(i, gate)).count();
        writeln!(out, "| {key} | {} | {} |", ix.len(), rate(k, ix.len())).unwrap();
    }
    if has_model {
        out.push_str("\nGate sweep:\n\n| gate | false-accept | clean kept |\n|---|---|---|\n");
        for g in [0.5, 0.7, 0.8, 0.9, 0.95, 0.98, 0.99] {
            let b = bad.iter().filter(|&&i| accept(i, g)).count();
            let c = clean.iter().filter(|&&i| accept(i, g)).count();
            writeln!(
                out,
                "| {g} | {} | {} |",
                rate(b, bad.len()),
                rate(c, clean.len())
            )
            .unwrap();
        }
    }
    out
}

/// Scores `labeller` on `rows` of `split` and writes a markdown report.
pub fn baseline(rows: &[Row], split: Split, labeller: &dyn HitLabeller, name: &str) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "# {} (`{}` split)\n\n\
         Run on the features of every detected hit.\n\
         A ride is right when the rules say `crash` (the app has no ride); `other` is right when they say `Unknown`.\n\
         Hits quieter than {} dB are left out (the app drops them), as are loop hits labelled from a placed hit mixed more than 35 dB down, and `mixed` or `truncated` hits in the clean column.\n",
        name,
        split.name(),
        -45
    )
    .unwrap();
    for (title, source, only_clean) in [
        ("One-shots, alone", "oneshot", false),
        ("Loops, all hits", "loop", false),
        ("Loops, clean hits only", "loop", true),
    ] {
        let mut cm = Confusion::default();
        let mut spurious = 0;
        let mut tom_pitch = (0, 0);
        for r in rows
            .iter()
            .filter(|r| r.split == split && r.source == source)
        {
            if r.level_db < -45.0
                || r.gain_db < MIN_GAIN_DB
                || (only_clean && (r.mixed || r.truncated))
            {
                continue;
            }
            if r.label == "spurious" {
                spurious += 1;
                continue;
            }
            let Some(class) = Class::from_name(&r.label) else {
                continue;
            };
            let predicted = guess_class(labeller, r);
            let predicted = match (class, predicted) {
                (Class::Ride, "crash") => "ride",
                (_, p) => p,
            };
            cm.add(class.name(), predicted);
            if class == Class::Tom {
                tom_pitch.1 += 1;
                if r.features.pitch_key.is_some() {
                    tom_pitch.0 += 1;
                }
            }
        }
        if cm.total() == 0 {
            continue;
        }
        let wrong_confident = cm
            .cells
            .iter()
            .flat_map(|(t, r)| r.iter().map(move |(p, n)| (t, p, *n)))
            .filter(|(t, p, _)| t != p && **p != "Unknown")
            .map(|(_, _, n)| n)
            .sum::<usize>();
        writeln!(
            out,
            "## {title}\n\n{} hits: {:.1}% right, {:.1}% confidently wrong, {} detections matched no placed hit.\n\
             Toms with a pitch: {}/{}.\n\n{}",
            cm.total(),
            100.0 * cm.correct() as f32 / cm.total() as f32,
            100.0 * wrong_confident as f32 / cm.total() as f32,
            spurious,
            tom_pitch.0,
            tom_pitch.1,
            cm.table()
        )
        .unwrap();
    }
    out.push('\n');
    out.push_str(&purity_report(rows, split, labeller, noodle_io::CLEAN_GATE));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(at_ms: usize, class: Class, group: Option<u32>, ring_ms: usize) -> Placed {
        // A hit at 0 dB that falls 60 dB over `ring_ms`.
        let n = (ring_ms * RATE as usize / 1000) / BLOCK;
        Placed {
            at: at_ms * RATE as usize / 1000,
            class,
            file: String::new(),
            gain_db: 0.0,
            env: (0..n).map(|b| -60.0 * b as f32 / n as f32).collect(),
            group,
        }
    }

    #[test]
    fn purity_is_worked_out_from_what_else_is_placed() {
        let of = |p: &[Placed]| purity_labels(p);
        // Alone, and spaced far apart: clean.
        let far = [
            placed(0, Class::Snare, None, 200),
            placed(1000, Class::Snare, None, 200),
        ];
        assert_eq!(of(&far), [Purity::Clean, Purity::Clean]);
        // The same drum again while it rings: both are doubles.
        let flam = [
            placed(0, Class::Snare, None, 400),
            placed(20, Class::Snare, None, 400),
        ];
        assert_eq!(of(&flam), [Purity::Double, Purity::Double]);
        // Placed together on purpose, even if another drum.
        let grouped = [
            placed(0, Class::Snare, Some(1), 400),
            placed(30, Class::Clap, Some(1), 400),
        ];
        assert_eq!(of(&grouped), [Purity::Double, Purity::Double]);
        // Another drum on top of a ringing one: the first is cut, the second mixed.
        let over = [
            placed(0, Class::Crash, None, 800),
            placed(50, Class::Kick, None, 200),
        ];
        assert_eq!(of(&over), [Purity::Truncated, Purity::Mixed]);
        // The next hit after the first has fallen away does not touch it.
        let after = [
            placed(0, Class::Kick, None, 100),
            placed(500, Class::Snare, None, 100),
        ];
        assert_eq!(of(&after), [Purity::Clean, Purity::Clean]);
    }

    fn row_with(purity: Purity, source: &'static str) -> Row {
        Row {
            split: Split::Test,
            source,
            origin: "x".into(),
            label: "snare".into(),
            onset_s: 0.0,
            level_db: -10.0,
            gain_db: 0.0,
            mixed: false,
            truncated: false,
            purity,
            damage: "none".into(),
            features: HitFeatures {
                bands: [0.0; 24],
                centroid: 1000.0,
                flatness: 0.5,
                split: [0.3, 0.4, 0.3],
                decay: 0.1,
                pitched: 0.0,
                pitch_key: None,
                attack_peaks: 1,
            },
        }
    }

    #[derive(Debug)]
    struct Gate(f32);
    impl HitLabeller for Gate {
        fn label(&self, _: &Hit) -> (DrumKind, f32) {
            (DrumKind::Snare, 1.0)
        }
        fn purity(&self, _: &Hit) -> Option<Vec<f32>> {
            Some(vec![self.0, 0.0, 0.0, 0.0, 0.0, 0.0])
        }
    }

    #[test]
    fn the_false_accept_rate_catches_a_double_let_into_a_clean_bin() {
        let rows = vec![
            row_with(Purity::Clean, "oneshot"),
            row_with(Purity::Double, "double"),
            row_with(Purity::Malformed, "damaged"),
        ];
        let lax = purity_report(&rows, Split::Test, &Gate(0.99), 0.9);
        assert!(lax.contains("False-accept rate: 100.0%"), "{lax}");
        assert!(lax.contains("| double | 1 | 100.0%"), "{lax}");
        let strict = purity_report(&rows, Split::Test, &Gate(0.1), 0.9);
        assert!(strict.contains("False-accept rate: 0.0%"), "{strict}");
    }

    #[test]
    fn pack_folders_map_to_classes() {
        let of = Class::of_sample;
        assert_eq!(of("kick", "a.wav"), Class::Kick);
        assert_eq!(of("m-room-snare", "a.wav"), Class::Snare);
        assert_eq!(of("rim", "x-rimshot-1.flac"), Class::Snare);
        assert_eq!(of("rim", "x-crossstick-1.flac"), Class::Other);
        assert_eq!(of("m-hat", "mj-hat-open-3.flac"), Class::OpenHat);
        assert_eq!(of("m-hat", "mj-hat-closed-3.flac"), Class::ClosedHat);
        assert_eq!(of("hihat-half", "a.wav"), Class::OpenHat);
        assert_eq!(of("china", "a.wav"), Class::Crash);
        assert_eq!(of("m-ride", "a.wav"), Class::Ride);
        assert_eq!(of("cowbell", "a.wav"), Class::Other);
        assert_eq!(Class::Ride.kind(None), DrumKind::Crash);
        assert_eq!(Class::Tom.kind(Some(45.0)), DrumKind::LowTom);
        assert_eq!(Class::Tom.kind(Some(60.0)), DrumKind::HighTom);
        for c in Class::ALL {
            assert_eq!(Class::from_name(c.name()), Some(c));
        }
    }

    #[test]
    fn mic_versions_of_one_hit_share_a_split() {
        let a = split_key("audio/m-snare/mj-snare-3-oh.wav", "muldjordkit");
        let b = split_key("audio/m-room-snare/mj-snare-3-room.wav", "muldjordkit");
        assert_eq!(a, b);
        assert_ne!(
            a,
            split_key("audio/m-snare/mj-snare-4-oh.wav", "muldjordkit")
        );
    }

    #[test]
    fn splits_are_stable_and_the_holdout_library_is_all_test() {
        assert_eq!(
            split_of("a/b.wav", "x", None),
            split_of("a/b.wav", "x", None)
        );
        assert_eq!(split_of("a/b.wav", "x", Some("x")), Split::Test);
        let n = (0..1000)
            .filter(|i| split_of(&format!("f{i}"), "x", None) == Split::Train)
            .count();
        assert!((600..800).contains(&n), "{n} of 1000 in train");
    }

    fn click(len: usize, decay: f32, freq: f32) -> Vec<f32> {
        (0..len)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                (-t / decay).exp() * (std::f32::consts::TAU * freq * t).sin()
            })
            .collect()
    }

    /// Loops come out with placed hits the detector finds, labelled from where
    /// they were put, and the rows survive the CSV.
    #[test]
    fn loops_are_labelled_from_placement_and_rows_round_trip() {
        let source = |file: &str, class| Source {
            file: file.into(),
            category: String::new(),
            library: "lib".into(),
            class,
            licence: "CC0-1.0".into(),
            credit: String::new(),
            split: Split::Train,
        };
        let kick = (
            source("k", Class::Kick),
            click(RATE as usize / 4, 0.08, 60.0),
        );
        let snare = (
            source("s", Class::Snare),
            click(RATE as usize / 4, 0.05, 1_800.0),
        );
        let mut by_class = BTreeMap::new();
        by_class.insert(Class::Kick, vec![(&kick.0, &kick.1)]);
        by_class.insert(Class::Snare, vec![(&snare.0, &snare.1)]);
        let pool = Pool { by_class };
        let mut rng = Rng::new(7);
        let lp = make_loop(&pool, &mut rng).expect("a loop");
        assert!(lp.placed.iter().any(|p| p.class == Class::Kick));
        assert!(lp.placed.iter().all(|p| p.at < lp.audio.len()));
        let hits = detect(&lp.audio);
        assert!(!hits.is_empty(), "the detector found nothing");
        let row = row_of(
            Split::Val,
            "loop",
            "v-0".into(),
            "kick".into(),
            -3.0,
            &hits[0],
            Purity::Clean,
            "none",
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hits.csv");
        std::fs::write(&path, format!("{}\n{}\n", csv_header(), csv_row(&row))).unwrap();
        let back = read_rows(&path).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].label, "kick");
        assert_eq!(back[0].split, Split::Val);
        assert!((back[0].features.centroid - row.features.centroid).abs() < 0.01);
        assert_eq!(back[0].features.attack_peaks, row.features.attack_peaks);
    }

    #[test]
    fn the_baseline_counts_other_as_right_when_nothing_is_guessed() {
        let mut cm = Confusion::default();
        cm.add("other", "Unknown");
        cm.add("kick", "snare");
        assert_eq!((cm.total(), cm.correct()), (2, 1));
    }
}
