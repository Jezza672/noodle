//! Live output to an audio device, through cpal.
//!
//! The device code is kept thin: [`DeviceWriter`] does everything the audio
//! callback does and can be tested without a device, and [`play`] only finds
//! the device and wires the writer into its callback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, I24, Sample, SampleFormat, SizedSample, StreamConfig};
use noodle_engine::{Controller, Processor, Settings, engine};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::devices::{
    AudioConfig, AudioError, Chosen, Direction, InputChoice, choose_config, device_name,
    find_device, host_for,
};
use crate::input::{Capture, Feed, input_path};

pub use cpal::{Error as DeviceError, ErrorKind as DeviceErrorKind};

/// Runs the [`Processor`] for an audio callback, converting its output to the
/// device's sample format, and feeding it device input if there is any.
/// Real-time safe: its buffers are allocated up front.
pub struct DeviceWriter {
    processor: Processor,
    /// One block of interleaved samples.
    scratch: Box<[f32]>,
    input: Option<Feed>,
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
            input: None,
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

    /// Fills `output`, interleaved with the engine's channel count. Samples
    /// are clamped to between -1 and 1, and anything that isn't finite
    /// becomes silence, so a misbehaving graph can't blast the speakers.
    pub fn write<T: Sample + FromSample<f32>>(&mut self, output: &mut [T]) {
        let channels = self.processor.settings().channels;
        for chunk in output.chunks_mut(self.scratch.len()) {
            let scratch = &mut self.scratch[..chunk.len()];
            match &mut self.input {
                Some(feed) => {
                    let channels_in = feed.channels();
                    let input = feed.read(chunk.len() / channels);
                    self.processor
                        .process_with_input(input, channels_in, scratch);
                }
                None => self.processor.process(scratch),
            }
            let stopping = self.fade.stop.load(Ordering::Relaxed);
            for (frame_out, frame) in chunk.chunks_mut(channels).zip(scratch.chunks(channels)) {
                if stopping {
                    self.gain = (self.gain - self.step).max(0.0);
                }
                for (out, &x) in frame_out.iter_mut().zip(frame) {
                    let x = if x.is_finite() {
                        x.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                    *out = T::from_sample(x * self.gain);
                }
            }
            if stopping && self.gain == 0.0 {
                self.fade.done.store(true, Ordering::Release);
            }
        }
    }
}

/// Audio playing on a device. Dropping it fades the sound out and stops it.
pub struct Playback {
    fade: Arc<Fade>,
    _stream: cpal::Stream,
    _input_stream: Option<cpal::Stream>,
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
    }
}

impl Playback {
    pub fn device(&self) -> &str {
        &self.device
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

/// What the devices have reported. Some backends (ALSA among them) report
/// errors from the audio thread itself, so the reports arrive lock-free:
/// underruns and input glitches are counted, and other errors are queued.
pub struct Health {
    underruns: Arc<AtomicU64>,
    pub(crate) input_glitches: Arc<AtomicU64>,
    /// One queue per stream.
    errors: Vec<Consumer<DeviceError>>,
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
    fn reporter(&mut self) -> Reporter {
        let (producer, consumer) = RingBuffer::new(ERROR_QUEUE);
        self.errors.push(consumer);
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

    /// Errors reported since the last call, other than underruns.
    pub fn errors(&mut self) -> impl Iterator<Item = DeviceError> + '_ {
        self.errors
            .iter_mut()
            .flat_map(|queue| std::iter::from_fn(|| queue.pop().ok()))
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
    let host_id = host_for(
        choice.host.as_deref(),
        choice.output.as_deref(),
        Direction::Output,
    )?;
    let host = open_host(host_id)?;
    let device = find_device(&host, choice.output.as_deref(), Direction::Output)?;
    let name = device_name(&device);
    let ranges: Vec<_> = device.supported_output_configs()?.collect();
    let Chosen { config, format } = choose_config(
        &ranges,
        device.default_output_config()?,
        choice.sample_rate,
        choice.buffer_size,
    )?;
    let settings = Settings {
        sample_rate: config.sample_rate as f32,
        max_frames,
        channels: usize::from(config.channels),
    };
    let (controller, processor) = engine(settings)?;
    let mut health = Health::new();

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
                Ok((stream, name, channels, feed)) => (
                    DeviceWriter::with_input(processor, feed),
                    Some((stream, name, channels)),
                ),
                Err(error) => {
                    input_problem = Some(error);
                    (DeviceWriter::new(processor), None)
                }
            }
        }
    };
    let fade = writer.fade.clone();
    let mut reporter = health.reporter();
    let on_error = move |error| reporter.report(error);

    let stream = match format {
        SampleFormat::F32 => open::<f32>(&device, config, writer, on_error),
        SampleFormat::F64 => open::<f64>(&device, config, writer, on_error),
        SampleFormat::I8 => open::<i8>(&device, config, writer, on_error),
        SampleFormat::I16 => open::<i16>(&device, config, writer, on_error),
        SampleFormat::I24 => open::<I24>(&device, config, writer, on_error),
        SampleFormat::I32 => open::<i32>(&device, config, writer, on_error),
        SampleFormat::U8 => open::<u8>(&device, config, writer, on_error),
        SampleFormat::U16 => open::<u16>(&device, config, writer, on_error),
        SampleFormat::U32 => open::<u32>(&device, config, writer, on_error),
        format => return Err(AudioError::UnsupportedFormat(format)),
    }?;
    // Input first, so the output finds some waiting.
    let (input_stream, input) = match input {
        Some((input_stream, name, channels)) => match input_stream.play() {
            Ok(()) => (Some(input_stream), Some((name, channels))),
            Err(error) => {
                input_problem = Some(error.into());
                (None, None)
            }
        },
        None => (None, None),
    };
    stream.play()?;
    Ok((
        Playback {
            fade,
            _stream: stream,
            _input_stream: input_stream,
            device: name,
            input,
            input_problem,
            settings,
            health,
        },
        controller,
    ))
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
) -> Result<(cpal::Stream, String, usize, Feed), AudioError> {
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
    let (capture, feed) = input_path(
        channels,
        settings.sample_rate,
        settings.max_frames,
        Arc::clone(&health.input_glitches),
    );
    let mut reporter = health.reporter();
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
    Ok((stream, name, channels, feed))
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
    mut writer: DeviceWriter,
    on_error: impl FnMut(DeviceError) + Send + 'static,
) -> Result<cpal::Stream, DeviceError> {
    device.build_output_stream(
        config,
        move |output: &mut [T], _: &cpal::OutputCallbackInfo| writer.write(output),
        on_error,
        None,
    )
}

#[cfg(test)]
mod tests {
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
        let mut output = health.reporter();
        let mut input = health.reporter();
        output.report(DeviceErrorKind::Xrun.into());
        input.report(DeviceErrorKind::Xrun.into());
        output.report(DeviceErrorKind::DeviceNotAvailable.into());
        input.report(DeviceErrorKind::BackendError.into());
        assert_eq!(health.underruns(), 2);
        assert_eq!(health.errors().count(), 2);
    }

    #[test]
    fn health_counts_underruns_and_queues_other_errors() {
        let mut health = Health::new();
        let mut reporter = health.reporter();
        reporter.report(DeviceErrorKind::Xrun.into());
        reporter.report(DeviceErrorKind::DeviceNotAvailable.into());
        reporter.report(DeviceErrorKind::Xrun.into());
        assert_eq!(health.underruns(), 2);
        let kinds: Vec<_> = health.errors().map(|e| e.kind()).collect();
        assert_eq!(kinds, [DeviceErrorKind::DeviceNotAvailable]);
        assert_eq!(health.errors().count(), 0, "errors are drained");
    }

    #[test]
    fn health_drops_errors_beyond_its_queue() {
        let mut health = Health::new();
        let mut reporter = health.reporter();
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
}
