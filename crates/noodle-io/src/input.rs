//! Live input from an audio device, through cpal.
//!
//! cpal runs input and output as separate streams, each with its own
//! callback, so input crosses between them through a lock-free ring buffer:
//! [`Capture`] fills it from the input callback, and [`Feed`] empties it in
//! the output callback, one engine block at a time. Both are real-time safe
//! and can be tested without a device.
//!
//! The two devices' clocks drift apart unless they're the same device, and
//! Noodle doesn't resample yet. If input runs fast, the backlog grows, so the
//! feed trims it back now and then; if input runs slow, the feed runs dry and
//! fills the gap with silence. Both count as input glitches in [`Health`].

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::{FromSample, Sample};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::output::Health;
use crate::record::{RecordTap, Recorder, record_path};

/// How much input the ring buffer holds, in seconds. Far more than any
/// sensible device buffer, so it only overflows when the output has stopped.
const RING_SECONDS: f32 = 0.5;

/// Frames of backlog the feed keeps in hand at its tightest point, to ride
/// out jitter between the two callbacks.
const MARGIN_FRAMES: usize = 64;

/// How often the feed checks whether the backlog has grown, in seconds.
const TRIM_SECONDS: f32 = 0.5;

/// Makes the two ends of an input path for `channels` channels at
/// `sample_rate`, feeding blocks of up to `max_frames`.
pub fn input_path(
    channels: usize,
    sample_rate: f32,
    max_frames: usize,
    glitches: Arc<AtomicU64>,
) -> (Capture, Feed) {
    build(channels, sample_rate, max_frames, glitches, None)
}

/// Like [`input_path`], and the input can also be recorded to a file.
pub fn recordable_input_path(
    channels: usize,
    sample_rate: f32,
    max_frames: usize,
    glitches: Arc<AtomicU64>,
) -> (Capture, Feed, Recorder) {
    let (tap, recorder) = record_path(channels, sample_rate);
    let (capture, feed) = build(channels, sample_rate, max_frames, glitches, Some(tap));
    (capture, feed, recorder)
}

fn build(
    channels: usize,
    sample_rate: f32,
    max_frames: usize,
    glitches: Arc<AtomicU64>,
    tap: Option<RecordTap>,
) -> (Capture, Feed) {
    let ring_frames = ((sample_rate * RING_SECONDS) as usize).max(4 * max_frames);
    let (producer, consumer) = RingBuffer::new(ring_frames * channels);
    let capture = Capture {
        producer,
        glitches: Arc::clone(&glitches),
        tap,
    };
    let feed = Feed {
        consumer,
        channels,
        scratch: vec![0.0; max_frames * channels].into_boxed_slice(),
        trim_every: ((sample_rate * TRIM_SECONDS) as usize).max(1),
        since_trim: 0,
        primed: false,
        short: false,
        least_backlog: usize::MAX,
        glitches,
    };
    (capture, feed)
}

/// The input callback's end: converts samples to f32 and queues them.
pub struct Capture {
    producer: Producer<f32>,
    glitches: Arc<AtomicU64>,
    tap: Option<RecordTap>,
}

impl Capture {
    /// Queues interleaved input, in whole frames. Frames that don't fit are
    /// dropped, and counted as a glitch.
    pub fn capture<T: Sample>(&mut self, input: &[T])
    where
        f32: FromSample<T>,
    {
        if let Some(tap) = &mut self.tap {
            tap.push(input);
        }
        // Both ends move whole frames, so this is a whole number of them.
        let fits = input.len().min(self.producer.slots());
        if fits < input.len() {
            self.glitches.fetch_add(1, Ordering::Relaxed);
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

/// The output callback's end: hands the engine one block of input at a
/// time.
pub struct Feed {
    consumer: Consumer<f32>,
    channels: usize,
    /// One block of interleaved input.
    scratch: Box<[f32]>,
    /// Frames between backlog checks.
    trim_every: usize,
    since_trim: usize,
    /// Input has arrived at least once.
    primed: bool,
    /// The last read came up short, so a gap is already being counted.
    short: bool,
    /// The smallest backlog, in frames, left after a read since the last
    /// check.
    least_backlog: usize,
    glitches: Arc<AtomicU64>,
}

impl Feed {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The next `frames` frames of input, interleaved. Missing input is
    /// silence. `frames` must be at most the block size.
    pub fn read(&mut self, frames: usize) -> &[f32] {
        let len = frames * self.channels;
        let block = &mut self.scratch[..len];
        let available = self.consumer.slots().min(len);
        // Until the input stream's first callback, there's nothing to miss.
        self.primed |= available > 0;
        // A late input callback leaves several blocks short, one after the
        // other, but it's one gap in the sound: count it once, when it starts.
        let short = available < len && self.primed;
        if short && !self.short {
            self.glitches.fetch_add(1, Ordering::Relaxed);
        }
        self.short = short;
        if let Ok(chunk) = self.consumer.read_chunk(available) {
            let (first, second) = chunk.as_slices();
            block[..first.len()].copy_from_slice(first);
            block[first.len()..available].copy_from_slice(second);
            chunk.commit_all();
        }
        block[available..].fill(0.0);
        self.trim(frames);
        &self.scratch[..len]
    }

    /// Drops input that's piling up, so latency doesn't creep upwards when
    /// the input device runs faster than the output.
    ///
    /// It watches the backlog left after each read. If even the smallest
    /// backlog over a while was more than the margin, that much was never
    /// needed, so it's dropped. Two devices on the same clock settle with
    /// the margin in hand and are then left alone.
    fn trim(&mut self, frames: usize) {
        let backlog = self.consumer.slots() / self.channels;
        self.least_backlog = self.least_backlog.min(backlog);
        self.since_trim += frames;
        if self.since_trim < self.trim_every {
            return;
        }
        let excess = self.least_backlog.saturating_sub(MARGIN_FRAMES);
        if excess > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(excess * self.channels) {
                chunk.commit_all();
            }
            self.glitches.fetch_add(1, Ordering::Relaxed);
        }
        self.since_trim = 0;
        self.least_backlog = usize::MAX;
    }
}

impl Health {
    /// Input glitches so far: input that was dropped because the output
    /// wasn't taking it, gaps filled with silence because the input device
    /// was late, and backlog trimmed because it was early. Each is an
    /// audible discontinuity in the input. A gap counts once, however many
    /// engine blocks it spans.
    pub fn input_glitches(&self) -> u64 {
        self.input_glitches.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 1000.0;

    fn path(channels: usize, max_frames: usize) -> (Capture, Feed, Arc<AtomicU64>) {
        let glitches = Arc::new(AtomicU64::new(0));
        let (capture, feed) = input_path(channels, RATE, max_frames, Arc::clone(&glitches));
        (capture, feed, glitches)
    }

    fn glitches(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    #[test]
    fn input_arrives_in_order_and_converted() {
        let (mut capture, mut feed, counter) = path(2, 4);
        capture.capture(&[i16::MAX, 0, i16::MIN, 16_384]);
        capture.capture(&[0.25f32, -0.25]);
        // Cast to f32 the way cpal does.
        let max = f32::from_sample(i16::MAX);
        assert_eq!(feed.read(3), [max, 0.0, -1.0, 0.5, 0.25, -0.25]);
        assert_eq!(glitches(&counter), 0);
    }

    #[test]
    fn waiting_for_the_first_input_is_not_a_glitch() {
        let (mut capture, mut feed, counter) = path(1, 4);
        assert_eq!(feed.read(4), [0.0; 4]);
        assert_eq!(feed.read(4), [0.0; 4]);
        assert_eq!(glitches(&counter), 0);
        capture.capture(&[1.0f32, 2.0]);
        assert_eq!(feed.read(4), [1.0, 2.0, 0.0, 0.0]);
        assert_eq!(glitches(&counter), 1, "short once input has started");
    }

    #[test]
    fn missing_input_is_silence_and_a_glitch() {
        let (mut capture, mut feed, counter) = path(1, 4);
        capture.capture(&[1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(feed.read(4), [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(glitches(&counter), 0);
        assert_eq!(feed.read(4), [5.0, 6.0, 0.0, 0.0]);
        assert_eq!(glitches(&counter), 1);
    }

    #[test]
    fn one_late_callback_is_one_glitch_however_many_blocks_it_spans() {
        // The engine takes 64 frames at a time; a 256-frame callback is late.
        let (mut capture, mut feed, counter) = path(1, 64);
        capture.capture(&[0.5f32; 256]);
        for _ in 0..4 {
            feed.read(64);
        }
        assert_eq!(glitches(&counter), 0);
        for _ in 0..4 {
            assert_eq!(feed.read(64), [0.0; 64]);
        }
        assert_eq!(glitches(&counter), 1, "one gap, spanning four blocks");
    }

    #[test]
    fn a_gap_that_runs_on_from_a_partial_block_is_still_one_glitch() {
        let (mut capture, mut feed, counter) = path(1, 4);
        capture.capture(&[1.0f32; 6]);
        feed.read(4);
        // Two frames short, then nothing at all.
        for _ in 0..3 {
            feed.read(4);
        }
        assert_eq!(glitches(&counter), 1);
    }

    #[test]
    fn each_gap_is_counted_once() {
        let (mut capture, mut feed, counter) = path(1, 4);
        capture.capture(&[1.0f32; 4]);
        feed.read(4);
        feed.read(4);
        feed.read(4);
        assert_eq!(glitches(&counter), 1);
        // The input comes back, then goes late again.
        capture.capture(&[1.0f32; 8]);
        feed.read(4);
        feed.read(4);
        assert_eq!(glitches(&counter), 1, "no gap while input is arriving");
        feed.read(4);
        feed.read(4);
        assert_eq!(glitches(&counter), 2);
    }

    #[test]
    fn input_that_does_not_fit_is_dropped() {
        let (mut capture, mut feed, counter) = path(2, 4);
        // The ring holds half a second: 500 frames.
        let frames: Vec<f32> = (0..1200).map(|x| x as f32).collect();
        capture.capture(&frames);
        assert_eq!(glitches(&counter), 1);
        // What did fit is the start.
        assert_eq!(feed.read(2), [0.0, 1.0, 2.0, 3.0]);
        let mut last = [0.0; 2];
        for _ in 0..(500 - 2) / 2 {
            last.copy_from_slice(feed.read(1));
            feed.read(1);
        }
        assert_eq!(glitches(&counter), 1);
        assert_eq!(feed.read(1), [0.0, 0.0], "empty now");
        assert_eq!(last, [996.0, 997.0]);
    }

    /// Two devices' callbacks: the input delivers 32 frames at a time and
    /// the output asks for 16, each on schedule at its own clock rate.
    struct Callbacks {
        input_rate: f64,
        output_rate: f64,
        next_in: f64,
        next_out: f64,
    }

    impl Callbacks {
        fn new(input_rate: f64, output_rate: f64) -> Self {
            Self {
                input_rate,
                output_rate,
                next_in: 0.0,
                next_out: 0.0,
            }
        }

        /// Runs until `seconds` of output have played, and returns the
        /// backlog in frames.
        fn run(&mut self, seconds: f64, capture: &mut Capture, feed: &mut Feed) -> usize {
            let silence = [0.0f32; 32];
            while self.next_out < seconds {
                if self.next_in <= self.next_out {
                    capture.capture(&silence);
                    self.next_in += 32.0 / self.input_rate;
                } else {
                    feed.read(16);
                    self.next_out += 16.0 / self.output_rate;
                }
            }
            feed.consumer.slots()
        }
    }

    #[test]
    fn devices_on_the_same_clock_settle_without_glitches() {
        let (mut capture, mut feed, counter) = path(1, 16);
        // A head start, as when the input stream opens first.
        capture.capture(&[0.0f32; 200]);
        let mut callbacks = Callbacks::new(RATE.into(), RATE.into());
        let backlog = callbacks.run(10.0, &mut capture, &mut feed);
        assert!(backlog <= MARGIN_FRAMES + 32, "{backlog} frames behind");
        // The head start was trimmed once. Nothing after.
        assert_eq!(glitches(&counter), 1);
        callbacks.run(20.0, &mut capture, &mut feed);
        assert_eq!(glitches(&counter), 1);
    }

    #[test]
    fn a_fast_input_is_trimmed_so_latency_stays_bounded() {
        let (mut capture, mut feed, counter) = path(1, 16);
        // 1% fast: ten seconds builds 100 frames of backlog untrimmed.
        let mut callbacks = Callbacks::new(1010.0, RATE.into());
        let backlog = callbacks.run(10.0, &mut capture, &mut feed);
        assert!(backlog <= MARGIN_FRAMES + 32, "{backlog} frames behind");
        assert!(glitches(&counter) > 0);
    }

    #[test]
    fn a_slow_input_leaves_gaps_but_no_backlog() {
        let (mut capture, mut feed, counter) = path(1, 16);
        let mut callbacks = Callbacks::new(990.0, RATE.into());
        let backlog = callbacks.run(10.0, &mut capture, &mut feed);
        assert!(backlog <= MARGIN_FRAMES + 32, "{backlog} frames behind");
        assert!(glitches(&counter) > 0);
    }
}
