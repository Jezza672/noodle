//! The waveforms of warped sections, drawn from the audio as it plays.
//!
//! A warped section is not the file's peaks laid across it: markers bend the
//! audio, and Grain and Beats move the energy about. So a worker renders the
//! section the way the engine does, offline (the same warp map, the same
//! stretcher, the same transients), works out its peaks, and the timeline draws
//! those. Until a render is back the section shows its previous render, if it
//! has had one (so an edit does not flash the waveform), and otherwise the
//! file's own waveform stretched evenly, so it is never blank.
//!
//! A render is kept under a key of everything it was worked out from (the
//! file, the window, the warp and the tempo map), so redrawing it costs
//! nothing and an edit renders it again. It is kept for a while after the
//! last time a section asked for it, so scrolling a clip out of view and back,
//! or undoing an edit, finds it still there. While one is being edited, only
//! the latest request is rendered. The worker keeps the last file it decoded,
//! so the steps of an edit do not read it again.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use noodle_core::{ClipId, ClipWarp, TempoMap, Tick, WarpMode};
use noodle_engine::TempoTable;
use noodle_io::{Audio, Peaks, PeaksBuilder, decode_file, detect_bass_onsets, detect_onsets};
use noodle_nodes::{StretchSource, Stretcher, WarpMap};

/// A file longer than this (seconds) is left to the stretched file waveform.
const MAX_FILE_SECONDS: f64 = 1200.0;
/// How many frames of output are rendered at a time.
const CHUNK: usize = 4096;
/// How long a render is kept after it was last asked for.
const GRACE: Duration = Duration::from_secs(30);
/// The most renders kept, whatever their age.
const MAX_KEPT: usize = 256;

/// What to render: one section, and everything the render depends on.
#[derive(Clone)]
pub struct Request {
    /// The section, to show its last render while a new one is worked out.
    pub clip: ClipId,
    pub path: PathBuf,
    pub tempo: TempoMap,
    pub start: Tick,
    pub warp: ClipWarp,
    /// The window of the file the section shows, in the file's frames.
    pub offset: u64,
    pub length: u64,
    /// The file's peaks, which identify the load of the file this is for.
    pub file: Arc<Peaks>,
    /// Where the section sits on the timeline, to draw a stand-in render
    /// where it was worked out rather than stretched to the section's new
    /// size. Not part of what is rendered.
    pub span: (Tick, Tick),
}

/// A render to draw, and whether it is the one asked for.
pub struct Shown {
    pub peaks: Arc<Peaks>,
    /// The ticks the peaks were worked out for. Only a stand-in for an edit
    /// that is still being rendered is drawn there: the current render fills
    /// the section.
    pub span: (Tick, Tick),
    pub current: bool,
}

impl Request {
    fn key(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.path.hash(&mut hasher);
        (Arc::as_ptr(&self.file) as usize).hash(&mut hasher);
        // Debug output of the parts is a complete, stable description. Where
        // the section starts only matters when the tempo changes, and the
        // time signature never does.
        let tempos = self.tempo.tempos();
        format!(
            "{:?}|{:?}|{}|{}",
            self.warp, tempos, self.offset, self.length
        )
        .hash(&mut hasher);
        if tempos.len() > 1 {
            self.start.hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// The last render a section had.
struct Kept {
    peaks: Arc<Peaks>,
    span: (Tick, Tick),
    seen: Instant,
}

enum Entry {
    Working {
        /// Which job this is waiting for, so the answer to an older one for
        /// the same key (the entry was forgotten and asked for again) is not
        /// taken for it.
        seq: u64,
        /// Keeps the load of the file alive, so its address can't be reused
        /// by another and make the key match.
        _file: Arc<Peaks>,
    },
    Ready {
        peaks: Arc<Peaks>,
        _file: Arc<Peaks>,
    },
    Failed,
}

struct Slot {
    entry: Entry,
    /// When a section last asked for it.
    seen: Instant,
}

struct Job {
    key: u64,
    seq: u64,
    request: Request,
    ctx: egui::Context,
}

enum Outcome {
    /// Nobody wanted it by the time the worker got to it.
    Stale,
    /// The file could not be rendered.
    Failed,
    Ready(Arc<Peaks>),
}

type Done = (u64, u64, Outcome);

pub struct WarpedWaves {
    entries: HashMap<u64, Slot>,
    /// The last render each section had, for while its next one is worked out.
    previous: HashMap<ClipId, Kept>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    next_seq: u64,
    /// The keys asked for since the last [`end_frame`](Self::end_frame).
    asked: HashSet<u64>,
    /// The keys still worth rendering, for the worker to check.
    wanted: Arc<Mutex<HashSet<u64>>>,
    grace: Duration,
    #[cfg(test)]
    rendered: usize,
}

/// The decoded file the worker last used, with the load of it (see
/// [`Request::file`]) it was read for.
type Decoded = Option<(PathBuf, Arc<Peaks>, Arc<Audio>)>;

fn audio_of(cache: &mut Decoded, request: &Request) -> Option<Arc<Audio>> {
    if let Some((path, file, audio)) = cache
        && *path == request.path
        && Arc::ptr_eq(file, &request.file)
    {
        return Some(audio.clone());
    }
    *cache = None;
    let audio = Arc::new(decode_file(&request.path).ok()?);
    *cache = Some((request.path.clone(), request.file.clone(), audio.clone()));
    Some(audio)
}

impl Default for WarpedWaves {
    fn default() -> Self {
        let (jobs, queue) = channel::<Job>();
        let (sender, done) = channel::<Done>();
        let wanted: Arc<Mutex<HashSet<u64>>> = Arc::default();
        let known = wanted.clone();
        std::thread::Builder::new()
            .name("noodle-warped-waves".into())
            .spawn(move || {
                let mut decoded: Decoded = None;
                // Ends when the `WarpedWaves` is dropped.
                while let Ok(job) = queue.recv() {
                    // A request for something no longer on the screen, such
                    // as an earlier step of a drag, isn't worth the time.
                    let outcome = if known.lock().unwrap().contains(&job.key) {
                        audio_of(&mut decoded, &job.request)
                            .and_then(|audio| render_audio(&job.request, &audio))
                            .map_or(Outcome::Failed, |peaks| Outcome::Ready(Arc::new(peaks)))
                    } else {
                        Outcome::Stale
                    };
                    let _ = sender.send((job.key, job.seq, outcome));
                    job.ctx.request_repaint();
                }
            })
            .expect("a thread");
        Self {
            entries: HashMap::new(),
            previous: HashMap::new(),
            jobs,
            done,
            next_seq: 0,
            asked: HashSet::new(),
            wanted,
            grace: GRACE,
            #[cfg(test)]
            rendered: 0,
        }
    }
}

impl WarpedWaves {
    /// The peaks of the section `request` describes as it plays, once they
    /// have been rendered; asks for the render the first time. While it is
    /// worked out, the section's previous render stands in for it.
    #[cfg(test)]
    pub fn get(&mut self, ctx: &egui::Context, request: &Request) -> Option<Arc<Peaks>> {
        self.shown(ctx, request).map(|shown| shown.peaks)
    }

    /// [`get`](Self::get), with where the peaks belong and whether they are
    /// the render of `request` itself.
    pub fn shown(&mut self, ctx: &egui::Context, request: &Request) -> Option<Shown> {
        self.collect();
        let now = Instant::now();
        let key = request.key();
        self.asked.insert(key);
        match self.entries.get_mut(&key) {
            Some(slot) => {
                slot.seen = now;
                match &slot.entry {
                    Entry::Ready { peaks, .. } => {
                        let peaks = peaks.clone();
                        self.previous.insert(
                            request.clip,
                            Kept {
                                peaks: peaks.clone(),
                                span: request.span,
                                seen: now,
                            },
                        );
                        return Some(Shown {
                            peaks,
                            span: request.span,
                            current: true,
                        });
                    }
                    Entry::Failed => return None,
                    Entry::Working { .. } => {}
                }
            }
            None => {
                self.wanted.lock().unwrap().insert(key);
                self.next_seq += 1;
                let seq = self.next_seq;
                self.entries.insert(
                    key,
                    Slot {
                        entry: Entry::Working {
                            seq,
                            _file: request.file.clone(),
                        },
                        seen: now,
                    },
                );
                let _ = self.jobs.send(Job {
                    key,
                    seq,
                    request: request.clone(),
                    ctx: ctx.clone(),
                });
            }
        }
        self.previous.get_mut(&request.clip).map(|kept| {
            kept.seen = now;
            Shown {
                peaks: kept.peaks.clone(),
                span: kept.span,
                current: false,
            }
        })
    }

    /// Ends a frame: what nothing has asked for lately is forgotten.
    pub fn end_frame(&mut self) {
        let asked = std::mem::take(&mut self.asked);
        let grace = self.grace;
        self.entries
            .retain(|key, slot| asked.contains(key) || slot.seen.elapsed() < grace);
        if self.entries.len() > MAX_KEPT {
            let mut seen: Vec<Instant> = self.entries.values().map(|s| s.seen).collect();
            seen.sort_unstable();
            let oldest_kept = seen[seen.len() - MAX_KEPT];
            self.entries
                .retain(|key, slot| asked.contains(key) || slot.seen >= oldest_kept);
        }
        self.previous.retain(|_, kept| kept.seen.elapsed() < grace);
        *self.wanted.lock().unwrap() = asked;
    }

    fn collect(&mut self) {
        for (key, answered, outcome) in self.done.try_iter() {
            let Some(slot) = self.entries.get_mut(&key) else {
                continue;
            };
            let Entry::Working { seq, _file } = &slot.entry else {
                continue;
            };
            if *seq != answered {
                continue;
            }
            let file = _file.clone();
            match outcome {
                Outcome::Ready(peaks) => {
                    #[cfg(test)]
                    {
                        self.rendered += 1;
                    }
                    slot.entry = Entry::Ready { peaks, _file: file };
                }
                // Out of date by the time the worker got to it: forgotten, so
                // that if it is wanted again after all it is rendered then.
                Outcome::Stale => {
                    self.entries.remove(&key);
                }
                // Not asked for again until what it was worked out from
                // changes.
                Outcome::Failed => slot.entry = Entry::Failed,
            }
        }
    }

    #[cfg(test)]
    pub fn rendered(&self) -> usize {
        self.rendered
    }
}

/// The file in memory, read through a section's window.
struct Memory {
    audio: Arc<Audio>,
    channels: usize,
    /// The window's first frame in the file.
    offset: i64,
}

impl StretchSource for Memory {
    fn channels(&self) -> usize {
        self.channels
    }

    fn read(&mut self, start: i64, out: &mut [f32]) {
        out.fill(0.0);
        let samples = &self.audio.samples;
        let frames = (out.len() / self.channels) as i64;
        let total = (samples.len() / self.channels) as i64;
        let from = (self.offset + start).max(0);
        let to = (self.offset + start + frames).min(total);
        if to <= from {
            return;
        }
        let at = ((from - self.offset - start) as usize) * self.channels;
        let source = &samples[from as usize * self.channels..to as usize * self.channels];
        out[at..at + source.len()].copy_from_slice(source);
    }
}

/// Renders the section as the engine plays it and returns its peaks: `None`
/// if the file can't be read or is too long.
#[cfg(test)]
pub fn render(request: &Request) -> Option<Peaks> {
    render_audio(request, &Arc::new(decode_file(&request.path).ok()?))
}

/// [`render`], for a file already decoded.
fn render_audio(request: &Request, audio: &Arc<Audio>) -> Option<Peaks> {
    let channels = audio.channels.max(1);
    let rate = audio.sample_rate;
    let frames = (audio.samples.len() / channels) as u64;
    if frames as f64 / f64::from(rate) > MAX_FILE_SECONDS || request.offset >= frames {
        return None;
    }
    let length = request.length.min(frames - request.offset).max(1);
    let table = TempoTable::new(&request.tempo, rate as f32);
    let map = WarpMap::build(
        &request.warp.curve(),
        &table,
        request.start,
        f64::from(rate),
        request.offset as f64 / f64::from(rate),
        length as f64 / f64::from(rate),
        length,
    );
    let total = (map.out_len().round() as u64).max(1);
    let mut builder = PeaksBuilder::new(
        if request.warp.mode == WarpMode::Repitch {
            1
        } else {
            2
        },
        rate,
    );
    match request.warp.mode {
        WarpMode::Repitch => {
            // Plain resampling: every output frame reads the source where the
            // map says, between its two nearest frames.
            let mono: Vec<f32> = audio
                .samples
                .chunks_exact(channels)
                .map(|f| f.iter().sum::<f32>() / channels as f32)
                .collect();
            let at = |frame: i64| -> f32 {
                usize::try_from(request.offset as i64 + frame)
                    .ok()
                    .and_then(|i| mono.get(i))
                    .copied()
                    .unwrap_or(0.0)
            };
            let mut chunk = Vec::with_capacity(CHUNK);
            for start in (0..total).step_by(CHUNK) {
                chunk.clear();
                for out in start..(start + CHUNK as u64).min(total) {
                    let source = map.source_at(out as f64);
                    let whole = source.floor();
                    let fraction = (source - whole) as f32;
                    let a = at(whole as i64);
                    let b = at(whole as i64 + 1);
                    chunk.push(a + (b - a) * fraction);
                }
                builder.push(&chunk);
            }
        }
        WarpMode::Grain | WarpMode::Beats => {
            // As the engine finds them: the bass transients for both modes,
            // those of the whole mix for Beats only.
            let from = request.offset as usize;
            let window: Vec<f32> = audio.samples
                [from * channels..(from + length as usize) * channels]
                .chunks_exact(channels)
                .map(|f| f.iter().sum::<f32>() / channels as f32)
                .collect();
            let bass = detect_bass_onsets(&window, rate);
            let all = if request.warp.mode == WarpMode::Beats {
                detect_onsets(&window, rate)
            } else {
                Vec::new()
            };
            let source = Memory {
                audio: audio.clone(),
                channels,
                offset: request.offset as i64,
            };
            let mut stretcher =
                Stretcher::new(source, Arc::new(map), (&all, &bass), total, rate as f32);
            let mut buffer = vec![0.0f32; CHUNK * 2];
            loop {
                let n = stretcher.read(&mut buffer);
                builder.push(&buffer[..n * 2]);
                if n < CHUNK {
                    break;
                }
            }
        }
    }
    Some(builder.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use noodle_core::{ClipId, WarpMarker};
    use noodle_io::write_wav;

    const RATE: u32 = 48_000;

    /// Four seconds of silence with a loud 0.2 s burst at each of `at`.
    fn bursts(at: &[f64]) -> Vec<f32> {
        let mut out = vec![0.0f32; 4 * RATE as usize];
        for &t in at {
            let start = (t * f64::from(RATE)) as usize;
            for i in 0..(0.2 * f64::from(RATE)) as usize {
                out[start + i] = 0.8 * (i as f32 * 0.3).sin();
            }
        }
        out
    }

    fn request(dir: &std::path::Path, samples: &[f32], warp: ClipWarp) -> Request {
        let path = dir.join("a.wav");
        write_wav(&path, samples, 1, RATE).unwrap();
        Request {
            clip: ClipId(1),
            path,
            tempo: TempoMap::default(),
            start: Tick::ZERO,
            warp,
            offset: 0,
            length: samples.len() as u64,
            file: Arc::new(Peaks::from_audio(&noodle_io::Audio {
                samples: samples.to_vec(),
                channels: 1,
                sample_rate: RATE,
            })),
            span: (Tick::ZERO, Tick(1920)),
        }
    }

    /// Where, in seconds of the render, the loudest column is.
    fn loudest(peaks: &Peaks) -> f64 {
        let columns = peaks.columns(None, 0, peaks.frames(), 400);
        let (at, _) = columns
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.max.total_cmp(&b.1.max))
            .unwrap();
        (at as f64 + 0.5) / 400.0 * peaks.frames() as f64 / f64::from(RATE)
    }

    #[test]
    fn a_section_at_twice_the_speed_renders_half_as_long() {
        let dir = tempfile::tempdir().unwrap();
        // A 60 bpm clip on the 120 bpm timeline plays twice as fast.
        let request = request(dir.path(), &bursts(&[2.0]), ClipWarp::at_tempo(60.0));
        let peaks = render(&request).expect("a render");
        assert!(
            (peaks.frames() as f64 - 2.0 * f64::from(RATE)).abs() < 5.0,
            "{}",
            peaks.frames()
        );
        // The burst at 2 s of the file is at 1 s of the render.
        assert!((loudest(&peaks) - 1.1).abs() < 0.15, "{}", loudest(&peaks));
    }

    #[test]
    fn a_marker_moves_the_audio_in_the_render() {
        let dir = tempfile::tempdir().unwrap();
        // At 120 bpm the burst at 1 s is at beat 2, and with the start
        // pinned to beat 0 and the burst to beat 4 the first second is
        // spread over two: the burst plays at 2 s.
        let mut warp = ClipWarp::at_tempo(120.0);
        for (seconds, beat) in [(0.0, 0.0), (1.0, 4.0)] {
            warp.markers.push(WarpMarker { seconds, beat });
        }
        let request = request(dir.path(), &bursts(&[1.0]), warp);
        let peaks = render(&request).expect("a render");
        let at = loudest(&peaks);
        assert!((at - 2.1).abs() < 0.15, "{at}");
    }

    #[test]
    fn grain_and_beats_render_the_stretched_audio_too() {
        let dir = tempfile::tempdir().unwrap();
        for mode in [WarpMode::Grain, WarpMode::Beats] {
            // Half speed: 240 bpm clip... the other way round: a 240 bpm clip
            // on a 120 bpm timeline plays at half speed, twice as long.
            let mut warp = ClipWarp::at_tempo(240.0);
            warp.mode = mode;
            let request = request(dir.path(), &bursts(&[1.0, 2.5]), warp);
            let peaks = render(&request).expect("a render");
            assert!(
                (peaks.frames() as f64 - 8.0 * f64::from(RATE)).abs() < 5.0,
                "{mode:?} {}",
                peaks.frames()
            );
            // Pitch-keeping stretch moves the bursts to 2 s and 5 s.
            let columns = peaks.columns(None, 0, peaks.frames(), 800);
            let loud = |t: f64| {
                let at = (t / 8.0 * 800.0) as usize;
                columns[at - 3..at + 3]
                    .iter()
                    .map(|p| p.max)
                    .fold(0.0f32, f32::max)
            };
            assert!(loud(2.1) > 0.3, "{mode:?} burst at 2 s: {}", loud(2.1));
            assert!(loud(5.1) > 0.3, "{mode:?} burst at 5 s: {}", loud(5.1));
            assert!(loud(3.8) < 0.05, "{mode:?} quiet between: {}", loud(3.8));
        }
    }

    #[test]
    fn a_file_that_cant_be_read_has_no_render() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = request(dir.path(), &bursts(&[1.0]), ClipWarp::at_tempo(60.0));
        request.path = dir.path().join("missing.wav");
        assert!(render(&request).is_none());
    }

    /// Asks for `request` every 10 ms until its render arrives.
    fn wait_for(waves: &mut WarpedWaves, ctx: &egui::Context, request: &Request) -> Arc<Peaks> {
        for _ in 0..1000 {
            let got = waves.get(ctx, request);
            waves.end_frame();
            if let Some(got) = got
                && matches!(
                    waves.entries.get(&request.key()),
                    Some(Slot {
                        entry: Entry::Ready { .. },
                        ..
                    })
                )
            {
                return got;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the render never arrived");
    }

    #[test]
    fn a_render_is_kept_while_asked_for_and_forgotten_after_a_while() {
        let dir = tempfile::tempdir().unwrap();
        let request = request(dir.path(), &bursts(&[1.0]), ClipWarp::at_tempo(60.0));
        let ctx = egui::Context::default();
        let mut waves = WarpedWaves::default();
        let first = wait_for(&mut waves, &ctx, &request);
        assert_eq!(waves.rendered(), 1);
        // Asked again, it is the same render and nothing is rendered twice.
        let again = waves.get(&ctx, &request).unwrap();
        waves.end_frame();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(waves.rendered(), 1);
        // An edit is a new key, rendered afresh; the old one is kept for a
        // while, in case it is wanted back (an undo).
        let mut edited = request.clone();
        edited.warp.mode = WarpMode::Grain;
        waves.get(&ctx, &edited);
        waves.end_frame();
        assert_eq!(waves.entries.len(), 2);
        let back = waves.get(&ctx, &request).unwrap();
        assert!(Arc::ptr_eq(&first, &back));
        // With no grace, what nothing asked for goes.
        waves.grace = Duration::ZERO;
        waves.end_frame();
        waves.end_frame();
        assert_eq!(waves.entries.len(), 0);
    }

    #[test]
    fn an_edited_section_keeps_showing_its_last_render_until_the_next_arrives() {
        let dir = tempfile::tempdir().unwrap();
        let request = request(dir.path(), &bursts(&[1.0]), ClipWarp::at_tempo(60.0));
        let ctx = egui::Context::default();
        let mut waves = WarpedWaves::default();
        let first = wait_for(&mut waves, &ctx, &request);
        let mut edited = request.clone();
        edited.warp.mode = WarpMode::Grain;
        // At once, before any work could have been done, it is the old one.
        let shown = waves.get(&ctx, &edited).expect("the previous render");
        assert!(Arc::ptr_eq(&first, &shown));
        waves.end_frame();
        let next = wait_for(&mut waves, &ctx, &edited);
        assert!(!Arc::ptr_eq(&first, &next));
        // The stand-in knows where it was worked out, so a resize can draw it
        // there instead of stretching it to the new size.
        let mut resized = edited.clone();
        resized.warp.mode = WarpMode::Beats;
        resized.span = (Tick::ZERO, Tick(960));
        let stand_in = waves.shown(&ctx, &resized).expect("the stand-in");
        assert!(!stand_in.current);
        assert_eq!(stand_in.span, (Tick::ZERO, Tick(1920)));
        let done = wait_for(&mut waves, &ctx, &resized);
        let current = waves.shown(&ctx, &resized).unwrap();
        assert!(current.current && Arc::ptr_eq(&current.peaks, &done));
        // A different section has no such stand-in.
        let mut other = edited.clone();
        other.clip = ClipId(2);
        other.offset = 1;
        assert!(waves.get(&ctx, &other).is_none());
    }

    #[test]
    fn where_a_section_starts_only_matters_when_the_tempo_changes() {
        let dir = tempfile::tempdir().unwrap();
        let steady = request(dir.path(), &bursts(&[1.0]), ClipWarp::at_tempo(60.0));
        let mut moved = steady.clone();
        moved.start = Tick(960);
        assert_eq!(steady.key(), moved.key());
        // With a tempo change on the timeline it does.
        let mut varying = steady.clone();
        varying.tempo = TempoMap::new(
            vec![
                noodle_core::TempoChange {
                    tick: Tick::ZERO,
                    bpm: 120.0,
                },
                noodle_core::TempoChange {
                    tick: Tick(1920),
                    bpm: 90.0,
                },
            ],
            varying.tempo.signatures().to_vec(),
        )
        .unwrap();
        let mut varying_moved = varying.clone();
        varying_moved.start = Tick(960);
        assert_ne!(varying.key(), varying_moved.key());
    }

    #[test]
    fn an_answer_to_an_older_job_for_a_key_is_not_taken_for_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let request = request(dir.path(), &bursts(&[1.0]), ClipWarp::at_tempo(60.0));
        let ctx = egui::Context::default();
        let mut waves = WarpedWaves::default();
        waves.get(&ctx, &request);
        waves.end_frame();
        // Forgotten, then asked for again, while the first job is out.
        waves.grace = Duration::ZERO;
        waves.end_frame();
        waves.grace = GRACE;
        assert!(waves.entries.is_empty());
        waves.get(&ctx, &request);
        let key = request.key();
        // The first job's answer is that nobody wanted it. It arrives after
        // the second was asked for, and must not cancel it.
        waves.jobs = channel().0;
        let (sender, done) = channel();
        waves.done = done;
        sender.send((key, 1, Outcome::Stale)).unwrap();
        waves.collect();
        assert!(matches!(
            waves.entries.get(&key).map(|s| &s.entry),
            Some(Entry::Working { seq: 2, .. })
        ));
    }
}
