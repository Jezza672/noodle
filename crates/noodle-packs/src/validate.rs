use std::collections::HashMap;
use std::path::{Component, Path};

use crate::{
    FORMAT_VERSION, Licence, PackDef, Problem, ProblemKind, SPDX_IDS, is_hash, is_pack_id,
};

/// Check a pack definition on its own. Returns every problem found, not just
/// the first; an empty list means the pack is valid.
pub fn validate(pack: &PackDef) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut push = |sample: Option<usize>, kind| out.push(Problem { sample, kind });

    if pack.format != FORMAT_VERSION {
        push(None, ProblemKind::UnsupportedFormat { found: pack.format });
    }
    if !is_pack_id(&pack.id) {
        push(None, ProblemKind::BadPackId);
    }
    if pack.name.trim().is_empty() {
        push(None, ProblemKind::EmptyPackName);
    }
    if pack.samples.is_empty() {
        push(None, ProblemKind::NoSamples);
    }
    check_licence(&pack.licence, None, &mut push);

    let mut hashes: HashMap<&str, usize> = HashMap::new();
    let mut names: HashMap<&str, usize> = HashMap::new();
    for (i, s) in pack.samples.iter().enumerate() {
        if !is_hash(&s.hash) {
            push(Some(i), ProblemKind::BadHash);
        } else if let Some(&first) = hashes.get(s.hash.as_str()) {
            push(Some(i), ProblemKind::DuplicateHash { first });
        } else {
            hashes.insert(&s.hash, i);
        }
        if s.name.trim().is_empty() {
            push(Some(i), ProblemKind::EmptyName);
        } else if let Some(&first) = names.get(s.name.as_str()) {
            push(Some(i), ProblemKind::DuplicateName { first });
        } else {
            names.insert(&s.name, i);
        }
        if s.tags.iter().any(|t| t.trim().is_empty()) {
            push(Some(i), ProblemKind::EmptyTag);
        }
        if pack.author_of(s).trim().is_empty() {
            push(Some(i), ProblemKind::MissingAuthor);
        }
        let licence = pack.licence_of(s);
        if s.licence.is_some() {
            check_licence(licence, Some(i), &mut push);
        }
        if licence.requires_credit() && s.credit.as_deref().is_none_or(|c| c.trim().is_empty()) {
            push(Some(i), ProblemKind::MissingCredit);
        }
        if let Some(file) = &s.file {
            let p = Path::new(file);
            let safe =
                !file.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)));
            if !safe {
                push(Some(i), ProblemKind::UnsafePath { file: file.clone() });
            }
        }
    }
    out
}

/// [`validate`], plus a check that every sample's `file` exists under `dir`.
pub fn validate_dir(pack: &PackDef, dir: &Path) -> Vec<Problem> {
    let mut out = validate(pack);
    for (i, s) in pack.samples.iter().enumerate() {
        let Some(file) = &s.file else { continue };
        let unsafe_path = out
            .iter()
            .any(|p| p.sample == Some(i) && matches!(p.kind, ProblemKind::UnsafePath { .. }));
        if !unsafe_path && !dir.join(file).is_file() {
            out.push(Problem {
                sample: Some(i),
                kind: ProblemKind::MissingFile { file: file.clone() },
            });
        }
    }
    out
}

fn check_licence(
    l: &Licence,
    sample: Option<usize>,
    push: &mut impl FnMut(Option<usize>, ProblemKind),
) {
    match l {
        Licence::Spdx(id) if !SPDX_IDS.contains(&id.as_str()) => {
            push(sample, ProblemKind::UnknownSpdx { id: id.clone() })
        }
        Licence::Custom(t) if t.trim().is_empty() => push(sample, ProblemKind::EmptyCustomLicence),
        _ => {}
    }
}
