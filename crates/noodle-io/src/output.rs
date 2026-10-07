//! Live output to an audio device, through cpal.
//!
//! The device code is kept thin: [`DeviceWriter`] does everything the audio
//! callback does and can be tested without a device, and [`play`] only finds
//! the device and wires the writer into its callback.

use std::fmt;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, I24, Sample, SampleFormat, SizedSample, StreamConfig};
use noodle_engine::{Controller, Processor, Settings, SettingsError, engine};

pub use cpal::Error as DeviceError;

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
}

impl Playback {
    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn settings(&self) -> Settings {
        self.settings
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
/// `on_error` is called with errors the device reports while playing. It's
/// called from a device thread, but not from inside the audio callback.
pub fn play(
    max_frames: usize,
    on_error: impl FnMut(DeviceError) + Send + 'static,
) -> Result<(Playback, Controller), OutputError> {
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
