//! Audio and MIDI device I/O, audio file decoding and disk streaming. Drives
//! the engine from device callbacks; the engine itself never touches devices.

mod cache;
mod decode;
mod devices;
mod export;
mod files;
mod hash;
mod input;
mod limiter;
mod loudness;
mod midi;
mod midi_out;
mod onsets;
mod output;
mod peaks;
mod pitch;
pub mod pool;
mod record;
mod resample;
mod stream;
mod ump;
mod wav;

pub use cache::{CacheInfo, CacheStore, CacheUsage, CacheWriter, CachedAudio};
pub use decode::{DecodeError, Decoder, FileInfo, decode_file};
pub use devices::{
    AudioConfig, AudioError, COMMON_SAMPLE_RATES, Capabilities, Chosen, DeviceInfo, DeviceList,
    Direction, HostInfo, InputChoice, LIVE_BUFFER, VIRTUAL_OUTPUT, buffer_request, capabilities,
    choose_config, devices, hosts, virtual_output,
};
pub use export::{
    ExportError, ExportFormat, ExportMetadata, ExportWriter, FLAC_MAX_CHANNELS, cue_sheet,
    normalize_isrc,
};
pub use files::{AudioFiles, FileStamp};
pub use hash::{FileHasher, Peek};
pub use input::{Capture, Feed, InputMeter, input_path, recordable_input_path};
pub use limiter::TruePeakLimiter;
pub use loudness::{Loudness, LoudnessMeter, measure as measure_loudness};
pub use midi::{
    ALL_INPUTS, AudioClock, ClockEvent, ClockHeard, ClockTap, Heard, MidiBus, MidiConnection,
    MidiError, MidiLister, MidiMessage, MidiReceiver, MidiTap, TimedMessage, TimedPacket,
    connect_midi, is_all_inputs, midi_inputs, now_micros, parse_clock, parse_message, same_port,
};
pub use midi_out::{
    MidiOutHub, MidiOutSender, MidiSink, OpenSink, OutMessage, midi_outputs, open_midi_output,
};
pub use onsets::{
    ANALYSIS_SECONDS, Analysis, CLEAR_PROMINENCE, CLEAR_TEMPO, DEFAULT_SENSITIVITY,
    PROMINENT_ONSETS, TempoEstimate, detect_bass_onsets, detect_bass_onsets_with, detect_onsets,
    detect_onsets_with, detect_treble_onsets, detect_treble_onsets_with, estimate_grid,
    estimate_tempo, rise_db,
};
pub use output::{
    DeviceError, DeviceErrorKind, DeviceWriter, ExtraOutput, Health, InputLatency, LatencyReport,
    OpenedOutput, OutputStatus, Playback, SLOW_OUTPUT, Stream, VirtualCapture, is_fatal,
    looks_like_bluetooth, play, play_with_outputs, play_with_outputs_stopped,
};
pub use peaks::{BLOCK_FRAMES, Peak, Peaks, PeaksBuilder};
pub use pitch::{
    FoundNote, HOP_SECONDS, NoteSegmenter, NoteStep, PitchFrame, PitchTracker, Transcription,
    key_of_hz, transcribe,
};
pub use record::{RecordError, RecordTap, Recorder, Take, record_path};
pub use resample::{ResampleError, StreamResampler, resample};
pub use stream::{ClipStream, StreamSpec, StreamWorker, clip_frames, open_stream};
pub use ump::{Attribute, Midi2, Packet, PacketReader, packet_words, widen};
pub use wav::{Audio, WavError, WavStreamWriter, read_wav, write_wav};
