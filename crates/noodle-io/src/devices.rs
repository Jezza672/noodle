//! Finding audio devices and choosing how to open them: what the device and
//! settings picker shows, and what it hands to [`play`](crate::play).
//!
//! Listing talks to the system and can take a while (ALSA probes every
//! device), so call it when the picker opens, not every frame. Choosing a
//! stream configuration is kept separate from cpal's devices, so it can be
//! tested without one.

use std::fmt;
use std::str::FromStr;

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{
    BufferSize, DeviceId, HostId, SampleFormat, SupportedBufferSize, SupportedStreamConfig,
    SupportedStreamConfigRange,
};

use crate::DeviceError;

/// The devices and settings to play with. The default is the system's
/// default output at its own sample rate and buffer size.
///
/// Hosts and devices are stored by their stable IDs, so a saved choice
/// finds the same device after a restart, and still works if its display
/// name changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioConfig {
    /// The audio API, by [`HostInfo::id`] (e.g. `alsa`, `jack`, `wasapi`,
    /// `asio`). `None` uses the platform's default.
    pub host: Option<String>,
    /// The output device, by [`DeviceInfo::id`]. `None` uses the host's
    /// default.
    pub output: Option<String>,
    /// `None` uses the output device's default rate.
    pub sample_rate: Option<u32>,
    /// Frames per device callback. `None` lets the device decide.
    pub buffer_size: Option<u32>,
}

/// An audio API the platform offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostInfo {
    /// Stable, for [`AudioConfig::host`].
    pub id: String,
    /// For showing to the user.
    pub name: &'static str,
    pub is_default: bool,
}

/// The audio APIs available right now. Hosts that are compiled in but not
/// running, such as JACK without its server, are left out.
pub fn hosts() -> Vec<HostInfo> {
    let default = cpal::default_host().id();
    cpal::available_hosts()
        .into_iter()
        .map(|id| HostInfo {
            id: id.to_string(),
            name: id.name(),
            is_default: id == default,
        })
        .collect()
}

/// One direction of a device: what it can play or record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable, for [`AudioConfig::output`].
    pub id: String,
    /// For showing to the user.
    pub name: String,
    pub is_default: bool,
    pub capabilities: Capabilities,
}

/// What a device supports in one direction, summarised for a picker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// The most channels any of its configurations has.
    pub max_channels: u16,
    /// The rate the device prefers, if it says.
    pub default_sample_rate: Option<u32>,
    /// Common rates the device supports, ascending, plus its default if
    /// that's unusual. Devices that support a continuous range would
    /// otherwise list thousands.
    pub sample_rates: Vec<u32>,
    /// The smallest and largest buffer sizes, in frames, if the device says.
    pub buffer_sizes: Option<(u32, u32)>,
}

/// The devices on one host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceList {
    pub outputs: Vec<DeviceInfo>,
    pub inputs: Vec<DeviceInfo>,
}

/// Lists the devices on a host (`None` for the default host). A device
/// that can't describe itself, e.g. because it was just unplugged, is left
/// out rather than failing the whole list.
pub fn devices(host: Option<&str>) -> Result<DeviceList, AudioError> {
    let host = open_host(host)?;
    let default_output = host.default_output_device().and_then(|d| d.id().ok());
    let default_input = host.default_input_device().and_then(|d| d.id().ok());
    let mut list = DeviceList::default();
    for device in host.devices()? {
        let Ok(id) = device.id() else { continue };
        let name = device_name(&device);
        if device.supports_output()
            && let Ok(ranges) = device.supported_output_configs()
        {
            let ranges: Vec<_> = ranges.collect();
            let default = device.default_output_config().ok();
            list.outputs.push(DeviceInfo {
                id: id.to_string(),
                name: name.clone(),
                is_default: Some(&id) == default_output.as_ref(),
                capabilities: capabilities(&ranges, default.as_ref()),
            });
        }
        if device.supports_input()
            && let Ok(ranges) = device.supported_input_configs()
        {
            let ranges: Vec<_> = ranges.collect();
            let default = device.default_input_config().ok();
            list.inputs.push(DeviceInfo {
                id: id.to_string(),
                name,
                is_default: Some(&id) == default_input.as_ref(),
                capabilities: capabilities(&ranges, default.as_ref()),
            });
        }
    }
    Ok(list)
}

/// Rates a picker offers, where the device supports them.
pub const COMMON_SAMPLE_RATES: [u32; 8] = [
    22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
];

/// Summarises a device's configurations for a picker.
pub fn capabilities(
    ranges: &[SupportedStreamConfigRange],
    default: Option<&SupportedStreamConfig>,
) -> Capabilities {
    let supports = |rate: u32| ranges.iter().any(|r| r.contains_rate(rate));
    let mut sample_rates: Vec<u32> = COMMON_SAMPLE_RATES
        .into_iter()
        .filter(|&rate| supports(rate))
        .collect();
    let default_sample_rate = default.map(|d| d.sample_rate());
    if let Some(rate) = default_sample_rate
        && !sample_rates.contains(&rate)
    {
        sample_rates.push(rate);
        sample_rates.sort_unstable();
    }
    let buffer_sizes = ranges
        .iter()
        .filter_map(|r| match *r.buffer_size() {
            SupportedBufferSize::Range { min, max } => Some((min, max)),
            SupportedBufferSize::Unknown => None,
        })
        .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)));
    Capabilities {
        max_channels: ranges.iter().map(|r| r.channels()).max().unwrap_or(0),
        default_sample_rate,
        sample_rates,
        buffer_sizes,
    }
}

/// Picks the configuration to open a stream with: the device's default, at
/// `sample_rate` if one is given, with `buffer_size` frames per callback if
/// one is given.
///
/// When the rate differs from the default's, it prefers a configuration
/// that keeps the default's channel count and sample format, then one in
/// 32-bit float, then the one with the most channels.
pub fn choose_config(
    ranges: &[SupportedStreamConfigRange],
    default: SupportedStreamConfig,
    sample_rate: Option<u32>,
    buffer_size: Option<u32>,
) -> Result<Chosen, AudioError> {
    let supported = match sample_rate {
        None => default,
        Some(rate) if rate == default.sample_rate() => default,
        Some(rate) => ranges
            .iter()
            .filter(|r| r.contains_rate(rate))
            .max_by_key(|r| {
                (
                    r.channels() == default.channels(),
                    r.sample_format() == default.sample_format(),
                    r.sample_format() == SampleFormat::F32,
                    r.channels(),
                )
            })
            .ok_or(AudioError::UnsupportedSampleRate(rate))?
            .with_sample_rate(rate),
    };
    let buffer_size = match buffer_size {
        None => BufferSize::Default,
        Some(frames) => {
            if let SupportedBufferSize::Range { min, max } = *supported.buffer_size()
                && !(min..=max).contains(&frames)
            {
                return Err(AudioError::UnsupportedBufferSize { frames, min, max });
            }
            if frames == 0 {
                return Err(AudioError::UnsupportedBufferSize {
                    frames,
                    min: 1,
                    max: u32::MAX,
                });
            }
            BufferSize::Fixed(frames)
        }
    };
    let mut config: cpal::StreamConfig = supported.into();
    config.buffer_size = buffer_size;
    Ok(Chosen {
        config,
        format: supported.sample_format(),
    })
}

/// A stream configuration and the sample format to open it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chosen {
    pub config: cpal::StreamConfig,
    pub format: SampleFormat,
}

pub(crate) fn open_host(id: Option<&str>) -> Result<cpal::Host, AudioError> {
    match id {
        None => Ok(cpal::default_host()),
        Some(id) => {
            let host = HostId::from_str(id).map_err(|_| AudioError::NoHost(id.to_owned()))?;
            Ok(cpal::host_from_id(host)?)
        }
    }
}

/// Finds a device by ID, or the default one, for output or input.
pub(crate) fn find_device(
    host: &cpal::Host,
    id: Option<&str>,
    direction: Direction,
) -> Result<cpal::Device, AudioError> {
    let found = match id {
        None => match direction {
            Direction::Output => host.default_output_device(),
            Direction::Input => host.default_input_device(),
        },
        Some(id) => DeviceId::from_str(id)
            .ok()
            .and_then(|id| host.device_by_id(&id)),
    };
    found.ok_or_else(|| AudioError::NoDevice {
        direction,
        id: id.map(str::to_owned),
    })
}

pub(crate) fn device_name(device: &cpal::Device) -> String {
    match device.description() {
        Ok(description) => description.name().to_owned(),
        Err(_) => "the default device".to_owned(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Output,
    Input,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Output => "output",
            Self::Input => "input",
        })
    }
}

/// Why audio couldn't start, or devices couldn't be listed.
#[derive(Debug)]
pub enum AudioError {
    /// No host with this ID is available here.
    NoHost(String),
    /// The device isn't there: unplugged, or (with no ID) there's no
    /// default.
    NoDevice {
        direction: Direction,
        id: Option<String>,
    },
    Device(DeviceError),
    Settings(noodle_engine::SettingsError),
    UnsupportedFormat(SampleFormat),
    UnsupportedSampleRate(u32),
    UnsupportedBufferSize {
        frames: u32,
        min: u32,
        max: u32,
    },
}

impl From<DeviceError> for AudioError {
    fn from(error: DeviceError) -> Self {
        Self::Device(error)
    }
}

impl From<noodle_engine::SettingsError> for AudioError {
    fn from(error: noodle_engine::SettingsError) -> Self {
        Self::Settings(error)
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHost(id) => write!(f, "no audio host called {id:?} is available"),
            Self::NoDevice {
                direction,
                id: None,
            } => write!(f, "no audio {direction} device"),
            Self::NoDevice {
                direction,
                id: Some(id),
            } => write!(f, "audio {direction} device {id:?} isn't available"),
            Self::Device(error) => write!(f, "audio device error: {error}"),
            Self::Settings(error) => write!(f, "unusable device settings: {error}"),
            Self::UnsupportedFormat(format) => {
                write!(f, "the device's sample format ({format}) isn't supported")
            }
            Self::UnsupportedSampleRate(rate) => {
                write!(f, "the device doesn't support a sample rate of {rate} Hz")
            }
            Self::UnsupportedBufferSize { frames, min, max } => write!(
                f,
                "a buffer of {frames} frames isn't supported (the device takes {min} to {max})"
            ),
        }
    }
}

impl std::error::Error for AudioError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(
        channels: u16,
        rates: (u32, u32),
        buffers: Option<(u32, u32)>,
        format: SampleFormat,
    ) -> SupportedStreamConfigRange {
        let buffer_size = match buffers {
            Some((min, max)) => SupportedBufferSize::Range { min, max },
            None => SupportedBufferSize::Unknown,
        };
        SupportedStreamConfigRange::new(channels, rates.0, rates.1, buffer_size, format)
    }

    /// A stereo i16 device at 44.1 kHz by default, which also has a
    /// four-channel f32 mode and a stereo f32 mode up to 96 kHz.
    fn device() -> (Vec<SupportedStreamConfigRange>, SupportedStreamConfig) {
        let ranges = vec![
            range(2, (44_100, 48_000), Some((64, 4096)), SampleFormat::I16),
            range(4, (44_100, 192_000), Some((32, 8192)), SampleFormat::F32),
            range(2, (8_000, 96_000), None, SampleFormat::F32),
        ];
        let default = ranges[0].with_sample_rate(44_100);
        (ranges, default)
    }

    #[test]
    fn capabilities_summarise_every_range() {
        let (ranges, default) = device();
        let caps = capabilities(&ranges, Some(&default));
        assert_eq!(caps.max_channels, 4);
        assert_eq!(caps.default_sample_rate, Some(44_100));
        assert_eq!(
            caps.sample_rates,
            [
                22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000
            ]
        );
        assert_eq!(caps.buffer_sizes, Some((32, 8192)));
    }

    #[test]
    fn capabilities_list_an_unusual_default_rate() {
        let ranges = [range(1, (16_000, 16_000), None, SampleFormat::I16)];
        let default = ranges[0].with_sample_rate(16_000);
        let caps = capabilities(&ranges, Some(&default));
        assert_eq!(caps.sample_rates, [16_000]);
        assert_eq!(caps.buffer_sizes, None);
    }

    #[test]
    fn with_nothing_chosen_the_default_is_used() {
        let (ranges, default) = device();
        let chosen = choose_config(&ranges, default, None, None).unwrap();
        assert_eq!(chosen.config.sample_rate, 44_100);
        assert_eq!(chosen.config.channels, 2);
        assert_eq!(chosen.config.buffer_size, BufferSize::Default);
        assert_eq!(chosen.format, SampleFormat::I16);
    }

    #[test]
    fn a_new_rate_keeps_the_default_channels_and_format_if_it_can() {
        let (ranges, default) = device();
        let chosen = choose_config(&ranges, default, Some(48_000), None).unwrap();
        assert_eq!(
            (chosen.config.sample_rate, chosen.config.channels),
            (48_000, 2)
        );
        assert_eq!(chosen.format, SampleFormat::I16);
    }

    #[test]
    fn a_rate_the_default_format_lacks_prefers_matching_channels() {
        let (ranges, default) = device();
        // Both f32 ranges have 96 kHz; only one is stereo.
        let chosen = choose_config(&ranges, default, Some(96_000), None).unwrap();
        assert_eq!(
            (chosen.config.sample_rate, chosen.config.channels),
            (96_000, 2)
        );
        assert_eq!(chosen.format, SampleFormat::F32);
        // Only the four-channel range has 192 kHz.
        let chosen = choose_config(&ranges, default, Some(192_000), None).unwrap();
        assert_eq!(chosen.config.channels, 4);
    }

    #[test]
    fn an_unsupported_rate_is_an_error() {
        let (ranges, default) = device();
        let error = choose_config(&ranges, default, Some(384_000), None).unwrap_err();
        assert!(
            matches!(error, AudioError::UnsupportedSampleRate(384_000)),
            "{error}"
        );
    }

    #[test]
    fn a_buffer_size_is_checked_against_the_chosen_range() {
        let (ranges, default) = device();
        let chosen = choose_config(&ranges, default, None, Some(256)).unwrap();
        assert_eq!(chosen.config.buffer_size, BufferSize::Fixed(256));
        let error = choose_config(&ranges, default, None, Some(32)).unwrap_err();
        assert!(
            matches!(
                error,
                AudioError::UnsupportedBufferSize {
                    frames: 32,
                    min: 64,
                    max: 4096
                }
            ),
            "{error}"
        );
        // The f32 stereo range doesn't say, so any size is tried.
        let chosen = choose_config(&ranges, default, Some(96_000), Some(32)).unwrap();
        assert_eq!(chosen.config.buffer_size, BufferSize::Fixed(32));
    }

    #[test]
    fn a_zero_buffer_size_is_an_error() {
        let (ranges, default) = device();
        let error = choose_config(&ranges, default, Some(96_000), Some(0)).unwrap_err();
        assert!(
            matches!(error, AudioError::UnsupportedBufferSize { frames: 0, .. }),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_host_is_an_error() {
        let error = open_host(Some("not-a-host")).err().unwrap();
        assert!(matches!(error, AudioError::NoHost(_)), "{error}");
    }
}
