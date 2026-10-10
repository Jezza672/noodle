//! Reading and writing WAV files.

use std::path::{Path, PathBuf};

pub use hound::Error as WavError;

/// Audio from a file: interleaved samples, scaled to between -1 and 1.
#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub channels: usize,
    pub sample_rate: u32,
}

/// Writes 32-bit float WAV, which keeps every sample exactly.
pub fn write_wav(
    path: &Path,
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
) -> Result<(), WavError> {
    let spec = hound::WavSpec {
        channels: u16::try_from(channels).map_err(|_| WavError::Unsupported)?,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &sample in samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()
}

/// Writes 32-bit float WAV a chunk at a time, for renders too long to hold in
/// memory. The audio goes to a sibling `.partial` file that replaces `path`
/// only when [`finish`](Self::finish) succeeds, so a render that fails or is
/// dropped never destroys an existing file.
pub struct WavStreamWriter {
    writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    partial: PathBuf,
    path: PathBuf,
}

impl WavStreamWriter {
    pub fn create(path: &Path, channels: usize, sample_rate: u32) -> Result<Self, WavError> {
        // A WAV needs at least one channel; hound would divide by zero.
        if channels == 0 {
            return Err(WavError::Unsupported);
        }
        let spec = hound::WavSpec {
            channels: u16::try_from(channels).map_err(|_| WavError::Unsupported)?,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut partial = path.as_os_str().to_owned();
        partial.push(".partial");
        let partial = PathBuf::from(partial);
        Ok(Self {
            writer: Some(hound::WavWriter::create(&partial, spec)?),
            partial,
            path: path.to_owned(),
        })
    }

    /// Appends interleaved samples.
    pub fn write(&mut self, samples: &[f32]) -> Result<(), WavError> {
        let writer = self.writer.as_mut().expect("writer used after finish");
        for &sample in samples {
            writer.write_sample(sample)?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), WavError> {
        self.writer.take().expect("finished twice").finalize()?;
        std::fs::rename(&self.partial, &self.path)?;
        Ok(())
    }
}

impl Drop for WavStreamWriter {
    fn drop(&mut self) {
        // Not finished: close the file and leave nothing behind.
        if self.writer.take().is_some() {
            let _ = std::fs::remove_file(&self.partial);
        }
    }
}

/// Reads integer or float WAV, scaling integer samples to between -1 and 1.
pub fn read_wav(path: &Path) -> Result<Audio, WavError> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let samples = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|s| s as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    Ok(Audio {
        samples,
        channels: usize::from(spec.channels),
        sample_rate: spec.sample_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("noodle-io-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn float_wav_round_trips_exactly() {
        let path = temp("round-trip.wav");
        let samples = [0.0, 0.5, -0.25, 1.0, -1.0, 0.123_456_79];
        write_wav(&path, &samples, 2, 44_100).unwrap();
        let audio = read_wav(&path).unwrap();
        assert_eq!(audio.samples, samples);
        assert_eq!((audio.channels, audio.sample_rate), (2, 44_100));
    }

    #[test]
    fn streamed_wav_matches_a_one_shot_write() {
        let samples = [0.0, 0.5, -0.25, 1.0, -1.0, 0.125];
        let path = temp("streamed.wav");
        let mut w = WavStreamWriter::create(&path, 2, 44_100).unwrap();
        w.write(&samples[..2]).unwrap();
        w.write(&samples[2..]).unwrap();
        w.finish().unwrap();
        let audio = read_wav(&path).unwrap();
        assert_eq!(audio.samples, samples);
        assert_eq!((audio.channels, audio.sample_rate), (2, 44_100));
    }

    #[test]
    fn an_abandoned_stream_leaves_an_existing_file_alone() {
        let path = temp("abandoned.wav");
        write_wav(&path, &[0.25, 0.5], 2, 48_000).unwrap();
        let mut w = WavStreamWriter::create(&path, 2, 48_000).unwrap();
        w.write(&[1.0; 8]).unwrap();
        drop(w);
        assert_eq!(read_wav(&path).unwrap().samples, [0.25, 0.5]);
        assert!(!path.with_extension("wav.partial").exists());
    }

    #[test]
    fn zero_channels_is_an_error_not_a_panic() {
        assert!(WavStreamWriter::create(&temp("zero.wav"), 0, 48_000).is_err());
    }

    #[test]
    fn integer_wav_is_scaled() {
        let path = temp("sixteen-bit.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for sample in [0i16, 16_384, -32_768] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        assert_eq!(read_wav(&path).unwrap().samples, [0.0, 0.5, -1.0]);
    }
}
