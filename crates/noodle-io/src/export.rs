//! Writing finished audio to a file in a chosen format, a chunk at a time.
//!
//! Every format goes to a sibling `.partial` file that replaces the target
//! only when [`ExportWriter::finish`] succeeds, so a render that fails or is
//! cancelled never destroys an existing file.

use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flacenc::component::BitRepr;
use flacenc::config::Encoder;
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};

/// What an exported file says about itself: written into the file (WAV
/// `LIST/INFO`, FLAC Vorbis comments) and into a CUE sheet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportMetadata {
    pub title: String,
    pub artist: String,
    /// An ISRC in its twelve-character form, as [`normalize_isrc`] returns it.
    pub isrc: String,
}

impl ExportMetadata {
    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.artist.is_empty() && self.isrc.is_empty()
    }

    /// The fields that are set, under their Vorbis comment names.
    fn fields(&self) -> Vec<(&'static str, &str)> {
        [
            ("TITLE", self.title.as_str()),
            ("ARTIST", self.artist.as_str()),
            ("ISRC", self.isrc.as_str()),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .collect()
    }
}

/// An ISRC (International Standard Recording Code) in its twelve-character
/// form: a two-letter country, a three-character registrant, a two-digit
/// year and a five-digit designation. Dashes and spaces are dropped and
/// letters upper-cased; `None` if what is left is not an ISRC.
pub fn normalize_isrc(text: &str) -> Option<String> {
    let code: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let b = code.as_bytes();
    let ok = b.len() == 12
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..5].iter().all(u8::is_ascii_alphanumeric)
        && b[5..].iter().all(u8::is_ascii_digit);
    ok.then_some(code)
}

/// A CUE sheet for one exported file holding one track: the title, the
/// artist and the ISRC, for CD burning and replication tools.
pub fn cue_sheet(file_name: &str, format: ExportFormat, metadata: &ExportMetadata) -> String {
    let quoted = |text: &str| format!("\"{}\"", text.replace('"', "'"));
    let mut cue = String::new();
    if !metadata.artist.is_empty() {
        cue.push_str(&format!("PERFORMER {}\n", quoted(&metadata.artist)));
    }
    if !metadata.title.is_empty() {
        cue.push_str(&format!("TITLE {}\n", quoted(&metadata.title)));
    }
    let kind = match format.extension() {
        "flac" => "FLAC",
        _ => "WAVE",
    };
    cue.push_str(&format!("FILE {} {kind}\n", quoted(file_name)));
    cue.push_str("  TRACK 01 AUDIO\n");
    if !metadata.title.is_empty() {
        cue.push_str(&format!("    TITLE {}\n", quoted(&metadata.title)));
    }
    if !metadata.artist.is_empty() {
        cue.push_str(&format!("    PERFORMER {}\n", quoted(&metadata.artist)));
    }
    if !metadata.isrc.is_empty() {
        cue.push_str(&format!("    ISRC {}\n", metadata.isrc));
    }
    cue.push_str("    INDEX 01 00:00:00\n");
    cue
}

/// Appends a `LIST`/`INFO` chunk to a finished WAV file. The tags are the
/// RIFF ones: `INAM` (title), `IART` (artist) and `ISRC`.
fn append_wav_info(path: &Path, metadata: &ExportMetadata) -> io::Result<()> {
    let tags = [
        (*b"INAM", metadata.title.as_str()),
        (*b"IART", metadata.artist.as_str()),
        (*b"ISRC", metadata.isrc.as_str()),
    ];
    let mut info = b"INFO".to_vec();
    for (id, text) in tags.into_iter().filter(|(_, t)| !t.is_empty()) {
        // Zero-terminated, and padded to an even length.
        let mut body = text.as_bytes().to_vec();
        body.push(0);
        info.extend_from_slice(&id);
        info.extend_from_slice(&(body.len() as u32).to_le_bytes());
        if body.len() % 2 == 1 {
            body.push(0);
        }
        info.extend_from_slice(&body);
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut end = file.seek(SeekFrom::End(0))?;
    if end % 2 == 1 {
        file.write_all(&[0])?;
        end += 1;
    }
    file.write_all(b"LIST")?;
    file.write_all(&(info.len() as u32).to_le_bytes())?;
    file.write_all(&info)?;
    // The RIFF size covers everything after its own eight bytes.
    let riff = end + 8 + info.len() as u64 - 8;
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&u32::try_from(riff).unwrap_or(u32::MAX).to_le_bytes())
}

/// A FLAC `VORBIS_COMMENT` block body for `metadata`.
fn vorbis_comments(metadata: &ExportMetadata) -> Vec<u8> {
    let vendor = b"noodle";
    let mut out = Vec::new();
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor);
    let fields = metadata.fields();
    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    for (name, value) in fields {
        let comment = format!("{name}={value}");
        out.extend_from_slice(&(comment.len() as u32).to_le_bytes());
        out.extend_from_slice(comment.as_bytes());
    }
    out
}

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

/// The most channels a FLAC file can have.
pub const FLAC_MAX_CHANNELS: usize = 8;

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
    channels: usize,
    dither: Dither,
    metadata: ExportMetadata,
}

enum Inner {
    Wav {
        writer: hound::WavWriter<BufWriter<File>>,
        format: ExportFormat,
    },
    Flac(Box<FlacState>),
}

struct FlacState {
    /// Holds the STREAMINFO only: frames go straight to `out`, so memory
    /// doesn't grow with the length of the export.
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
    /// Smallest and largest encoded frame so far, in bytes.
    frame_bytes: Option<(usize, usize)>,
    out: BufWriter<File>,
}

/// Triangular-PDF dither: two uniform values subtracted, so the noise is
/// up to one step either way and the rounding error no longer follows the
/// signal. The generator is seeded the same for every file, so exporting the
/// same audio twice writes the same bytes.
struct Dither(u64);

impl Dither {
    fn new() -> Self {
        Self(0x9E37_79B9_7F4A_7C15)
    }

    /// A value in `[0, 1)` (xorshift64*).
    fn uniform(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        bits as f32 / (1u32 << 24) as f32
    }

    /// One step of dither, in `(-1, 1)`.
    fn next(&mut self) -> f32 {
        self.uniform() - self.uniform()
    }
}

/// Rounds a sample to `bits` bits, with TPDF dither added first. Digital
/// silence stays silent, so a track file is exactly empty where its track is.
fn scale(sample: f32, bits: usize, dither: &mut Dither) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    let sample = if sample.is_finite() { sample } else { 0.0 };
    let noise = if sample == 0.0 { 0.0 } else { dither.next() };
    (sample.clamp(-1.0, 1.0) * max + noise)
        .round()
        .clamp(-max, max) as i32
}

impl ExportWriter {
    pub fn create(
        path: &Path,
        format: ExportFormat,
        channels: usize,
        sample_rate: u32,
        frames: Option<u64>,
    ) -> Result<Self, ExportError> {
        Self::create_tagged(
            path,
            format,
            channels,
            sample_rate,
            frames,
            ExportMetadata::default(),
        )
    }

    /// Like [`Self::create`], writing `metadata` into the file.
    pub fn create_tagged(
        path: &Path,
        format: ExportFormat,
        channels: usize,
        sample_rate: u32,
        frames: Option<u64>,
        metadata: ExportMetadata,
    ) -> Result<Self, ExportError> {
        if channels == 0 || channels > noodle_engine::MAX_LAYOUT_CHANNELS {
            return Err(ExportError::Unsupported(format!(
                "can't export {channels} channels"
            )));
        }
        if channels > FLAC_MAX_CHANNELS
            && matches!(format, ExportFormat::Flac24 | ExportFormat::Flac16)
        {
            return Err(ExportError::Unsupported(format!(
                "FLAC holds at most {FLAC_MAX_CHANNELS} channels, not {channels}; use WAV"
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
                if !metadata.is_empty() {
                    // A VORBIS_COMMENT block (type 4).
                    let comments = flacenc::component::MetadataBlockData::new_unknown(
                        4,
                        &vorbis_comments(&metadata),
                    )
                    .map_err(|e| ExportError::Unsupported(e.to_string()))?;
                    stream.add_metadata_block(comments);
                }
                let mut out = BufWriter::new(File::create(&partial)?);
                // A placeholder header, the same length as the final one,
                // overwritten in `finish` once the totals are known.
                out.write_all(&flac_header(&stream)?)?;
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
                    frame_bytes: None,
                    out,
                }))
            }
        };
        Ok(Self {
            inner: Some(inner),
            partial,
            path: path.to_owned(),
            channels,
            dither: Dither::new(),
            metadata,
        })
    }

    /// Appends interleaved samples. The 16 and 24-bit formats round them
    /// with TPDF dither; 32-bit float keeps them as they are.
    pub fn write(&mut self, samples: &[f32]) -> Result<(), ExportError> {
        match self.inner.as_mut().expect("writer used after finish") {
            Inner::Wav { writer, format } => {
                for &sample in samples {
                    match *format {
                        ExportFormat::WavFloat => writer.write_sample(sample)?,
                        other => {
                            writer.write_sample(scale(sample, other.bits(), &mut self.dither))?
                        }
                    }
                }
            }
            Inner::Flac(state) => {
                state.real_frames += (samples.len() / state.channels) as u64;
                for &sample in samples {
                    state
                        .pending
                        .push(scale(sample, state.bits, &mut self.dither));
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
            Inner::Wav { writer, .. } => {
                writer.finalize()?;
                crate::wav::set_wav_channel_mask(&self.partial, self.channels)?;
                if !self.metadata.is_empty() {
                    append_wav_info(&self.partial, &self.metadata)?;
                }
            }
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
                // The encoder would record the last, shorter block as the
                // smallest, which makes decoders such as Symphonia take the
                // stream for a variable-block one. The format counts every
                // block but the last.
                let info = state.stream.stream_info_mut();
                info.set_block_sizes(size, size)
                    .map_err(|e| ExportError::Unsupported(e.to_string()))?;
                let (min, max) = state.frame_bytes.unwrap_or((0, 0));
                info.set_frame_sizes(min, max)
                    .map_err(|e| ExportError::Unsupported(e.to_string()))?;
                // A checksum of the padding would not match the length.
                if !state.padded {
                    let digest = state.context.md5_digest();
                    info.set_md5_digest(&digest);
                }
                info.set_total_samples(state.real_frames as usize);
                let header = flac_header(&state.stream)?;
                state.out.seek(SeekFrom::Start(0))?;
                state.out.write_all(&header)?;
                state.out.flush()?;
            }
        }
        std::fs::rename(&self.partial, &self.path)?;
        Ok(())
    }
}

/// The bytes before the first frame: `fLaC` and the STREAMINFO block.
fn flac_header(stream: &flacenc::component::Stream) -> Result<Vec<u8>, ExportError> {
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| ExportError::Unsupported(e.to_string()))?;
    Ok(sink.as_slice().to_vec())
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
        let mut sink = flacenc::bitsink::ByteSink::new();
        frame.write(&mut sink).map_err(|e| fail(&e))?;
        let bytes = sink.as_slice();
        self.out.write_all(bytes)?;
        let len = bytes.len();
        self.frame_bytes = Some(match self.frame_bytes {
            Some((min, max)) => (min.min(len), max.max(len)),
            None => (len, len),
        });
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
    #[test]
    fn a_surround_wav_export_carries_the_layouts_mask() {
        let dir = tempfile::tempdir().unwrap();
        for (channels, mask) in [(4usize, 0x33u32), (8, 0x63F), (10, 0x563F), (12, 0x2D63F)] {
            let path = dir.path().join(format!("{channels}.wav"));
            let mut writer =
                ExportWriter::create(&path, ExportFormat::Wav24, channels, 48_000, Some(10))
                    .unwrap();
            writer.write(&vec![0.1; channels * 10]).unwrap();
            writer.finish().unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(
                u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
                mask,
                "{channels} channels"
            );
        }
    }

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
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".partial"))
            .count();
        assert_eq!(leftovers, 0);
        decode_file(&path).unwrap()
    }

    #[test]
    fn sixteen_and_twenty_four_bits_are_tpdf_dithered_and_float_is_not() {
        // A level a third of the way between two 16-bit steps. Rounded
        // plainly every sample is the same step; dithered, the steps either
        // side share it out so the mean lands on the true level, with
        // triangular spread up to one step each way.
        let step = 1.0 / 32767.0;
        let level = (1000.0 + 1.0 / 3.0) * step;
        let samples = vec![level; 30_000];
        for format in [ExportFormat::Wav16, ExportFormat::Flac16] {
            let audio = export(format, &samples, 1);
            let codes: Vec<i32> = audio
                .samples
                .iter()
                .map(|s| (s * 32767.0).round() as i32)
                .collect();
            let distinct: std::collections::BTreeSet<i32> = codes.iter().copied().collect();
            assert!(distinct.len() >= 3, "{format:?}: {distinct:?}");
            assert!(
                codes.iter().all(|&c| (999..=1002).contains(&c)),
                "{format:?}"
            );
            let mean = codes.iter().map(|&c| f64::from(c)).sum::<f64>() / codes.len() as f64;
            assert!(
                (mean - (1000.0 + 1.0 / 3.0)).abs() < 0.02,
                "{format:?}: {mean}"
            );
            // Triangular, not uniform: the middle step is the commonest.
            let at = |c: i32| codes.iter().filter(|&&x| x == c).count();
            assert!(at(1000) > at(999) && at(1000) > at(1002), "{format:?}");
        }
        // 24 bits are dithered too, at their own, finer step.
        let audio = export(ExportFormat::Wav24, &samples, 1);
        let distinct: std::collections::BTreeSet<i64> = audio
            .samples
            .iter()
            .map(|s| (f64::from(*s) * 8_388_607.0).round() as i64)
            .collect();
        assert!(distinct.len() >= 2, "{distinct:?}");
        // Float is written as it is, and digital silence stays silent.
        assert!(export(ExportFormat::WavFloat, &samples, 1).samples == samples);
        let silent = export(ExportFormat::Wav16, &vec![0.0; 5_000], 1);
        assert!(silent.samples.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn the_same_audio_exports_to_the_same_bytes() {
        let samples = tone(5_000, 2);
        let a = export(ExportFormat::Wav16, &samples, 2).samples;
        let b = export(ExportFormat::Wav16, &samples, 2).samples;
        assert_eq!(a, b);
    }

    #[test]
    fn every_format_reads_back_what_was_written() {
        let samples = tone(10_000, 2);
        for (format, tolerance) in [
            (ExportFormat::WavFloat, 0.0),
            // Up to one step of dither and half a step of rounding.
            (ExportFormat::Wav24, 3e-7),
            (ExportFormat::Wav16, 7e-5),
            (ExportFormat::Flac24, 3e-7),
            (ExportFormat::Flac16, 7e-5),
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
        // 5 frames is shorter than FLAC's shortest block, and so is 1.
        for frames in [4097, 8193, 100, 16, 4096 * 3 + 15, 5, 1] {
            let samples = tone(frames, 2);
            let audio = export(ExportFormat::Flac16, &samples, 2);
            if frames < MIN_BLOCK {
                // Padded with silence up to the shortest block.
                assert!(audio.samples.len() >= samples.len(), "{frames} frames");
            } else {
                assert_eq!(audio.samples.len(), samples.len(), "{frames} frames");
            }
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
        // A stream shorter than the shortest block is padded to it.
        let path = dir.path().join("tiny.flac");
        let mut writer =
            ExportWriter::create(&path, ExportFormat::Flac16, 1, 44_100, None).unwrap();
        writer.write(&tone(5, 1)).unwrap();
        writer.finish().unwrap();
        let audio = decode_file(&path).unwrap();
        assert!(audio.samples.len() >= 5);
    }

    #[test]
    fn a_long_flac_stream_goes_to_disk_as_it_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.flac");
        let mut writer =
            ExportWriter::create(&path, ExportFormat::Flac16, 1, 44_100, None).unwrap();
        for _ in 0..20 {
            writer.write(&tone(BLOCK * 4, 1)).unwrap();
        }
        // Before `finish`, the encoded frames are already written, not held.
        let state = match writer.inner.as_ref().unwrap() {
            Inner::Flac(state) => state,
            Inner::Wav { .. } => unreachable!(),
        };
        assert_eq!(state.stream.frame_count(), 0);
        assert!(state.frame >= 80);
        writer.finish().unwrap();
    }

    #[test]
    fn isrcs_are_checked_and_tidied() {
        assert_eq!(
            normalize_isrc("us-rc1-76-07839").as_deref(),
            Some("USRC17607839")
        );
        assert_eq!(
            normalize_isrc(" GBAYE 6800011 ").as_deref(),
            Some("GBAYE6800011")
        );
        for bad in [
            "",
            "USRC1760783",
            "USRC176078390",
            "1SRC17607839",
            "USRC1760783A",
            "US_C17607839",
        ] {
            assert_eq!(normalize_isrc(bad), None, "{bad:?}");
        }
    }

    fn tagged() -> ExportMetadata {
        ExportMetadata {
            title: "Night Drive".into(),
            artist: "The Noodles".into(),
            isrc: "USRC17607839".into(),
        }
    }

    #[test]
    fn a_wav_carries_its_isrc_and_title_and_still_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        // An odd number of 24-bit mono bytes, to meet the padding.
        for (name, frames) in [("even.wav", 100usize), ("odd.wav", 101)] {
            let path = dir.path().join(name);
            let samples = tone(frames, 1);
            let mut writer = ExportWriter::create_tagged(
                &path,
                ExportFormat::Wav24,
                1,
                44_100,
                Some(frames as u64),
                tagged(),
            )
            .unwrap();
            writer.write(&samples).unwrap();
            writer.finish().unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let riff = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
            assert_eq!(
                riff,
                bytes.len() - 8,
                "{name}: the RIFF size covers the tags"
            );
            let text = String::from_utf8_lossy(&bytes);
            for want in [
                "LIST",
                "INFO",
                "INAM",
                "Night Drive",
                "IART",
                "ISRC",
                "USRC17607839",
            ] {
                assert!(text.contains(want), "{name}: {want}");
            }
            let audio = decode_file(&path).unwrap();
            assert_eq!(audio.samples.len(), samples.len(), "{name}");
        }
    }

    #[test]
    fn a_flac_carries_its_isrc_as_a_vorbis_comment_and_still_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tagged.flac");
        let samples = tone(9_000, 2);
        let mut writer = ExportWriter::create_tagged(
            &path,
            ExportFormat::Flac16,
            2,
            44_100,
            Some(9_000),
            tagged(),
        )
        .unwrap();
        writer.write(&samples).unwrap();
        writer.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes[..300]);
        assert!(text.contains("ISRC=USRC17607839"), "{text}");
        assert!(text.contains("TITLE=Night Drive"));
        let audio = decode_file(&path).unwrap();
        assert_eq!(audio.samples.len(), samples.len());
        assert_eq!(audio.channels, 2);
    }

    #[test]
    fn a_cue_sheet_names_the_file_the_track_and_its_isrc() {
        let cue = cue_sheet("mix.wav", ExportFormat::Wav16, &tagged());
        assert_eq!(
            cue,
            "PERFORMER \"The Noodles\"\nTITLE \"Night Drive\"\nFILE \"mix.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Night Drive\"\n    PERFORMER \"The Noodles\"\n    ISRC USRC17607839\n    INDEX 01 00:00:00\n"
        );
        // Quotes in a title can't break the sheet; nothing set, nothing written.
        let plain = ExportMetadata {
            title: "Say \"hi\"".into(),
            ..Default::default()
        };
        let cue = cue_sheet("a.flac", ExportFormat::Flac16, &plain);
        assert!(cue.contains("TITLE \"Say 'hi'\"") && cue.contains("FLAC"));
        assert!(!cue.contains("ISRC") && !cue.contains("PERFORMER"));
    }

    #[test]
    fn bad_channel_counts_are_refused() {
        let path = std::env::temp_dir().join("noodle-export-never.wav");
        assert!(ExportWriter::create(&path, ExportFormat::Wav16, 0, 44_100, None).is_err());
        assert!(ExportWriter::create(&path, ExportFormat::Flac16, 9, 44_100, None).is_err());
        assert!(ExportWriter::create(&path, ExportFormat::Wav16, 13, 44_100, None).is_err());
        // WAV takes the height layouts; FLAC refuses past 8, saying why.
        let wav = ExportWriter::create(&path, ExportFormat::Wav24, 12, 44_100, None);
        assert!(wav.is_ok());
        drop(wav);
        let flac = ExportWriter::create(&path, ExportFormat::Flac24, 12, 44_100, None);
        assert!(
            flac.err()
                .unwrap()
                .to_string()
                .contains("FLAC holds at most 8")
        );
    }
}
