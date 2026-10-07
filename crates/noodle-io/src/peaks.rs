//! Waveform overviews: the minimum and maximum of each short block of a file,
//! so the arrangement view can draw a clip at any zoom without touching the
//! samples again.
//!
//! Positions are in the file's own frames, the same unit a clip uses for its
//! source offset and length, so a clip's visible part maps straight onto a
//! range of the peaks whatever the engine's sample rate.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Audio, DecodeError, Decoder};

/// Frames summarised by each stored peak.
pub const BLOCK_FRAMES: u32 = 256;

/// The extremes of a stretch of audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Peak {
    pub min: f32,
    pub max: f32,
}

impl Peak {
    const EMPTY: Self = Self { min: 0.0, max: 0.0 };

    fn of(sample: f32) -> Self {
        Self {
            min: sample,
            max: sample,
        }
    }

    fn include(&mut self, other: Self) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }
}

/// A file's waveform overview. It is plain data, so a project can save it
/// next to the audio instead of decoding the file again on every open.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Peaks {
    channels: usize,
    sample_rate: u32,
    frames: u64,
    /// One entry per block per channel, block-major.
    data: Vec<Peak>,
}

impl Peaks {
    pub fn from_audio(audio: &Audio) -> Self {
        let mut builder = PeaksBuilder::new(audio.channels, audio.sample_rate);
        builder.push(&audio.samples);
        builder.finish()
    }

    /// Decodes the file once, a chunk at a time, so a long file never has to
    /// be in memory.
    pub fn from_file(path: &Path) -> Result<Self, DecodeError> {
        let mut decoder = Decoder::open(path)?;
        let info = decoder.info();
        let mut builder = PeaksBuilder::new(info.channels, info.sample_rate);
        let mut chunk = Vec::new();
        while decoder.read_chunk(&mut chunk)?.is_some() {
            builder.push(&chunk);
        }
        Ok(builder.finish())
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The file's length in frames.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Splits the frames `start..end` into `columns` equal parts and returns
    /// each part's extremes, for drawing one pixel column each. `channel`
    /// picks one channel, or `None` combines them all. Frames past the end of
    /// the file read as silence.
    ///
    /// The resolution is [`BLOCK_FRAMES`]: a column covering fewer frames
    /// than that repeats its block's extremes, so zoomed in past that the view
    /// should read the samples instead.
    pub fn columns(
        &self,
        channel: Option<usize>,
        start: u64,
        end: u64,
        columns: usize,
    ) -> Vec<Peak> {
        let mut out = vec![Peak::EMPTY; columns];
        if columns == 0 || end <= start || self.channels == 0 {
            return out;
        }
        let block = u64::from(BLOCK_FRAMES);
        let blocks = self.data.len() / self.channels;
        let span = u128::from(end - start);
        let boundary = |i: usize| start + (span * i as u128 / columns as u128) as u64;
        for (i, slot) in out.iter_mut().enumerate() {
            let (a, b) = (boundary(i), boundary(i + 1));
            let first = (a / block) as usize;
            // Every column covers at least its first block.
            let last = (b.saturating_sub(1).max(a) / block) as usize;
            let mut peak: Option<Peak> = None;
            for blk in first..=last.min(blocks.saturating_sub(1)) {
                if blk >= blocks {
                    break;
                }
                let row = &self.data[blk * self.channels..(blk + 1) * self.channels];
                let chans = match channel {
                    Some(c) => std::slice::from_ref(&row[c.min(self.channels - 1)]),
                    None => row,
                };
                for &p in chans {
                    match &mut peak {
                        Some(acc) => acc.include(p),
                        None => peak = Some(p),
                    }
                }
            }
            *slot = peak.unwrap_or(Peak::EMPTY);
        }
        out
    }
}

/// Builds [`Peaks`] from interleaved samples, in as many pieces as they
/// arrive.
pub struct PeaksBuilder {
    channels: usize,
    sample_rate: u32,
    frames: u64,
    data: Vec<Peak>,
    /// The block being filled, one peak per channel.
    current: Vec<Peak>,
    in_current: u32,
}

impl PeaksBuilder {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
            frames: 0,
            data: Vec::new(),
            current: Vec::new(),
            in_current: 0,
        }
    }

    /// Adds interleaved samples. A trailing partial frame is ignored.
    pub fn push(&mut self, samples: &[f32]) {
        if self.channels == 0 {
            return;
        }
        for frame in samples.chunks_exact(self.channels) {
            if self.in_current == 0 {
                self.current.clear();
                self.current.extend(frame.iter().map(|&s| Peak::of(s)));
            } else {
                for (peak, &s) in self.current.iter_mut().zip(frame) {
                    peak.include(Peak::of(s));
                }
            }
            self.in_current += 1;
            self.frames += 1;
            if self.in_current == BLOCK_FRAMES {
                self.data.append(&mut self.current);
                self.in_current = 0;
            }
        }
    }

    pub fn finish(mut self) -> Peaks {
        if self.in_current > 0 {
            self.data.append(&mut self.current);
        }
        Peaks {
            channels: self.channels,
            sample_rate: self.sample_rate,
            frames: self.frames,
            data: self.data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono(samples: Vec<f32>) -> Audio {
        Audio {
            samples,
            channels: 1,
            sample_rate: 48_000,
        }
    }

    #[test]
    fn a_loud_spike_survives_zooming_out() {
        let mut samples = vec![0.0; 100_000];
        samples[54_321] = 0.9;
        samples[54_322] = -0.7;
        let peaks = Peaks::from_audio(&mono(samples));
        let cols = peaks.columns(None, 0, 100_000, 10);
        let max = cols.iter().map(|p| p.max).fold(f32::MIN, f32::max);
        let min = cols.iter().map(|p| p.min).fold(f32::MAX, f32::min);
        assert_eq!((max, min), (0.9, -0.7));
        // The spike is in column 5 and nowhere else.
        assert_eq!(
            cols[5],
            Peak {
                min: -0.7,
                max: 0.9
            }
        );
        assert_eq!(cols[4], Peak::EMPTY);
    }

    #[test]
    fn streamed_in_pieces_equals_in_one_go() {
        let samples: Vec<f32> = (0..5000)
            .map(|i| ((i * 7919) % 200) as f32 / 100.0 - 1.0)
            .collect();
        let whole = Peaks::from_audio(&mono(samples.clone()));
        let mut builder = PeaksBuilder::new(1, 48_000);
        for piece in samples.chunks(333) {
            builder.push(piece);
        }
        assert_eq!(builder.finish(), whole);
    }

    #[test]
    fn channels_are_separate_or_combined() {
        // Left rises to 1, right falls to -1.
        let mut samples = Vec::new();
        for i in 0..1024 {
            let x = i as f32 / 1023.0;
            samples.extend([x, -x]);
        }
        let peaks = Peaks::from_audio(&Audio {
            samples,
            channels: 2,
            sample_rate: 44_100,
        });
        assert_eq!(peaks.frames(), 1024);
        let left = peaks.columns(Some(0), 0, 1024, 1)[0];
        let right = peaks.columns(Some(1), 0, 1024, 1)[0];
        let both = peaks.columns(None, 0, 1024, 1)[0];
        assert_eq!((left.min, left.max), (0.0, 1.0));
        assert_eq!((right.min, right.max), (-1.0, 0.0));
        assert_eq!((both.min, both.max), (-1.0, 1.0));
    }

    #[test]
    fn a_partial_last_block_counts_and_past_the_end_is_silent() {
        let mut samples = vec![0.0; 300];
        samples[299] = 0.5;
        let peaks = Peaks::from_audio(&mono(samples));
        assert_eq!(peaks.frames(), 300);
        // Ask for more than the file has: only the block holding frame 299
        // is loud, and everything after it is silence.
        let cols = peaks.columns(None, 0, 1024, 4);
        assert_eq!(cols[0], Peak::EMPTY);
        assert_eq!(cols[1].max, 0.5);
        assert_eq!(&cols[2..], &[Peak::EMPTY; 2]);
    }

    #[test]
    fn degenerate_ranges_give_silence_not_a_panic() {
        let peaks = Peaks::from_audio(&mono(vec![1.0; 1000]));
        assert!(peaks.columns(None, 0, 1000, 0).is_empty());
        assert_eq!(peaks.columns(None, 500, 500, 3), vec![Peak::EMPTY; 3]);
        assert_eq!(peaks.columns(None, 10_000, 20_000, 2), vec![Peak::EMPTY; 2]);
        let empty = Peaks::from_audio(&mono(vec![]));
        assert_eq!(empty.columns(None, 0, 100, 4), vec![Peak::EMPTY; 4]);
    }

    #[test]
    fn from_file_matches_from_audio() {
        let path = std::env::temp_dir().join("noodle-io-tests/peaks-file.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let samples: Vec<f32> = (0..20_000).map(|i| (i as f32 * 0.05).sin()).collect();
        crate::write_wav(&path, &samples, 2, 44_100).unwrap();
        let from_file = Peaks::from_file(&path).unwrap();
        let audio = crate::decode_file(&path).unwrap();
        assert_eq!(from_file, Peaks::from_audio(&audio));
    }
}
