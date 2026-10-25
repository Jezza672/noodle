//! Audio and MIDI device I/O, audio file decoding and disk streaming. Drives
//! the engine from device callbacks; the engine itself never touches devices.

mod cache;
mod decode;
mod devices;
mod export;
mod files;
mod hash;
mod input;
mod midi;
mod onsets;
mod output;
mod peaks;
pub mod pool;
mod record;
mod resample;
mod stream;
mod ump;
mod wav;

pub use cache::{CacheInfo, CacheStore, CacheWriter, CachedAudio};
pub use decode::{DecodeError, Decoder, FileInfo, decode_file};
pub use devices::{
    AudioConfig, AudioError, COMMON_SAMPLE_RATES, Capabilities, Chosen, DeviceInfo, DeviceList,
    Direction, HostInfo, InputChoice, LIVE_BUFFER, VIRTUAL_OUTPUT, buffer_request, capabilities,
    choose_config, devices, hosts, virtual_output,
};
pub use export::{ExportError, ExportFormat, ExportWriter};
pub use files::{AudioFiles, FileStamp};
pub use hash::{FileHasher, Peek};
pub use input::{Capture, Feed, InputMeter, input_path, recordable_input_path};
pub use midi::{
    AudioClock, MidiBus, MidiConnection, MidiError, MidiLister, MidiMessage, MidiReceiver,
    TimedMessage, TimedPacket, connect_midi, midi_inputs, now_micros, parse_message, same_port,
};
pub use onsets::{
    ANALYSIS_SECONDS, Analysis, CLEAR_PROMINENCE, CLEAR_TEMPO, PROMINENT_ONSETS, TempoEstimate,
    detect_bass_onsets, detect_onsets, detect_treble_onsets, estimate_grid, estimate_tempo,
};
pub use output::{
    DeviceError, DeviceErrorKind, DeviceWriter, ExtraOutput, Health, InputLatency, LatencyReport,
    OpenedOutput, OutputStatus, Playback, SLOW_OUTPUT, Stream, VirtualCapture, is_fatal,
    looks_like_bluetooth, play, play_with_outputs, play_with_outputs_stopped,
};
pub use peaks::{BLOCK_FRAMES, Peak, Peaks, PeaksBuilder};
pub use record::{RecordError, RecordTap, Recorder, Take, record_path};
pub use resample::{ResampleError, StreamResampler, resample};
pub use stream::{ClipStream, StreamSpec, StreamWorker, clip_frames, open_stream};
pub use ump::{Attribute, Midi2, Packet, PacketReader, packet_words, widen};
pub use wav::{Audio, WavError, WavStreamWriter, read_wav, write_wav};
