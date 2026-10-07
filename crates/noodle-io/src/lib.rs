//! Audio and MIDI device I/O, audio file decoding and disk streaming. Drives
//! the engine from device callbacks; the engine itself never touches devices.

mod devices;
mod input;
mod output;
mod record;
mod wav;

pub use devices::{
    AudioConfig, AudioError, COMMON_SAMPLE_RATES, Capabilities, Chosen, DeviceInfo, DeviceList,
    Direction, HostInfo, InputChoice, capabilities, choose_config, devices, hosts,
};
pub use input::{Capture, Feed, input_path, recordable_input_path};
pub use output::{
    DeviceError, DeviceErrorKind, DeviceWriter, Health, Playback, Stream, is_fatal, play,
};
pub use record::{RecordError, RecordTap, Recorder, Take, record_path};
pub use wav::{Audio, WavError, read_wav, write_wav};
