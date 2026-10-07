//! Decoding audio files (WAV, FLAC, MP3, Ogg Vorbis, AAC and more) with
//! symphonia.
//!
//! A [`Decoder`] reads a file a chunk at a time, which is what disk streaming
//! and peak computation use. [`decode_file`] reads a whole file into memory.
//! Decoding allocates and does I/O, so it runs on a worker thread, never the
//! audio thread.

use std::fmt;
use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::Audio;

#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    /// The container or codec isn't one we can read.
    Unsupported(String),
    /// The file has no audio track.
    NoAudio,
    /// The data is damaged beyond recovery.
    Corrupt(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "can't read the file: {e}"),
            Self::Unsupported(what) => write!(f, "unsupported audio format: {what}"),
            Self::NoAudio => f.write_str("the file has no audio track"),
            Self::Corrupt(what) => write!(f, "the audio is damaged: {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<Error> for DecodeError {
    fn from(e: Error) -> Self {
        match e {
            Error::IoError(e) => Self::Io(e),
            Error::Unsupported(what) => Self::Unsupported(what.to_owned()),
            other => Self::Corrupt(other.to_string()),
        }
    }
}

/// The longest stretch of silence put in for one packet that can't be
/// decoded, so a bad length field can't make us allocate gigabytes.
const MAX_GAP_FRAMES: u64 = 1 << 16;

/// Replaces `out` with `frames` frames of silence.
fn silence(out: &mut Vec<f32>, frames: usize, channels: usize) {
    out.clear();
    out.resize(frames * channels, 0.0);
}

/// What a file says about its audio, before any of it is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub channels: usize,
    pub sample_rate: u32,
    /// The length in frames, when the container records it. Some streams
    /// (raw MP3, for one) don't, and the count is only known after decoding.
    pub frames: Option<u64>,
}

/// Reads a file's audio in chunks of interleaved `f32` samples.
pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    info: FileInfo,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let file = File::open(path)?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe().probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::NoAudio)?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(DecodeError::NoAudio)?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())?;
        let channels = params.channels.as_ref().map_or(0, |c| c.count());
        let sample_rate = params.sample_rate.unwrap_or(0);
        if channels == 0 || sample_rate == 0 {
            return Err(DecodeError::Unsupported(
                "the file doesn't say its channel count or sample rate".into(),
            ));
        }
        let info = FileInfo {
            channels,
            sample_rate,
            frames: track.num_frames,
        };
        let track_id = track.id;
        Ok(Self {
            format,
            decoder,
            track_id,
            info,
        })
    }

    pub fn info(&self) -> FileInfo {
        self.info
    }

    /// Replaces `out` with the next chunk of interleaved samples. Returns the
    /// number of frames in it, or `None` at the end of the file. A packet
    /// that fails to decode comes out as silence of the same length, so one
    /// bad frame doesn't lose the rest of the file or shift it earlier.
    pub fn read_chunk(&mut self, out: &mut Vec<f32>) -> Result<Option<usize>, DecodeError> {
        loop {
            let Some(packet) = self.format.next_packet()? else {
                return Ok(None);
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    let frames = buf.frames();
                    if frames == 0 {
                        continue;
                    }
                    buf.copy_to_vec_interleaved(out);
                    return Ok(Some(frames));
                }
                Err(Error::DecodeError(_) | Error::IoError(_)) => {
                    // Keep the later audio where it belongs: a clip's offset
                    // counts frames from the start of the file.
                    let frames = packet.dur.get().min(MAX_GAP_FRAMES) as usize;
                    if frames == 0 {
                        continue;
                    }
                    silence(out, frames, self.info.channels);
                    return Ok(Some(frames));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Decodes a whole file into memory. Use [`Decoder`] for files too long for
/// that.
pub fn decode_file(path: &Path) -> Result<Audio, DecodeError> {
    let mut decoder = Decoder::open(path)?;
    let info = decoder.info();
    let reserve = info.frames.unwrap_or(0) as usize * info.channels;
    let mut samples = Vec::with_capacity(reserve);
    let mut chunk = Vec::new();
    while decoder.read_chunk(&mut chunk)?.is_some() {
        samples.extend_from_slice(&chunk);
    }
    Ok(Audio {
        samples,
        channels: info.channels,
        sample_rate: info.sample_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write_wav;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("noodle-io-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn decodes_a_float_wav_exactly() {
        let path = temp("decode-float.wav");
        // Long enough to span many packets.
        let samples: Vec<f32> = (0..200_000)
            .map(|i| (i as f32 * 0.01).sin() * 0.5)
            .collect();
        write_wav(&path, &samples, 2, 44_100).unwrap();
        let audio = decode_file(&path).unwrap();
        assert_eq!(audio.samples, samples);
        assert_eq!((audio.channels, audio.sample_rate), (2, 44_100));
    }

    #[test]
    fn info_reports_the_length_before_decoding() {
        let path = temp("decode-info.wav");
        write_wav(&path, &vec![0.0; 600], 2, 48_000).unwrap();
        let info = Decoder::open(&path).unwrap().info();
        assert_eq!(
            info,
            FileInfo {
                channels: 2,
                sample_rate: 48_000,
                frames: Some(300)
            }
        );
    }

    #[test]
    fn a_bad_packet_becomes_silence_of_its_length() {
        let mut out = vec![0.7; 10];
        silence(&mut out, 3, 2);
        assert_eq!(out, [0.0; 6]);
    }

    #[test]
    fn rejects_a_file_that_isnt_audio() {
        let path = temp("decode-garbage.wav");
        std::fs::write(&path, b"this is not audio, just some text").unwrap();
        assert!(matches!(
            decode_file(&path),
            Err(DecodeError::Unsupported(_) | DecodeError::Corrupt(_))
        ));
        assert!(matches!(
            decode_file(&temp("does-not-exist.wav")),
            Err(DecodeError::Io(_))
        ));
    }
}
