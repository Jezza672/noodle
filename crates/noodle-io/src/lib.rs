//! Audio and MIDI device I/O, audio file decoding and disk streaming. Drives
//! the engine from device callbacks; the engine itself never touches devices.

mod decode;
mod devices;
mod input;
mod output;
mod peaks;
mod record;
mod resample;
mod stream;
mod wav;

pub use decode::{DecodeError, Decoder, FileInfo, decode_file};
pub use devices::{
    AudioConfig, AudioError, COMMON_SAMPLE_RATES, Capabilities, Chosen, DeviceInfo, DeviceList,
    Direction, HostInfo, InputChoice, capabilities, choose_config, devices, hosts,
};
pub use input::{Capture, Feed, input_path, recordable_input_path};
pub use output::{
    DeviceError, DeviceErrorKind, DeviceWriter, Health, Playback, Stream, is_fatal, play,
};
pub use peaks::{BLOCK_FRAMES, Peak, Peaks, PeaksBuilder};
pub use record::{RecordError, RecordTap, Recorder, Take, record_path};
pub use resample::{ResampleError, resample};
pub use stream::{ClipStream, StreamSpec, StreamWorker, clip_frames, open_stream};
pub use wav::{Audio, WavError, read_wav, write_wav};
