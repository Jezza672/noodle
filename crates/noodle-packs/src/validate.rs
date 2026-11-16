use std::collections::HashMap;
use std::path::Path;

use crate::{
    FORMAT_VERSION, ItemFault, Licence, PackDef, Problem, ProblemKind, SPDX_IDS, is_hash,
    is_pack_id,
};

/// Judged on the string, not the host's `Path`, because the format is
/// cross-platform: `/` is the only separator, and `\` or `:` could climb out
/// of the directory on Windows.
fn is_safe_path(file: &str) -> bool {
    file.split('/')
        .all(|seg| !matches!(seg, "" | "." | "..") && !seg.contains(['\\', ':']))
}

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
    if pack.samples.is_empty() && pack.items.is_empty() {
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
            let safe = is_safe_path(file);
            if !safe {
                push(Some(i), ProblemKind::UnsafePath { file: file.clone() });
            }
        }
    }
    validate_items(pack, &hashes, &mut out);
    out
}

fn validate_items(pack: &PackDef, samples: &HashMap<&str, usize>, out: &mut Vec<Problem>) {
    let mut hashes: HashMap<&str, usize> = HashMap::new();
    let mut names: HashMap<&str, usize> = HashMap::new();
    for (index, item) in pack.items.iter().enumerate() {
        let mut push = |fault| {
            out.push(Problem {
                sample: None,
                kind: ProblemKind::Item { index, fault },
            })
        };
        if !is_hash(&item.hash) {
            push(ItemFault::BadHash);
        } else if let Some(&first) = hashes.get(item.hash.as_str()) {
            push(ItemFault::DuplicateHash { first });
        } else {
            hashes.insert(&item.hash, index);
        }
        if item.name.trim().is_empty() {
            push(ItemFault::EmptyName);
        } else if let Some(&first) = names.get(item.name.as_str()) {
            push(ItemFault::DuplicateName { first });
        } else {
            names.insert(&item.name, index);
        }
        if item.tags.iter().any(|t| t.trim().is_empty()) {
            push(ItemFault::EmptyTag);
        }
        if pack.item_author(item).trim().is_empty() {
            push(ItemFault::MissingAuthor);
        }
        let licence = pack.item_licence(item);
        if item.licence.is_some() {
            match licence {
                Licence::Spdx(id) if !SPDX_IDS.contains(&id.as_str()) => {
                    push(ItemFault::UnknownSpdx { id: id.clone() })
                }
                Licence::Custom(t) if t.trim().is_empty() => push(ItemFault::EmptyCustomLicence),
                Licence::Unknown => push(ItemFault::UnknownLicence),
                _ => {}
            }
        }
        if licence.requires_credit() && item.credit.as_deref().is_none_or(|c| c.trim().is_empty())
        {
            push(ItemFault::MissingCredit);
        }
        if let Some(file) = &item.file
            && !is_safe_path(file)
        {
            push(ItemFault::UnsafePath { file: file.clone() });
        }
        for need in &item.needs {
            if !is_hash(need) {
                push(ItemFault::BadNeed { hash: need.clone() });
            } else if !samples.contains_key(need.as_str()) {
                push(ItemFault::NeedNotInPack { hash: need.clone() });
            }
        }
    }
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
    for (index, item) in pack.items.iter().enumerate() {
        let Some(file) = &item.file else {
            // Without a file there is nothing to publish from this directory.
            out.push(Problem {
                sample: None,
                kind: ProblemKind::Item {
                    index,
                    fault: ItemFault::NoFile,
                },
            });
            continue;
        };
        let unsafe_path = out.iter().any(|p| {
            matches!(&p.kind, ProblemKind::Item { index: i, fault: ItemFault::UnsafePath { .. } } if *i == index)
        });
        if !unsafe_path && !dir.join(file).is_file() {
            out.push(Problem {
                sample: None,
                kind: ProblemKind::Item {
                    index,
                    fault: ItemFault::MissingFile { file: file.clone() },
                },
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
        Licence::Unknown => push(sample, ProblemKind::UnknownLicence),
        _ => {}
    }
}
