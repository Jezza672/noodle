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
pub const DATASET_VERSION: u32 = 1;
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

/// A hit placed in a loop.
#[derive(Clone, Debug)]
pub struct Placed {
    pub at: usize,
    pub class: Class,
    pub file: String,
    /// The loudness it was mixed at (dB, after bleed).
    pub gain_db: f32,
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
        placed.push(Placed {
            at,
            class,
            file: source.file.clone(),
            gain_db,
        });
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
    pub features: HitFeatures,
}

const CSV_HEAD: &str = "split,source,origin,label,onset_s,level_db,gain_db,mixed,truncated,centroid,flatness,low,mid,high,decay,pitched,pitch_key,attack_peaks";

fn csv_header() -> String {
    let mut s = CSV_HEAD.to_string();
    for b in 0..24 {
        write!(s, ",b{b}").unwrap();
    }
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
    s
}

/// Reads `hits.csv` back.
pub fn read_rows(path: &Path) -> Result<Vec<Row>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate().skip(1) {
        let c: Vec<&str> = line.split(',').collect();
        if c.len() != 18 + 24 {
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
            source: if c[1] == "loop" { "loop" } else { "oneshot" },
            origin: c[2].to_string(),
            label: c[3].to_string(),
            onset_s: num(4)?,
            level_db: num(5)?,
            gain_db: num(6)?,
            mixed: c[7] == "1",
            truncated: c[8] == "1",
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
    for s in &sources {
        let pad = (0.1 * RATE as f32) as usize;
        let mut x = vec![0.0; pad];
        x.extend_from_slice(&audio[&s.file]);
        x.extend(std::iter::repeat_n(0.0, pad * 4));
        let hits = detect(&x);
        let Some(hit) = hits
            .iter()
            .find(|h| (h.onset as f64 / f64::from(RATE) - 0.1).abs() < 0.03)
        else {
            undetected += 1;
            continue;
        };
        rows.push(row_of(
            s.split,
            "oneshot",
            s.file.clone(),
            s.class.name().into(),
            0.0,
            hit,
        ));
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
            // Each detection takes the nearest placed hit within MATCH; of
            // several detections near one placed hit only the nearest is
            // labelled, and the rest are spurious.
            let window = MATCH * f64::from(RATE);
            let nearest: Vec<Option<(usize, f64)>> = hits
                .iter()
                .map(|h| {
                    lp.placed
                        .iter()
                        .enumerate()
                        .map(|(k, p)| (k, (p.at as f64 - h.onset as f64).abs()))
                        .filter(|&(_, d)| d <= window)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                })
                .collect();
            let mut owner: Vec<Option<usize>> = vec![None; lp.placed.len()];
            for (j, n) in nearest.iter().enumerate() {
                if let Some((k, d)) = *n
                    && owner[k].is_none_or(|o| d < nearest[o].map_or(f64::MAX, |x| x.1))
                {
                    owner[k] = Some(j);
                }
            }
            let found: Vec<bool> = owner.iter().map(Option::is_some).collect();
            for (j, h) in hits.iter().enumerate() {
                let (label, gain) = match nearest[j] {
                    Some((k, _)) if owner[k] == Some(j) => {
                        (lp.placed[k].class.name(), lp.placed[k].gain_db)
                    }
                    _ => {
                        spurious += 1;
                        ("spurious", 0.0)
                    }
                };
                rows.push(row_of(
                    split,
                    "loop",
                    format!("{}-{i}", split.name()),
                    label.into(),
                    gain,
                    h,
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

fn row_of(
    split: Split,
    source: &'static str,
    origin: String,
    label: String,
    gain_db: f32,
    h: &Hit,
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
        features: h.features.clone(),
    }
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
fn guess_class(labeller: &dyn HitLabeller, row: &Row) -> &'static str {
    let hit = Hit {
        onset: 0,
        start: 0,
        end: 0,
        truncated: row.truncated,
        mixed: row.mixed,
        level_db: row.level_db,
        features: row.features.clone(),
    };
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
