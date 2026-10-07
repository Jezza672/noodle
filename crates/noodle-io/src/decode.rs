//! Decoding audio files (WAV, FLAC, MP3, Ogg Vorbis, AAC and more) with
//! symphonia.
//!
//! A [`Decoder`] reads a file a chunk at a time, which is what disk streaming
//! and peak computation use. [`decode_file`] reads a whole file into memory.
//! Decoding allocates and does I/O, so it runs on a worker thread, never the
//! audio thread.

use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::{Error, SeekErrorKind};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

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

/// What to do when the container refuses a seek.
#[derive(Debug, PartialEq)]
enum SeekFallback {
    /// The position is past the end, so there is nothing left to read.
    End,
    /// The container can't go there directly, but can be read from the start.
    Restart,
    /// Nothing sensible to do.
    Fail,
}

fn on_seek_error(kind: &SeekErrorKind) -> SeekFallback {
    match kind {
        SeekErrorKind::OutOfRange => SeekFallback::End,
        SeekErrorKind::Unseekable | SeekErrorKind::ForwardOnly => SeekFallback::Restart,
        // InvalidTrack, and any kind added later: don't guess.
        _ => SeekFallback::Fail,
    }
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
    path: PathBuf,
    /// Set by a seek past the end, so reads end instead of continuing from
    /// wherever the reader was.
    at_end: bool,
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
            path: path.to_path_buf(),
            at_end: false,
            format,
            decoder,
            track_id,
            info,
        })
    }

    pub fn info(&self) -> FileInfo {
        self.info
    }

    /// Moves to `frame`, a position in the file's own frames. Returns the
    /// frame the decoder actually landed on, which is at or before the one
    /// asked for in compressed formats; the caller skips the difference. A
    /// seek past the end lands at the end.
    pub fn seek(&mut self, frame: u64) -> Result<u64, DecodeError> {
        let ts = Timestamp::new(i64::try_from(frame).unwrap_or(i64::MAX));
        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
                ts,
                track_id: self.track_id,
            },
        );
        match seeked {
            Ok(to) => {
                self.decoder.reset();
                self.at_end = false;
                Ok(u64::try_from(to.actual_ts.get()).unwrap_or(0))
            }
            Err(Error::SeekError(kind)) => match on_seek_error(&kind) {
                SeekFallback::End => {
                    self.decoder.reset();
                    self.at_end = true;
                    Ok(frame)
                }
                // Start again from the top: the caller skips forward from
                // there, which is slow for a far seek but always right.
                SeekFallback::Restart => self.restart(),
                SeekFallback::Fail => Err(Error::SeekError(kind).into()),
            },
            Err(e) => Err(e.into()),
        }
    }

    /// Reopens the file, so the next read is its first frame. Returns 0, the
    /// frame landed on.
    fn restart(&mut self) -> Result<u64, DecodeError> {
        *self = Self::open(&self.path)?;
        Ok(0)
    }

    /// Replaces `out` with the next chunk of interleaved samples. Returns the
    /// number of frames in it, or `None` at the end of the file. A packet
    /// that fails to decode comes out as silence of the same length, so one
    /// bad frame doesn't lose the rest of the file or shift it earlier.
    pub fn read_chunk(&mut self, out: &mut Vec<f32>) -> Result<Option<usize>, DecodeError> {
        if self.at_end {
            return Ok(None);
        }
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
    fn only_a_seek_out_of_range_means_the_end() {
        assert_eq!(on_seek_error(&SeekErrorKind::OutOfRange), SeekFallback::End);
        assert_eq!(
            on_seek_error(&SeekErrorKind::ForwardOnly),
            SeekFallback::Restart
        );
        assert_eq!(
            on_seek_error(&SeekErrorKind::Unseekable),
            SeekFallback::Restart
        );
        assert_eq!(
            on_seek_error(&SeekErrorKind::InvalidTrack),
            SeekFallback::Fail
        );
    }

    #[test]
    fn a_seek_goes_where_asked_and_past_the_end_reads_nothing() {
        let path = temp("decode-seek.wav");
        let samples: Vec<f32> = (0..30_000).map(|i| i as f32 / 30_000.0).collect();
        write_wav(&path, &samples, 1, 44_100).unwrap();
        let mut decoder = Decoder::open(&path).unwrap();
        let mut chunk = Vec::new();
        decoder.read_chunk(&mut chunk).unwrap();
        let landed = decoder.seek(12_345).unwrap();
        assert!(landed <= 12_345);
        decoder.read_chunk(&mut chunk).unwrap();
        assert_eq!(chunk[0], samples[landed as usize]);
        // Past the end: nothing more, rather than the old position's audio.
        decoder.seek(1_000_000).unwrap();
        assert_eq!(decoder.read_chunk(&mut chunk).unwrap(), None);
        // And a seek back works again afterwards.
        let landed = decoder.seek(100).unwrap();
        decoder.read_chunk(&mut chunk).unwrap();
        assert_eq!(chunk[0], samples[landed as usize]);
    }

    #[test]
    fn restarting_reads_the_file_from_its_first_frame() {
        let path = temp("decode-restart.wav");
        let samples: Vec<f32> = (0..30_000).map(|i| i as f32 / 30_000.0).collect();
        write_wav(&path, &samples, 1, 44_100).unwrap();
        let mut decoder = Decoder::open(&path).unwrap();
        let mut chunk = Vec::new();
        decoder.seek(20_000).unwrap();
        decoder.read_chunk(&mut chunk).unwrap();
        assert_eq!(decoder.restart().unwrap(), 0);
        decoder.read_chunk(&mut chunk).unwrap();
        assert_eq!(chunk[..10], samples[..10]);
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
