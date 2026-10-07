//! Recording the live input to a WAV file.
//!
//! The input callback can't touch the disk, so recording crosses to a writer
//! thread the way input crosses to the output: through a lock-free ring.
//! [`RecordTap`] sits inside the input callback ([`Capture`](crate::Capture))
//! and queues samples while recording is on, and the thread started by
//! [`Recorder::start`] writes them out. The tap is always there and costs one
//! relaxed load while idle, so starting a recording never touches the
//! stream. Recording takes its own copy of the input, so it carries on if the
//! output stalls.
//!
//! The file is 32-bit float, at the input's channel count and the engine's
//! sample rate, and its header is kept valid about once a second, so a crash
//! loses at most that much.

use std::fmt;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::{FromSample, Sample};
use hound::{SampleFormat, WavSpec, WavWriter};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::WavError;

/// How much the ring holds, in seconds. The writer drains it every few
/// milliseconds, so this only fills if the disk stalls.
const RING_SECONDS: f32 = 2.0;

/// How long the writer sleeps when there's nothing to write.
const POLL: Duration = Duration::from_millis(10);

/// How often the writer brings the file's header up to date, in seconds of
/// audio.
const FLUSH_SECONDS: f32 = 1.0;

/// Why a recording failed.
#[derive(Debug)]
pub enum RecordError {
    /// There's no input to record: it's off, or it couldn't be opened.
    NoInput,
    /// A recording is already running.
    Busy,
    /// There is no recording to stop.
    NotRecording,
    /// The file couldn't be written.
    File(WavError),
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoInput => write!(f, "there is no input to record"),
            Self::Busy => write!(f, "already recording"),
            Self::NotRecording => write!(f, "not recording"),
            Self::File(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RecordError {}

impl From<WavError> for RecordError {
    fn from(error: WavError) -> Self {
        Self::File(error)
    }
}

/// A finished recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Take {
    pub path: PathBuf,
    /// Frames in the file.
    pub frames: u64,
    pub channels: usize,
    pub sample_rate: u32,
    /// Frames of input that didn't reach the file because the writer fell
    /// behind. Zero unless the disk stalled for about two seconds.
    pub dropped: u64,
}

/// The input callback's end: queues samples while recording is on.
pub struct RecordTap {
    producer: Producer<f32>,
    armed: Arc<AtomicBool>,
    /// Samples that didn't fit.
    dropped: Arc<AtomicU64>,
}

impl RecordTap {
    /// Queues interleaved input, in whole frames, if recording is on. Real-time
    /// safe, like [`Capture::capture`](crate::Capture::capture).
    pub fn push<T: Sample>(&mut self, input: &[T])
    where
        f32: FromSample<T>,
    {
        if !self.armed.load(Ordering::Relaxed) {
            return;
        }
        let fits = input.len().min(self.producer.slots());
        if fits < input.len() {
            self.dropped
                .fetch_add((input.len() - fits) as u64, Ordering::Relaxed);
        }
        if let Ok(mut chunk) = self.producer.write_chunk_uninit(fits) {
            let (first, second) = chunk.as_mut_slices();
            for (slot, &x) in first.iter_mut().chain(second).zip(input) {
                slot.write(f32::from_sample(x));
            }
            // SAFETY: every slot was written above: `input` has at least
            // `fits` samples.
            unsafe { chunk.commit_all() };
        }
    }
}

/// The recording on its way to disk.
struct Active {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Written>,
}

/// What the writer thread hands back when it ends.
struct Written {
    consumer: Consumer<f32>,
    frames: u64,
    result: Result<(), WavError>,
}

/// Starts and stops recording of one input stream.
pub struct Recorder {
    armed: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    channels: usize,
    sample_rate: u32,
    flush_every: usize,
    /// The ring's reading end, while no writer has it.
    idle: Option<Consumer<f32>>,
    active: Option<Active>,
}

/// Makes the two ends of a recording path for `channels` channels at
/// `sample_rate`.
pub fn record_path(channels: usize, sample_rate: f32) -> (RecordTap, Recorder) {
    let ring_frames = (sample_rate * RING_SECONDS) as usize;
    let flush_frames = (sample_rate * FLUSH_SECONDS) as usize;
    record_path_with(channels, sample_rate, ring_frames, flush_frames)
}

fn record_path_with(
    channels: usize,
    sample_rate: f32,
    ring_frames: usize,
    flush_frames: usize,
) -> (RecordTap, Recorder) {
    let (producer, consumer) = RingBuffer::new(ring_frames * channels);
    let armed = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let tap = RecordTap {
        producer,
        armed: Arc::clone(&armed),
        dropped: Arc::clone(&dropped),
    };
    let recorder = Recorder {
        armed,
        dropped,
        channels,
        sample_rate: sample_rate as u32,
        flush_every: (flush_frames * channels).max(1),
        idle: Some(consumer),
        active: None,
    };
    (tap, recorder)
}

impl Recorder {
    /// Whether a recording is running. It stops being true if the writer
    /// fails, and [`stop`](Self::stop) then says why.
    pub fn is_recording(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| !active.thread.is_finished())
    }

    /// Starts recording to a new WAV file at `path`, replacing any file
    /// there. Input from before this call isn't included.
    pub fn start(&mut self, path: &Path) -> Result<(), RecordError> {
        if self.active.is_some() {
            return Err(RecordError::Busy);
        }
        let spec = WavSpec {
            channels: u16::try_from(self.channels).map_err(|_| WavError::Unsupported)?,
            sample_rate: self.sample_rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        // Create the file here, so a bad path fails the call.
        let writer = WavWriter::create(path, spec)?;
        let Some(mut consumer) = self.idle.take() else {
            return Err(RecordError::Busy);
        };
        // Input that was queued after the last recording stopped.
        if let Ok(stale) = consumer.read_chunk(consumer.slots()) {
            stale.commit_all();
        }
        self.dropped.store(0, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let flush_every = self.flush_every;
        let thread = std::thread::Builder::new()
            .name("noodle-record".into())
            .spawn({
                let stop = Arc::clone(&stop);
                move || write_out(writer, consumer, &stop, flush_every)
            })
            .map_err(WavError::IoError)?;
        self.active = Some(Active {
            path: path.to_owned(),
            stop,
            thread,
        });
        self.armed.store(true, Ordering::Release);
        Ok(())
    }

    /// Stops recording and finishes the file.
    pub fn stop(&mut self) -> Result<Take, RecordError> {
        let Some(active) = self.active.take() else {
            return Err(RecordError::NotRecording);
        };
        self.armed.store(false, Ordering::Release);
        active.stop.store(true, Ordering::Release);
        let written = active
            .thread
            .join()
            .expect("the writer thread doesn't panic");
        self.idle = Some(written.consumer);
        written.result?;
        let channels = self.channels;
        let dropped = self.dropped.load(Ordering::Relaxed) / channels as u64;
        Ok(Take {
            path: active.path,
            frames: written.frames,
            channels,
            sample_rate: self.sample_rate,
            dropped,
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Finish the file, so it's readable.
        let _ = self.stop();
    }
}

/// The writer thread: copies the ring into the file until told to stop, then
/// finishes the file.
fn write_out(
    mut writer: WavWriter<BufWriter<File>>,
    mut consumer: Consumer<f32>,
    stop: &AtomicBool,
    flush_every: usize,
) -> Written {
    let mut samples = 0u64;
    let mut since_flush = 0usize;
    let mut result = Ok(());
    let mut drain = |writer: &mut WavWriter<BufWriter<File>>, consumer: &mut Consumer<f32>| {
        let available = consumer.slots();
        if available == 0 {
            return Ok(false);
        }
        let Ok(chunk) = consumer.read_chunk(available) else {
            return Ok(false);
        };
        let (first, second) = chunk.as_slices();
        for &sample in first.iter().chain(second) {
            writer.write_sample(sample)?;
        }
        samples += available as u64;
        since_flush += available;
        chunk.commit_all();
        if since_flush >= flush_every {
            // Keeps the file valid up to here.
            writer.flush()?;
            since_flush = 0;
        }
        Ok::<_, WavError>(true)
    };
    while !stop.load(Ordering::Acquire) {
        match drain(&mut writer, &mut consumer) {
            Ok(true) => {}
            Ok(false) => std::thread::sleep(POLL),
            Err(error) => {
                result = Err(error);
                break;
            }
        }
    }
    if result.is_ok() {
        // A callback that began before recording was switched off may still
        // be queueing its block. It takes microseconds, but wait a moment.
        std::thread::sleep(POLL);
        while let Ok(true) = drain(&mut writer, &mut consumer) {}
    }
    let channels = u64::from(writer.spec().channels);
    let finished = writer.finalize();
    if result.is_ok() {
        result = finished;
    }
    Written {
        consumer,
        frames: samples / channels,
        result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_wav;

    const RATE: f32 = 1000.0;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("noodle-io-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// Waits for `condition`, which depends on the writer thread.
    fn eventually(condition: impl Fn() -> bool) {
        for _ in 0..500 {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out");
    }

    #[test]
    fn a_recording_is_what_the_input_delivered() {
        let path = temp("recorded.wav");
        let (mut tap, mut recorder) = record_path(2, RATE);
        recorder.start(&path).unwrap();
        // As cpal delivers it: converted to f32 in the callback.
        tap.push(&[i16::MAX, 0, i16::MIN, 16_384]);
        tap.push(&[0.25f32, -0.25]);
        let take = recorder.stop().unwrap();
        assert_eq!(
            (take.frames, take.channels, take.sample_rate, take.dropped),
            (3, 2, 1000, 0)
        );
        assert_eq!(take.path, path);
        let audio = read_wav(&path).unwrap();
        let max = f32::from_sample(i16::MAX);
        assert_eq!(audio.samples, [max, 0.0, -1.0, 0.5, 0.25, -0.25]);
        assert_eq!((audio.channels, audio.sample_rate), (2, 1000));
    }

    #[test]
    fn nothing_is_recorded_outside_a_recording() {
        let path = temp("outside.wav");
        let (mut tap, mut recorder) = record_path(1, RATE);
        tap.push(&[1.0f32, 2.0]);
        recorder.start(&path).unwrap();
        tap.push(&[3.0f32]);
        recorder.stop().unwrap();
        tap.push(&[4.0f32]);
        // The next recording starts clean, too.
        recorder.start(&path).unwrap();
        tap.push(&[5.0f32]);
        recorder.stop().unwrap();
        assert_eq!(read_wav(&path).unwrap().samples, [5.0]);
    }

    #[test]
    fn an_idle_tap_queues_nothing() {
        let (mut tap, recorder) = record_path(1, RATE);
        tap.push(&[1.0f32, 2.0]);
        assert_eq!(recorder.idle.as_ref().unwrap().slots(), 0);
    }

    #[test]
    fn a_block_that_lands_after_stopping_is_not_in_the_next_recording() {
        let path = temp("late-block.wav");
        let (mut tap, mut recorder) = record_path(1, RATE);
        recorder.start(&path).unwrap();
        recorder.stop().unwrap();
        // A callback that read the flag just before it was cleared, and
        // queued its block after the writer's last look.
        recorder.armed.store(true, Ordering::Relaxed);
        tap.push(&[9.0f32]);
        recorder.armed.store(false, Ordering::Relaxed);
        recorder.start(&path).unwrap();
        tap.push(&[1.0f32]);
        recorder.stop().unwrap();
        assert_eq!(read_wav(&path).unwrap().samples, [1.0]);
    }

    #[test]
    fn only_one_recording_runs_at_a_time() {
        let (_tap, mut recorder) = record_path(1, RATE);
        assert!(matches!(recorder.stop(), Err(RecordError::NotRecording)));
        recorder.start(&temp("busy.wav")).unwrap();
        assert!(recorder.is_recording());
        assert!(matches!(
            recorder.start(&temp("busy-too.wav")),
            Err(RecordError::Busy)
        ));
        recorder.stop().unwrap();
        assert!(!recorder.is_recording());
    }

    #[test]
    fn a_bad_path_fails_the_start_and_leaves_the_recorder_usable() {
        let (mut tap, mut recorder) = record_path(1, RATE);
        let nowhere = temp("no-such-directory").join("file.wav");
        assert!(matches!(
            recorder.start(&nowhere),
            Err(RecordError::File(_))
        ));
        assert!(!recorder.is_recording());
        let path = temp("after-a-bad-path.wav");
        recorder.start(&path).unwrap();
        tap.push(&[1.0f32]);
        assert_eq!(recorder.stop().unwrap().frames, 1);
    }

    #[test]
    fn input_the_writer_cannot_take_is_counted() {
        let path = temp("overflow.wav");
        // Room for 8 frames.
        let (mut tap, mut recorder) = record_path_with(2, RATE, 8, 1000);
        recorder.start(&path).unwrap();
        let frames: Vec<f32> = (0..30).map(|x| x as f32).collect();
        tap.push(&frames);
        let take = recorder.stop().unwrap();
        // 15 frames were offered. What fit is the start of it.
        assert_eq!((take.frames, take.dropped), (8, 7));
        assert_eq!(read_wav(&path).unwrap().samples, &frames[..16]);
    }

    #[test]
    fn the_file_is_valid_while_it_is_still_recording() {
        let path = temp("mid-recording.wav");
        // Flush after every frame.
        let (mut tap, mut recorder) = record_path_with(1, RATE, 1000, 1);
        recorder.start(&path).unwrap();
        tap.push(&[0.5f32; 10]);
        eventually(|| read_wav(&path).is_ok_and(|audio| audio.samples.len() == 10));
        assert!(recorder.is_recording());
        recorder.stop().unwrap();
    }

    #[test]
    fn dropping_the_recorder_finishes_the_file() {
        let path = temp("dropped.wav");
        let (mut tap, mut recorder) = record_path(1, RATE);
        recorder.start(&path).unwrap();
        tap.push(&[0.5f32; 100]);
        drop(recorder);
        assert_eq!(read_wav(&path).unwrap().samples.len(), 100);
    }

    #[test]
    fn the_capture_callback_feeds_the_recording_without_the_output() {
        let path = temp("through-capture.wav");
        let glitches = Arc::new(AtomicU64::new(0));
        let (mut capture, _feed, mut recorder) = crate::recordable_input_path(1, RATE, 4, glitches);
        recorder.start(&path).unwrap();
        // Nothing ever reads the feed.
        capture.capture(&[1.0f32, 2.0, 3.0]);
        recorder.stop().unwrap();
        assert_eq!(read_wav(&path).unwrap().samples, [1.0, 2.0, 3.0]);
    }
}
