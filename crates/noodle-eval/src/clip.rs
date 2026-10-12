//! Audio handed to the metrics.

use noodle_io::Audio;

/// Interleaved audio at a sample rate, borrowed.
#[derive(Debug, Clone, Copy)]
pub struct Clip<'a> {
    pub samples: &'a [f32],
    pub channels: usize,
    pub rate: u32,
}

impl<'a> Clip<'a> {
    pub fn new(samples: &'a [f32], channels: usize, rate: u32) -> Self {
        assert!(channels > 0, "a clip needs at least one channel");
        Self {
            samples,
            channels,
            rate,
        }
    }

    pub fn from_audio(audio: &'a Audio) -> Self {
        Self::new(&audio.samples, audio.channels, audio.sample_rate)
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate)
    }

    /// The average of the channels.
    pub fn mono(&self) -> Vec<f32> {
        let c = self.channels;
        self.samples
            .chunks_exact(c)
            .map(|f| f.iter().sum::<f32>() / c as f32)
            .collect()
    }

    /// One channel's samples.
    pub fn channel(&self, index: usize) -> Vec<f32> {
        self.samples
            .chunks_exact(self.channels)
            .map(|f| f[index])
            .collect()
    }
}
