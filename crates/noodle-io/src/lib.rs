//! Audio and MIDI device I/O, audio file decoding and disk streaming. Drives
//! the engine from device callbacks; the engine itself never touches devices.

mod wav;

pub use wav::{Audio, WavError, read_wav, write_wav};
