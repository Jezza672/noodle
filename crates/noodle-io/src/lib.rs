//! Audio and MIDI device I/O, audio file decoding and disk streaming. Drives
//! the engine from device callbacks; the engine itself never touches devices.

mod cache;
mod decode;
mod devices;
mod export;
mod hash;
mod input;
mod midi;
mod output;
mod peaks;
mod record;
mod resample;
mod stream;
mod wav;

pub use cache::{CacheInfo, CacheStore, CacheWriter, CachedAudio};
pub use decode::{DecodeError, Decoder, FileInfo, decode_file};
pub use devices::{
    AudioConfig, AudioError, COMMON_SAMPLE_RATES, Capabilities, Chosen, DeviceInfo, DeviceList,
    Direction, HostInfo, InputChoice, capabilities, choose_config, devices, hosts,
};
pub use export::{ExportError, ExportFormat, ExportWriter};
pub use hash::{FileHasher, Peek};
pub use input::{Capture, Feed, input_path, recordable_input_path};
pub use midi::{
    MidiBus, MidiConnection, MidiError, MidiLister, MidiMessage, MidiReceiver, connect_midi,
    midi_inputs, parse_message, same_port,
};
pub use output::{
    DeviceError, DeviceErrorKind, DeviceWriter, ExtraOutput, Health, OpenedOutput, OutputStatus,
    Playback, Stream, is_fatal, play, play_with_outputs,
};
pub use peaks::{BLOCK_FRAMES, Peak, Peaks, PeaksBuilder};
pub use record::{RecordError, RecordTap, Recorder, Take, record_path};
pub use resample::{ResampleError, resample};
pub use stream::{ClipStream, StreamSpec, StreamWorker, clip_frames, open_stream};
pub use wav::{Audio, WavError, WavStreamWriter, read_wav, write_wav};
