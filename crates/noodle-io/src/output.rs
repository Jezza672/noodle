//! Live output to an audio device, through cpal.
//!
//! The device code is kept thin: [`DeviceWriter`] does everything the audio
//! callback does and can be tested without a device, and [`play`] only finds
//! the device and wires the writer into its callback.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, I24, Sample, SampleFormat, SizedSample, StreamConfig};
use noodle_engine::{
    Bus, Controller, Doorbell, Processor, Settings, TransportControl, engine_with_workers,
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::devices::{
    AudioConfig, AudioError, Chosen, Direction, InputChoice, VIRTUAL_OUTPUT, buffer_request,
    choose_config, device_name, find_device, host_for,
};
use crate::input::{Capture, Feed, InputMeter, recordable_input_path};
use crate::midi::{AudioClock, MidiBus, now_micros};
use crate::record::{RecordError, Recorder, Take};
use crate::resample::StreamResampler;

pub use cpal::{Error as DeviceError, ErrorKind as DeviceErrorKind};

/// Runs the [`Processor`] for an audio callback, converting its output to the
/// device's sample format, and feeding it device input if there is any.
/// Real-time safe: its buffers are allocated up front.
///
/// The engine can render more channels than the device has: the device takes
/// the first ones, and each [`Tap`] hands a later range to another device's
/// stream.
pub struct DeviceWriter {
    processor: Processor,
    /// One block of interleaved samples, at the engine's channel count.
    scratch: Box<[f32]>,
    /// The channels this device plays: the first ones the engine renders.
    channels: usize,
    input: Option<Feed>,
    /// The other devices' channels.
    taps: Vec<Tap>,
    fade: Arc<Fade>,
    /// The gain applied to the output: 1 until a fade-out starts.
    gain: f32,
    /// How much the gain falls per frame while fading out.
    step: f32,
    /// Where to say when the blocks run, for MIDI timing and recording.
    midi_clock: Option<(Arc<AudioClock>, Arc<TransportControl>)>,
}

/// How long the output takes to fade out when playback stops. Cutting a
/// stream off mid-wave is a click.
const FADE_OUT: Duration = Duration::from_millis(10);

/// Lets the thread that owns the stream ask the audio callback for a fade-out
/// and hear when it is done.
#[derive(Default)]
struct Fade {
    stop: AtomicBool,
    done: AtomicBool,
    /// The most frames the callback has been asked for at once, which is how
    /// much the device queues ahead of what it is playing.
    period: AtomicU32,
    /// How long after the callback the device says its samples play, in
    /// microseconds: the output latency, as far as the driver knows it.
    latency_micros: AtomicU64,
}

impl DeviceWriter {
    pub fn new(processor: Processor) -> Self {
        let Settings {
            max_frames,
            channels,
            sample_rate,
        } = processor.settings();
        Self {
            processor,
            scratch: vec![0.0; max_frames * channels].into_boxed_slice(),
            channels,
            input: None,
            taps: Vec::new(),
            fade: Arc::default(),
            gain: 1.0,
            step: 1.0 / (FADE_OUT.as_secs_f32() * sample_rate).max(1.0),
            midi_clock: None,
        }
    }

    /// Has the writer say, on every callback, when its blocks run: where the
    /// transport is at the callback's start, and for each block which span of
    /// MIDI arrivals it plays (the callback's length before it, so MIDI
    /// played during one device period sounds in the next, at the same place
    /// in it). `transport` is the engine's.
    pub fn with_midi_clock(
        mut self,
        clock: Arc<AudioClock>,
        transport: Arc<TransportControl>,
    ) -> Self {
        self.midi_clock = Some((clock, transport));
        self
    }

    /// A writer whose Input nodes play what `feed` delivers.
    pub fn with_input(processor: Processor, feed: Feed) -> Self {
        Self {
            input: Some(feed),
            ..Self::new(processor)
        }
    }

    /// Splits the engine's channels among devices: this one plays the first
    /// `main_channels`, and each of the `extra_channels` counts that follow
    /// gets an [`ExtraOutput`] to play on its own device, at the engine's
    /// rate. The counts must add up to the engine's channels. Anything the
    /// devices fall behind by counts in `glitches`.
    pub fn with_outputs(
        self,
        main_channels: usize,
        extra_channels: &[usize],
        glitches: &Arc<AtomicU64>,
    ) -> (Self, Vec<ExtraOutput>) {
        let rate = self.processor.settings().sample_rate as u32;
        let extras: Vec<_> = extra_channels.iter().map(|&c| (c, rate)).collect();
        let (writer, outputs) = self.with_resampled_outputs(main_channels, &extras, glitches);
        let outputs = outputs
            .into_iter()
            .map(|output| output.expect("same-rate outputs need no converter"))
            .collect();
        (writer, outputs)
    }

    /// Like [`with_outputs`](Self::with_outputs), for devices that may run at
    /// other sample rates than the engine: each extra is `(channels, rate)`.
    /// Audio for a device at another rate is converted by a thread of its own
    /// (see [`ExtraOutput`]), so neither stream's callback does the work. A
    /// device whose converter can't start gets its error in place of an
    /// output, and its channels are dropped.
    pub fn with_resampled_outputs(
        mut self,
        main_channels: usize,
        extras: &[(usize, u32)],
        glitches: &Arc<AtomicU64>,
    ) -> (Self, Vec<Result<ExtraOutput, AudioError>>) {
        let rate = self.processor.settings().sample_rate;
        assert_eq!(
            main_channels + extras.iter().map(|e| e.0).sum::<usize>(),
            self.processor.settings().channels,
            "the devices' channels must add up to the engine's"
        );
        self.channels = main_channels;
        let mut first = main_channels;
        let mut outputs = Vec::with_capacity(extras.len());
        for &(channels, device_rate) in extras {
            let capacity = (EXTRA_QUEUE_SECONDS * rate) as usize * channels;
            let (queue, tapped) = RingBuffer::new(capacity);
            let bell = (device_rate != rate as u32).then(|| Arc::new(Doorbell::new()));
            self.taps.push(Tap {
                queue,
                first,
                channels,
                overruns: Arc::clone(glitches),
                full: false,
                bell: bell.clone(),
            });
            first += channels;
            let made = if device_rate == rate as u32 {
                Ok(ExtraOutput::new(
                    tapped, channels, rate, &self.fade, glitches,
                ))
            } else {
                // The tap's queue goes to a converter, and the device plays
                // what that makes, at its own rate.
                let capacity = (EXTRA_QUEUE_SECONDS * device_rate as f32) as usize * channels;
                let (converted, playing) = RingBuffer::new(capacity);
                RateConverter::spawn(
                    bell.expect("made for a converter"),
                    tapped,
                    converted,
                    channels,
                    (rate as u32, device_rate),
                    Arc::clone(glitches),
                )
                .map(|converter| {
                    let mut output = ExtraOutput::new(
                        playing,
                        channels,
                        device_rate as f32,
                        &self.fade,
                        glitches,
                    );
                    output.ratio = f64::from(device_rate) / f64::from(rate);
                    // The converter works a chunk at a time, and has to be
                    // woken, so what it makes arrives later than the tap's
                    // block does.
                    output.slack = (f64::from(device_rate)
                        * (CONVERTER_CHUNK as f64 / f64::from(rate) + CONVERTER_WAKE.as_secs_f64()))
                    .ceil() as usize;
                    output._converter = Some(converter);
                    output
                })
            };
            outputs.push(made);
        }
        (self, outputs)
    }

    /// Fills `output`, interleaved with the device's channels. Samples are
    /// clamped to between -1 and 1, and anything that isn't finite becomes
    /// silence, so a misbehaving graph can't blast the speakers.
    pub fn write<T: Sample + FromSample<f32>>(&mut self, output: &mut [T]) {
        let engine_channels = self.processor.settings().channels;
        let channels = self.channels;
        self.fade
            .period
            .fetch_max((output.len() / channels) as u32, Ordering::Relaxed);
        let block = self.scratch.len() / engine_channels;
        let rate = f64::from(self.processor.settings().sample_rate);
        let called = now_micros();
        let period = ((output.len() / channels) as f64 * 1e6 / rate) as u64;
        if let Some((clock, transport)) = &self.midi_clock {
            clock.publish_anchor(called, transport.position());
        }
        let mut done = 0usize;
        for chunk in output.chunks_mut(block * channels) {
            let frames = chunk.len() / channels;
            if let Some((clock, _)) = &self.midi_clock {
                let into = (done as f64 * 1e6 / rate) as u64;
                clock.set_window(called.saturating_sub(period) + into);
            }
            done += frames;
            let scratch = &mut self.scratch[..frames * engine_channels];
            match &mut self.input {
                Some(feed) => {
                    let channels_in = feed.channels();
                    let input = feed.read(frames);
                    self.processor
                        .process_with_input(input, channels_in, scratch);
                }
                None => self.processor.process(scratch),
            }
            let stopping = self.fade.stop.load(Ordering::Relaxed);
            for (frame_out, frame) in chunk
                .chunks_mut(channels)
                .zip(scratch.chunks_mut(engine_channels))
            {
                if stopping {
                    self.gain = (self.gain - self.step).max(0.0);
                }
                for x in frame.iter_mut() {
                    let finite = if x.is_finite() {
                        x.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                    *x = finite * self.gain;
                }
                for (out, &x) in frame_out.iter_mut().zip(frame.iter()) {
                    *out = T::from_sample(x);
                }
            }
            for tap in &mut self.taps {
                tap.push(scratch, engine_channels);
            }
            if stopping && self.gain == 0.0 {
                self.fade.done.store(true, Ordering::Release);
            }
        }
    }
}

/// The writing end of the path to another output device: some of the
/// engine's channels, queued for that device's own callback to play. Real-time
/// safe.
struct Tap {
    queue: Producer<f32>,
    /// The first engine channel this device plays, and how many it has.
    first: usize,
    channels: usize,
    /// Counted with the devices' underruns: a block that didn't fit is a gap.
    overruns: Arc<AtomicU64>,
    /// The last block didn't fit. A queue that stays full (a device that has
    /// stopped) counts once, not on every block.
    full: bool,
    /// Rung when a block is queued, if a converter is waiting for it.
    bell: Option<Arc<Doorbell>>,
}

impl Tap {
    /// Queues this device's channels from interleaved `block`. If the device
    /// is behind and the queue is full, the block is dropped.
    fn push(&mut self, block: &[f32], engine_channels: usize) {
        // A device whose stream couldn't start has dropped its end.
        if self.queue.is_abandoned() {
            return;
        }
        let frames = block.len() / engine_channels;
        let Ok(chunk) = self.queue.write_chunk_uninit(frames * self.channels) else {
            if !self.full {
                self.overruns.fetch_add(1, Ordering::Relaxed);
            }
            self.full = true;
            return;
        };
        self.full = false;
        let (first, channels) = (self.first, self.channels);
        chunk.fill_from_iter(
            block
                .chunks(engine_channels)
                .flat_map(|frame| frame[first..first + channels].iter().copied()),
        );
        if let Some(bell) = &self.bell {
            bell.ring();
        }
    }
}

/// How many frames a device whose samples play `own` after its callback
/// must hold back to sound with a main device that plays them `main` after
/// its. Zero if it is the slower of the two: the main stream can't be held
/// back by its extras.
fn latency_gap_frames(main: Duration, own: Duration, sample_rate: f32) -> usize {
    (main.saturating_sub(own).as_secs_f64() * f64::from(sample_rate)) as usize
}

/// How much one extra output queues, in seconds. The queue is for tolerating
/// the gap between the two devices' callbacks, not for latency: playback
/// starts with only as much as the callbacks need.
const EXTRA_QUEUE_SECONDS: f32 = 0.5;

/// The playing end of an extra output device: the callback of a stream that
/// plays what the main stream's [`Tap`] queued. The two devices' clocks drift
/// apart, so it watches its backlog the way the input feed does: when it runs
/// dry it plays silence until enough has built up again, and when too much
/// has built up it skips ahead.
pub struct ExtraOutput {
    queue: Consumer<f32>,
    channels: usize,
    /// The device's rate over the main stream's: how many frames this device
    /// plays for each one the main stream renders.
    ratio: f64,
    /// Frames, at this device's rate, that the audio arrives late by beyond
    /// the main stream's block: nothing when it comes straight from the main
    /// stream, the converter's lag when it is resampled.
    slack: usize,
    /// Keeps the converter running (and stops it when this is dropped).
    _converter: Option<RateConverter>,
    /// How much the main stream asks for at once, which the queue must cover
    /// to keep the two apart.
    main_period: Arc<Fade>,
    sample_rate: f32,
    /// How long after its callback this device says its samples play (see
    /// [`Fill::playback_latency`]).
    own_latency: Duration,
    /// Waiting for enough to be queued before playing.
    filling: bool,
    glitches: Arc<AtomicU64>,
    /// Set once the fade-out has played here: the main stream has finished
    /// it and this device has since played a whole block of silence.
    drained: Arc<AtomicBool>,
}

impl ExtraOutput {
    /// An output playing `queue` at `sample_rate`.
    fn new(
        queue: Consumer<f32>,
        channels: usize,
        sample_rate: f32,
        fade: &Arc<Fade>,
        glitches: &Arc<AtomicU64>,
    ) -> Self {
        Self {
            queue,
            channels,
            ratio: 1.0,
            slack: 0,
            _converter: None,
            main_period: Arc::clone(fade),
            sample_rate,
            own_latency: Duration::ZERO,
            filling: true,
            glitches: Arc::clone(glitches),
            drained: Arc::default(),
        }
    }

    /// Fills a device's `out`, interleaved at its channel count.
    pub fn fill<T: Sample + FromSample<f32>>(&mut self, out: &mut [T]) {
        let channels = self.channels;
        let wanted = out.len();
        let available = self.queue.slots();
        // Enough to survive until the main stream's next callback: its block,
        // and this callback's. Never less than the time this device gets its
        // samples to the speakers faster than the main one does: holding the
        // queue that deep is what makes the two sound together, and the
        // cushion is a floor on that, not an addition to it.
        let main_latency =
            Duration::from_micros(self.main_period.latency_micros.load(Ordering::Relaxed));
        let aligned = latency_gap_frames(main_latency, self.own_latency, self.sample_rate);
        let period = self.main_period.period.load(Ordering::Relaxed) as f64;
        let cushion = (period * self.ratio).ceil() as usize + self.slack + wanted / channels;
        let lead = cushion.max(aligned) * channels;
        // A queue too small for that would never fill.
        let lead = lead.min(self.queue.buffer().capacity() / 3);
        if self.filling {
            if available < lead {
                out.fill(T::EQUILIBRIUM);
                return;
            }
            self.filling = false;
        }
        let mut available = available;
        if available > 3 * lead {
            // Too far behind: skip to a safe backlog, a glitch like a gap.
            let skip = (available - lead) / channels * channels;
            if let Ok(chunk) = self.queue.read_chunk(skip) {
                chunk.commit_all();
            }
            available -= skip;
            self.glitches.fetch_add(1, Ordering::Relaxed);
        }
        let take = wanted.min(available / channels * channels);
        if take < wanted {
            self.glitches.fetch_add(1, Ordering::Relaxed);
            self.filling = true;
        }
        if let Ok(chunk) = self.queue.read_chunk(take) {
            let (a, b) = chunk.as_slices();
            let mut silent = true;
            for (out, &x) in out.iter_mut().zip(a.iter().chain(b)) {
                silent &= x == 0.0;
                *out = T::from_sample(x);
            }
            chunk.commit_all();
            // After the fade has finished, a block of silence means the ramp
            // has been heard.
            if silent && self.main_period.done.load(Ordering::Acquire) {
                self.drained.store(true, Ordering::Release);
            }
        }
        out[take..].fill(T::EQUILIBRIUM);
    }
}

/// How many frames the converter takes in at a time, at the engine's rate.
const CONVERTER_CHUNK: usize = 128;
/// What a converter's wake-up is allowed to add to its delay.
const CONVERTER_WAKE: Duration = Duration::from_millis(1);

/// Converts an extra device's audio from the engine's rate to the device's, on
/// a thread of its own: the main stream's callback only queues the engine's
/// audio, and the device's callback only plays what has been converted.
/// Dropping it stops the thread.
struct RateConverter {
    stop: Arc<AtomicBool>,
    bell: Arc<Doorbell>,
    thread: Option<JoinHandle<()>>,
}

impl RateConverter {
    fn spawn(
        bell: Arc<Doorbell>,
        mut input: Consumer<f32>,
        mut output: Producer<f32>,
        channels: usize,
        (from, to): (u32, u32),
        glitches: Arc<AtomicU64>,
    ) -> Result<Self, AudioError> {
        let mut resampler =
            StreamResampler::new(from, to, channels, CONVERTER_CHUNK).map_err(|error| {
                AudioError::Resample {
                    reason: error.to_string(),
                }
            })?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let waiting = Arc::clone(&bell);
        let thread = thread::Builder::new()
            .name("noodle-resample".into())
            .spawn(move || {
                // Whole frames, a few chunks at a time.
                let mut block = vec![0.0f32; CONVERTER_CHUNK * 8 * channels];
                let mut full = false;
                while !stopped.load(Ordering::Relaxed) && !output.is_abandoned() {
                    let frames = input.slots() / channels;
                    if frames == 0 {
                        if input.is_abandoned() {
                            break;
                        }
                        // Rung by the tap when it queues a block. If the tap
                        // is gone it sleeps on, until the output is dropped.
                        waiting.wait(None);
                        continue;
                    }
                    let take = frames.min(block.len() / channels) * channels;
                    let Ok(chunk) = input.read_chunk(take) else {
                        continue;
                    };
                    let (a, b) = chunk.as_slices();
                    block[..a.len()].copy_from_slice(a);
                    block[a.len()..take].copy_from_slice(b);
                    chunk.commit_all();
                    resampler.process(&block[..take], |converted| {
                        match output.write_chunk_uninit(converted.len()) {
                            Ok(room) => {
                                full = false;
                                room.fill_from_iter(converted.iter().copied());
                            }
                            // The device has stopped taking audio: drop it, and
                            // count the gap once.
                            Err(_) => {
                                if !full {
                                    glitches.fetch_add(1, Ordering::Relaxed);
                                }
                                full = true;
                            }
                        }
                    });
                }
            })
            .map_err(|error| AudioError::Resample {
                reason: error.to_string(),
            })?;
        Ok(Self {
            stop,
            bell,
            thread: Some(thread),
        })
    }
}

impl Drop for RateConverter {
    fn drop(&mut self) {
        // Joining blocks until the thread notices `stop` (a few milliseconds).
        // Some backends drop the stream's callback on their own audio thread,
        // so this can run there; that only happens when the stream is torn
        // down, never while audio is playing.
        self.stop.store(true, Ordering::Relaxed);
        self.bell.ring();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Something a stream's callback fills with audio.
trait Fill: Send + 'static {
    /// How long after this callback the samples it fills will play.
    fn playback_latency(&mut self, _latency: Duration) {}
    fn fill<T: Sample + FromSample<f32>>(&mut self, output: &mut [T]);
}

impl Fill for DeviceWriter {
    fn playback_latency(&mut self, latency: Duration) {
        self.fade
            .latency_micros
            .store(latency.as_micros() as u64, Ordering::Relaxed);
    }

    fn fill<T: Sample + FromSample<f32>>(&mut self, output: &mut [T]) {
        self.write(output);
    }
}

impl Fill for ExtraOutput {
    fn playback_latency(&mut self, latency: Duration) {
        self.own_latency = latency;
    }

    fn fill<T: Sample + FromSample<f32>>(&mut self, output: &mut [T]) {
        ExtraOutput::fill(self, output);
    }
}

/// Audio playing on a device. Dropping it fades the sound out and stops it.
pub struct Playback {
    fade: Arc<Fade>,
    _stream: MainStream,
    /// Dropped before the input stream, so a recording in progress is
    /// finished while the input is still running.
    recorder: Option<Recorder>,
    _input_stream: Option<cpal::Stream>,
    /// The streams of the extra output devices that opened.
    _extra_streams: Vec<cpal::Stream>,
    /// Each extra stream's flag for having played the fade-out.
    drained: Vec<Arc<AtomicBool>>,
    outputs: Vec<OutputStatus>,
    device: String,
    input: Option<(String, usize)>,
    input_meter: Option<Arc<InputMeter>>,
    input_problem: Option<AudioError>,
    settings: Settings,
    health: Health,
}

impl Drop for Playback {
    fn drop(&mut self) {
        // Wait for the callback to bring the output down to silence; the
        // streams stop right after. A stream that has stopped calling back
        // (an unplugged device) would never finish, so don't wait forever.
        self.fade.stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + FADE_OUT + Duration::from_millis(100);
        while !self.fade.done.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        // The ramp is queued behind the audio already in the device's buffer,
        // and closing the stream throws the queue away. Let that play out
        // first: two periods, as devices queue about that much.
        if self.fade.done.load(Ordering::Acquire) {
            let frames = self.fade.period.load(Ordering::Relaxed) as f32;
            let queued = Duration::from_secs_f32(2.0 * frames / self.settings.sample_rate);
            std::thread::sleep(queued.min(Duration::from_millis(500)));
            // The extra devices play from queues that were behind the main
            // one, so the ramp reaches them later.
            let deadline = Instant::now() + Duration::from_millis(500);
            while self.drained.iter().any(|d| !d.load(Ordering::Acquire))
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            if !self.drained.is_empty() {
                std::thread::sleep(queued.min(Duration::from_millis(500)));
            }
        }
    }
}

/// What runs the engine: a device's stream, or the virtual output's timer.
/// Held for being dropped, which stops it.
#[allow(dead_code)]
enum MainStream {
    Device(cpal::Stream),
    Virtual(VirtualClock),
}

/// The virtual output: a thread that calls the writer once per block's
/// duration, in real time, and discards what it renders. It stands in for a
/// device that plays to nowhere, with the same fade-out on stopping, the same
/// MIDI timing and the same pace.
struct VirtualClock {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    tap: Arc<Mutex<Option<Vec<f32>>>>,
}

/// A way to hear what the virtual output would have played, for tests. It is
/// off until [`start`](Self::start): the output discards audio otherwise.
///
/// A capture belongs to one run of the output. When playback restarts (a
/// device change, an Output node tied to another device), the old handle
/// stays valid but never fills again: fetch a new one with
/// [`Playback::virtual_capture`].
#[derive(Clone)]
pub struct VirtualCapture(Arc<Mutex<Option<Vec<f32>>>>);

impl VirtualCapture {
    /// Starts keeping the interleaved samples the output renders, dropping
    /// any kept before.
    pub fn start(&self) {
        *self.0.lock().unwrap() = Some(Vec::new());
    }

    /// Hands over what has been kept since [`start`](Self::start) or the last
    /// call, and keeps going. Every block rendered since is in it, whole and
    /// in order: the output waits for this rather than skipping a block, and
    /// taking the buffer instead of copying it keeps that wait short however
    /// long the capture runs.
    pub fn take(&self) -> Vec<f32> {
        self.0
            .lock()
            .unwrap()
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }
}

/// Frames per callback of the virtual output unless the settings say.
const VIRTUAL_BUFFER: u32 = 256;
/// The sample rate of the virtual output unless the settings say.
const VIRTUAL_RATE: u32 = 48_000;
/// The smallest and largest buffer the virtual output takes, in frames.
pub(crate) const VIRTUAL_BUFFERS: (u32, u32) = (16, 8192);
/// The channels of the virtual output unless the settings say.
const VIRTUAL_CHANNELS: usize = 2;
/// The most channels the virtual output takes.
pub(crate) const VIRTUAL_MAX_CHANNELS: usize = 8;

impl VirtualClock {
    fn start(
        mut writer: Box<DeviceWriter>,
        frames: u32,
        rate: u32,
        channels: usize,
    ) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let tap = Arc::new(Mutex::<Option<Vec<f32>>>::new(None));
        let tapped = Arc::clone(&tap);
        let period = Duration::from_secs_f64(f64::from(frames) / f64::from(rate));
        let thread = std::thread::Builder::new()
            .name("noodle virtual output".into())
            .spawn(move || {
                let mut buffer = vec![0.0f32; frames as usize * channels];
                let mut next = Instant::now();
                while !flag.load(Ordering::Relaxed) {
                    writer.write(&mut buffer);
                    // A reader holds the lock only to swap the buffer out, so
                    // waiting costs next to nothing and no block is lost. The
                    // clock catches up if it ever does cost one.
                    if let Some(kept) = tapped.lock().unwrap().as_mut() {
                        kept.extend_from_slice(&buffer);
                    }
                    next += period;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else if now - next > period * 8 {
                        // Far behind (the machine was suspended, say): carry
                        // on from now rather than rushing to catch up.
                        next = now;
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
            tap,
        })
    }
}

impl Drop for VirtualClock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What became of one extra output device that playback was asked to open.
#[derive(Debug)]
pub struct OutputStatus {
    /// The device's ID, as asked for.
    pub device: String,
    /// What it opened as, or why it couldn't.
    pub result: Result<OpenedOutput, AudioError>,
}

/// An extra output device that is playing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedOutput {
    pub name: String,
    pub channels: usize,
    /// The sample rate the device runs at. When it is not the engine's, the
    /// audio sent to it is converted.
    pub rate: u32,
}

impl Playback {
    /// How long after the audio thread renders a sample the device plays it,
    /// as the driver reports it: zero until the first callback.
    pub fn output_latency(&self) -> Duration {
        Duration::from_micros(self.fade.latency_micros.load(Ordering::Relaxed))
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    /// The tap on the virtual output, if that is what plays. Tests use it to
    /// check the audio the app made.
    pub fn virtual_capture(&self) -> Option<VirtualCapture> {
        match &self._stream {
            MainStream::Virtual(clock) => Some(VirtualCapture(Arc::clone(&clock.tap))),
            MainStream::Device(_) => None,
        }
    }

    /// How each extra output device fared, in the order asked for. Devices
    /// that couldn't open are left out of the engine's buses, so Output nodes
    /// tied to them report that they have nowhere to play.
    pub fn outputs(&self) -> &[OutputStatus] {
        &self.outputs
    }

    /// How long before the app got it the input was captured, as the driver
    /// reports it: zero until the first callback, or without input.
    pub fn input_latency(&self) -> Duration {
        self.recorder
            .as_ref()
            .map_or(Duration::ZERO, Recorder::input_latency)
    }

    /// Where the time between an input sound and its output goes, as far as
    /// the drivers and the callbacks tell. It fills in over the first
    /// callbacks and then follows the backlog live.
    pub fn latency_report(&self) -> LatencyReport {
        LatencyReport {
            sample_rate: self.settings.sample_rate,
            block_frames: self.settings.max_frames as u32,
            output_buffer_frames: self.fade.period.load(Ordering::Relaxed),
            output_driver: self.output_latency(),
            input: self.input_meter.as_ref().map(|meter| InputLatency {
                sample_rate: meter.sample_rate(),
                buffer_frames: meter.callback_frames(),
                driver: self.input_latency(),
                backlog_frames: meter.backlog_frames(),
            }),
        }
    }

    /// The input device's name and channel count, if input is on.
    pub fn input(&self) -> Option<(&str, usize)> {
        self.input
            .as_ref()
            .map(|(name, channels)| (name.as_str(), *channels))
    }

    /// Why input isn't on, if it was asked for and couldn't be opened.
    pub fn input_problem(&self) -> Option<&AudioError> {
        self.input_problem.as_ref()
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Starts recording the input to a WAV file at `path`. Input that
    /// arrived before this call isn't included.
    pub fn start_recording(&mut self, path: &Path) -> Result<(), RecordError> {
        self.recorder
            .as_mut()
            .ok_or(RecordError::NoInput)?
            .start(path)
    }

    /// Stops recording and finishes the file.
    pub fn stop_recording(&mut self) -> Result<Take, RecordError> {
        self.recorder
            .as_mut()
            .ok_or(RecordError::NotRecording)?
            .stop()
    }

    /// The recording so far as peaks, for drawing it while it runs. See
    /// [`Recorder::live_peaks`].
    pub fn live_peaks(&self, columns: usize) -> Option<(u64, Vec<crate::Peak>)> {
        self.recorder.as_ref().map(|r| r.live_peaks(columns))
    }

    /// Whether a recording is running. A recording that failed (the disk
    /// filled up, say) stops being one; [`stop_recording`](Self::stop_recording)
    /// says why.
    pub fn is_recording(&self) -> bool {
        self.recorder.as_ref().is_some_and(Recorder::is_recording)
    }

    /// The devices' health since playback started. Check it regularly, e.g.
    /// every UI frame.
    pub fn health(&mut self) -> &mut Health {
        &mut self.health
    }
}

/// Whether an error means the stream has stopped for good. Underruns, a
/// rerouted device and a refused real-time priority are survivable.
pub fn is_fatal(error: &DeviceError) -> bool {
    !matches!(
        error.kind(),
        DeviceErrorKind::Xrun | DeviceErrorKind::DeviceChanged | DeviceErrorKind::RealtimeDenied
    )
}

/// Which of the two streams something came from. They fail separately:
/// losing the input leaves the output playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Output,
    Input,
    /// An output device other than the main one. Losing it silences the
    /// Output nodes tied to it and nothing else.
    ExtraOutput,
}

/// Where the time from a sound entering the input to it leaving the output
/// goes. Built by [`Playback::latency_report`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatencyReport {
    pub sample_rate: f32,
    /// The engine's block size, the most frames it renders at once.
    pub block_frames: u32,
    /// Frames in the output device's callbacks, as granted: zero until the
    /// first.
    pub output_buffer_frames: u32,
    /// How long after the callback the device plays, as the driver reports:
    /// zero if it doesn't say, or before the first callback.
    pub output_driver: Duration,
    /// `None` when input is off.
    pub input: Option<InputLatency>,
}

/// The input half of a [`LatencyReport`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputLatency {
    /// The input device's sample rate, which may differ from the engine's.
    pub sample_rate: f32,
    /// Frames in the input device's callbacks, as granted, at its rate: zero
    /// until the first.
    pub buffer_frames: u32,
    /// How long before its callback the driver says the input was captured:
    /// zero if it doesn't say.
    pub driver: Duration,
    /// Frames queued between the input and the engine.
    pub backlog_frames: u32,
}

/// Drivers that report a playback latency over this are slow enough that
/// live playing suffers; Bluetooth output is the usual cause.
pub const SLOW_OUTPUT: Duration = Duration::from_millis(80);

impl LatencyReport {
    fn frames(&self, frames: u32) -> Duration {
        Duration::from_secs_f32(frames as f32 / self.sample_rate.max(1.0))
    }

    /// The output side: the driver's figure, or the buffer when it gives
    /// none (ALSA reports zero).
    pub fn output(&self) -> Duration {
        if self.output_driver.is_zero() {
            self.frames(self.output_buffer_frames)
        } else {
            self.output_driver
        }
    }

    /// The input side, the same way. `None` when input is off.
    pub fn input_side(&self) -> Option<Duration> {
        let input = self.input?;
        Some(if input.driver.is_zero() {
            Duration::from_secs_f32(input.buffer_frames as f32 / input.sample_rate.max(1.0))
        } else {
            input.driver
        })
    }

    /// The backlog's wait, with one engine block for the block being
    /// rendered. `None` when input is off.
    pub fn queue(&self) -> Option<Duration> {
        let input = self.input?;
        Some(self.frames(input.backlog_frames + self.block_frames))
    }

    /// An estimate of the time from a sound entering the input to it leaving
    /// the output: the input's capture delay, the wait in the backlog, and
    /// the output's delay. `None` when input is off or hasn't called back
    /// yet.
    pub fn round_trip(&self) -> Option<Duration> {
        let input = self.input?;
        if input.buffer_frames == 0 || self.output_buffer_frames == 0 {
            return None;
        }
        Some(self.input_side()? + self.queue()? + self.output())
    }

    /// Whether the output is slow enough to hurt live playing.
    pub fn output_is_slow(&self) -> bool {
        self.output_driver > SLOW_OUTPUT
    }
}

/// Whether a device's name suggests a Bluetooth link, for when its driver
/// doesn't report the delay.
pub fn looks_like_bluetooth(name: &str) -> bool {
    let name = name.to_lowercase();
    ["bluetooth", "airpods", "buds", "wh-1000", "wf-1000", "bt "]
        .iter()
        .any(|hint| name.contains(hint))
}

/// What the devices have reported. Some backends (ALSA among them) report
/// errors from the audio thread itself, so the reports arrive lock-free:
/// underruns and input glitches are counted, and other errors are queued.
pub struct Health {
    underruns: Arc<AtomicU64>,
    pub(crate) input_glitches: Arc<AtomicU64>,
    /// One queue per stream.
    errors: Vec<(Stream, Consumer<DeviceError>)>,
}

impl Health {
    fn new() -> Self {
        Self {
            underruns: Arc::new(AtomicU64::new(0)),
            input_glitches: Arc::new(AtomicU64::new(0)),
            errors: Vec::new(),
        }
    }

    /// A reporter for one more stream.
    fn reporter(&mut self, stream: Stream) -> Reporter {
        let (producer, consumer) = RingBuffer::new(ERROR_QUEUE);
        self.errors.push((stream, consumer));
        Reporter {
            underruns: Arc::clone(&self.underruns),
            errors: producer,
        }
    }

    /// Underruns and overruns so far, on either stream. A device's buffer
    /// ran dry or overflowed, so there was a gap in the sound.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Errors reported since the last call, other than underruns, with the
    /// stream each came from.
    pub fn errors(&mut self) -> impl Iterator<Item = (Stream, DeviceError)> + '_ {
        self.errors.iter_mut().flat_map(|(stream, queue)| {
            let stream = *stream;
            std::iter::from_fn(move || queue.pop().ok().map(|error| (stream, error)))
        })
    }
}

/// A stream's side of [`Health`]. Real-time safe.
struct Reporter {
    underruns: Arc<AtomicU64>,
    errors: Producer<DeviceError>,
}

/// More errors than this between checks are dropped. They're likely repeats.
const ERROR_QUEUE: usize = 16;

impl Reporter {
    fn report(&mut self, error: DeviceError) {
        if error.kind() == DeviceErrorKind::Xrun {
            self.underruns.fetch_add(1, Ordering::Relaxed);
        } else {
            let _ = self.errors.push(error);
        }
    }
}

/// Starts an engine playing on an output device, chosen by `choice`, with
/// `max_frames` frames per engine block. The channel count is the device's.
/// If `choice` turns input on, the input device records into Input nodes,
/// converted to the output's sample rate if it runs at another. Input that
/// can't be opened leaves Input nodes silent rather than failing: see
/// [`Playback::input_problem`]. Send the engine graphs with the returned
/// [`Controller`].
///
/// `midi` is told when the blocks run, so the MIDI In nodes on it place
/// notes within a block and recordings know where the transport was.
///
/// Errors while playing are reported through [`Playback::health`].
pub fn play(choice: &AudioConfig, max_frames: usize) -> Result<(Playback, Controller), AudioError> {
    play_with_outputs(choice, &[], max_frames, &MidiBus::new())
}

/// [`play`] with more output devices, `extra` by ID, for Output nodes tied to
/// them. The main device drives the engine; the others play what it renders
/// for them through a queue each, at the main device's sample rate, so a
/// device that can't take that rate can't open. A device that can't open
/// doesn't stop playback: see [`Playback::outputs`]. The extras' channels
/// follow the main device's in the engine's output, and the returned
/// controller's buses say so ([`Controller::set_buses`]).
///
/// The devices' clocks drift apart unless they share one, and the queue
/// between them copes by playing silence or skipping ahead, each counted as
/// an underrun in [`Health`]. Devices equal to the main one, and repeats,
/// are ignored.
pub fn play_with_outputs(
    choice: &AudioConfig,
    extra: &[String],
    max_frames: usize,
    midi: &MidiBus,
) -> Result<(Playback, Controller), AudioError> {
    open_playback_retrying(choice, extra, max_frames, midi, true)
}

/// [`play_with_outputs`] with the transport stopped from the first block, for
/// an engine that runs before anything is played: the graph renders, but a
/// timeline waits for the transport to start.
pub fn play_with_outputs_stopped(
    choice: &AudioConfig,
    extra: &[String],
    max_frames: usize,
    midi: &MidiBus,
) -> Result<(Playback, Controller), AudioError> {
    open_playback_retrying(choice, extra, max_frames, midi, false)
}

/// [`open_playback`], and when the automatic live-sized buffer made it fail
/// (a driver that lists a range but refuses a fixed size when it builds the
/// stream), once more leaving the size to the device. A size the user chose
/// still fails loudly.
fn open_playback_retrying(
    choice: &AudioConfig,
    extra: &[String],
    max_frames: usize,
    midi: &MidiBus,
    running: bool,
) -> Result<(Playback, Controller), AudioError> {
    match open_playback(choice, extra, max_frames, midi, running, true) {
        Err(_) if choice.buffer_size.is_none() => {
            open_playback(choice, extra, max_frames, midi, running, false)
        }
        result => result,
    }
}

fn open_playback(
    choice: &AudioConfig,
    extra: &[String],
    max_frames: usize,
    midi: &MidiBus,
    running: bool,
    live_default: bool,
) -> Result<(Playback, Controller), AudioError> {
    let (host_id, target, name, main_id, main_channels, rate) =
        if choice.output.as_deref() == Some(VIRTUAL_OUTPUT) {
            // Input and the default host still come from the settings, so a
            // virtual output can be paired with a real microphone.
            let host_id = host_for(choice.host.as_deref(), None, Direction::Output)?;
            // What `virtual_output()` advertises; a hand-edited setting
            // outside it is refused, not trusted.
            let frames = choice.buffer_size.unwrap_or(VIRTUAL_BUFFER);
            if !(VIRTUAL_BUFFERS.0..=VIRTUAL_BUFFERS.1).contains(&frames) {
                return Err(AudioError::UnsupportedBufferSize {
                    frames,
                    min: VIRTUAL_BUFFERS.0,
                    max: VIRTUAL_BUFFERS.1,
                });
            }
            let rate = choice.sample_rate.unwrap_or(VIRTUAL_RATE);
            if rate == 0 {
                return Err(AudioError::UnsupportedSampleRate(rate));
            }
            let channels = choice.channels.map_or(VIRTUAL_CHANNELS, usize::from);
            if !(1..=VIRTUAL_MAX_CHANNELS).contains(&channels) {
                return Err(AudioError::UnsupportedChannels {
                    wanted: channels as u16,
                    max: VIRTUAL_MAX_CHANNELS as u16,
                });
            }
            (
                host_id,
                Target::Virtual { frames },
                "Virtual (no sound)".to_owned(),
                Some(VIRTUAL_OUTPUT.to_owned()),
                channels,
                rate,
            )
        } else {
            let host_id = host_for(
                choice.host.as_deref(),
                choice.output.as_deref(),
                Direction::Output,
            )?;
            let host = open_host(host_id)?;
            let device = find_device(&host, choice.output.as_deref(), Direction::Output)?;
            let name = device_name(&device);
            let main_id = device.id().ok().map(|id| id.to_string());
            let ranges: Vec<_> = device.supported_output_configs()?.collect();
            let default = device.default_output_config()?;
            // Left to the device, the buffer is often large (512 on macOS):
            // ask for a live-sized one where the device allows.
            let live = match choice.buffer_size {
                Some(_) => None,
                None if !live_default => None,
                None => buffer_request(
                    &ranges,
                    choice.sample_rate.unwrap_or(default.sample_rate()),
                    None,
                ),
            };
            let chosen = |buffer| {
                choose_config(
                    &ranges,
                    default,
                    choice.sample_rate,
                    buffer,
                    choice.channels,
                )
            };
            let Chosen { config, format } = match live {
                Some(frames) => chosen(Some(frames)).or_else(|_| chosen(None))?,
                None => chosen(choice.buffer_size)?,
            };
            let channels = usize::from(config.channels);
            let rate = config.sample_rate;
            (
                host_id,
                Target::Device {
                    device,
                    config,
                    format,
                },
                name,
                main_id,
                channels,
                rate,
            )
        };
    let mut health = Health::new();

    // The extra devices, opened but not started. The engine's channels are
    // the main device's and then each of these, in order.
    let mut statuses = Vec::new();
    let mut extras: Vec<OpenedExtra> = Vec::new();
    let mut seen = Vec::new();
    for id in extra {
        if Some(id) == main_id.as_ref() || seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let result = match open_extra(id, rate) {
            Ok(opened) => {
                let info = OpenedOutput {
                    name: opened.name.clone(),
                    channels: opened.channels,
                    rate: opened.rate,
                };
                extras.push(OpenedExtra {
                    status: statuses.len(),
                    ..opened
                });
                Ok(info)
            }
            Err(error) => Err(error),
        };
        statuses.push(OutputStatus {
            device: id.clone(),
            result,
        });
    }

    let engine_channels = main_channels + extras.iter().map(|e| e.channels).sum::<usize>();
    let settings = Settings {
        sample_rate: rate as f32,
        max_frames,
        channels: engine_channels,
    };
    let (mut controller, processor) = engine_with_workers(settings, choice.threads, false)?;
    let mut buses = vec![Bus {
        device: main_id.clone().unwrap_or_default(),
        channels: main_channels,
    }];
    buses.extend(extras.iter().map(|e| Bus {
        device: e.device.clone(),
        channels: e.channels,
    }));
    controller.set_buses(buses)?;
    if !running {
        controller.transport().stop();
    }

    // Input that can't be opened doesn't stop the sound: playback goes on
    // without it, and says why.
    let mut input_problem = None;
    let mut input_meter = None;
    let (writer, input) = match choice_id(&choice.input) {
        None => (DeviceWriter::new(processor), None),
        Some(id) => {
            // A chosen input device opens on its own host, which may differ
            // from the output's (JACK in, ALSA out, say). The default one
            // comes from the output's host.
            let opened = match id {
                Some(_) => host_for(None, id, Direction::Input),
                None => Ok(host_id),
            }
            .and_then(open_host)
            .and_then(|host| {
                open_input(
                    &host,
                    id,
                    &settings,
                    choice.buffer_size,
                    live_default,
                    &mut health,
                )
            });
            match opened {
                Ok((stream, name, channels, feed, recorder)) => {
                    input_meter = Some(feed.meter());
                    (
                        DeviceWriter::with_input(processor, feed),
                        Some((stream, name, channels, recorder)),
                    )
                }
                Err(error) => {
                    input_problem = Some(error);
                    (DeviceWriter::new(processor), None)
                }
            }
        }
    };
    let writer = writer.with_midi_clock(Arc::clone(midi.clock()), controller.transport());
    let (writer, outputs) = writer.with_resampled_outputs(
        main_channels,
        &extras
            .iter()
            .map(|e| (e.channels, e.rate))
            .collect::<Vec<_>>(),
        &health.underruns,
    );
    let fade = Arc::clone(&writer.fade);
    // An extra that won't build keeps its channels, which are then dropped,
    // but doesn't stop the rest from playing.
    let mut extra_streams = Vec::with_capacity(extras.len());
    for (opened, output) in extras.into_iter().zip(outputs) {
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                statuses[opened.status].result = Err(error);
                continue;
            }
        };
        let flag = Arc::clone(&output.drained);
        let mut reporter = health.reporter(Stream::ExtraOutput);
        let on_error = move |error| reporter.report(error);
        match open_format(
            &opened.device_handle,
            opened.config,
            opened.format,
            output,
            on_error,
        ) {
            Ok(stream) => extra_streams.push((opened.status, stream, flag)),
            Err(error) => statuses[opened.status].result = Err(error),
        }
    }
    let mut reporter = health.reporter(Stream::Output);
    let on_error = move |error| reporter.report(error);
    let stream = match target {
        Target::Device {
            device,
            config,
            format,
        } => Unstarted::Device(open_format(&device, config, format, writer, on_error)?),
        Target::Virtual { frames } => Unstarted::Virtual(Box::new(writer), frames),
    };
    // Input first, so the output finds some waiting.
    let (input_stream, input, recorder) = match input {
        Some((input_stream, name, channels, recorder)) => match input_stream.play() {
            Ok(()) => (Some(input_stream), Some((name, channels)), Some(recorder)),
            Err(error) => {
                input_problem = Some(error.into());
                (None, None, None)
            }
        },
        None => (None, None, None),
    };
    // The extras wait for the main stream's first blocks before they make a
    // sound, so they can start first.
    let mut playing = Vec::with_capacity(extra_streams.len());
    let mut drained = Vec::with_capacity(extra_streams.len());
    for (status, stream, flag) in extra_streams {
        match stream.play() {
            Ok(()) => {
                playing.push(stream);
                drained.push(flag);
            }
            Err(error) => statuses[status].result = Err(error.into()),
        }
    }
    let extra_streams = playing;
    let stream = match stream {
        Unstarted::Device(stream) => {
            stream.play()?;
            MainStream::Device(stream)
        }
        Unstarted::Virtual(writer, frames) => MainStream::Virtual(
            VirtualClock::start(writer, frames, rate, main_channels).map_err(|error| {
                AudioError::Virtual {
                    reason: error.to_string(),
                }
            })?,
        ),
    };
    Ok((
        Playback {
            fade,
            _stream: stream,
            recorder,
            _input_stream: input_stream,
            _extra_streams: extra_streams,
            drained,
            outputs: statuses,
            device: name,
            input,
            input_meter,
            input_problem,
            settings,
            health,
        },
        controller,
    ))
}

/// What the main output plays on.
enum Target {
    Device {
        device: cpal::Device,
        config: StreamConfig,
        format: SampleFormat,
    },
    Virtual {
        frames: u32,
    },
}

/// The main output, built and waiting for the inputs and extras to start.
enum Unstarted {
    Device(cpal::Stream),
    Virtual(Box<DeviceWriter>, u32),
}

/// An extra output device, ready to open a stream on.
struct OpenedExtra {
    /// Its place in the statuses.
    status: usize,
    device: String,
    name: String,
    channels: usize,
    /// The sample rate the device runs at.
    rate: u32,
    device_handle: cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
}

/// Finds the extra output device `id` and picks its configuration: at `rate`
/// if the device runs at it, else at its own default rate, and the engine's
/// audio is converted.
fn open_extra(id: &str, rate: u32) -> Result<OpenedExtra, AudioError> {
    let host = open_host(host_for(None, Some(id), Direction::Output)?)?;
    let device = find_device(&host, Some(id), Direction::Output)?;
    let ranges: Vec<_> = device.supported_output_configs()?.collect();
    // The queue decouples the callbacks, so the device uses whatever buffer
    // size suits it.
    let default = device.default_output_config()?;
    let Chosen { config, format } = match choose_config(&ranges, default, Some(rate), None, None) {
        Err(AudioError::UnsupportedSampleRate(_)) => {
            choose_config(&ranges, default, None, None, None)?
        }
        chosen => chosen?,
    };
    Ok(OpenedExtra {
        status: 0,
        device: id.to_owned(),
        name: device_name(&device),
        channels: usize::from(config.channels),
        rate: config.sample_rate,
        device_handle: device,
        config,
        format,
    })
}

/// Opens (but doesn't start) an output stream in the device's sample format.
fn open_format(
    device: &cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
    filler: impl Fill,
    on_error: impl FnMut(DeviceError) + Send + 'static,
) -> Result<cpal::Stream, AudioError> {
    Ok(match format {
        SampleFormat::F32 => open::<f32>(device, config, filler, on_error),
        SampleFormat::F64 => open::<f64>(device, config, filler, on_error),
        SampleFormat::I8 => open::<i8>(device, config, filler, on_error),
        SampleFormat::I16 => open::<i16>(device, config, filler, on_error),
        SampleFormat::I24 => open::<I24>(device, config, filler, on_error),
        SampleFormat::I32 => open::<i32>(device, config, filler, on_error),
        SampleFormat::U8 => open::<u8>(device, config, filler, on_error),
        SampleFormat::U16 => open::<u16>(device, config, filler, on_error),
        SampleFormat::U32 => open::<u32>(device, config, filler, on_error),
        format => return Err(AudioError::UnsupportedFormat(format)),
    }?)
}

fn open_host(id: Option<cpal::HostId>) -> Result<cpal::Host, AudioError> {
    Ok(match id {
        Some(id) => cpal::host_from_id(id)?,
        None => cpal::default_host(),
    })
}

/// `None` for no input, or `Some` device ID (`None` inside for the default).
fn choice_id(choice: &InputChoice) -> Option<Option<&str>> {
    match choice {
        InputChoice::Off => None,
        InputChoice::Default => Some(None),
        InputChoice::Device(id) => Some(Some(id)),
    }
}

/// Opens (but doesn't start) an input stream, at the engine's sample rate if
/// the device has it and at its own otherwise (the feed converts), returning
/// it, the device's name, its channel count and the feed for the
/// output callback.
///
/// The input gets the same buffer size as the output (see
/// [`AudioConfig::buffer_size`]): the ring decouples the two callbacks, but
/// a smaller input buffer cuts latency directly. A size the device can't
/// take is moved into its range, and a device that doesn't say which sizes
/// it takes keeps its default.
fn open_input(
    host: &cpal::Host,
    id: Option<&str>,
    settings: &Settings,
    buffer_size: Option<u32>,
    live_default: bool,
    health: &mut Health,
) -> Result<(cpal::Stream, String, usize, Feed, Recorder), AudioError> {
    let device = find_device(host, id, Direction::Input)?;
    let name = device_name(&device);
    let ranges: Vec<_> = device.supported_input_configs()?.collect();
    let default = device.default_input_config()?;
    // The engine's rate if the device has it; otherwise its own, which the
    // feed converts.
    let engine_rate = settings.sample_rate as u32;
    let rate = if ranges.iter().any(|range| range.contains_rate(engine_rate)) {
        engine_rate
    } else {
        default.sample_rate()
    };
    let buffer = if buffer_size.is_none() && !live_default {
        None
    } else {
        buffer_request(&ranges, rate, buffer_size)
    };
    let chosen = |buffer| choose_config(&ranges, default, Some(rate), buffer, None);
    let Chosen { config, format } = match buffer {
        Some(frames) => chosen(Some(frames)).or_else(|_| chosen(None))?,
        None => chosen(None)?,
    };
    let channels = usize::from(config.channels);
    let (capture, feed, recorder) = recordable_input_path(
        channels,
        config.sample_rate as f32,
        settings.sample_rate,
        settings.max_frames,
        Arc::clone(&health.input_glitches),
    );
    let mut reporter = health.reporter(Stream::Input);
    let on_error = move |error| reporter.report(error);
    let stream = match format {
        SampleFormat::F32 => record::<f32>(&device, config, capture, on_error),
        SampleFormat::F64 => record::<f64>(&device, config, capture, on_error),
        SampleFormat::I8 => record::<i8>(&device, config, capture, on_error),
        SampleFormat::I16 => record::<i16>(&device, config, capture, on_error),
        SampleFormat::I24 => record::<I24>(&device, config, capture, on_error),
        SampleFormat::I32 => record::<i32>(&device, config, capture, on_error),
        SampleFormat::U8 => record::<u8>(&device, config, capture, on_error),
        SampleFormat::U16 => record::<u16>(&device, config, capture, on_error),
        SampleFormat::U32 => record::<u32>(&device, config, capture, on_error),
        format => return Err(AudioError::UnsupportedFormat(format)),
    };
    // A device can list a range and still refuse the size when the stream
    // builds: leave it to the device, unless the size was chosen.
    let stream = match stream {
        Err(_) if buffer.is_some() && buffer_size.is_none() => {
            return open_input(host, id, settings, None, false, health);
        }
        stream => stream?,
    };
    Ok((stream, name, channels, feed, recorder))
}

fn record<T: SizedSample>(
    device: &cpal::Device,
    config: StreamConfig,
    mut capture: Capture,
    on_error: impl FnMut(DeviceError) + Send + 'static,
) -> Result<cpal::Stream, DeviceError>
where
    f32: FromSample<T>,
{
    device.build_input_stream(
        config,
        move |input: &[T], info: &cpal::InputCallbackInfo| {
            let time = info.timestamp();
            capture.set_latency(time.callback.duration_since(time.capture));
            capture.capture(input)
        },
        on_error,
        None,
    )
}

fn open<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: StreamConfig,
    mut filler: impl Fill,
    on_error: impl FnMut(DeviceError) + Send + 'static,
) -> Result<cpal::Stream, DeviceError> {
    device.build_output_stream(
        config,
        move |output: &mut [T], info: &cpal::OutputCallbackInfo| {
            let time = info.timestamp();
            filler.playback_latency(time.playback.duration_since(time.callback));
            filler.fill(output);
        },
        on_error,
        None,
    )
}

#[cfg(test)]
mod tests {
    use crate::input::input_path;
    use noodle_core::{Command, Connection, Endpoint, Node, Project};
    use noodle_engine::{INPUT_ID, OUTPUT_ID, Registry, engine};

    use super::*;

    const SETTINGS: Settings = Settings {
        sample_rate: 48_000.0,
        max_frames: 64,
        channels: 2,
    };

    /// A writer playing `level`, a constant, to every channel.
    fn writer(level: f32) -> DeviceWriter {
        // An unconnected input plays its parameter value.
        let mut project = Project::new();
        let output = project.new_node_id();
        let node = Node::new(OUTPUT_ID).with_param("in", level);
        Command::AddNode { id: output, node }
            .apply(&mut project)
            .unwrap();
        let (mut controller, processor) = engine(SETTINGS).unwrap();
        let diagnostics = controller.update(project.graph(), &Registry::with_builtins());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        // The controller can go: the plan is already queued.
        DeviceWriter::new(processor)
    }

    /// input → gain 1 → output, fed by a stereo input path.
    fn passthrough() -> (DeviceWriter, Capture) {
        let mut project = Project::new();
        let input = project.new_node_id();
        let output = project.new_node_id();
        for (id, node) in [(input, Node::new(INPUT_ID)), (output, Node::new(OUTPUT_ID))] {
            Command::AddNode { id, node }.apply(&mut project).unwrap();
        }
        Command::Connect(Connection {
            from: Endpoint::new(input, "out"),
            to: Endpoint::new(output, "in"),
        })
        .apply(&mut project)
        .unwrap();
        let (mut controller, processor) = engine(SETTINGS).unwrap();
        let diagnostics = controller.update(project.graph(), &Registry::with_builtins());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let glitches = Arc::new(AtomicU64::new(0));
        let (capture, feed) = input_path(
            2,
            SETTINGS.sample_rate,
            SETTINGS.sample_rate,
            SETTINGS.max_frames,
            glitches,
        );
        (DeviceWriter::with_input(processor, feed), capture)
    }

    #[test]
    fn input_plays_through_the_graph() {
        let (mut writer, mut capture) = passthrough();
        // Longer than a block, and not a multiple of one. The input is
        // converted, so it comes out after the converter's short delay.
        let mut output = vec![0.0f32; 150 * 2];
        for _ in 0..6 {
            capture.capture(&[0.5f32; 150 * 2]);
            writer.write(&mut output);
        }
        assert!(output.iter().all(|x| (x - 0.5).abs() < 0.01), "{output:?}");
    }

    #[test]
    fn health_gathers_every_stream() {
        let mut health = Health::new();
        let mut output = health.reporter(Stream::Output);
        let mut input = health.reporter(Stream::Input);
        output.report(DeviceErrorKind::Xrun.into());
        input.report(DeviceErrorKind::Xrun.into());
        output.report(DeviceErrorKind::DeviceNotAvailable.into());
        input.report(DeviceErrorKind::BackendError.into());
        assert_eq!(health.underruns(), 2);
        assert_eq!(health.errors().count(), 2);
    }

    #[test]
    fn errors_say_which_stream_they_came_from() {
        let mut health = Health::new();
        let mut output = health.reporter(Stream::Output);
        let mut input = health.reporter(Stream::Input);
        input.report(DeviceErrorKind::DeviceNotAvailable.into());
        output.report(DeviceErrorKind::BackendError.into());
        input.report(DeviceErrorKind::PermissionDenied.into());
        let mut errors: Vec<_> = health.errors().map(|(s, e)| (s, e.kind())).collect();
        errors.sort_by_key(|(stream, _)| *stream as u8);
        assert_eq!(
            errors,
            [
                (Stream::Output, DeviceErrorKind::BackendError),
                (Stream::Input, DeviceErrorKind::DeviceNotAvailable),
                (Stream::Input, DeviceErrorKind::PermissionDenied),
            ]
        );
    }

    #[test]
    fn health_counts_underruns_and_queues_other_errors() {
        let mut health = Health::new();
        let mut reporter = health.reporter(Stream::Output);
        reporter.report(DeviceErrorKind::Xrun.into());
        reporter.report(DeviceErrorKind::DeviceNotAvailable.into());
        reporter.report(DeviceErrorKind::Xrun.into());
        assert_eq!(health.underruns(), 2);
        let kinds: Vec<_> = health.errors().map(|(_, e)| e.kind()).collect();
        assert_eq!(kinds, [DeviceErrorKind::DeviceNotAvailable]);
        assert_eq!(health.errors().count(), 0, "errors are drained");
    }

    #[test]
    fn health_drops_errors_beyond_its_queue() {
        let mut health = Health::new();
        let mut reporter = health.reporter(Stream::Output);
        for _ in 0..ERROR_QUEUE + 5 {
            reporter.report(DeviceErrorKind::BackendError.into());
        }
        assert_eq!(health.errors().count(), ERROR_QUEUE);
    }

    #[test]
    fn only_some_errors_are_fatal() {
        assert!(!is_fatal(&DeviceErrorKind::Xrun.into()));
        assert!(!is_fatal(&DeviceErrorKind::DeviceChanged.into()));
        assert!(is_fatal(&DeviceErrorKind::DeviceNotAvailable.into()));
    }

    #[test]
    fn writes_what_the_processor_renders() {
        // Longer than a block, and not a multiple of one.
        let mut output = vec![0.0f32; 150 * 2];
        writer(0.25).write(&mut output);
        assert!(output.iter().all(|&x| x == 0.25), "{output:?}");
    }

    #[test]
    fn stopping_fades_the_output_out_without_a_jump() {
        let mut writer = writer(0.5);
        let mut output = vec![0.0f32; 64 * 2];
        writer.write(&mut output);
        assert!(output.iter().all(|&x| x == 0.5));

        writer.fade.stop.store(true, Ordering::Relaxed);
        // 10 ms at 48 kHz is 480 frames; write 600.
        let mut faded = vec![0.0f32; 600 * 2];
        writer.write(&mut faded);
        // Falls steadily from the last level to silence: no step bigger than
        // one frame's share of the ramp.
        let biggest_step = std::iter::once(0.5)
            .chain(faded.iter().step_by(2).copied())
            .collect::<Vec<_>>()
            .windows(2)
            .map(|pair| (pair[0] - pair[1]).abs())
            .fold(0.0f32, f32::max);
        assert!(biggest_step < 0.5 / 400.0, "{biggest_step}");
        assert!(faded.iter().rev().take(2 * 100).all(|&x| x == 0.0));
        assert!(writer.fade.done.load(Ordering::Acquire));
    }

    #[test]
    fn the_writer_notes_how_much_the_device_asks_for_at_once() {
        let mut writer = writer(0.0);
        writer.write(&mut vec![0.0f32; 32 * 2]);
        writer.write(&mut vec![0.0f32; 150 * 2]);
        writer.write(&mut [0.0f32; 10 * 2]);
        assert_eq!(writer.fade.period.load(Ordering::Relaxed), 150);
    }

    #[test]
    fn converts_to_the_device_format() {
        let mut output = vec![0i16; 10 * 2];
        writer(0.5).write(&mut output);
        assert!(output.iter().all(|&x| x == 16_384), "{output:?}");
    }

    #[test]
    fn clamps_loud_samples() {
        let mut output = vec![0.0f32; 10 * 2];
        writer(4.0).write(&mut output);
        assert!(output.iter().all(|&x| x == 1.0), "{output:?}");
        let mut output = vec![0.0f32; 10 * 2];
        writer(-4.0).write(&mut output);
        assert!(output.iter().all(|&x| x == -1.0), "{output:?}");
    }

    #[test]
    fn silences_samples_that_are_not_finite() {
        let mut output = vec![1.0f32; 10 * 2];
        writer(f32::NAN).write(&mut output);
        assert!(output.iter().all(|&x| x == 0.0), "{output:?}");
    }

    /// Two Output nodes: one on the main device (2 channels, playing 0.25)
    /// and one tied to "second" (1 channel, playing 0.5). Returns the main
    /// writer and what the second device would play.
    fn two_devices() -> (DeviceWriter, ExtraOutput) {
        let mut project = Project::new();
        for (device, level) in [("", 0.25), ("second", 0.5)] {
            let id = project.new_node_id();
            let mut node = Node::new(OUTPUT_ID).with_param("in", level);
            if !device.is_empty() {
                let mut config = noodle_core::Config::new();
                config.set("device", noodle_core::Value::Text(device.into()));
                node = node.with_config(config);
            }
            Command::AddNode { id, node }.apply(&mut project).unwrap();
        }
        let settings = Settings {
            channels: 3,
            ..SETTINGS
        };
        let (mut controller, processor) = engine(settings).unwrap();
        controller
            .set_buses(vec![
                Bus {
                    device: "main".into(),
                    channels: 2,
                },
                Bus {
                    device: "second".into(),
                    channels: 1,
                },
            ])
            .unwrap();
        let diagnostics = controller.update(project.graph(), &Registry::with_builtins());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let glitches = Arc::new(AtomicU64::new(0));
        let (writer, mut extras) = DeviceWriter::new(processor).with_outputs(2, &[1], &glitches);
        let extra = extras.remove(0);
        (writer, extra)
    }

    /// A writer with one extra device at `device_rate` (one channel). Nothing
    /// is rendered for it: tests queue audio with [`queue_sine`].
    fn device_at(device_rate: u32) -> (DeviceWriter, ExtraOutput, Arc<AtomicU64>) {
        let settings = Settings {
            channels: 3,
            ..SETTINGS
        };
        let (_controller, processor) = engine(settings).unwrap();
        let glitches = Arc::new(AtomicU64::new(0));
        let (writer, mut extras) =
            DeviceWriter::new(processor).with_resampled_outputs(2, &[(1, device_rate)], &glitches);
        let extra = extras.remove(0).unwrap();
        (writer, extra, glitches)
    }

    /// Queues `blocks` blocks of 64 frames of a 1000 Hz sine for the extra
    /// device, from engine frame `from`, as the main stream's callback does.
    fn queue_sine(writer: &mut DeviceWriter, from: usize, blocks: usize) {
        for block in 0..blocks {
            let frames: Vec<f32> = (0..64)
                .flat_map(|i| {
                    let n = from + block * 64 + i;
                    let x = (std::f32::consts::TAU * 1000.0 * n as f32 / 48_000.0).sin();
                    [0.0, 0.0, x]
                })
                .collect();
            writer.taps[0].push(&frames, 3);
        }
    }

    /// Queues `blocks` blocks of sine, waits for the converter to catch up
    /// with them, and takes everything it made from the device's queue.
    fn converted(
        writer: &mut DeviceWriter,
        extra: &mut ExtraOutput,
        blocks: usize,
        at_least: usize,
    ) -> Vec<f32> {
        queue_sine(writer, 0, blocks);
        let started = Instant::now();
        while extra.queue.slots() < at_least {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "only {} of {at_least} frames arrived",
                extra.queue.slots()
            );
            thread::sleep(Duration::from_millis(1));
        }
        // Let it finish the chunk it is on.
        thread::sleep(Duration::from_millis(20));
        let chunk = extra.queue.read_chunk(extra.queue.slots()).unwrap();
        let (a, b) = chunk.as_slices();
        let all: Vec<f32> = a.iter().chain(b).copied().collect();
        chunk.commit_all();
        all
    }

    fn crossings_per_second(samples: &[f32], rate: u32) -> f32 {
        let rising = samples
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        rising as f32 * rate as f32 / samples.len() as f32
    }

    #[test]
    fn a_device_at_another_rate_plays_the_same_pitch() {
        for device_rate in [44_100u32, 96_000] {
            let (mut writer, mut extra, glitches) = device_at(device_rate);
            // 100 blocks of 64 frames at 48 kHz, a bit under 133 ms.
            let expected = (6_400.0 * f64::from(device_rate) / 48_000.0) as usize;
            let played = converted(&mut writer, &mut extra, 100, expected - 400);
            // Everything that went in comes out, less what the converter still
            // holds (a chunk) and the filter's delay.
            assert!(
                played.len() <= expected && played.len() + 700 >= expected,
                "{} frames of {expected}",
                played.len()
            );
            // Skip the start, where the filter is still filling.
            let settled = &played[played.len() / 3..];
            let pitch = crossings_per_second(settled, device_rate);
            assert!(
                (pitch - 1000.0).abs() < 25.0,
                "{device_rate} Hz device plays {pitch} Hz"
            );
            let peak = settled.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            assert!(peak > 0.9 && peak <= 1.01, "peak {peak}");
            assert_eq!(glitches.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn a_converted_device_keeps_a_bigger_cushion_than_a_direct_one() {
        // The cushion covers the main stream's block at the device's rate and
        // the converter's lag, so it is bigger for a faster device.
        let (mut writer, mut extra, _) = device_at(96_000);
        // Three blocks: enough for the converter to make a chunk.
        queue_sine(&mut writer, 0, 3);
        let started = Instant::now();
        while extra.queue.slots() == 0 && started.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(1));
        }
        // Barely anything has been converted: not enough to start on.
        let mut out = vec![9.0f32; 32];
        extra.fill(&mut out);
        assert!(out.iter().all(|&x| x == 0.0), "{out:?}");
        assert!(extra.filling);
        assert_eq!(extra.glitches.load(Ordering::Relaxed), 0);
        assert!(extra.slack > 128 && extra.ratio == 2.0);
    }

    #[test]
    fn the_converter_stops_when_its_device_goes() {
        let (_writer, mut extra, _) = device_at(44_100);
        let converter = extra._converter.take().expect("a converter");
        let stop = Arc::clone(&converter.stop);
        drop(extra);
        // Dropping the output left the converter running (it was taken); now
        // dropping that stops and joins its thread.
        assert!(!stop.load(Ordering::Relaxed));
        drop(converter);
        assert!(stop.load(Ordering::Relaxed));
    }

    #[test]
    fn a_device_at_the_engines_rate_needs_no_converter() {
        let (mut writer, mut extra, _) = device_at(48_000);
        assert!(extra._converter.is_none());
        queue_sine(&mut writer, 0, 4);
        let mut buffer = vec![0.0f32; 32];
        extra.fill(&mut buffer);
        assert!(buffer.iter().any(|&x| x != 0.0));
    }

    #[test]
    fn a_device_that_is_quicker_than_the_main_one_is_held_back_to_match() {
        let gap = latency_gap_frames(
            Duration::from_millis(20),
            Duration::from_millis(5),
            48_000.0,
        );
        assert_eq!(gap, 720);
        // The slower of the two can't be sped up.
        assert_eq!(
            latency_gap_frames(
                Duration::from_millis(5),
                Duration::from_millis(20),
                48_000.0
            ),
            0
        );
    }

    #[test]
    fn an_extra_device_waits_for_the_main_devices_extra_latency() {
        let (mut writer, mut extra) = two_devices();
        let rate = SETTINGS.sample_rate;
        // The main device plays 40 ms after its callback; this one at once.
        writer.fade.latency_micros.store(40_000, Ordering::Relaxed);
        let held = latency_gap_frames(Duration::from_millis(40), Duration::ZERO, rate);
        let mut second = vec![9.0f32; 32];
        let blocks = held.div_ceil(64);
        // A cushion that would be enough without the latency gap.
        for _ in 0..blocks - 2 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.0), "{second:?}");
        // It plays once the backlog is as deep as the gap, not the gap plus
        // the cushion (96 frames, more than the two blocks it gets).
        for _ in 0..2 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.5), "{second:?}");
        // A device that is as slow as the main one needs no gap.
        let (mut writer, mut extra) = two_devices();
        writer.fade.latency_micros.store(40_000, Ordering::Relaxed);
        extra.playback_latency(Duration::from_millis(40));
        for _ in 0..3 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.5), "{second:?}");
    }

    #[test]
    fn each_device_plays_only_its_own_channels() {
        let (mut writer, mut extra) = two_devices();
        let mut main = vec![0.0f32; 64 * 2];
        writer.write(&mut main);
        assert!(main.iter().all(|&x| x == 0.25), "{main:?}");
        writer.write(&mut main);
        // The second device has what it needs once the main stream has
        // delivered a period plus its own block.
        let mut second = vec![9.0f32; 32];
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.5), "{second:?}");
    }

    #[test]
    fn an_extra_device_waits_for_a_cushion_before_it_plays() {
        let (mut writer, mut extra) = two_devices();
        // The main stream asks for 64 frames at a time and has delivered
        // one block; this device wants 128, so it can't be covered yet.
        writer.write(&mut vec![0.0f32; 64 * 2]);
        let mut second = vec![9.0f32; 128];
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.0), "{second:?}");
        assert_eq!(extra.glitches.load(Ordering::Relaxed), 0);
        writer.write(&mut vec![0.0f32; 64 * 2]);
        writer.write(&mut vec![0.0f32; 64 * 2]);
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.5), "{second:?}");
    }

    #[test]
    fn an_extra_device_that_runs_dry_plays_silence_and_refills() {
        let (mut writer, mut extra) = two_devices();
        writer.write(&mut vec![0.0f32; 64 * 2]);
        writer.write(&mut vec![0.0f32; 64 * 2]);
        let mut second = vec![9.0f32; 32];
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.5));
        // 96 frames are left; asking for 120 runs dry: the 96 that exist,
        // then silence, then silence until a cushion has built up.
        let mut second = vec![9.0f32; 120];
        extra.fill(&mut second);
        assert!(second[..96].iter().all(|&x| x == 0.5), "{second:?}");
        assert!(second[96..].iter().all(|&x| x == 0.0), "{second:?}");
        assert_eq!(extra.glitches.load(Ordering::Relaxed), 1);
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.0));
        for _ in 0..3 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.5), "{second:?}");
    }

    #[test]
    fn an_extra_device_that_falls_behind_skips_ahead() {
        let (mut writer, mut extra) = two_devices();
        // The main stream delivers far more than the device takes.
        for _ in 0..20 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        let mut second = vec![9.0f32; 32];
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x == 0.5));
        assert_eq!(extra.glitches.load(Ordering::Relaxed), 1);
        // What's left is a cushion, not the whole backlog.
        assert!(extra.queue.slots() <= 3 * (64 + 32));
    }

    #[test]
    fn a_callback_tells_the_clock_where_the_transport_is_and_which_span_each_block_plays() {
        let (controller, processor) = engine(SETTINGS).unwrap();
        let clock = Arc::new(AudioClock::default());
        let mut writer = DeviceWriter::new(processor)
            .with_midi_clock(Arc::clone(&clock), controller.transport());
        assert_eq!(clock.anchor(), None);
        assert_eq!(clock.window(), None);
        // Four 64-frame blocks (1333 microseconds each) in one callback.
        let before = now_micros();
        writer.write(&mut [0.0f32; 256 * 2]);
        let after = now_micros();
        let (called, sample) = clock.anchor().expect("an anchor");
        assert!((before..=after).contains(&called));
        assert_eq!(sample, 0);
        // The last block plays the span that began three blocks into the
        // period before this callback.
        let window = clock.window().expect("a window");
        let period = 5_333;
        let into = 4_000;
        assert!(
            (before.saturating_sub(period) + into..=after.saturating_sub(period) + into)
                .contains(&window),
            "{window} vs {before}..{after}"
        );
    }

    #[test]
    fn a_full_queue_drops_blocks_and_counts_them() {
        let (mut writer, extra) = two_devices();
        // Half a second (24000 frames) fits; each block queues 64.
        for _ in 0..400 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        // A queue that stays full is one gap, not one per block.
        assert_eq!(extra.glitches.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_fade_out_reaches_the_extra_devices() {
        let (mut writer, mut extra) = two_devices();
        writer.fade.stop.store(true, Ordering::Relaxed);
        for _ in 0..20 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        let mut second = vec![9.0f32; 32];
        extra.fill(&mut second);
        assert!(second.iter().all(|&x| x <= 0.5), "{second:?}");
        assert!(*second.last().unwrap() == 0.0 || extra.filling);
    }

    #[test]
    fn an_extra_device_says_when_the_fade_out_has_played() {
        let (mut writer, mut extra) = two_devices();
        writer.fade.stop.store(true, Ordering::Relaxed);
        for _ in 0..20 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        assert!(writer.fade.done.load(Ordering::Acquire));
        assert!(!extra.drained.load(Ordering::Acquire));
        let mut block = vec![0.0f32; 32];
        // Playing the queued audio, the ramp comes down and then it is silent.
        for _ in 0..100 {
            extra.fill(&mut block);
        }
        assert!(extra.drained.load(Ordering::Acquire));
    }

    #[test]
    fn an_extra_whose_cushion_is_bigger_than_its_queue_still_plays() {
        let (mut writer, mut extra) = two_devices();
        for _ in 0..130 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        // Asking for more than the queue could ever hold ahead.
        let mut block = vec![9.0f32; 40_000];
        extra.fill(&mut block);
        assert!(block.contains(&0.5), "never started");
    }

    fn virtual_config() -> AudioConfig {
        AudioConfig {
            output: Some(VIRTUAL_OUTPUT.to_owned()),
            ..AudioConfig::default()
        }
    }

    #[test]
    fn the_virtual_output_runs_the_engine_in_real_time() {
        let (playback, controller) = play(&virtual_config(), 128).unwrap();
        assert_eq!(playback.device(), "Virtual (no sound)");
        assert_eq!(playback.settings().channels, 2);
        let transport = controller.transport();
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(400));
        let seconds = transport.position() as f32 / playback.settings().sample_rate;
        let elapsed = started.elapsed().as_secs_f32();
        // It paces itself by the clock: neither flat out nor stalled. A
        // callback runs its blocks at once, so it can lead the clock by a
        // period or so; flat out would be far past that.
        assert!(
            seconds > 0.2 && seconds <= elapsed + 0.05,
            "{seconds} in {elapsed}"
        );
    }

    #[test]
    fn the_virtual_outputs_tap_keeps_what_it_plays_only_once_started() {
        let (playback, _controller) = play(&virtual_config(), 128).unwrap();
        let tap = playback.virtual_capture().expect("a virtual output");
        std::thread::sleep(Duration::from_millis(60));
        assert!(tap.take().is_empty(), "nothing is kept until asked");
        // The window is timed rather than assumed: a loaded machine can keep
        // this thread asleep well past 300 ms (macOS CI once took 480 ms),
        // and the output plays on meanwhile.
        let began = Instant::now();
        tap.start();
        std::thread::sleep(Duration::from_millis(300));
        let kept = tap.take();
        let window = began.elapsed().as_secs_f32();
        // A whole number of stereo frames, and as many of them as the window
        // holds at the output's pace: not flat out (it may catch up a few
        // blocks it was late with, never more), not stalled.
        let rate = playback.settings().sample_rate;
        let seconds = kept.len() as f32 / 2.0 / rate;
        assert_eq!(kept.len() % 2, 0);
        assert!(
            seconds > window * 0.5 && seconds < window + 0.1,
            "{seconds} s kept in {window} s"
        );
    }

    #[test]
    fn the_virtual_output_can_have_more_channels() {
        let config = AudioConfig {
            channels: Some(6),
            ..virtual_config()
        };
        let (playback, _controller) = play(&config, 128).unwrap();
        assert_eq!(playback.settings().channels, 6);
        let tap = playback.virtual_capture().unwrap();
        tap.start();
        std::thread::sleep(Duration::from_millis(100));
        let kept = tap.take();
        assert!(
            !kept.is_empty() && kept.len() % 6 == 0,
            "{} samples",
            kept.len()
        );
        for channels in [0, 9] {
            let config = AudioConfig {
                channels: Some(channels),
                ..virtual_config()
            };
            assert!(
                matches!(
                    play(&config, 128),
                    Err(AudioError::UnsupportedChannels { .. })
                ),
                "{channels}"
            );
        }
    }

    #[test]
    fn polling_the_tap_never_loses_or_splits_a_block() {
        let (playback, _controller) = play(&virtual_config(), 128).unwrap();
        let tap = playback.virtual_capture().unwrap();
        tap.start();
        let mut total = 0;
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(5));
            let chunk = tap.take();
            // 128 frames of 2 channels per block, handed over whole.
            assert_eq!(chunk.len() % 256, 0, "{} samples", chunk.len());
            total += chunk.len();
        }
        let seconds = total as f32 / 2.0 / playback.settings().sample_rate;
        assert!(seconds > 0.15, "{seconds}");
    }

    #[test]
    fn the_virtual_output_stops_with_the_playback() {
        let (playback, controller) = play(&virtual_config(), 128).unwrap();
        let transport = controller.transport();
        std::thread::sleep(Duration::from_millis(50));
        drop(playback);
        let stopped = transport.position();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(transport.position(), stopped);
    }

    #[test]
    fn the_virtual_output_publishes_when_its_blocks_run_for_midi_timing() {
        let bus = MidiBus::new();
        let (_playback, _controller) =
            play_with_outputs(&virtual_config(), &[], 128, &bus).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(bus.clock().window().is_some());
        assert!(bus.clock().anchor().is_some());
    }

    #[test]
    fn the_virtual_output_takes_the_chosen_rate_and_rejects_empty_buffers() {
        let config = AudioConfig {
            sample_rate: Some(44_100),
            buffer_size: Some(64),
            ..virtual_config()
        };
        let (playback, _) = play(&config, 128).unwrap();
        assert_eq!(playback.settings().sample_rate, 44_100.0);
        for frames in [0, 15, 8193, u32::MAX] {
            let config = AudioConfig {
                buffer_size: Some(frames),
                ..virtual_config()
            };
            assert!(
                matches!(
                    play(&config, 128),
                    Err(AudioError::UnsupportedBufferSize { .. })
                ),
                "{frames}"
            );
        }
        let silent_clock = AudioConfig {
            sample_rate: Some(0),
            ..virtual_config()
        };
        assert!(matches!(
            play(&silent_clock, 128),
            Err(AudioError::UnsupportedSampleRate(0))
        ));
    }

    #[test]
    fn an_engine_opened_stopped_holds_the_transport_until_it_is_started() {
        let bus = MidiBus::new();
        let (_playback, controller) =
            play_with_outputs_stopped(&virtual_config(), &[], 128, &bus).unwrap();
        let transport = controller.transport();
        std::thread::sleep(Duration::from_millis(150));
        assert!(!transport.is_playing());
        assert_eq!(transport.position(), 0);
        // The graph still renders: the clock shows the blocks running.
        assert!(bus.clock().window().is_some());
        transport.play();
        std::thread::sleep(Duration::from_millis(150));
        assert!(transport.position() > 0);
    }

    fn report(output_driver_ms: u64, input_driver_ms: u64) -> LatencyReport {
        LatencyReport {
            sample_rate: 48_000.0,
            block_frames: 128,
            output_buffer_frames: 256,
            output_driver: Duration::from_millis(output_driver_ms),
            input: Some(InputLatency {
                sample_rate: 48_000.0,
                buffer_frames: 256,
                driver: Duration::from_millis(input_driver_ms),
                backlog_frames: 112,
            }),
        }
    }

    #[test]
    fn the_round_trip_adds_the_input_the_queue_and_the_output() {
        // 112 + 128 frames queued is 5 ms at 48 kHz.
        let trip = report(6, 4).round_trip().unwrap();
        assert_eq!(trip, Duration::from_millis(15));
    }

    #[test]
    fn a_driver_that_reports_no_latency_falls_back_to_the_buffers() {
        // 256 frames at 48 kHz is 5.33 ms, each side.
        let trip = report(0, 0).round_trip().unwrap();
        let expected = Duration::from_secs_f32((256.0 + 256.0 + 240.0) / 48_000.0);
        assert!(
            trip.abs_diff(expected) < Duration::from_micros(10),
            "{trip:?}"
        );
    }

    #[test]
    fn there_is_no_round_trip_without_input_or_before_the_callbacks() {
        let mut off = report(6, 4);
        off.input = None;
        assert_eq!(off.round_trip(), None);
        let mut early = report(6, 4);
        early.output_buffer_frames = 0;
        assert_eq!(early.round_trip(), None);
    }

    #[test]
    fn slow_outputs_and_bluetooth_names_are_recognised() {
        assert!(report(150, 0).output_is_slow());
        assert!(!report(10, 0).output_is_slow());
        assert!(looks_like_bluetooth("Jeremy's AirPods Pro"));
        assert!(looks_like_bluetooth("Bluetooth Speaker"));
        assert!(!looks_like_bluetooth("MacBook Pro Speakers"));
    }
}
