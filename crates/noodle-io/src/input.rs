//! Live input from an audio device, through cpal.
//!
//! cpal runs input and output as separate streams, each with its own
//! callback, so input crosses between them through a lock-free ring buffer:
//! [`Capture`] fills it from the input callback, and [`Feed`] empties it in
//! the output callback, one engine block at a time. Both are real-time safe
//! and can be tested without a device.
//!
//! The input device runs at its own sample rate and on its own clock, so the
//! feed resamples in the output callback: the ring holds input at the
//! device's rate, and each engine block is converted to the engine's rate on
//! the way out. The conversion ratio isn't fixed. A slow controller watches
//! how much input is queued and nudges the ratio to hold it steady, so clocks
//! that drift apart (or a 24 kHz mic against a 48 kHz output) never drop or
//! repeat frames. The converter is a short sinc kernel, about half a
//! millisecond of delay. Only a large jump, a stall or a stream that starts
//! far ahead, is trimmed or filled with silence, and both count as input
//! glitches in [`Health`].

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::{FromSample, Sample};
use rtrb::{Consumer, Producer, RingBuffer};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

use crate::output::Health;
use crate::record::{RecordTap, Recorder, record_path};

/// How much input the ring buffer holds, in seconds. Far more than any
/// sensible device buffer, so it only overflows when the output has stopped.
const RING_SECONDS: f32 = 0.5;

/// Frames of backlog (at the engine's rate) the controller aims to keep in
/// hand at its tightest point, beyond the largest block asked for. A block's
/// worth is needed because when the input callback slips a block later
/// against the output's, the backlog dips by that much; the rest covers the
/// converter's look-ahead and jitter.
const MARGIN_FRAMES: usize = 32;

/// Output frames the converter produces per call. Blocks that aren't a
/// multiple of this leave a few frames waiting, so it's kept small.
const CHUNK_FRAMES: usize = 16;

/// How often the controller looks at the backlog, in seconds.
const WINDOW_SECONDS: f64 = 0.05;

/// Proportional gain: ratio change per second of backlog error. It pulls an
/// offset in within about half a second, and is low enough that jitter in the
/// measured backlog (a millisecond is 0.2%) stays inaudible as pitch.
const KP: f64 = 2.0;

/// Integral gain, which learns the clocks' steady offset in a few seconds.
const KI: f64 = 0.3;

/// The integral term only learns while the backlog is this close to the
/// margin, in seconds. A stream that opens a long way ahead is then pulled
/// in by the proportional term alone, which would otherwise wind the
/// integral up and overshoot into a gap.
const LEARN_WITHIN: f64 = 0.006;

/// The most the ratio is moved from its nominal value, either way. Real clock
/// error is hundreds of ppm; this leaves room for a bad one.
const MAX_TRIM: f64 = 0.05;

/// A backlog this many seconds over the target is dropped at once, because
/// steering would take too long. A stream that opens a long way ahead does
/// this.
const TRIM_SECONDS: f64 = 0.05;

/// A short kernel: it adds under a millisecond of delay, at the price of a
/// little less rejection than the file-grade filter.
const LIVE_SINC: SincInterpolationParameters = SincInterpolationParameters {
    sinc_len: 32,
    f_cutoff: Some(0.92),
    interpolation: SincInterpolationType::Linear,
    oversampling_factor: 256,
    window: WindowFunction::BlackmanHarris2,
};

/// Makes the two ends of an input path for `channels` channels. The device
/// captures at `input_rate`; the feed delivers blocks of up to `max_frames` at
/// `output_rate`, the engine's.
pub fn input_path(
    channels: usize,
    input_rate: f32,
    output_rate: f32,
    max_frames: usize,
    glitches: Arc<AtomicU64>,
) -> (Capture, Feed) {
    build(
        channels,
        input_rate,
        output_rate,
        max_frames,
        glitches,
        None,
    )
}

/// Like [`input_path`], and the input can also be recorded to a file.
pub fn recordable_input_path(
    channels: usize,
    input_rate: f32,
    output_rate: f32,
    max_frames: usize,
    glitches: Arc<AtomicU64>,
) -> (Capture, Feed, Recorder) {
    // A recording is the device's own samples, at the device's rate.
    let (tap, recorder) = record_path(channels, input_rate);
    let (capture, feed) = build(
        channels,
        input_rate,
        output_rate,
        max_frames,
        glitches,
        Some(tap),
    );
    (capture, feed, recorder)
}

fn build(
    channels: usize,
    input_rate: f32,
    output_rate: f32,
    max_frames: usize,
    glitches: Arc<AtomicU64>,
    tap: Option<RecordTap>,
) -> (Capture, Feed) {
    let ring_frames = ((input_rate * RING_SECONDS) as usize)
        .max(4 * max_frames * (input_rate / output_rate).ceil() as usize);
    let (producer, consumer) = RingBuffer::new(ring_frames * channels);
    let capture = Capture {
        producer,
        glitches: Arc::clone(&glitches),
        tap,
    };
    let in_rate = f64::from(input_rate);
    let out_rate = f64::from(output_rate);
    let resampler = Async::<f32>::new_sinc(
        out_rate / in_rate,
        1.0 + 2.0 * MAX_TRIM,
        &LIVE_SINC,
        CHUNK_FRAMES,
        channels,
        FixedAsync::Output,
    )
    .expect("the live resampler's parameters are valid");
    let input = vec![0.0; resampler.input_frames_max() * channels].into_boxed_slice();
    let feed = Feed {
        consumer,
        channels,
        resampler,
        input,
        chunk: vec![0.0; CHUNK_FRAMES * channels].into_boxed_slice(),
        chunk_left: 0,
        scratch: vec![0.0; max_frames * channels].into_boxed_slice(),
        in_rate,
        out_rate,
        largest: 0,
        window: ((out_rate * WINDOW_SECONDS) as usize).max(1),
        since_window: 0,
        least_backlog: usize::MAX,
        integral: 0.0,
        primed: false,
        short: false,
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
    /// Notes how long before this callback the driver captured its input
    /// (see [`Recorder::input_latency`]).
    pub fn set_latency(&self, latency: std::time::Duration) {
        if let Some(tap) = &self.tap {
            tap.set_latency(latency);
        }
    }

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
/// time, converted to the engine's rate.
pub struct Feed {
    consumer: Consumer<f32>,
    channels: usize,
    resampler: Async<f32>,
    /// Input for one conversion, interleaved.
    input: Box<[f32]>,
    /// The last converted chunk; its final `chunk_left` frames haven't been
    /// handed out yet.
    chunk: Box<[f32]>,
    chunk_left: usize,
    /// One block of interleaved input at the engine's rate.
    scratch: Box<[f32]>,
    in_rate: f64,
    out_rate: f64,
    /// The largest block asked for so far.
    largest: usize,
    /// Output frames between looks at the backlog.
    window: usize,
    since_window: usize,
    /// The smallest backlog, in input frames, left after a read since the
    /// last look.
    least_backlog: usize,
    /// The ratio offset the controller has learned, relative to nominal.
    integral: f64,
    /// Input has arrived at least once.
    primed: bool,
    /// The last conversion came up short, so a gap is already being counted.
    short: bool,
    glitches: Arc<AtomicU64>,
}

impl Feed {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The next `frames` frames of input, interleaved, at the engine's rate.
    /// Missing input is silence. `frames` must be at most the block size.
    pub fn read(&mut self, frames: usize) -> &[f32] {
        let channels = self.channels;
        self.largest = self.largest.max(frames);
        let mut done = 0;
        while done < frames {
            if self.chunk_left == 0 {
                self.convert();
            }
            let take = self.chunk_left.min(frames - done);
            let from = (CHUNK_FRAMES - self.chunk_left) * channels;
            self.scratch[done * channels..(done + take) * channels]
                .copy_from_slice(&self.chunk[from..from + take * channels]);
            self.chunk_left -= take;
            done += take;
        }
        self.steer(frames);
        &self.scratch[..frames * channels]
    }

    /// Converts the next chunk of input into `chunk`.
    fn convert(&mut self) {
        let channels = self.channels;
        let needed = self.resampler.input_frames_next();
        let available = self.consumer.slots() / channels;
        let taken = available.min(needed);
        // Until the input stream's first callback, there's nothing to miss.
        self.primed |= taken > 0;
        // A late input callback leaves several chunks short, one after the
        // other, but it's one gap in the sound: count it once, when it starts.
        let short = taken < needed && self.primed;
        if short && !self.short {
            self.glitches.fetch_add(1, Ordering::Relaxed);
        }
        self.short = short;
        self.chunk_left = CHUNK_FRAMES;
        if !self.primed {
            self.chunk.fill(0.0);
            return;
        }
        if let Ok(read) = self.consumer.read_chunk(taken * channels) {
            let (first, second) = read.as_slices();
            self.input[..first.len()].copy_from_slice(first);
            self.input[first.len()..first.len() + second.len()].copy_from_slice(second);
            read.commit_all();
        }
        // Short input is zero-padded by the converter (`partial_len`).
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: Some(taken),
            active_channels_mask: None,
        };
        let source = InterleavedSlice::new(&self.input[..needed * channels], channels, needed);
        let target = InterleavedSlice::new_mut(&mut self.chunk, channels, CHUNK_FRAMES);
        let converted = match (source, target) {
            (Ok(source), Ok(mut target)) => self
                .resampler
                .process_into_buffer(&source, &mut target, Some(&indexing))
                .is_ok(),
            _ => false,
        };
        if !converted {
            self.chunk.fill(0.0);
        }
    }

    /// Holds the backlog steady by nudging the conversion ratio.
    ///
    /// It watches the backlog left after each read, and once a window has
    /// passed, compares the smallest with the margin. Too much input queued
    /// means the input is running fast, so the converter consumes it a little
    /// faster; too little, slower. A proportional term corrects an offset
    /// quickly and an integral term learns the clocks' steady difference, so
    /// the ratio settles at the true one and the backlog at the margin. Two
    /// devices on the same clock settle with the margin in hand and are then
    /// left alone.
    fn steer(&mut self, frames: usize) {
        let backlog = self.consumer.slots() / self.channels;
        self.least_backlog = self.least_backlog.min(backlog);
        self.since_window += frames;
        if self.since_window < self.window {
            return;
        }
        let least = std::mem::replace(&mut self.least_backlog, usize::MAX);
        let seconds = self.since_window as f64 / self.out_rate;
        self.since_window = 0;
        if !self.primed {
            return;
        }
        let target = (self.largest + MARGIN_FRAMES) as f64 * self.in_rate / self.out_rate;
        let mut error = (least as f64 - target) / self.in_rate;
        // Too far ahead for steering: the stream opened early, or the output
        // stalled. Skip to the margin, as a gap would.
        if error > TRIM_SECONDS {
            let excess = (least as f64 - target) as usize;
            if let Ok(read) = self.consumer.read_chunk(excess * self.channels) {
                read.commit_all();
            }
            self.glitches.fetch_add(1, Ordering::Relaxed);
            error = 0.0;
        }
        if error.abs() < LEARN_WITHIN {
            self.integral = (self.integral + KI * error * seconds).clamp(-MAX_TRIM, MAX_TRIM);
        }
        // More backlog than wanted needs a smaller ratio: less output per
        // input, so input is used up faster.
        let trim = (KP * error + self.integral).clamp(-MAX_TRIM, MAX_TRIM);
        // In bounds by construction, and a failure would only leave the
        // ratio where it was.
        let _ = self.resampler.set_resample_ratio_relative(1.0 - trim, true);
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

    const RATE: f32 = 48_000.0;

    fn path(channels: usize, input_rate: f32) -> (Capture, Feed, Arc<AtomicU64>) {
        let glitches = Arc::new(AtomicU64::new(0));
        let (capture, feed) = input_path(channels, input_rate, RATE, 128, Arc::clone(&glitches));
        (capture, feed, glitches)
    }

    fn glitches(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    fn backlog(feed: &Feed) -> usize {
        feed.consumer.slots() / feed.channels
    }

    #[test]
    fn input_arrives_converted_and_with_its_channels_apart() {
        let (mut capture, mut feed, counter) = path(2, RATE);
        // Left is a constant, right is its negative: they must not mix.
        let frame = [i16::MAX / 2, -(i16::MAX / 2)];
        for _ in 0..2000 {
            capture.capture(&frame);
        }
        let want = f32::from_sample(i16::MAX / 2);
        let mut last = [0.0; 2];
        for _ in 0..8 {
            let block = feed.read(128);
            last.copy_from_slice(&block[block.len() - 2..]);
        }
        assert!((last[0] - want).abs() < 0.01, "{last:?}");
        assert!((last[1] + want).abs() < 0.01, "{last:?}");
        assert_eq!(glitches(&counter), 0);
    }

    #[test]
    fn waiting_for_the_first_input_is_not_a_glitch() {
        let (mut capture, mut feed, counter) = path(1, RATE);
        assert_eq!(feed.read(128), [0.0; 128]);
        assert_eq!(feed.read(128), [0.0; 128]);
        assert_eq!(glitches(&counter), 0);
        capture.capture(&[1.0f32; 20]);
        feed.read(128);
        assert_eq!(glitches(&counter), 1, "short once input has started");
    }

    #[test]
    fn missing_input_is_silence_and_a_glitch() {
        let (mut capture, mut feed, counter) = path(1, RATE);
        capture.capture(&[0.5f32; 1000]);
        for _ in 0..4 {
            feed.read(128);
        }
        assert_eq!(glitches(&counter), 0);
        for _ in 0..8 {
            feed.read(128);
        }
        assert_eq!(glitches(&counter), 1);
        assert!(feed.read(128).iter().all(|&x| x.abs() < 1e-3), "silence");
    }

    #[test]
    fn each_gap_is_counted_once() {
        let (mut capture, mut feed, counter) = path(1, RATE);
        capture.capture(&[0.5f32; 1000]);
        for _ in 0..20 {
            feed.read(128);
        }
        assert_eq!(glitches(&counter), 1);
        // The input comes back, then goes late again.
        capture.capture(&[0.5f32; 4000]);
        for _ in 0..8 {
            feed.read(128);
        }
        assert_eq!(glitches(&counter), 1, "no gap while input is arriving");
        for _ in 0..40 {
            feed.read(128);
        }
        assert_eq!(glitches(&counter), 2);
    }

    #[test]
    fn blocks_of_any_size_come_out_continuous() {
        // 100 isn't a multiple of the converter's chunk.
        let (mut capture, mut feed, counter) = path(1, RATE);
        let ramp: Vec<f32> = (0..10_000).map(|x| (x as f32 * 0.01).sin() * 0.5).collect();
        capture.capture(&ramp[..200]);
        let mut sent = 200;
        let mut out = Vec::new();
        for frames in [100, 7, 128, 1, 33, 100, 128, 100, 5, 128].repeat(10) {
            capture.capture(&ramp[sent..sent + frames]);
            sent += frames;
            out.extend_from_slice(feed.read(frames));
        }
        // No jumps: a sine at 0.01 rad/sample never moves more than 0.005
        // per frame.
        let jump = out[400..]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(jump < 0.01, "a jump of {jump}");
        assert_eq!(glitches(&counter), 0);
    }

    #[test]
    fn input_that_does_not_fit_is_dropped() {
        let (mut capture, _feed, counter) = path(2, RATE);
        // The ring holds half a second.
        capture.capture(&vec![0.0f32; 48_000]);
        assert_eq!(glitches(&counter), 0);
        capture.capture(&[0.0f32; 4]);
        assert_eq!(glitches(&counter), 1);
    }

    /// Two devices' callbacks, each on schedule at its own clock rate: the
    /// input delivers `in_chunk` frames at a time of a sine wave, and the
    /// output asks for blocks of 128.
    struct Callbacks {
        input_rate: f64,
        output_rate: f64,
        in_chunk: usize,
        next_in: f64,
        next_out: f64,
        sent: usize,
        /// What the output played.
        played: Vec<f32>,
        /// The smallest and largest backlog seen after a read.
        least: usize,
        most: usize,
    }

    impl Callbacks {
        fn new(input_rate: f64, output_rate: f64) -> Self {
            Self {
                input_rate,
                output_rate,
                in_chunk: 512,
                next_in: 0.0,
                next_out: 0.0,
                sent: 0,
                played: Vec::new(),
                least: usize::MAX,
                most: 0,
            }
        }

        /// Runs until `seconds` of output have played, then forgets the
        /// backlog extremes, so the next run reports its own.
        fn run(&mut self, seconds: f64, capture: &mut Capture, feed: &mut Feed) {
            self.least = usize::MAX;
            self.most = 0;
            let tone = 1000.0 / self.input_rate;
            while self.next_out < seconds {
                if self.next_in <= self.next_out {
                    let chunk: Vec<f32> = (self.sent..self.sent + self.in_chunk)
                        .map(|n| (n as f64 * tone * std::f64::consts::TAU).sin() as f32 * 0.5)
                        .collect();
                    capture.capture(&chunk);
                    self.sent += self.in_chunk;
                    self.next_in += self.in_chunk as f64 / self.input_rate;
                } else {
                    self.played.extend_from_slice(feed.read(128));
                    self.next_out += 128.0 / self.output_rate;
                    self.least = self.least.min(backlog(feed));
                    self.most = self.most.max(backlog(feed));
                }
            }
        }
    }

    /// Runs the input at `input_rate` against a 48 kHz output, starting with
    /// a device buffer of head start, as when the input stream opens first.
    /// The controller gets `warm_up` seconds to learn the clocks. After that,
    /// a minute must pass with nothing dropped, repeated or gapped, and the
    /// backlog must stay put.
    fn settles(input_rate: f64, warm_up: f64) {
        let (mut capture, mut feed, counter) = path(1, input_rate as f32);
        capture.capture(&vec![0.0f32; 600]);
        let mut callbacks = Callbacks::new(input_rate, RATE.into());
        callbacks.run(warm_up, &mut capture, &mut feed);
        let before = glitches(&counter);
        callbacks.run(warm_up + 60.0, &mut capture, &mut feed);
        assert_eq!(glitches(&counter), before, "dropped or repeated input");
        // At the margin, plus the device's buffer swinging it.
        assert!(
            callbacks.most <= 128 + 32 + 512 + 128,
            "{} behind",
            callbacks.most
        );
    }

    #[test]
    fn devices_on_the_same_clock_settle_without_glitches() {
        settles(48_000.0, 5.0);
    }

    #[test]
    fn real_clock_error_costs_nothing_from_the_start() {
        // Crystals are good for a few hundred ppm. With a head start and
        // that much error, not even the warm-up drops anything.
        for input_rate in [48_000.0 * 1.0003, 48_000.0 * 0.9997] {
            let (mut capture, mut feed, counter) = path(1, input_rate as f32);
            capture.capture(&vec![0.0f32; 600]);
            let mut callbacks = Callbacks::new(input_rate, RATE.into());
            callbacks.run(60.0, &mut capture, &mut feed);
            assert_eq!(glitches(&counter), 0, "{input_rate} Hz");
        }
    }

    #[test]
    fn a_fast_input_is_steered_not_trimmed() {
        // 1% fast: a minute would build 28800 frames of backlog unsteered.
        settles(48_480.0, 30.0);
    }

    #[test]
    fn a_slow_input_is_steered_not_starved() {
        settles(47_520.0, 30.0);
    }

    #[test]
    fn a_tone_keeps_its_pitch_and_level_across_sample_rates() {
        for input_rate in [24_000.0, 44_100.0, 96_000.0] {
            let (mut capture, mut feed, counter) = path(1, input_rate as f32);
            capture.capture(&vec![0.0f32; 600]);
            let mut callbacks = Callbacks::new(input_rate, RATE.into());
            callbacks.run(10.0, &mut capture, &mut feed);
            assert_eq!(glitches(&counter), 0, "{input_rate} Hz");
            // The last second.
            let tail = &callbacks.played[callbacks.played.len() - 48_000..];
            let rms = (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt();
            assert!(
                (rms - 0.5 / 2f32.sqrt()).abs() < 0.01,
                "{input_rate} Hz: level {rms}"
            );
            let crossings = tail
                .windows(2)
                .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
                .count();
            assert!(
                (999..=1001).contains(&crossings),
                "{input_rate} Hz: {crossings} cycles a second"
            );
        }
    }
}
