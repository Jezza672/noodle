//! Playing part of an audio file from disk, without the audio thread ever
//! waiting for the disk.
//!
//! A worker thread decodes the file, resamples it to the engine's rate and
//! hands it over in fixed-size chunks through a pair of lock-free queues: full
//! chunks go to the audio thread, and the audio thread returns spent ones to
//! be refilled. The chunks are allocated up front, so the audio side
//! ([`ClipStream`]) never allocates, frees, locks or does I/O.
//!
//! A stream plays one clip: `length` frames of a file from `offset`, both in
//! the file's own frames (the unit an audio clip keeps them in), at the
//! engine's rate. Positions on the stream count in engine frames from the
//! start of the clip. Seeking is a request: the worker restarts from the new
//! position, and chunks it made before are recognised by where they start and
//! dropped (a stale chunk that happens to start where the stream now is holds
//! the right audio, so it is used). Until new chunks arrive, [`ClipStream::read`] returns short and
//! counts an underrun, which is the caller's cue to play silence.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use crate::{DecodeError, Decoder};

/// Engine frames in a chunk.
const CHUNK_FRAMES: usize = 2048;
/// Source frames fed to the resampler at a time.
const RESAMPLE_IN: usize = 1024;
/// Source frames decoded before the wanted position when resampling, so the
/// filter has its context and a seek leaves no transient.
const PREROLL: u64 = 256;
const SINC_LEN: usize = 128;
const IDLE: Duration = Duration::from_millis(2);

/// What to play.
#[derive(Clone, Debug)]
pub struct StreamSpec {
    pub path: PathBuf,
    /// The sample rate to deliver audio at.
    pub rate: u32,
    /// Where in the file the clip starts, in the file's frames.
    pub offset: u64,
    /// How much of the file plays, in the file's frames. Shorter if the file
    /// ends first.
    pub length: u64,
    /// How many chunks to keep ahead of the audio thread, at least 2. Each is
    /// 2048 frames, so 8 is about 340 ms at 48 kHz.
    pub chunks: usize,
}

struct Chunk {
    /// The clip frame (at the engine's rate) of the first frame.
    start: u64,
    frames: usize,
    data: Vec<f32>,
}

struct Shared {
    /// Bumped by each seek, and published after `target`.
    generation: AtomicU64,
    target: AtomicU64,
    stop: AtomicBool,
    underruns: AtomicU64,
    failed: AtomicBool,
    /// The clip frame where the audio really ends, if that is before the
    /// length the clip asked for (a file that is shorter than it said).
    end: AtomicU64,
}

/// The audio-thread end of a stream. Every method is real-time safe.
pub struct ClipStream {
    channels: usize,
    total: u64,
    full: Consumer<Chunk>,
    spent: Producer<Chunk>,
    shared: Arc<Shared>,
    generation: u64,
    current: Option<Chunk>,
    /// Frames of `current` already used.
    used: usize,
    /// The clip frame the next `read` returns.
    position: u64,
}

/// Owns the worker thread. Dropping it stops the worker.
pub struct StreamWorker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// Opens the file and starts the worker, positioned at the clip's start.
/// Opening happens here, off the audio thread, so errors are reported now.
pub fn open_stream(spec: StreamSpec) -> Result<(ClipStream, StreamWorker), DecodeError> {
    let decoder = Decoder::open(&spec.path)?;
    let info = decoder.info();
    let ratio = f64::from(spec.rate) / f64::from(info.sample_rate);
    let available = info
        .frames
        .map_or(spec.length, |f| f.saturating_sub(spec.offset));
    let length = spec.length.min(available);
    let total = (length as f64 * ratio).round() as u64;
    let channels = info.channels;

    let chunks = spec.chunks.max(2);
    let (mut spent_tx, spent_rx) = RingBuffer::new(chunks);
    let (full_tx, full_rx) = RingBuffer::new(chunks);
    for _ in 0..chunks {
        let _ = spent_tx.push(Chunk {
            start: 0,
            frames: 0,
            data: vec![0.0; CHUNK_FRAMES * channels],
        });
    }
    let shared = Arc::new(Shared {
        generation: AtomicU64::new(0),
        target: AtomicU64::new(0),
        stop: AtomicBool::new(false),
        underruns: AtomicU64::new(0),
        failed: AtomicBool::new(false),
        end: AtomicU64::new(u64::MAX),
    });
    let source = Source::new(decoder, spec.offset, length, ratio, total)?;
    let worker = {
        let shared = shared.clone();
        thread::Builder::new()
            .name("noodle-stream".into())
            .spawn(move || run(source, spent_rx, full_tx, &shared))
            .map_err(DecodeError::Io)?
    };
    Ok((
        ClipStream {
            channels,
            total,
            full: full_rx,
            spent: spent_tx,
            shared: shared.clone(),
            generation: 0,
            current: None,
            used: 0,
            position: 0,
        },
        StreamWorker {
            shared,
            thread: Some(worker),
        },
    ))
}

impl Drop for StreamWorker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl ClipStream {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The clip's length in engine frames.
    pub fn total_frames(&self) -> u64 {
        self.total
    }

    /// The clip frame the next [`read`](Self::read) returns.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// How many reads came back short before the end of the clip.
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// True once the worker has hit an error it can't recover from (the file
    /// vanished or is damaged). Reads then return short for good.
    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }

    /// Moves to `frame` of the clip. The audio resumes once the worker has
    /// caught up; until then `read` returns short.
    pub fn seek(&mut self, frame: u64) {
        let frame = frame.min(self.total);
        self.generation += 1;
        self.shared.target.store(frame, Ordering::Relaxed);
        self.shared
            .generation
            .store(self.generation, Ordering::Release);
        if let Some(chunk) = self.current.take() {
            let _ = self.spent.push(chunk);
        }
        self.used = 0;
        self.position = frame;
    }

    /// Fills `out` (interleaved, [`channels`](Self::channels) per frame) with
    /// the next audio and returns the number of frames written. That is fewer
    /// than asked for at the end of the clip, or when the worker hasn't kept
    /// up, in which case an underrun is counted. The caller plays silence for
    /// the rest.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        let channels = self.channels;
        self.total = self.total.min(self.shared.end.load(Ordering::Relaxed));
        let want = (out.len() / channels).min(self.total.saturating_sub(self.position) as usize);
        let mut done = 0;
        while done < want {
            if self.current.is_none() && !self.next_chunk(self.position + done as u64) {
                break;
            }
            let chunk = self.current.as_ref().expect("set above");
            let n = (chunk.frames - self.used).min(want - done);
            out[done * channels..(done + n) * channels]
                .copy_from_slice(&chunk.data[self.used * channels..(self.used + n) * channels]);
            self.used += n;
            done += n;
            if self.used == chunk.frames {
                let chunk = self.current.take().expect("set above");
                let _ = self.spent.push(chunk);
                self.used = 0;
            }
        }
        self.position += done as u64;
        if done < want {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
        done
    }

    /// Takes the next chunk that belongs to the current position.
    fn next_chunk(&mut self, position: u64) -> bool {
        while let Ok(chunk) = self.full.pop() {
            if chunk.start == position {
                self.current = Some(chunk);
                self.used = 0;
                return true;
            }
            let _ = self.spent.push(chunk);
        }
        false
    }
}

fn run(mut source: Source, mut spent: Consumer<Chunk>, mut full: Producer<Chunk>, shared: &Shared) {
    let mut generation = 0;
    // The chunk being filled when the queue of spent ones ran dry.
    let mut spare: Option<Chunk> = None;
    if source.seek(0).is_err() {
        shared.failed.store(true, Ordering::Relaxed);
        return;
    }
    while !shared.stop.load(Ordering::Acquire) {
        let wanted = shared.generation.load(Ordering::Acquire);
        if wanted != generation {
            generation = wanted;
            let target = shared.target.load(Ordering::Relaxed);
            if source.seek(target).is_err() {
                shared.failed.store(true, Ordering::Relaxed);
                return;
            }
        }
        if source.done() {
            thread::sleep(IDLE);
            continue;
        }
        let mut chunk = match spare.take().map(Ok).unwrap_or_else(|| spent.pop()) {
            Ok(chunk) => chunk,
            Err(_) => {
                thread::sleep(IDLE);
                continue;
            }
        };
        chunk.start = source.position();
        match source.fill(&mut chunk.data) {
            Ok(frames) => {
                chunk.frames = frames;
                shared.end.fetch_min(source.total, Ordering::Relaxed);
            }
            Err(_) => {
                shared.failed.store(true, Ordering::Relaxed);
                return;
            }
        }
        if chunk.frames == 0 {
            spare = Some(chunk);
            continue;
        }
        // The queue holds as many slots as chunks exist, so this can't fail.
        let _ = full.push(chunk);
    }
}

/// The worker's side: decoding, trimming to the clip and resampling.
struct Source {
    decoder: Decoder,
    channels: usize,
    /// Clip start and length in file frames.
    offset: u64,
    length: u64,
    ratio: f64,
    total: u64,
    resampler: Option<Async<f32>>,
    /// Decoded, not yet consumed: interleaved file-rate frames.
    input: Vec<f32>,
    input_used: usize,
    /// Resampled, not yet delivered.
    output: Vec<f32>,
    output_used: usize,
    /// File frames still to read for the clip, counted from the next one the
    /// decoder will produce, and file frames of that to drop first.
    to_read: u64,
    to_skip: u64,
    /// Output frames still to drop (the filter's delay and the pre-roll).
    out_skip: usize,
    /// The next clip frame to deliver, and how many remain.
    position: u64,
    flushed: bool,
    scratch: Vec<f32>,
    decoded: Vec<f32>,
}

impl Source {
    fn new(
        decoder: Decoder,
        offset: u64,
        length: u64,
        ratio: f64,
        total: u64,
    ) -> Result<Self, DecodeError> {
        let channels = decoder.info().channels;
        let resampler = if (ratio - 1.0).abs() < 1e-12 {
            None
        } else {
            let params = SincInterpolationParameters {
                sinc_len: SINC_LEN,
                f_cutoff: Some(0.95),
                interpolation: SincInterpolationType::Cubic,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };
            Some(
                Async::<f32>::new_sinc(
                    ratio,
                    1.1,
                    &params,
                    RESAMPLE_IN,
                    channels,
                    FixedAsync::Input,
                )
                .map_err(|e| DecodeError::Unsupported(e.to_string()))?,
            )
        };
        Ok(Self {
            decoder,
            channels,
            offset,
            length,
            ratio,
            total,
            resampler,
            input: Vec::new(),
            input_used: 0,
            output: Vec::new(),
            output_used: 0,
            to_read: 0,
            to_skip: 0,
            out_skip: 0,
            position: 0,
            flushed: false,
            scratch: Vec::new(),
            decoded: Vec::new(),
        })
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn done(&self) -> bool {
        self.position >= self.total
    }

    /// Restarts at `frame` of the clip (in engine frames).
    fn seek(&mut self, frame: u64) -> Result<(), DecodeError> {
        self.position = frame.min(self.total);
        self.input.clear();
        self.input_used = 0;
        self.output.clear();
        self.output_used = 0;
        self.flushed = false;
        // Where in the file the wanted clip frame falls, to a fraction.
        let exact = self.offset as f64 + self.position as f64 / self.ratio;
        let want_abs = (exact.floor() as u64).min(self.offset + self.length);
        let start = if self.resampler.is_some() {
            want_abs.saturating_sub(PREROLL)
        } else {
            want_abs
        };
        let landed = if start == 0 {
            self.decoder.seek(0)?
        } else {
            self.decoder.seek(start)?
        };
        let landed = landed.min(start);
        self.to_skip = start - landed;
        // Read up to the end of the clip, plus a little for the filter's tail.
        let end = self.offset + self.length;
        self.to_read = end.saturating_sub(start);
        if let Some(r) = &mut self.resampler {
            r.reset();
            let delay = r.output_delay();
            let lead = ((exact - start as f64) * self.ratio).round() as usize;
            self.out_skip = delay + lead;
        } else {
            self.out_skip = 0;
        }
        Ok(())
    }

    /// Writes up to a chunk of delivered frames into `data` and returns how
    /// many.
    fn fill(&mut self, data: &mut [f32]) -> Result<usize, DecodeError> {
        let channels = self.channels;
        let want = (data.len() / channels)
            .min(self.total.saturating_sub(self.position) as usize)
            .min(CHUNK_FRAMES);
        while (self.output.len() - self.output_used) / channels < want {
            if !self.produce()? {
                break;
            }
        }
        let have = (self.output.len() - self.output_used) / channels;
        let n = have.min(want);
        data[..n * channels]
            .copy_from_slice(&self.output[self.output_used..self.output_used + n * channels]);
        self.output_used += n * channels;
        if self.output_used == self.output.len() {
            self.output.clear();
            self.output_used = 0;
        }
        self.position += n as u64;
        if n < want {
            // The file ended early (or damaged): end the clip here.
            self.total = self.position;
        }
        Ok(n)
    }

    /// Adds some more output. Returns false when nothing more can come.
    fn produce(&mut self) -> Result<bool, DecodeError> {
        let channels = self.channels;
        let Some(resampler) = self.resampler.as_mut() else {
            // Same rate: decoded frames are the output.
            return Ok(self.read_source_into_output()? > 0);
        };
        let need = resampler.input_frames_next();
        // Gather a full input chunk, or what is left of the clip.
        while (self.input.len() - self.input_used) / channels < need && self.to_read > 0 {
            if self.read_source()? == 0 {
                self.to_read = 0;
            }
        }
        let available = (self.input.len() - self.input_used) / channels;
        if available == 0 && self.flushed {
            return Ok(false);
        }
        let partial = available < need;
        if partial && self.to_read == 0 {
            // The clip's end: one more chunk of silence pushes the tail out.
            if available == 0 {
                self.flushed = true;
            }
        }
        let resampler = self.resampler.as_mut().expect("checked above");
        let out_frames = resampler.output_frames_next();
        self.scratch.resize(out_frames * channels, 0.0);
        let in_len = available.min(need);
        let mut padded;
        let input: &[f32] = if partial {
            padded = vec![0.0; need * channels];
            padded[..in_len * channels]
                .copy_from_slice(&self.input[self.input_used..self.input_used + in_len * channels]);
            &padded
        } else {
            &self.input[self.input_used..self.input_used + need * channels]
        };
        let input = InterleavedSlice::new(input, channels, need)
            .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
        let mut output = InterleavedSlice::new_mut(&mut self.scratch, channels, out_frames)
            .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
        let indexing = Indexing::new().partial_len(in_len);
        let (_, written) = resampler
            .process_into_buffer(&input, &mut output, partial.then_some(&indexing))
            .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
        self.input_used += in_len * channels;
        if self.input_used == self.input.len() {
            self.input.clear();
            self.input_used = 0;
        }
        let skip = self.out_skip.min(written);
        self.out_skip -= skip;
        self.output
            .extend_from_slice(&self.scratch[skip * channels..written * channels]);
        if partial && self.to_read == 0 && available == 0 {
            self.flushed = true;
        }
        Ok(true)
    }

    /// Reads and trims one decoded chunk onto the end of `input`. Returns the
    /// frames added.
    fn read_source(&mut self) -> Result<usize, DecodeError> {
        let channels = self.channels;
        let Some(frames) = self.decoder.read_chunk(&mut self.decoded)? else {
            return Ok(0);
        };
        let chunk = &self.decoded;
        let mut from = 0usize;
        let skip = self.to_skip.min(frames as u64) as usize;
        self.to_skip -= skip as u64;
        from += skip;
        let take = ((frames - from) as u64).min(self.to_read) as usize;
        self.to_read -= take as u64;
        self.input
            .extend_from_slice(&chunk[from * channels..(from + take) * channels]);
        Ok(frames)
    }

    fn read_source_into_output(&mut self) -> Result<usize, DecodeError> {
        let before = self.input.len();
        let frames = loop {
            if self.to_read == 0 {
                return Ok(0);
            }
            let frames = self.read_source()?;
            if frames == 0 {
                self.to_read = 0;
                return Ok(0);
            }
            if self.input.len() > before {
                break frames;
            }
        };
        self.output.extend_from_slice(&self.input[before..]);
        self.input.truncate(before);
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::write_wav;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("noodle-io-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// A stereo file whose left channel counts up and whose right counts down,
    /// so any misplaced frame shows.
    fn ramp_file(name: &str, frames: usize, rate: u32) -> (PathBuf, Vec<f32>) {
        let path = temp(name);
        let samples: Vec<f32> = (0..frames)
            .flat_map(|i| [i as f32 / frames as f32, -(i as f32) / frames as f32])
            .collect();
        write_wav(&path, &samples, 2, rate).unwrap();
        (path, samples)
    }

    fn spec(path: &Path, rate: u32, offset: u64, length: u64) -> StreamSpec {
        StreamSpec {
            path: path.to_path_buf(),
            rate,
            offset,
            length,
            chunks: 4,
        }
    }

    /// Reads `frames` frames, waiting for the worker as needed. Returns
    /// what arrived.
    fn read_all(stream: &mut ClipStream, frames: usize) -> Vec<f32> {
        let channels = stream.channels();
        let mut out = vec![0.0; frames * channels];
        let mut got = 0;
        let start = std::time::Instant::now();
        while got < frames
            && stream.position() < stream.total_frames()
            && start.elapsed() < Duration::from_secs(10)
        {
            let n = stream.read(&mut out[got * channels..]);
            got += n;
            if n == 0 {
                thread::sleep(Duration::from_millis(1));
            }
        }
        out.truncate(got * channels);
        out
    }

    fn assert_same(got: &[f32], want: &[f32]) {
        let first = got.iter().zip(want).position(|(a, b)| a != b);
        assert!(
            got.len() == want.len() && first.is_none(),
            "lengths {} vs {}, first difference at sample {first:?}",
            got.len(),
            want.len()
        );
    }

    #[test]
    fn plays_the_clips_part_of_the_file_exactly() {
        let (path, samples) = ramp_file("stream-exact.wav", 20_000, 48_000);
        let (mut stream, _worker) = open_stream(spec(&path, 48_000, 3_000, 10_000)).unwrap();
        assert_eq!(stream.total_frames(), 10_000);
        let got = read_all(&mut stream, 12_000);
        // 10 000 frames, then the end: nothing more.
        assert_same(&got, &samples[3_000 * 2..13_000 * 2]);
        assert_eq!(stream.read(&mut [0.0; 8]), 0);
    }

    #[test]
    fn a_clip_longer_than_the_file_ends_with_the_file() {
        let (path, samples) = ramp_file("stream-short.wav", 5_000, 44_100);
        let (mut stream, _worker) = open_stream(spec(&path, 44_100, 1_000, 99_999)).unwrap();
        assert_eq!(stream.total_frames(), 4_000);
        assert_same(&read_all(&mut stream, 5_000), &samples[2_000..]);
    }

    #[test]
    fn seeking_gives_the_same_audio_as_playing_through() {
        let (path, samples) = ramp_file("stream-seek.wav", 40_000, 48_000);
        let (mut stream, _worker) = open_stream(spec(&path, 48_000, 500, 30_000)).unwrap();
        read_all(&mut stream, 5_000);
        // Forward past what was buffered, then back to the middle of it.
        for target in [17_321u64, 100, 29_990, 0] {
            stream.seek(target);
            let want = (30_000 - target as usize).min(700);
            let got = read_all(&mut stream, want);
            let from = (500 + target as usize) * 2;
            assert_eq!(got, samples[from..from + want * 2], "seek to {target}");
        }
    }

    #[test]
    fn a_resampled_stream_has_the_right_length_pitch_and_no_gaps() {
        let rate_in = 44_100;
        let path = temp("stream-resample.wav");
        let samples: Vec<f32> = (0..rate_in * 2)
            .map(|i| (std::f32::consts::TAU * 1000.0 * i as f32 / rate_in as f32).sin() * 0.8)
            .collect();
        write_wav(&path, &samples, 1, rate_in as u32).unwrap();
        let (mut stream, _worker) =
            open_stream(spec(&path, 48_000, 0, (rate_in * 2) as u64)).unwrap();
        assert_eq!(stream.total_frames(), 96_000);
        let got = read_all(&mut stream, 100_000);
        assert_eq!(got.len(), 96_000);
        // Compare with the whole-file resampler everywhere but the very ends.
        let reference = crate::resample(
            &crate::Audio {
                samples: samples.clone(),
                channels: 1,
                sample_rate: rate_in as u32,
            },
            48_000,
        )
        .unwrap();
        let worst = (2_000..94_000)
            .map(|i| (got[i] - reference.samples[i]).abs())
            .fold(0.0, f32::max);
        assert!(worst < 0.01, "worst difference {worst}");
    }

    #[test]
    fn seeking_in_a_resampled_stream_is_seamless() {
        let rate_in = 44_100;
        let path = temp("stream-resample-seek.wav");
        let samples: Vec<f32> = (0..rate_in * 2)
            .map(|i| (std::f32::consts::TAU * 700.0 * i as f32 / rate_in as f32).sin() * 0.8)
            .collect();
        write_wav(&path, &samples, 1, rate_in as u32).unwrap();
        let (mut stream, _worker) =
            open_stream(spec(&path, 48_000, 1_000, (rate_in) as u64)).unwrap();
        let through = read_all(&mut stream, 20_000);
        stream.seek(7_777);
        let sought = read_all(&mut stream, 5_000);
        let worst = (0..5_000)
            .map(|i| (sought[i] - through[7_777 + i]).abs())
            .fold(0.0, f32::max);
        // Not on a whole file frame, so up to half an output frame off: this
        // tone moves 0.073 a frame.
        assert!(worst < 0.04, "worst difference {worst}");
        // On a whole file frame (160 output frames is 147 input frames at
        // this ratio) it joins up exactly.
        stream.seek(8_000);
        let sought = read_all(&mut stream, 5_000);
        let worst = (0..5_000)
            .map(|i| (sought[i] - through[8_000 + i]).abs())
            .fold(0.0, f32::max);
        assert!(worst < 0.002, "worst difference {worst}");
    }

    #[test]
    fn a_stalled_worker_is_an_underrun_not_a_wait() {
        let (path, _) = ramp_file("stream-underrun.wav", 20_000, 48_000);
        let (mut stream, worker) = open_stream(spec(&path, 48_000, 0, 20_000)).unwrap();
        drop(worker); // nothing will ever fill the queue again
        // Whatever was buffered may play; after that reads are short, at once.
        let mut buf = vec![0.0; 2 * 4096];
        let mut total = 0;
        for _ in 0..8 {
            total += stream.read(&mut buf);
        }
        assert!(total <= 4 * CHUNK_FRAMES);
        assert!(stream.underruns() > 0);
    }

    #[test]
    fn a_worker_that_finds_the_file_ends_early_shortens_the_clip() {
        let (path, _) = ramp_file("stream-early-end.wav", 20_000, 48_000);
        let (mut stream, _worker) = open_stream(spec(&path, 48_000, 0, 20_000)).unwrap();
        read_all(&mut stream, 3_000);
        // What the worker records when the decoder runs out before the clip.
        stream.shared.end.store(3_500, Ordering::Relaxed);
        let mut out = vec![0.0; 2 * 4_000];
        let before = stream.underruns();
        let got = read_all(&mut stream, 4_000).len() / 2;
        assert_eq!(got, 500);
        assert_eq!(stream.total_frames(), 3_500);
        assert_eq!(stream.read(&mut out), 0);
        assert_eq!(stream.underruns(), before, "the end is not an underrun");
    }

    #[test]
    fn a_file_that_cannot_open_is_an_error_now() {
        assert!(matches!(
            open_stream(spec(&temp("stream-nothing.wav"), 48_000, 0, 10)),
            Err(DecodeError::Io(_))
        ));
    }
}
