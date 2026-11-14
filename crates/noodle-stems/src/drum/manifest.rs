//! What each supported model is: parts, licence, source, chunking.
//!
//! These are facts about the weights, written down once, so the downloader,
//! the settings tab, the first-use popup and the runtimes read the same thing.

use crate::drum::DrumPart;

/// How a model's STFT and segments are laid out. The STFT sits outside the
/// ONNX graph, so the app has to match these exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunking {
    /// Samples the model takes at once (at the model's rate).
    pub segment_samples: usize,
    pub stft_size: usize,
    pub hop: usize,
}

impl Chunking {
    /// Frequency bins the model sees: the STFT's, without the Nyquist bin.
    pub fn bins(&self) -> usize {
        self.stft_size / 2
    }

    /// Spectrogram frames in a segment.
    pub fn frames(&self) -> usize {
        self.segment_samples / self.hop
    }
}

/// What a mask-style network returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskOutput {
    /// An estimate of the part's own magnitude spectrogram.
    Magnitude,
    /// A gain (0 to 1) to multiply the mix's spectrogram by.
    Mask,
}

/// How a model is built, and so which runtime opens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// One network per part, each taking the mix's magnitude spectrogram
    /// `[1, 2, bins, frames]` and returning the same shape. LarsNet and
    /// Timbrica are U-Nets of this kind.
    PerPartSpectrogram(MaskOutput),
    /// One Hybrid Demucs network: a waveform `[1, 2, T]` and a complex-as-
    /// channels spectrogram `[1, 4, bins, frames]` in; the spectrogram branch
    /// `[1, parts, 4, bins, frames]` and the time branch `[1, parts, 2, T]` out,
    /// summed after the inverse STFT. drumsep.
    HybridDemucs,
}

/// One file of a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelFile {
    /// The part this file makes, for per-part models.
    pub part: Option<DrumPart>,
    /// Where the file is kept inside the model's folder.
    pub name: &'static str,
    /// Where to download it. `None` until the address is published.
    pub url: Option<&'static str>,
    /// The SHA-256 of the file (lowercase hex). `None` until it is known;
    /// nothing is downloaded without one.
    pub sha256: Option<&'static str>,
    /// Bytes, when known.
    pub size: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrumModelManifest {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    /// What it returns, in order.
    pub parts: &'static [DrumPart],
    /// Drum parts it cannot give on their own.
    pub lacks: &'static [DrumPart],
    pub licence: &'static str,
    /// The licence bars commercial use: the download says so.
    pub non_commercial: bool,
    /// The authors, credited wherever the model is offered.
    pub credit: &'static str,
    /// Where people read about it.
    pub source_url: &'static str,
    pub sample_rate: u32,
    pub chunking: Chunking,
    pub layout: Layout,
    pub files: &'static [ModelFile],
    /// A line for the download dialog on where the weights come from.
    pub provenance: &'static str,
}

impl DrumModelManifest {
    /// Total bytes to download, when every file's size is known.
    pub fn size(&self) -> Option<u64> {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Whether every file has an address and a checksum, so it can be
    /// downloaded.
    pub fn is_published(&self) -> bool {
        self.files
            .iter()
            .all(|f| f.url.is_some() && f.sha256.is_some())
    }
}

/// LarsNet (Mezza, Giampiccolo, Pezzoli, Bernardini, Polimi ISPL): five
/// parts. The authors publish a PyTorch zip, which the app cannot run, so the
/// weights here are our own ONNX conversion, hosted as a separate download
/// (never in the repository or the binary), credited, and under the authors'
/// CC BY-NC 4.0.
pub const LARSNET: DrumModelManifest = DrumModelManifest {
    id: "larsnet",
    name: "LarsNet",
    version: "1",
    parts: &[
        DrumPart::Kick,
        DrumPart::Snare,
        DrumPart::Toms,
        DrumPart::HiHat,
        DrumPart::Cymbals,
    ],
    lacks: &[],
    licence: "CC BY-NC 4.0",
    non_commercial: true,
    credit: "Mezza, Giampiccolo, Pezzoli and Bernardini, Politecnico di Milano (ISPL)",
    source_url: "https://github.com/polimi-ispl/larsnet",
    sample_rate: 44_100,
    chunking: Chunking {
        segment_samples: 512 * 1024,
        stft_size: 4096,
        hop: 1024,
    },
    layout: Layout::PerPartSpectrogram(MaskOutput::Magnitude),
    files: &[
        ModelFile {
            part: Some(DrumPart::Kick),
            name: "kick.onnx",
            url: None,
            sha256: None,
            size: None,
        },
        ModelFile {
            part: Some(DrumPart::Snare),
            name: "snare.onnx",
            url: None,
            sha256: None,
            size: None,
        },
        ModelFile {
            part: Some(DrumPart::Toms),
            name: "toms.onnx",
            url: None,
            sha256: None,
            size: None,
        },
        ModelFile {
            part: Some(DrumPart::HiHat),
            name: "hihat.onnx",
            url: None,
            sha256: None,
            size: None,
        },
        ModelFile {
            part: Some(DrumPart::Cymbals),
            name: "cymbals.onnx",
            url: None,
            sha256: None,
            size: None,
        },
    ],
    provenance: "Converted to ONNX by the Noodle project from the authors' PyTorch weights, \
                 under their CC BY-NC 4.0. Not for commercial use.",
};

/// Timbrica (Faridzar): kick, snare and toms, three ONNX fp16 models of the
/// LarsNet U-Net; hi-hat and cymbals are the remainder.
pub const TIMBRICA: DrumModelManifest = DrumModelManifest {
    id: "timbrica",
    name: "Timbrica",
    version: "1",
    parts: &[
        DrumPart::Kick,
        DrumPart::Snare,
        DrumPart::Toms,
        DrumPart::Rest,
    ],
    lacks: &[DrumPart::HiHat, DrumPart::Cymbals],
    licence: "CC BY-NC-SA 4.0",
    non_commercial: true,
    credit: "Faridzar (Timbrica)",
    source_url: "https://huggingface.co/Faridzar/timbrica-drum-separator",
    sample_rate: 44_100,
    chunking: Chunking {
        segment_samples: 512 * 1024,
        stft_size: 4096,
        hop: 1024,
    },
    layout: Layout::PerPartSpectrogram(MaskOutput::Magnitude),
    files: &[
        ModelFile {
            part: Some(DrumPart::Kick),
            name: "kick.onnx",
            url: None,
            sha256: None,
            size: None,
        },
        ModelFile {
            part: Some(DrumPart::Snare),
            name: "snare.onnx",
            url: None,
            sha256: None,
            size: None,
        },
        ModelFile {
            part: Some(DrumPart::Toms),
            name: "toms.onnx",
            url: None,
            sha256: None,
            size: None,
        },
    ],
    provenance: "Downloaded from the author's Hugging Face repository.",
};

/// drumsep (inagoy): a Hybrid Demucs fine-tune giving kick, snare, cymbals and
/// toms; no hi-hat of its own. The only MIT one.
pub const DRUMSEP: DrumModelManifest = DrumModelManifest {
    id: "drumsep",
    name: "drumsep",
    version: "1",
    parts: &[
        DrumPart::Kick,
        DrumPart::Snare,
        DrumPart::Cymbals,
        DrumPart::Toms,
    ],
    lacks: &[DrumPart::HiHat],
    licence: "MIT",
    non_commercial: false,
    credit: "inagoy (drumsep), ONNX conversion by splitzo",
    source_url: "https://github.com/inagoy/drumsep",
    sample_rate: 44_100,
    chunking: Chunking {
        segment_samples: 40 * 44_100,
        stft_size: 4096,
        hop: 1024,
    },
    layout: Layout::HybridDemucs,
    files: &[ModelFile {
        part: None,
        name: "drumsep.onnx",
        url: None,
        sha256: None,
        size: None,
    }],
    provenance: "ONNX conversion by splitzo on Hugging Face. The licence on the training data \
                 is not stated by the author.",
};

/// Every supported model, in the order the app lists them.
pub const ALL_MODELS: [&DrumModelManifest; 3] = [&LARSNET, &TIMBRICA, &DRUMSEP];

pub fn manifest_by_id(id: &str) -> Option<&'static DrumModelManifest> {
    ALL_MODELS.iter().copied().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_are_listed_in_the_agreed_order() {
        let ids: Vec<_> = ALL_MODELS.iter().map(|m| m.id).collect();
        assert_eq!(ids, ["larsnet", "timbrica", "drumsep"]);
    }

    #[test]
    fn each_manifest_is_consistent() {
        for m in ALL_MODELS {
            assert_eq!(manifest_by_id(m.id), Some(m));
            // A lacked part is never also returned.
            for lacked in m.lacks {
                assert!(!m.parts.contains(lacked), "{}: {lacked}", m.id);
            }
            // Per-part models have one file per part, in order, except the
            // remainder, which is computed.
            match m.layout {
                Layout::PerPartSpectrogram(_) => {
                    let made: Vec<_> = m.files.iter().map(|f| f.part.unwrap()).collect();
                    let listed: Vec<_> = m
                        .parts
                        .iter()
                        .copied()
                        .filter(|p| *p != DrumPart::Rest)
                        .collect();
                    assert_eq!(made, listed, "{}", m.id);
                }
                Layout::HybridDemucs => assert_eq!(m.files.len(), 1),
            }
            if matches!(m.layout, Layout::PerPartSpectrogram(_)) {
                assert_eq!(m.chunking.segment_samples % m.chunking.hop, 0);
            }
            assert!(m.chunking.stft_size.is_power_of_two());
            assert!(!m.credit.is_empty() && !m.licence.is_empty());
            let names: std::collections::HashSet<_> = m.files.iter().map(|f| f.name).collect();
            assert_eq!(names.len(), m.files.len(), "{}: file names", m.id);
        }
    }

    #[test]
    fn the_non_commercial_flags_follow_the_licences() {
        for m in ALL_MODELS {
            assert_eq!(m.non_commercial, m.licence.contains("NC"), "{}", m.id);
        }
        assert_eq!(DRUMSEP.licence, "MIT");
    }

    #[test]
    fn what_each_model_lacks_is_stated() {
        assert!(LARSNET.lacks.is_empty());
        assert_eq!(TIMBRICA.lacks, [DrumPart::HiHat, DrumPart::Cymbals]);
        assert_eq!(DRUMSEP.lacks, [DrumPart::HiHat]);
    }
}
