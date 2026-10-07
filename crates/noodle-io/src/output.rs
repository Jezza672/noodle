//! Live output to an audio device, through cpal.
//!
//! The device code is kept thin: [`DeviceWriter`] does everything the audio
//! callback does and can be tested without a device, and [`play`] only finds
//! the device and wires the writer into its callback.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, I24, Sample, SampleFormat, SizedSample, StreamConfig};
use noodle_engine::{Controller, Processor, Settings, SettingsError, engine};
use rtrb::{Consumer, Producer, RingBuffer};

pub use cpal::{Error as DeviceError, ErrorKind as DeviceErrorKind};

/// Runs the [`Processor`] for an audio callback, converting its output to the
/// device's sample format. Real-time safe: its buffer is allocated up front.
pub struct DeviceWriter {
    processor: Processor,
    /// One block of interleaved samples.
    scratch: Box<[f32]>,
}

impl DeviceWriter {
    pub fn new(processor: Processor) -> Self {
        let Settings {
            max_frames,
            channels,
            ..
        } = processor.settings();
        Self {
            processor,
            scratch: vec![0.0; max_frames * channels].into_boxed_slice(),
        }
    }

    /// Fills `output`, interleaved with the engine's channel count. Samples
    /// are clamped to between -1 and 1, and anything that isn't finite
    /// becomes silence, so a misbehaving graph can't blast the speakers.
    pub fn write<T: Sample + FromSample<f32>>(&mut self, output: &mut [T]) {
        for chunk in output.chunks_mut(self.scratch.len()) {
            let scratch = &mut self.scratch[..chunk.len()];
            self.processor.process(scratch);
            for (out, &x) in chunk.iter_mut().zip(scratch.iter()) {
                let x = if x.is_finite() {
                    x.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                *out = T::from_sample(x);
            }
        }
    }
}

/// Audio playing on a device. Dropping it stops the sound.
pub struct Playback {
    _stream: cpal::Stream,
    device: String,
    settings: Settings,
    health: Health,
}

impl Playback {
    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// The device's health since playback started. Check it regularly, e.g.
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

/// What the device has reported. Some backends (ALSA among them) report
/// errors from the audio thread itself, so the reports arrive lock-free:
/// underruns are counted, and other errors are queued.
pub struct Health {
    underruns: Arc<AtomicU64>,
    errors: Consumer<DeviceError>,
}

impl Health {
    /// Underruns and overruns so far. The device's buffer ran dry, so there
    /// was a gap in the sound.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Errors reported since the last call, other than underruns.
    pub fn errors(&mut self) -> impl Iterator<Item = DeviceError> + '_ {
        std::iter::from_fn(|| self.errors.pop().ok())
    }
}

/// The device's side of [`Health`]. Real-time safe.
struct Reporter {
    underruns: Arc<AtomicU64>,
    errors: Producer<DeviceError>,
}

/// More errors than this between checks are dropped. They're likely repeats.
const ERROR_QUEUE: usize = 16;

fn health() -> (Reporter, Health) {
    let underruns = Arc::new(AtomicU64::new(0));
    let (producer, consumer) = RingBuffer::new(ERROR_QUEUE);
    let reporter = Reporter {
        underruns: Arc::clone(&underruns),
        errors: producer,
    };
    let health = Health {
        underruns,
        errors: consumer,
    };
    (reporter, health)
}

impl Reporter {
    fn report(&mut self, error: DeviceError) {
        if error.kind() == DeviceErrorKind::Xrun {
            self.underruns.fetch_add(1, Ordering::Relaxed);
        } else {
            let _ = self.errors.push(error);
        }
    }
}

#[derive(Debug)]
pub enum OutputError {
    NoDevice,
    Device(DeviceError),
    Settings(SettingsError),
    UnsupportedFormat(SampleFormat),
}

impl From<DeviceError> for OutputError {
    fn from(error: DeviceError) -> Self {
        Self::Device(error)
    }
}

impl From<SettingsError> for OutputError {
    fn from(error: SettingsError) -> Self {
        Self::Settings(error)
    }
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => f.write_str("no audio output device"),
            Self::Device(error) => write!(f, "audio device error: {error}"),
            Self::Settings(error) => write!(f, "unusable device settings: {error}"),
            Self::UnsupportedFormat(format) => {
                write!(f, "the device's sample format ({format}) isn't supported")
            }
        }
    }
}

impl std::error::Error for OutputError {}

/// Starts an engine playing on the default output device, at the device's
/// default sample rate and channel count. Send it graphs with the returned
/// [`Controller`].
///
/// Errors while playing are reported through [`Playback::health`].
pub fn play(max_frames: usize) -> Result<(Playback, Controller), OutputError> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or(OutputError::NoDevice)?;
    let name = match device.description() {
        Ok(description) => description.name().to_owned(),
        Err(_) => "the default device".to_owned(),
    };
    let supported = device.default_output_config()?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let settings = Settings {
        sample_rate: config.sample_rate as f32,
        max_frames,
        channels: usize::from(config.channels),
    };
    let (controller, processor) = engine(settings)?;
    let writer = DeviceWriter::new(processor);
    let (mut reporter, health) = health();
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
        format => return Err(OutputError::UnsupportedFormat(format)),
    }?;
    stream.play()?;
    Ok((
        Playback {
            _stream: stream,
            device: name,
            settings,
            health,
        },
        controller,
    ))
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
    use noodle_core::{Command, Node, Project};
    use noodle_engine::{OUTPUT_ID, Registry};

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

    #[test]
    fn health_counts_underruns_and_queues_other_errors() {
        let (mut reporter, mut health) = health();
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
        let (mut reporter, mut health) = health();
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
