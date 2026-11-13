//! Sample packs (ADR 0012): the RON pack definition that lives in git, the
//! manifest and `index.json` a source publishes, licences, and the validator
//! shared by CI, the CLI and the GUI wizard.
//!
//! This crate does no network I/O and reads no audio. Hashes are of the
//! *decoded* audio (`Sample::hash()`), so checking them against files is the
//! publisher's job; the validator checks only that they are well formed.

mod error;
mod licence;
mod pack;
mod published;
mod validate;

pub use error::{Error, Problem, ProblemKind};
pub use licence::{Licence, SPDX_IDS};
pub use pack::{PackDef, SampleDef};
pub use published::{Index, IndexEntry, Manifest, ManifestSample};
pub use validate::{validate, validate_dir};

/// The version of every file format in this crate. Bump it on any change a
/// hosted source would notice; readers refuse versions they do not know.
pub const FORMAT_VERSION: u32 = 1;

/// Whether `s` is a sample hash: 64 lowercase hex digits.
pub fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `s` is a pack id: lowercase letters, digits and single hyphens.
pub fn is_pack_id(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

fn check_format(format: u32) -> Result<(), Error> {
    if format == FORMAT_VERSION {
        Ok(())
    } else {
        Err(Error::UnsupportedFormat {
            found: format,
            supported: FORMAT_VERSION,
        })
    }
}
