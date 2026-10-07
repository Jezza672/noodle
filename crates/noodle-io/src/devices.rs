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
use serde::{Deserialize, Serialize};

use crate::DeviceError;

/// The devices and settings to play with. The default is the system's
/// default output at its own sample rate and buffer size, with no input.
///
/// Hosts and devices are stored by their stable IDs, so a saved choice
/// finds the same device after a restart, and still works if its display
/// name changes.
///
/// It's saved in the app's preferences, so missing fields take their
/// defaults when it's read back, and new fields can be added.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// The audio API, by [`HostInfo::id`] (e.g. `alsa`, `jack`, `wasapi`,
    /// `asio`), for default devices. `None` uses the platform's default. A
    /// device ID names its own host: the output's must agree with this if
    /// it's given, and an input device may be on any host.
    pub host: Option<String>,
    /// The output device, by [`DeviceInfo::id`]. `None` uses the host's
    /// default.
    pub output: Option<String>,
    pub input: InputChoice,
    /// `None` uses the output device's default rate. The input always runs
    /// at the output's rate: Noodle doesn't resample between devices.
    pub sample_rate: Option<u32>,
    /// Frames per device callback. `None` lets the device decide.
    pub buffer_size: Option<u32>,
}

/// Which device, if any, records into Input nodes. Off by default, since
/// opening a microphone can ask the user for permission.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputChoice {
    #[default]
    Off,
    /// The host's default input device.
    Default,
    /// A device, by [`DeviceInfo::id`].
    Device(String),
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
    /// Stable, for [`AudioConfig::output`] or [`InputChoice::Device`].
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
        // Each query probes the device, which is slow on some hosts, so
        // they're made once and an empty answer means "can't".
        if let Some(ranges) = usable(device.supported_output_configs()) {
            let default = device.default_output_config().ok();
            list.outputs.push(DeviceInfo {
                id: id.to_string(),
                name: name.clone(),
                is_default: Some(&id) == default_output.as_ref(),
                capabilities: capabilities(&ranges, default.as_ref()),
            });
        }
        if let Some(ranges) = usable(device.supported_input_configs()) {
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

/// A device's configurations in formats Noodle can play, if it has any.
fn usable(
    ranges: Result<impl Iterator<Item = SupportedStreamConfigRange>, DeviceError>,
) -> Option<Vec<SupportedStreamConfigRange>> {
    let ranges: Vec<_> = ranges
        .ok()?
        .filter(|r| is_supported(r.sample_format()))
        .collect();
    (!ranges.is_empty()).then_some(ranges)
}

/// The sample formats the device streams convert to and from.
pub const SUPPORTED_FORMATS: [SampleFormat; 9] = [
    SampleFormat::F32,
    SampleFormat::F64,
    SampleFormat::I8,
    SampleFormat::I16,
    SampleFormat::I24,
    SampleFormat::I32,
    SampleFormat::U8,
    SampleFormat::U16,
    SampleFormat::U32,
];

fn is_supported(format: SampleFormat) -> bool {
    SUPPORTED_FORMATS.contains(&format)
}

/// Rates a picker offers, where the device supports them.
pub const COMMON_SAMPLE_RATES: [u32; 8] = [
    22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
];

/// Summarises a device's configurations for a picker. Configurations in
/// formats Noodle can't play are left out.
pub fn capabilities(
    ranges: &[SupportedStreamConfigRange],
    default: Option<&SupportedStreamConfig>,
) -> Capabilities {
    let ranges: Vec<_> = ranges
        .iter()
        .filter(|r| is_supported(r.sample_format()))
        .collect();
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
    Capabilities {
        max_channels: ranges.iter().map(|r| r.channels()).max().unwrap_or(0),
        default_sample_rate,
        sample_rates,
        buffer_sizes: buffer_span(ranges.iter().copied()),
    }
}

/// The smallest and largest buffer sizes across `ranges`, where known.
fn buffer_span<'a>(
    ranges: impl Iterator<Item = &'a SupportedStreamConfigRange>,
) -> Option<(u32, u32)> {
    ranges
        .filter_map(|r| match *r.buffer_size() {
            SupportedBufferSize::Range { min, max } => Some((min, max)),
            SupportedBufferSize::Unknown => None,
        })
        .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
}

/// Picks the configuration to open a stream with: the device's default, at
/// `sample_rate` if one is given, with `buffer_size` frames per callback if
/// one is given.
///
/// When the default won't do, it picks among the configurations that have
/// the rate, a format Noodle can play, and room for the buffer size (a
/// device that doesn't say which sizes it takes is given the benefit of the
/// doubt). It prefers one that keeps the default's channel count, then its
/// sample format, then 32-bit float, then one that says it takes the
/// buffer size, then the most channels.
pub fn choose_config(
    ranges: &[SupportedStreamConfigRange],
    default: SupportedStreamConfig,
    sample_rate: Option<u32>,
    buffer_size: Option<u32>,
) -> Result<Chosen, AudioError> {
    if buffer_size == Some(0) {
        return Err(AudioError::UnsupportedBufferSize {
            frames: 0,
            min: 1,
            max: u32::MAX,
        });
    }
    // Some for a buffer range that says it takes the size, None for one
    // that doesn't say, false for one that says it doesn't.
    let takes_buffer = |size: &SupportedBufferSize| match (buffer_size, *size) {
        (Some(frames), SupportedBufferSize::Range { min, max }) => {
            Some((min..=max).contains(&frames))
        }
        _ => None,
    };
    let rate = sample_rate.unwrap_or(default.sample_rate());
    let supported = if rate == default.sample_rate()
        && is_supported(default.sample_format())
        && takes_buffer(default.buffer_size()) != Some(false)
    {
        default
    } else {
        let at_rate: Vec<_> = ranges
            .iter()
            .filter(|r| is_supported(r.sample_format()) && r.contains_rate(rate))
            .collect();
        if at_rate.is_empty() {
            return Err(match sample_rate {
                Some(rate) => AudioError::UnsupportedSampleRate(rate),
                None => AudioError::UnsupportedFormat(default.sample_format()),
            });
        }
        let best = at_rate
            .iter()
            .filter(|r| takes_buffer(r.buffer_size()) != Some(false))
            .max_by_key(|r| {
                (
                    r.channels() == default.channels(),
                    r.sample_format() == default.sample_format(),
                    r.sample_format() == SampleFormat::F32,
                    takes_buffer(r.buffer_size()) == Some(true),
                    r.channels(),
                )
            });
        match (best, buffer_size) {
            (Some(range), _) => range.with_sample_rate(rate),
            // Every range at this rate said no, so they all have a span.
            (None, Some(frames)) => {
                let (min, max) = buffer_span(at_rate.into_iter()).unwrap_or((1, u32::MAX));
                return Err(AudioError::UnsupportedBufferSize { frames, min, max });
            }
            (None, None) => unreachable!("with no buffer size, every range takes it"),
        }
    };
    let mut config: cpal::StreamConfig = supported.into();
    config.buffer_size = match buffer_size {
        None => BufferSize::Default,
        Some(frames) => BufferSize::Fixed(frames),
    };
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
        Some(id) => Ok(cpal::host_from_id(parse_host(id)?)?),
    }
}

fn parse_host(id: &str) -> Result<HostId, AudioError> {
    HostId::from_str(id).map_err(|_| AudioError::NoHost(id.to_owned()))
}

/// The host to use: the one a device ID names, if a device is chosen,
/// otherwise `host` (`None` for the platform's default). A device ID names
/// its own host, so `host` only has to agree with it.
pub(crate) fn host_for(
    host: Option<&str>,
    device: Option<&str>,
    direction: Direction,
) -> Result<Option<HostId>, AudioError> {
    let Some(device) = device else {
        return host.map(parse_host).transpose();
    };
    let no_device = || AudioError::NoDevice {
        direction,
        id: Some(device.to_owned()),
    };
    // Checked before parsing, so a mismatch is reported as one even when
    // the device's host isn't available here.
    let (named, _) = device.split_once(':').ok_or_else(no_device)?;
    if let Some(host) = host
        && !host.eq_ignore_ascii_case(named)
    {
        return Err(AudioError::HostMismatch {
            host: host.to_owned(),
            device: device.to_owned(),
        });
    }
    let id = DeviceId::from_str(device).map_err(|_| no_device())?;
    Ok(Some(id.host()))
}

/// Finds a device by ID, or the host's default one, for output or input.
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
        Err(_) => "an unnamed device".to_owned(),
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
    /// A host was chosen, and a device on a different one.
    HostMismatch {
        host: String,
        device: String,
    },
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
            Self::HostMismatch { host, device } => {
                write!(f, "device {device:?} isn't on the {host:?} host")
            }
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
    fn a_buffer_size_the_default_lacks_picks_another_range() {
        let (ranges, default) = device();
        let chosen = choose_config(&ranges, default, None, Some(256)).unwrap();
        assert_eq!(chosen.config.buffer_size, BufferSize::Fixed(256));
        assert_eq!(chosen.format, SampleFormat::I16, "the default takes 256");
        // The default takes 64 to 4096. The stereo f32 range doesn't say,
        // so it's tried rather than the four-channel one that does.
        let chosen = choose_config(&ranges, default, None, Some(32)).unwrap();
        assert_eq!(chosen.config.buffer_size, BufferSize::Fixed(32));
        assert_eq!(
            (chosen.config.channels, chosen.format),
            (2, SampleFormat::F32)
        );
        assert_eq!(chosen.config.sample_rate, 44_100);
    }

    #[test]
    fn a_range_that_says_it_takes_the_buffer_size_beats_one_that_does_not_say() {
        // Neither matches the default's channels, and both are f32, so
        // without the buffer size the four-channel one would win.
        let ranges = [
            range(4, (48_000, 48_000), None, SampleFormat::F32),
            range(2, (48_000, 48_000), Some((16, 64)), SampleFormat::F32),
        ];
        let default = range(1, (44_100, 44_100), None, SampleFormat::F32).with_sample_rate(44_100);
        let chosen = choose_config(&ranges, default, Some(48_000), Some(32)).unwrap();
        assert_eq!(chosen.config.channels, 2);
        let chosen = choose_config(&ranges, default, Some(48_000), None).unwrap();
        assert_eq!(chosen.config.channels, 4);
    }

    #[test]
    fn a_buffer_size_no_range_takes_is_an_error() {
        let (ranges, default) = device();
        // Only the four-channel range has 192 kHz, and it takes 32 to 8192.
        let error = choose_config(&ranges, default, Some(192_000), Some(16)).unwrap_err();
        assert!(
            matches!(
                error,
                AudioError::UnsupportedBufferSize {
                    frames: 16,
                    min: 32,
                    max: 8192
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn formats_noodle_cannot_play_are_passed_over() {
        let ranges = [
            range(2, (44_100, 48_000), None, SampleFormat::U64),
            range(2, (44_100, 48_000), None, SampleFormat::I16),
        ];
        let default = ranges[0].with_sample_rate(48_000);
        let chosen = choose_config(&ranges, default, None, None).unwrap();
        assert_eq!(chosen.format, SampleFormat::I16);
        assert_eq!(chosen.config.sample_rate, 48_000);

        let caps = capabilities(&ranges[..1], Some(&default));
        assert_eq!(caps.max_channels, 0);
        let error = choose_config(&ranges[..1], default, None, None).unwrap_err();
        assert!(
            matches!(error, AudioError::UnsupportedFormat(SampleFormat::U64)),
            "{error}"
        );
    }

    #[test]
    fn a_device_id_names_its_own_host() {
        let host = cpal::default_host().id().to_string();
        let device = format!("{host}:some-device");
        assert_eq!(
            host_for(None, Some(&device), Direction::Output).unwrap(),
            Some(cpal::default_host().id())
        );
        assert_eq!(
            host_for(Some(&host), Some(&device), Direction::Output).unwrap(),
            Some(cpal::default_host().id())
        );
        assert_eq!(host_for(None, None, Direction::Output).unwrap(), None);
    }

    #[test]
    fn a_host_must_agree_with_the_device_id() {
        let error = host_for(Some("alsa"), Some("jack:system"), Direction::Output).unwrap_err();
        assert!(matches!(error, AudioError::HostMismatch { .. }), "{error}");
        let error = host_for(Some("ALSA"), Some("jack:system"), Direction::Output).unwrap_err();
        assert!(matches!(error, AudioError::HostMismatch { .. }), "{error}");
    }

    #[test]
    fn a_device_id_that_does_not_parse_is_no_device() {
        let error = host_for(None, Some("no colon"), Direction::Input).unwrap_err();
        assert!(
            matches!(
                error,
                AudioError::NoDevice {
                    direction: Direction::Input,
                    id: Some(_)
                }
            ),
            "{error}"
        );
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
