//! Writing finished audio to a file in a chosen format, a chunk at a time.
//!
//! Every format goes to a sibling `.partial` file that replaces the target
//! only when [`ExportWriter::finish`] succeeds, so a render that fails or is
//! cancelled never destroys an existing file.

use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use flacenc::component::BitRepr;
use flacenc::config::Encoder;
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};

/// A file format and sample depth to export in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    /// 32-bit float WAV: keeps every sample exactly, and can hold levels
    /// above full scale.
    WavFloat,
    Wav24,
    Wav16,
    Flac24,
    Flac16,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 5] = [
        Self::WavFloat,
        Self::Wav24,
        Self::Wav16,
        Self::Flac24,
        Self::Flac16,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::WavFloat => "WAV, 32-bit float",
            Self::Wav24 => "WAV, 24-bit",
            Self::Wav16 => "WAV, 16-bit",
            Self::Flac24 => "FLAC, 24-bit",
            Self::Flac16 => "FLAC, 16-bit",
        }
    }

    /// The file name extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::WavFloat | Self::Wav24 | Self::Wav16 => "wav",
            Self::Flac24 | Self::Flac16 => "flac",
        }
    }

    /// Whether samples beyond full scale are cut off. Integer formats
    /// can't hold them.
    pub fn clips(self) -> bool {
        self != Self::WavFloat
    }

    fn bits(self) -> usize {
        match self {
            Self::WavFloat => 32,
            Self::Wav24 | Self::Flac24 => 24,
            Self::Wav16 | Self::Flac16 => 16,
        }
    }
}

#[derive(Debug)]
pub enum ExportError {
    Io(io::Error),
    /// The format can't hold this many channels or this rate.
    Unsupported(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Unsupported(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<io::Error> for ExportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<hound::Error> for ExportError {
    fn from(error: hound::Error) -> Self {
        match error {
            hound::Error::IoError(error) => Self::Io(error),
            other => Self::Unsupported(other.to_string()),
        }
    }
}

/// Frames per FLAC block, at most.
const BLOCK: usize = 4096;
/// The shortest block FLAC decoders accept (the spec lets a stream's last
/// block be shorter, but some, Symphonia among them, refuse it).
const MIN_BLOCK: usize = 16;

/// A block size no larger than `BLOCK` that leaves no last block under
/// `MIN_BLOCK` frames, given the stream's length if it is known.
fn block_size(frames: Option<u64>) -> usize {
    let Some(frames) = frames.filter(|&f| f >= MIN_BLOCK as u64) else {
        return BLOCK;
    };
    (BLOCK - MIN_BLOCK..=BLOCK)
        .rev()
        .find(|&b| {
            let tail = frames % b as u64;
            tail == 0 || tail >= MIN_BLOCK as u64
        })
        .unwrap_or(BLOCK)
}

/// An audio file being written.
pub struct ExportWriter {
    inner: Option<Inner>,
    partial: PathBuf,
    path: PathBuf,
}

enum Inner {
    Wav {
        writer: hound::WavWriter<BufWriter<File>>,
        format: ExportFormat,
    },
    Flac(Box<FlacState>),
}

struct FlacState {
    stream: flacenc::component::Stream,
    config: flacenc::error::Verified<Encoder>,
    framebuf: FrameBuf,
    context: Context,
    channels: usize,
    bits: usize,
    /// Interleaved samples waiting for a whole block.
    pending: Vec<i32>,
    /// Frames per block.
    block: usize,
    /// Whether the last block was padded with silence to reach the shortest
    /// length decoders take.
    padded: bool,
    /// Frames written by the caller.
    real_frames: u64,
    frame: usize,
    out: BufWriter<File>,
}

fn scale(sample: f32, bits: usize) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    let sample = if sample.is_finite() { sample } else { 0.0 };
    (sample.clamp(-1.0, 1.0) * max).round() as i32
}

impl ExportWriter {
    pub fn create(
        path: &Path,
        format: ExportFormat,
        channels: usize,
        sample_rate: u32,
        frames: Option<u64>,
    ) -> Result<Self, ExportError> {
        if channels == 0 || channels > 8 {
            return Err(ExportError::Unsupported(format!(
                "can't export {channels} channels"
            )));
        }
        let mut partial = path.as_os_str().to_owned();
        partial.push(".partial");
        let partial = PathBuf::from(partial);
        let inner = match format {
            ExportFormat::WavFloat | ExportFormat::Wav24 | ExportFormat::Wav16 => {
                let spec = hound::WavSpec {
                    channels: channels as u16,
                    sample_rate,
                    bits_per_sample: format.bits() as u16,
                    sample_format: if format == ExportFormat::WavFloat {
                        hound::SampleFormat::Float
                    } else {
                        hound::SampleFormat::Int
                    },
                };
                Inner::Wav {
                    writer: hound::WavWriter::create(&partial, spec)?,
                    format,
                }
            }
            ExportFormat::Flac24 | ExportFormat::Flac16 => {
                let bits = format.bits();
                let block = block_size(frames);
                let config = Encoder::default()
                    .into_verified()
                    .map_err(|(_, e)| ExportError::Unsupported(e.to_string()))?;
                let mut stream =
                    flacenc::component::Stream::new(sample_rate as usize, channels, bits)
                        .map_err(|e| ExportError::Unsupported(e.to_string()))?;
                stream
                    .stream_info_mut()
                    .set_block_sizes(block, block)
                    .map_err(|e| ExportError::Unsupported(e.to_string()))?;
                Inner::Flac(Box::new(FlacState {
                    stream,
                    config,
                    framebuf: FrameBuf::with_size(channels, block)
                        .map_err(|e| ExportError::Unsupported(e.to_string()))?,
                    context: Context::new(bits, channels),
                    channels,
                    bits,
                    pending: Vec::with_capacity(block * channels),
                    block,
                    padded: false,
                    real_frames: 0,
                    frame: 0,
                    out: BufWriter::new(File::create(&partial)?),
                }))
            }
        };
        Ok(Self {
            inner: Some(inner),
            partial,
            path: path.to_owned(),
        })
    }

    /// Appends interleaved samples.
    pub fn write(&mut self, samples: &[f32]) -> Result<(), ExportError> {
        match self.inner.as_mut().expect("writer used after finish") {
            Inner::Wav { writer, format } => {
                for &sample in samples {
                    match *format {
                        ExportFormat::WavFloat => writer.write_sample(sample)?,
                        other => writer.write_sample(scale(sample, other.bits()))?,
                    }
                }
            }
            Inner::Flac(state) => {
                state.real_frames += (samples.len() / state.channels) as u64;
                for &sample in samples {
                    state.pending.push(scale(sample, state.bits));
                    if state.pending.len() == state.block * state.channels {
                        state.encode_block()?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Completes the file and moves it into place.
    pub fn finish(mut self) -> Result<(), ExportError> {
        match self.inner.take().expect("finished twice") {
            Inner::Wav { writer, .. } => writer.finalize()?,
            Inner::Flac(mut state) => {
                let waiting = state.pending.len() / state.channels;
                let mut size = state.block;
                if waiting > 0 {
                    // A stream of one short block says so in its header.
                    if state.frame == 0 {
                        size = waiting.max(MIN_BLOCK);
                    }
                    if waiting < MIN_BLOCK {
                        let len = MIN_BLOCK * state.channels;
                        state.pending.resize(len, 0);
                        state.padded = true;
                    }
                    state.encode_block()?;
                }
                // The encoder records the last, shorter block as the
                // smallest, which makes decoders such as Symphonia take the
                // stream for a variable-block one. The format counts every
                // block but the last.
                state
                    .stream
                    .stream_info_mut()
                    .set_block_sizes(size, size)
                    .map_err(|e| ExportError::Unsupported(e.to_string()))?;
                // A checksum of the padding would not match the length.
                if !state.padded {
                    let digest = state.context.md5_digest();
                    state.stream.stream_info_mut().set_md5_digest(&digest);
                }
                let total = state.real_frames as usize;
                state.stream.stream_info_mut().set_total_samples(total);
                let mut sink = flacenc::bitsink::ByteSink::new();
                state
                    .stream
                    .write(&mut sink)
                    .map_err(|e| ExportError::Unsupported(e.to_string()))?;
                state.out.write_all(sink.as_slice())?;
                state.out.flush()?;
            }
        }
        std::fs::rename(&self.partial, &self.path)?;
        Ok(())
    }
}

impl FlacState {
    /// Encodes the waiting samples (a whole block, or the last short one).
    fn encode_block(&mut self) -> Result<(), ExportError> {
        let fail = |e: &dyn fmt::Display| ExportError::Unsupported(e.to_string());
        let mut dest = (&mut self.framebuf, &mut self.context);
        dest.fill_interleaved(&self.pending).map_err(|e| fail(&e))?;
        let frame = flacenc::encode_fixed_size_frame(
            &self.config,
            &self.framebuf,
            self.frame,
            self.stream.stream_info(),
        )
        .map_err(|e| fail(&e))?;
        self.stream.add_frame(frame);
        self.frame += 1;
        self.pending.clear();
        Ok(())
    }
}

impl Drop for ExportWriter {
    fn drop(&mut self) {
        // Unfinished: close the file, then take the leftover away.
        if self.inner.take().is_some() {
            let _ = std::fs::remove_file(&self.partial);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode_file;

    fn tone(frames: usize, channels: usize) -> Vec<f32> {
        (0..frames * channels)
            .map(|i| {
                let (frame, channel) = (i / channels, i % channels);
                0.5 * (frame as f32 * 0.05 * (channel + 1) as f32).sin()
            })
            .collect()
    }

    fn export(format: ExportFormat, samples: &[f32], channels: usize) -> crate::Audio {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("out.{}", format.extension()));
        let mut writer = ExportWriter::create(
            &path,
            format,
            channels,
            44_100,
            Some((samples.len() / channels) as u64),
        )
        .unwrap();
        // In uneven pieces, to cross block boundaries.
        for piece in samples.chunks(channels * 1000 + channels) {
            writer.write(piece).unwrap();
        }
        writer.finish().unwrap();
        assert!(!dir.path().join("out.partial").exists());
        decode_file(&path).unwrap()
    }

    #[test]
    fn every_format_reads_back_what_was_written() {
        let samples = tone(10_000, 2);
        for (format, tolerance) in [
            (ExportFormat::WavFloat, 0.0),
            (ExportFormat::Wav24, 2e-7),
            (ExportFormat::Wav16, 5e-5),
            (ExportFormat::Flac24, 2e-7),
            (ExportFormat::Flac16, 5e-5),
        ] {
            let audio = export(format, &samples, 2);
            assert_eq!(audio.channels, 2, "{format:?}");
            assert_eq!(audio.sample_rate, 44_100, "{format:?}");
            assert_eq!(audio.samples.len(), samples.len(), "{format:?}");
            let worst = samples
                .iter()
                .zip(&audio.samples)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(worst <= tolerance, "{format:?}: off by {worst}");
        }
    }

    #[test]
    fn integer_formats_cut_off_what_they_cant_hold() {
        let samples = [2.0, -3.0, f32::NAN, 0.5];
        let audio = export(ExportFormat::Flac16, &samples, 1);
        assert!(audio.samples.len() >= 4);
        assert!((audio.samples[0] - 1.0).abs() < 1e-3);
        assert!((audio.samples[1] + 1.0).abs() < 1e-3);
        assert_eq!(audio.samples[2], 0.0);
    }

    #[test]
    fn an_unfinished_file_leaves_nothing_and_keeps_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        for format in [ExportFormat::Wav16, ExportFormat::Flac16] {
            let path = dir.path().join(format!("keep.{}", format.extension()));
            std::fs::write(&path, b"old").unwrap();
            let mut writer = ExportWriter::create(&path, format, 2, 44_100, Some(5_000)).unwrap();
            writer.write(&tone(5_000, 2)).unwrap();
            drop(writer);
            assert_eq!(std::fs::read(&path).unwrap(), b"old");
            let leftovers: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".partial"))
                .collect();
            assert!(leftovers.is_empty(), "{leftovers:?}");
        }
    }

    #[test]
    fn lengths_that_leave_a_tiny_last_block_still_decode() {
        // 4097 frames would leave a 1-frame block; 100 is one short block;
        // 5 frames is shorter than FLAC's shortest block.
        for frames in [4097, 8193, 100, 16, 4096 * 3 + 15] {
            let samples = tone(frames, 2);
            let audio = export(ExportFormat::Flac16, &samples, 2);
            assert_eq!(audio.samples.len(), samples.len(), "{frames} frames");
        }
        // Without the length ahead of time a short tail is padded, and the
        // file says how long the audio really is.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("padded.flac");
        let mut writer =
            ExportWriter::create(&path, ExportFormat::Flac16, 1, 44_100, None).unwrap();
        writer.write(&tone(4097, 1)).unwrap();
        writer.finish().unwrap();
        let audio = decode_file(&path).unwrap();
        assert!(audio.samples.len() >= 4097);
    }

    #[test]
    fn bad_channel_counts_are_refused() {
        let path = std::env::temp_dir().join("noodle-export-never.wav");
        assert!(ExportWriter::create(&path, ExportFormat::Wav16, 0, 44_100, None).is_err());
        assert!(ExportWriter::create(&path, ExportFormat::Flac16, 9, 44_100, None).is_err());
    }
}
