//! Separating audio into stems (drums, bass, vocals, other).
//!
//! The separator is a trait ([`Separator`]), so the model and its runtime can
//! change and tests can use [`FakeSeparator`] instead of real weights (ADR
//! 0006). [`split`] runs one over an audio file and writes the stems and the
//! residual as WAV files; [`ModelStore`] keeps the downloaded weights. Nothing
//! here runs in the audio path: separating is slow and done once per clip.

mod fake;
mod model;
#[cfg(feature = "onnx")]
mod onnx;
mod split;

use std::fmt;

pub use fake::FakeSeparator;
pub use model::{DownloadError, ModelSpec, ModelStore, SHA256_VARIABLE, URL_VARIABLE};
#[cfg(feature = "onnx")]
pub use onnx::{OnnxSeparator, runtime_file_name};
pub use split::{SplitError, SplitOutput, SplitRequest, StemFile, split};

/// The sample rate the models work at.
pub const MODEL_RATE: u32 = 44_100;

/// One of the parts a model separates audio into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stem {
    Drums,
    Bass,
    Vocals,
    Other,
}

impl Stem {
    /// In the order a [`Separator`] returns them.
    pub const ALL: [Stem; 4] = [Stem::Drums, Stem::Bass, Stem::Vocals, Stem::Other];

    pub fn label(self) -> &'static str {
        match self {
            Stem::Drums => "drums",
            Stem::Bass => "bass",
            Stem::Vocals => "vocals",
            Stem::Other => "other",
        }
    }
}

/// A part of a split: a stem, or what no stem holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Stem(Stem),
    /// The audio minus its stems, so that stems plus residual add back up to
    /// the audio.
    Residual,
}

impl Part {
    /// The stems in order, then the residual.
    pub const ALL: [Part; 5] = [
        Part::Stem(Stem::Drums),
        Part::Stem(Stem::Bass),
        Part::Stem(Stem::Vocals),
        Part::Stem(Stem::Other),
        Part::Residual,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Part::Stem(stem) => stem.label(),
            Part::Residual => "residual",
        }
    }
}

/// Two channels of audio, one vector each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stereo {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Stereo {
    pub fn silent(frames: usize) -> Self {
        Self {
            left: vec![0.0; frames],
            right: vec![0.0; frames],
        }
    }

    pub fn len(&self) -> usize {
        self.left.len()
    }

    pub fn is_empty(&self) -> bool {
        self.left.is_empty()
    }

    /// Interleaved audio of any channel count as stereo: mono is copied to
    /// both channels and channels past the second are dropped.
    pub fn from_interleaved(samples: &[f32], channels: usize) -> Self {
        let channels = channels.max(1);
        let frames = samples.len() / channels;
        let mut out = Self::silent(frames);
        for (i, frame) in samples.chunks_exact(channels).enumerate() {
            out.left[i] = frame[0];
            out.right[i] = frame[channels.min(2) - 1];
        }
        out
    }

    pub fn to_interleaved(&self) -> Vec<f32> {
        self.left
            .iter()
            .zip(&self.right)
            .flat_map(|(&l, &r)| [l, r])
            .collect()
    }
}

/// Names a model and its version, for caches that depend on which made them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelId {
    pub id: String,
    pub version: String,
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.id, self.version)
    }
}

/// Why a separation stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeparateError {
    Cancelled,
    Failed(String),
}

impl fmt::Display for SeparateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SeparateError::Cancelled => write!(f, "cancelled"),
            SeparateError::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for SeparateError {}

/// Something that separates audio into the four [`Stem`]s.
pub trait Separator: Send {
    fn model(&self) -> ModelId;

    /// The sample rate the audio must be at. Callers resample to it.
    fn sample_rate(&self) -> u32 {
        MODEL_RATE
    }

    /// Separates `mix`, which is at [`sample_rate`](Self::sample_rate), into
    /// the stems in [`Stem::ALL`] order, each as long as `mix`. `progress` is
    /// told how far along it is (0 to 1) and answers whether to go on; on
    /// `false`, stop and return [`SeparateError::Cancelled`].
    fn separate(
        &mut self,
        mix: &Stereo,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<[Stereo; 4], SeparateError>;
}
