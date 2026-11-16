use std::fmt;

/// A file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not valid RON: {0}")]
    Ron(#[from] ron::error::SpannedError),
    #[error("could not write RON: {0}")]
    RonWrite(#[from] ron::Error),
    #[error("not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("format version {found} is not supported (this reader knows {supported})")]
    UnsupportedFormat { found: u32, supported: u32 },
}

/// One thing wrong with a pack definition. Structured so a CLI, CI and the
/// wizard can each present it their own way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The offending sample's index in the pack, or `None` for the pack itself.
    pub sample: Option<usize>,
    pub kind: ProblemKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemKind {
    UnsupportedFormat {
        found: u32,
    },
    BadPackId,
    EmptyPackName,
    NoSamples,
    /// A fault in the pack's `index`th library entry.
    Item {
        index: usize,
        fault: ItemFault,
    },
    BadHash,
    DuplicateHash {
        first: usize,
    },
    EmptyName,
    DuplicateName {
        first: usize,
    },
    EmptyTag,
    /// The sample (after the pack's default) has no author.
    MissingAuthor,
    /// The licence requires attribution and the sample has no credit text.
    MissingCredit,
    EmptyCustomLicence,
    /// A licence of `unknown`, which only a folder on disk may have.
    UnknownLicence,
    UnknownSpdx {
        id: String,
    },
    /// A `file` that is absolute or climbs out of the pack directory.
    UnsafePath {
        file: String,
    },
    /// A `file` that does not exist in the directory the pack was checked against.
    MissingFile {
        file: String,
    },
}

/// What is wrong with a library entry of a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemFault {
    BadHash,
    DuplicateHash { first: usize },
    EmptyName,
    DuplicateName { first: usize },
    EmptyTag,
    MissingAuthor,
    MissingCredit,
    EmptyCustomLicence,
    UnknownLicence,
    UnknownSpdx { id: String },
    /// No `file`, so there is nothing to publish.
    NoFile,
    UnsafePath { file: String },
    MissingFile { file: String },
    /// A sample the entry needs that is not a hash.
    BadNeed { hash: String },
    /// A sample the entry needs that the pack does not hold.
    NeedNotInPack { hash: String },
}

impl fmt::Display for ItemFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemFault::BadHash => write!(f, "hash must be 64 lowercase hex digits"),
            ItemFault::DuplicateHash { first } => write!(f, "same hash as item {first}"),
            ItemFault::EmptyName => write!(f, "name is empty"),
            ItemFault::DuplicateName { first } => write!(f, "same name as item {first}"),
            ItemFault::EmptyTag => write!(f, "empty tag"),
            ItemFault::MissingAuthor => write!(f, "no author (set one on the item or the pack)"),
            ItemFault::MissingCredit => {
                write!(f, "the licence requires a credit and there is none")
            }
            ItemFault::EmptyCustomLicence => write!(f, "custom licence text is empty"),
            ItemFault::UnknownLicence => write!(f, "the licence is unknown, and a pack must say"),
            ItemFault::UnknownSpdx { id } => write!(f, "unknown SPDX id {id:?}"),
            ItemFault::NoFile => write!(f, "names no file, so there is nothing to publish"),
            ItemFault::UnsafePath { file } => {
                write!(f, "file {file:?} must be a relative path inside the pack")
            }
            ItemFault::MissingFile { file } => write!(f, "file {file:?} does not exist"),
            ItemFault::BadNeed { hash } => write!(f, "needs {hash:?}, which is not a sample hash"),
            ItemFault::NeedNotInPack { hash } => {
                write!(f, "needs sample {hash}, which is not in the pack")
            }
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(i) = self.sample {
            write!(f, "sample {i}: ")?;
        }
        match &self.kind {
            ProblemKind::UnsupportedFormat { found } => {
                write!(f, "format version {found} is not supported")
            }
            ProblemKind::BadPackId => {
                write!(f, "pack id must be lowercase letters, digits and hyphens")
            }
            ProblemKind::EmptyPackName => write!(f, "pack name is empty"),
            ProblemKind::NoSamples => write!(f, "pack has no samples or library entries"),
            ProblemKind::Item { index, fault } => write!(f, "item {index}: {fault}"),
            ProblemKind::BadHash => write!(f, "hash must be 64 lowercase hex digits"),
            ProblemKind::DuplicateHash { first } => write!(f, "same hash as sample {first}"),
            ProblemKind::EmptyName => write!(f, "name is empty"),
            ProblemKind::DuplicateName { first } => write!(f, "same name as sample {first}"),
            ProblemKind::EmptyTag => write!(f, "empty tag"),
            ProblemKind::MissingAuthor => {
                write!(f, "no author (set one on the sample or the pack)")
            }
            ProblemKind::MissingCredit => {
                write!(f, "the licence requires a credit and there is none")
            }
            ProblemKind::EmptyCustomLicence => write!(f, "custom licence text is empty"),
            ProblemKind::UnknownLicence => write!(f, "the licence is unknown, and a pack must say"),
            ProblemKind::UnknownSpdx { id } => write!(f, "unknown SPDX id {id:?}"),
            ProblemKind::UnsafePath { file } => {
                write!(f, "file {file:?} must be a relative path inside the pack")
            }
            ProblemKind::MissingFile { file } => write!(f, "file {file:?} does not exist"),
        }
    }
}
