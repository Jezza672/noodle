//! The waveforms of warped sections, drawn from the audio as it plays.
//!
//! A warped section is not the file's peaks laid across it: markers bend the
//! audio, and Grain and Beats move the energy about. So a worker renders the
//! section the way the engine does, offline (the same warp map, the same
//! stretcher, the same transients), works out its peaks, and the timeline draws
//! those. Until a render is back the section shows the file's own waveform
//! stretched evenly, so it is never blank.
//!
//! A render is kept for as long as a section asks for it, under a key of
//! everything it was worked out from (the file, the window, the warp and the
//! tempo map), so redrawing it costs nothing and an edit renders it again.
//! While one is being edited, only the latest request is rendered.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use noodle_core::{ClipWarp, TempoMap, Tick, WarpMode};
use noodle_engine::TempoTable;
use noodle_io::{Peaks, PeaksBuilder, decode_file, detect_onsets};
use noodle_nodes::{StretchSource, Stretcher, WarpMap};

/// A file longer than this (seconds) is left to the stretched file waveform.
const MAX_FILE_SECONDS: f64 = 1200.0;
/// How many frames of output are rendered at a time.
const CHUNK: usize = 4096;

/// What to render: one section, and everything the render depends on.
#[derive(Clone)]
pub struct Request {
    pub path: PathBuf,
    pub tempo: TempoMap,
    pub start: Tick,
    pub warp: ClipWarp,
    /// The window of the file the section shows, in the file's frames.
    pub offset: u64,
    pub length: u64,
    /// The file's peaks, which identify the load of the file this is for.
    pub file: Arc<Peaks>,
}

impl Request {
    fn key(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.path.hash(&mut hasher);
        (Arc::as_ptr(&self.file) as usize).hash(&mut hasher);
        // Debug output of the parts is a complete, stable description.
        format!(
            "{:?}|{:?}|{:?}|{}|{}",
            self.warp, self.tempo, self.start, self.offset, self.length
        )
        .hash(&mut hasher);
        hasher.finish()
    }
}

enum Entry {
    Working {
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

struct Job {
    key: u64,
    request: Request,
    ctx: egui::Context,
}

type Done = (u64, Option<Arc<Peaks>>);

pub struct WarpedWaves {
    entries: HashMap<u64, Entry>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    /// The keys asked for since the last [`end_frame`](Self::end_frame).
    asked: HashSet<u64>,
    /// The keys still worth rendering, for the worker to check.
    wanted: Arc<Mutex<HashSet<u64>>>,
    #[cfg(test)]
    rendered: usize,
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
                // Ends when the `WarpedWaves` is dropped.
                while let Ok(job) = queue.recv() {
                    // A request for something no longer on the screen, such
                    // as an earlier step of a drag, isn't worth the time.
                    if !known.lock().unwrap().contains(&job.key) {
                        let _ = sender.send((job.key, None));
                        continue;
                    }
                    let peaks = render(&job.request).map(Arc::new);
                    let _ = sender.send((job.key, peaks));
                    job.ctx.request_repaint();
                }
            })
            .expect("a thread");
        Self {
            entries: HashMap::new(),
            jobs,
            done,
            asked: HashSet::new(),
            wanted,
            #[cfg(test)]
            rendered: 0,
        }
    }
}

impl WarpedWaves {
    /// The peaks of the section `request` describes as it plays, once they
    /// have been rendered; asks for the render the first time.
    pub fn get(&mut self, ctx: &egui::Context, request: &Request) -> Option<Arc<Peaks>> {
        self.collect();
        let key = request.key();
        self.asked.insert(key);
        match self.entries.get(&key) {
            Some(Entry::Ready { peaks, .. }) => Some(peaks.clone()),
            Some(_) => None,
            None => {
                self.wanted.lock().unwrap().insert(key);
                self.entries.insert(
                    key,
                    Entry::Working {
                        _file: request.file.clone(),
                    },
                );
                let _ = self.jobs.send(Job {
                    key,
                    request: request.clone(),
                    ctx: ctx.clone(),
                });
                None
            }
        }
    }

    /// Ends a frame: what nothing asked for in it is forgotten.
    pub fn end_frame(&mut self) {
        let asked = std::mem::take(&mut self.asked);
        self.entries.retain(|key, _| asked.contains(key));
        *self.wanted.lock().unwrap() = asked;
    }

    fn collect(&mut self) {
        for (key, peaks) in self.done.try_iter() {
            let Some(entry) = self.entries.get_mut(&key) else {
                continue;
            };
            let Entry::Working { _file } = entry else {
                continue;
            };
            let file = _file.clone();
            *entry = match peaks {
                Some(peaks) => {
                    #[cfg(test)]
                    {
                        self.rendered += 1;
                    }
                    Entry::Ready { peaks, _file: file }
                }
                // Out of date by the time the worker got to it, or the file
                // could not be rendered. Either way it is not asked for again
                // until what it was worked out from changes.
                None => Entry::Failed,
            };
        }
    }

    #[cfg(test)]
    pub fn rendered(&self) -> usize {
        self.rendered
    }
}

/// The file in memory, read through a section's window.
struct Memory {
    samples: Vec<f32>,
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
        let frames = (out.len() / self.channels) as i64;
        let total = (self.samples.len() / self.channels) as i64;
        let from = (self.offset + start).max(0);
        let to = (self.offset + start + frames).min(total);
        if to <= from {
            return;
        }
        let at = ((from - self.offset - start) as usize) * self.channels;
        let source = &self.samples[from as usize * self.channels..to as usize * self.channels];
        out[at..at + source.len()].copy_from_slice(source);
    }
}

/// Renders the section as the engine plays it and returns its peaks: `None`
/// if the file can't be read or is too long.
pub fn render(request: &Request) -> Option<Peaks> {
    let audio = decode_file(&request.path).ok()?;
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
            let onsets = if request.warp.mode == WarpMode::Beats {
                let from = request.offset as usize;
                let window: Vec<f32> = audio.samples
                    [from * channels..(from + length as usize) * channels]
                    .chunks_exact(channels)
                    .map(|f| f.iter().sum::<f32>() / channels as f32)
                    .collect();
                detect_onsets(&window, rate)
            } else {
                Vec::new()
            };
            let source = Memory {
                samples: audio.samples,
                channels,
                offset: request.offset as i64,
            };
            let mut stretcher = Stretcher::new(source, Arc::new(map), &onsets, total, rate as f32);
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
    use noodle_core::WarpMarker;
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

    #[test]
    fn a_render_is_kept_while_asked_for_and_forgotten_when_not() {
        let dir = tempfile::tempdir().unwrap();
        let request = request(dir.path(), &bursts(&[1.0]), ClipWarp::at_tempo(60.0));
        let ctx = egui::Context::default();
        let mut waves = WarpedWaves::default();
        let mut got = None;
        for _ in 0..500 {
            got = waves.get(&ctx, &request);
            waves.end_frame();
            if got.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let first = got.expect("the render arrives");
        assert_eq!(waves.rendered(), 1);
        // Asked again, it is the same render and nothing is rendered twice.
        let again = waves.get(&ctx, &request).unwrap();
        waves.end_frame();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(waves.rendered(), 1);
        // An edit is a new key, rendered afresh; the old one goes.
        let mut edited = request.clone();
        edited.warp.mode = WarpMode::Grain;
        assert!(waves.get(&ctx, &edited).is_none());
        waves.end_frame();
        assert_eq!(waves.entries.len(), 1);
        // Nothing asked for, nothing kept.
        waves.end_frame();
        assert_eq!(waves.entries.len(), 0);
    }
}
