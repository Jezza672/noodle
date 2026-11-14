//! Drum-part separation: pulling kick, snare, toms, hi-hat and cymbals out of
//! a drum recording, for the drum rack analyser.
//!
//! Three models are supported, in this order: LarsNet, Timbrica and drumsep.
//! Each has a [`DrumModelManifest`] (parts, licence, source, checksum,
//! chunking), a runtime that matches how it is built, and the same
//! [`PartSeparator`] trait, which gives N named parts rather than the four
//! fixed stems of [`Separator`](crate::Separator). [`mapping`] puts a model's
//! parts onto the drum rack's pads, so what reads the result need not know
//! which model made it. Nothing is bundled: [`DrumModelStore`] downloads the
//! weights through `noodle-download` once the user has said yes.
//!
//! The STFT front end of every model is here, outside the ONNX graph. The
//! neural step is behind [`MaskNet`] and [`HybridNet`], so the chunking,
//! blending, remainder and cancelling are tested without a runtime; the ONNX
//! implementations are in `onnx.rs` and run only against real files.

mod hybrid;
mod manifest;
mod mapping;
mod mask;
#[cfg(feature = "onnx")]
mod onnx;
pub mod stft;
mod store;

use std::fmt;

pub use hybrid::{HybridNet, HybridOutput, HybridSeparator};
pub use manifest::{
    ALL_MODELS, Chunking, DRUMSEP, DrumModelManifest, LARSNET, Layout, MaskOutput, ModelFile,
    TIMBRICA, manifest_by_id,
};
pub use mapping::{pad_sources, pads_for, part_for};
pub use mask::{MaskNet, MaskSeparator};
#[cfg(feature = "onnx")]
pub use onnx::{open_files, open_separator};
pub use store::{DrumModelStore, StoreError};

use crate::{ModelId, SeparateError, Stereo};

/// One named output of a drum separation model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DrumPart {
    Kick,
    Snare,
    Toms,
    HiHat,
    Cymbals,
    /// Everything the model's other parts leave of the mix: Timbrica's hat and
    /// cymbals together. It is the mix minus the other parts, so it also holds
    /// whatever those parts missed.
    Rest,
}

impl DrumPart {
    pub fn label(self) -> &'static str {
        match self {
            DrumPart::Kick => "kick",
            DrumPart::Snare => "snare",
            DrumPart::Toms => "toms",
            DrumPart::HiHat => "hi-hat",
            DrumPart::Cymbals => "cymbals",
            DrumPart::Rest => "rest (hat and cymbals)",
        }
    }
}

impl fmt::Display for DrumPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Something that separates a drum recording into named parts.
pub trait PartSeparator: Send {
    fn model(&self) -> ModelId;

    /// The sample rate the audio must be at. Callers resample to it.
    fn sample_rate(&self) -> u32;

    /// The parts [`separate`](Self::separate) returns, in order.
    fn parts(&self) -> Vec<DrumPart>;

    /// Separates `mix` (at [`sample_rate`](Self::sample_rate)) into
    /// [`parts`](Self::parts), each as long as `mix`. `progress` is told how
    /// far along it is (0 to 1) and answers whether to go on; on `false`,
    /// stop and return [`SeparateError::Cancelled`].
    fn separate(
        &mut self,
        mix: &Stereo,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<Vec<(DrumPart, Stereo)>, SeparateError>;
}

/// A triangle
/// weight for position `i` of `n`, so a chunk counts least at its ends.
pub(crate) fn triangle(i: usize, n: usize) -> f32 {
    (i + 1).min(n - i) as f32
}

/// The chunk starts that cover `total` units with chunks of `size`, `stride`
/// apart, the last one ending where the signal does. A signal shorter than a
/// chunk has one, at 0.
pub(crate) fn chunk_starts(total: usize, size: usize, stride: usize) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut next = 0;
    loop {
        if next + size >= total {
            starts.push(total.saturating_sub(size));
            return starts;
        }
        starts.push(next);
        next += stride.max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_cover_the_signal_and_the_last_ends_with_it() {
        assert_eq!(chunk_starts(10, 4, 2), vec![0, 2, 4, 6]);
        assert_eq!(chunk_starts(11, 4, 2), vec![0, 2, 4, 6, 7]);
        assert_eq!(chunk_starts(3, 4, 2), vec![0]);
        assert_eq!(chunk_starts(4, 4, 2), vec![0]);
    }
}
