//! Live output to an audio device, through cpal.
//!
//! The device code is kept thin: [`DeviceWriter`] does everything the audio
//! callback does and can be tested without a device, and [`play`] only finds
//! the device and wires the writer into its callback.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, I24, Sample, SampleFormat, SizedSample, StreamConfig};
use noodle_engine::{Bus, Controller, Processor, Settings, engine};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::devices::{
    AudioConfig, AudioError, Chosen, Direction, InputChoice, choose_config, device_name,
    find_device, host_for,
};
use crate::input::{Capture, Feed, recordable_input_path};
use crate::record::{RecordError, Recorder, Take};

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
        }
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
    /// gets an [`ExtraOutput`] to play on its own device. The counts must add
    /// up to the engine's channels. Anything the devices fall behind by
    /// counts in `glitches`.
    pub fn with_outputs(
        mut self,
        main_channels: usize,
        extra_channels: &[usize],
        glitches: &Arc<AtomicU64>,
    ) -> (Self, Vec<ExtraOutput>) {
        let rate = self.processor.settings().sample_rate;
        debug_assert_eq!(
            main_channels + extra_channels.iter().sum::<usize>(),
            self.processor.settings().channels
        );
        self.channels = main_channels;
        let mut first = main_channels;
        let mut outputs = Vec::with_capacity(extra_channels.len());
        for &channels in extra_channels {
            let capacity = (EXTRA_QUEUE_SECONDS * rate) as usize * channels;
            let (queue, playing) = RingBuffer::new(capacity);
            self.taps.push(Tap {
                queue,
                first,
                channels,
                overruns: Arc::clone(glitches),
            });
            outputs.push(ExtraOutput {
                queue: playing,
                channels,
                main_period: Arc::clone(&self.fade),
                filling: true,
                glitches: Arc::clone(glitches),
            });
            first += channels;
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
        for chunk in output.chunks_mut(block * channels) {
            let frames = chunk.len() / channels;
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
}

impl Tap {
    /// Queues this device's channels from interleaved `block`. If the device
    /// is behind and the queue is full, the block is dropped.
    fn push(&mut self, block: &[f32], engine_channels: usize) {
        let frames = block.len() / engine_channels;
        let Ok(chunk) = self.queue.write_chunk_uninit(frames * self.channels) else {
            self.overruns.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let (first, channels) = (self.first, self.channels);
        chunk.fill_from_iter(
            block
                .chunks(engine_channels)
                .flat_map(|frame| frame[first..first + channels].iter().copied()),
        );
    }
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
    /// How much the main stream asks for at once, which the queue must cover
    /// to keep the two apart.
    main_period: Arc<Fade>,
    /// Waiting for enough to be queued before playing.
    filling: bool,
    glitches: Arc<AtomicU64>,
}

impl ExtraOutput {
    /// Fills a device's `out`, interleaved at its channel count.
    pub fn fill<T: Sample + FromSample<f32>>(&mut self, out: &mut [T]) {
        let channels = self.channels;
        let wanted = out.len();
        let available = self.queue.slots();
        // Enough to survive until the main stream's next callback: its block,
        // and this callback's.
        let lead = (self.main_period.period.load(Ordering::Relaxed) as usize + wanted / channels)
            * channels;
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
            for (out, &x) in out.iter_mut().zip(a.iter().chain(b)) {
                *out = T::from_sample(x);
            }
            chunk.commit_all();
        }
        out[take..].fill(T::EQUILIBRIUM);
    }
}

/// Something a stream's callback fills with audio.
trait Fill: Send + 'static {
    fn fill<T: Sample + FromSample<f32>>(&mut self, output: &mut [T]);
}

impl Fill for DeviceWriter {
    fn fill<T: Sample + FromSample<f32>>(&mut self, output: &mut [T]) {
        self.write(output);
    }
}

impl Fill for ExtraOutput {
    fn fill<T: Sample + FromSample<f32>>(&mut self, output: &mut [T]) {
        ExtraOutput::fill(self, output);
    }
}

/// Audio playing on a device. Dropping it fades the sound out and stops it.
pub struct Playback {
    fade: Arc<Fade>,
    _stream: cpal::Stream,
    /// Dropped before the input stream, so a recording in progress is
    /// finished while the input is still running.
    recorder: Option<Recorder>,
    _input_stream: Option<cpal::Stream>,
    /// The streams of the extra output devices that opened.
    _extra_streams: Vec<cpal::Stream>,
    outputs: Vec<OutputStatus>,
    device: String,
    input: Option<(String, usize)>,
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
}

impl Playback {
    pub fn device(&self) -> &str {
        &self.device
    }

    /// How each extra output device fared, in the order asked for. Devices
    /// that couldn't open are left out of the engine's buses, so Output nodes
    /// tied to them report that they have nowhere to play.
    pub fn outputs(&self) -> &[OutputStatus] {
        &self.outputs
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
/// If `choice` turns input on, the input device records into Input nodes at
/// the output's sample rate. Input that can't be opened, e.g. because the
/// device doesn't support that rate, leaves Input nodes silent rather than
/// failing: see [`Playback::input_problem`]. Send the engine graphs
/// with the returned [`Controller`].
///
/// Errors while playing are reported through [`Playback::health`].
pub fn play(choice: &AudioConfig, max_frames: usize) -> Result<(Playback, Controller), AudioError> {
    play_with_outputs(choice, &[], max_frames)
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
) -> Result<(Playback, Controller), AudioError> {
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
    let Chosen { config, format } = choose_config(
        &ranges,
        device.default_output_config()?,
        choice.sample_rate,
        choice.buffer_size,
    )?;
    let mut health = Health::new();
    let main_channels = usize::from(config.channels);
    let rate = config.sample_rate;

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
                };
                extras.push(opened);
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
    let (mut controller, processor) = engine(settings)?;
    let mut buses = vec![Bus {
        device: main_id.clone().unwrap_or_default(),
        channels: main_channels,
    }];
    buses.extend(extras.iter().map(|e| Bus {
        device: e.device.clone(),
        channels: e.channels,
    }));
    controller.set_buses(buses)?;

    // Input that can't be opened doesn't stop the sound: playback goes on
    // without it, and says why.
    let mut input_problem = None;
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
            .and_then(|host| open_input(&host, id, &settings, &mut health));
            match opened {
                Ok((stream, name, channels, feed, recorder)) => (
                    DeviceWriter::with_input(processor, feed),
                    Some((stream, name, channels, recorder)),
                ),
                Err(error) => {
                    input_problem = Some(error);
                    (DeviceWriter::new(processor), None)
                }
            }
        }
    };
    let (writer, outputs) = writer.with_outputs(
        main_channels,
        &extras.iter().map(|e| e.channels).collect::<Vec<_>>(),
        &health.underruns,
    );
    let fade = Arc::clone(&writer.fade);
    let mut extra_streams = Vec::with_capacity(extras.len());
    for (opened, output) in extras.into_iter().zip(outputs) {
        let mut reporter = health.reporter(Stream::ExtraOutput);
        let on_error = move |error| reporter.report(error);
        extra_streams.push(open_format(
            &opened.device_handle,
            opened.config,
            opened.format,
            output,
            on_error,
        )?);
    }
    let mut reporter = health.reporter(Stream::Output);
    let on_error = move |error| reporter.report(error);
    let stream = open_format(&device, config, format, writer, on_error)?;
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
    for stream in &extra_streams {
        stream.play()?;
    }
    stream.play()?;
    Ok((
        Playback {
            fade,
            _stream: stream,
            recorder,
            _input_stream: input_stream,
            _extra_streams: extra_streams,
            outputs: statuses,
            device: name,
            input,
            input_problem,
            settings,
            health,
        },
        controller,
    ))
}

/// An extra output device, ready to open a stream on.
struct OpenedExtra {
    device: String,
    name: String,
    channels: usize,
    device_handle: cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
}

/// Finds the extra output device `id` and picks its configuration at `rate`.
fn open_extra(id: &str, rate: u32) -> Result<OpenedExtra, AudioError> {
    let host = open_host(host_for(None, Some(id), Direction::Output)?)?;
    let device = find_device(&host, Some(id), Direction::Output)?;
    let ranges: Vec<_> = device.supported_output_configs()?.collect();
    // The queue decouples the callbacks, so the device uses whatever buffer
    // size suits it.
    let Chosen { config, format } =
        choose_config(&ranges, device.default_output_config()?, Some(rate), None)?;
    Ok(OpenedExtra {
        device: id.to_owned(),
        name: device_name(&device),
        channels: usize::from(config.channels),
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

/// Opens (but doesn't start) an input stream at the engine's sample rate,
/// returning it, the device's name, its channel count and the feed for the
/// output callback.
fn open_input(
    host: &cpal::Host,
    id: Option<&str>,
    settings: &Settings,
    health: &mut Health,
) -> Result<(cpal::Stream, String, usize, Feed, Recorder), AudioError> {
    let device = find_device(host, id, Direction::Input)?;
    let name = device_name(&device);
    let ranges: Vec<_> = device.supported_input_configs()?.collect();
    let Chosen { config, format } = choose_config(
        &ranges,
        device.default_input_config()?,
        Some(settings.sample_rate as u32),
        // The ring decouples the two callbacks, so the input can use
        // whatever buffer size suits it.
        None,
    )?;
    let channels = usize::from(config.channels);
    let (capture, feed, recorder) = recordable_input_path(
        channels,
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
    }?;
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
        move |input: &[T], _: &cpal::InputCallbackInfo| capture.capture(input),
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
        move |output: &mut [T], _: &cpal::OutputCallbackInfo| filler.fill(output),
        on_error,
        None,
    )
}

#[cfg(test)]
mod tests {
    use crate::input::input_path;
    use noodle_core::{Command, Connection, Endpoint, Node, Project};
    use noodle_engine::{INPUT_ID, OUTPUT_ID, Registry};

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
        let (capture, feed) = input_path(2, SETTINGS.sample_rate, SETTINGS.max_frames, glitches);
        (DeviceWriter::with_input(processor, feed), capture)
    }

    #[test]
    fn input_plays_through_the_graph() {
        let (mut writer, mut capture) = passthrough();
        // Longer than a block, and not a multiple of one.
        let input: Vec<f32> = (0..150 * 2).map(|x| x as f32 / 1000.0).collect();
        capture.capture(&input);
        let mut output = vec![0.0f32; 150 * 2];
        writer.write(&mut output);
        assert_eq!(output, input);
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
    fn a_full_queue_drops_blocks_and_counts_them() {
        let (mut writer, extra) = two_devices();
        // Half a second (24000 frames) fits; each block queues 64.
        for _ in 0..400 {
            writer.write(&mut vec![0.0f32; 64 * 2]);
        }
        assert!(extra.glitches.load(Ordering::Relaxed) > 0);
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
}
