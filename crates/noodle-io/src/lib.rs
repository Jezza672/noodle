//! Audio and MIDI device I/O, audio file decoding and disk streaming. Drives
//! the engine from device callbacks; the engine itself never touches devices.

mod output;
mod wav;

pub use output::{
    DeviceError, DeviceErrorKind, DeviceWriter, Health, OutputError, Playback, is_fatal, play,
};
pub use wav::{Audio, WavError, read_wav, write_wav};
