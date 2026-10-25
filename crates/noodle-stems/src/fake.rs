//! A separator for tests: it splits by frequency band, which needs no model.

use crate::{ModelId, SeparateError, Separator, Stem, Stereo};

/// Splits audio into bands with one-pole filters: `bass` is below about
/// 150 Hz, `drums` above about 2 kHz, and the band between goes half to
/// `vocals`, a quarter to `other` and a quarter to nowhere, so the residual is
/// not empty. Stems plus residual add back up to the input.
#[derive(Clone, Debug)]
pub struct FakeSeparator {
    rate: u32,
}

impl FakeSeparator {
    pub fn new() -> Self {
        Self {
            rate: crate::MODEL_RATE,
        }
    }

    /// One that asks for audio at `rate`, to test resampling around it.
    pub fn at_rate(rate: u32) -> Self {
        Self { rate }
    }
}

impl Default for FakeSeparator {
    fn default() -> Self {
        Self::new()
    }
}

fn low_pass(input: &[f32], hz: f32, rate: u32) -> Vec<f32> {
    let k = 1.0 - (-std::f32::consts::TAU * hz / rate as f32).exp();
    let mut state = 0.0;
    input
        .iter()
        .map(|&x| {
            state += k * (x - state);
            state
        })
        .collect()
}

impl Separator for FakeSeparator {
    fn model(&self) -> ModelId {
        ModelId {
            id: "fake".into(),
            version: "1".into(),
        }
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn separate(
        &mut self,
        mix: &Stereo,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<[Stereo; 4], SeparateError> {
        let mut stems = Stem::ALL.map(|_| Stereo::silent(mix.len()));
        for (channel, input) in [&mix.left, &mix.right].into_iter().enumerate() {
            if !progress(channel as f32 / 2.0) {
                return Err(SeparateError::Cancelled);
            }
            let low = low_pass(input, 150.0, self.rate);
            let below_high = low_pass(input, 2_000.0, self.rate);
            for i in 0..input.len() {
                let band = below_high[i] - low[i];
                let parts = [input[i] - below_high[i], low[i], 0.5 * band, 0.25 * band];
                for (stem, value) in stems.iter_mut().zip(parts) {
                    if channel == 0 {
                        stem.left[i] = value;
                    } else {
                        stem.right[i] = value;
                    }
                }
            }
        }
        if !progress(1.0) {
            return Err(SeparateError::Cancelled);
        }
        Ok(stems)
    }
}
