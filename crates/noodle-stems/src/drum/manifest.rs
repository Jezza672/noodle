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

/// LarsNet (Mezza, Giampiccolo, Bernardini, Sarti, Polimi ISPL): five
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
    credit: "Mezza, Giampiccolo, Bernardini and Sarti, Politecnico di Milano (ISPL)",
    source_url: "https://github.com/polimi-ispl/larsnet",
    sample_rate: 44_100,
    chunking: Chunking {
        segment_samples: 512 * 1024,
        stft_size: 4096,
        hop: 1024,
    },
    layout: Layout::PerPartSpectrogram(MaskOutput::Mask),
    files: &[
        ModelFile {
            part: Some(DrumPart::Kick),
            name: "kick.onnx",
            url: Some(
                "https://huggingface.co/jezza672/larsnet-onnx/resolve/dca039f93defe74448ff0d7d53f079270019cfb2/kick.onnx",
            ),
            sha256: Some("e7c994e9f191b7d66490bbc98561d368c8b86fcac8994582596dc028178c1fcf"),
            size: Some(39_346_035),
        },
        ModelFile {
            part: Some(DrumPart::Snare),
            name: "snare.onnx",
            url: Some(
                "https://huggingface.co/jezza672/larsnet-onnx/resolve/dca039f93defe74448ff0d7d53f079270019cfb2/snare.onnx",
            ),
            sha256: Some("89d28d42d83316fe9da367a95b21538ddb5a8fdab2382b57ecfe6cc1f94a5b27"),
            size: Some(39_346_035),
        },
        ModelFile {
            part: Some(DrumPart::Toms),
            name: "toms.onnx",
            url: Some(
                "https://huggingface.co/jezza672/larsnet-onnx/resolve/dca039f93defe74448ff0d7d53f079270019cfb2/toms.onnx",
            ),
            sha256: Some("3122013fdb5e303a97ec0f9c9b650b25670a8aab90a2877b3a50f9a4f34a9f6a"),
            size: Some(39_346_035),
        },
        ModelFile {
            part: Some(DrumPart::HiHat),
            name: "hihat.onnx",
            url: Some(
                "https://huggingface.co/jezza672/larsnet-onnx/resolve/dca039f93defe74448ff0d7d53f079270019cfb2/hihat.onnx",
            ),
            sha256: Some("cacb88ee7f58d057d0c050972ab2a230450571a7f61c326aa5a79581327e42dd"),
            size: Some(39_346_035),
        },
        ModelFile {
            part: Some(DrumPart::Cymbals),
            name: "cymbals.onnx",
            url: Some(
                "https://huggingface.co/jezza672/larsnet-onnx/resolve/dca039f93defe74448ff0d7d53f079270019cfb2/cymbals.onnx",
            ),
            sha256: Some("2eb483ea3eebb8f64d96a1a87fd00e2a537fefd699533308f9e398eef0b80173"),
            size: Some(39_346_035),
        },
    ],
    provenance: "Converted to ONNX by the Noodle project from the authors' PyTorch weights \
                 (unchanged), under their CC BY-NC 4.0. Not for commercial use.",
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
    layout: Layout::PerPartSpectrogram(MaskOutput::Mask),
    files: &[
        ModelFile {
            part: Some(DrumPart::Kick),
            name: "kick.onnx",
            url: Some(
                "https://huggingface.co/Faridzar/timbrica-drum-separator/resolve/3b3475f9b84c676d05441e38b16ed5deaa74d582/drums-kick-fp16.onnx",
            ),
            sha256: Some("f3dd5b5e3b23dafaa41d401c90595bb8ea7f121424c3b90885d751e3e96cb0c7"),
            size: Some(19_681_453),
        },
        ModelFile {
            part: Some(DrumPart::Snare),
            name: "snare.onnx",
            url: Some(
                "https://huggingface.co/Faridzar/timbrica-drum-separator/resolve/3b3475f9b84c676d05441e38b16ed5deaa74d582/drums-snare-r2-fp16.onnx",
            ),
            sha256: Some("fc1706e740ff17e4c7fd4c487016bafc53d7500cad874c829d69e54682061f4f"),
            size: Some(19_681_453),
        },
        ModelFile {
            part: Some(DrumPart::Toms),
            name: "toms.onnx",
            url: Some(
                "https://huggingface.co/Faridzar/timbrica-drum-separator/resolve/3b3475f9b84c676d05441e38b16ed5deaa74d582/drums-toms-fp16.onnx",
            ),
            sha256: Some("35bc5fe22f162cb3d8aa9b2b17f960006d5bc9b98b6452a944e7338f0afd360e"),
            size: Some(19_681_453),
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
        url: Some(
            "https://huggingface.co/jezza672/drumsep-onnx/resolve/206f4dbc9f55e8c6ccea54173d82d168fea26bd6/drumsep.onnx",
        ),
        sha256: Some("349e911e9c9739b6328f59c13cabac8e152e07fd1a614a272206486fa176b975"),
        size: Some(335_086_863),
    }],
    provenance: "splitzo's ONNX conversion, with four boolean EyeLike inputs changed to float so it \
                 loads in ONNX Runtime (no weights changed). The licence on the training data \
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
