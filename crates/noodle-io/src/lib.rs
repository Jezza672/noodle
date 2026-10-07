//! Audio and MIDI device I/O, audio file decoding and disk streaming. Drives
//! the engine from device callbacks; the engine itself never touches devices.

mod devices;
mod output;
mod wav;

pub use devices::{
    AudioConfig, AudioError, COMMON_SAMPLE_RATES, Capabilities, Chosen, DeviceInfo, DeviceList,
    Direction, HostInfo, capabilities, choose_config, devices, hosts,
};
pub use output::{DeviceError, DeviceErrorKind, DeviceWriter, Health, Playback, is_fatal, play};
pub use wav::{Audio, WavError, read_wav, write_wav};
