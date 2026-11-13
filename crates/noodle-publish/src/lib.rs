//! Builds the static site a sample-pack source serves (ADR 0012): one FLAC
//! object per sample, named by the hash of its decoded audio, a manifest per
//! pack and the `index.json` listing them.
//!
//! ```text
//! <out>/index.json
//! <out>/packs/<pack id>/manifest.json
//! <out>/objects/<hash>.flac
//! ```
//!
//! The input is a directory with one subdirectory per pack, each holding a
//! `pack.ron` and the audio files it names. This crate only writes files:
//! uploading the result to a bucket is the CI workflow's job, which is safe
//! to repeat because objects never change once written and the site is
//! uploaded objects first, `index.json` last.

use std::fs;
use std::path::{Path, PathBuf};

use flacenc::component::BitRepr;
use flacenc::error::Verify;
use noodle_io::{DecodeError, decode_file};
use noodle_nodes::Sample;
use noodle_packs::{Error as PackError, Index, Manifest, PackDef, Problem, is_hash, validate_dir};

/// Bits per sample in a published FLAC. 24 holds every sample a 16 or 24-bit
/// source file can, exactly.
const BITS: usize = 24;
const SCALE: f32 = (1 << (BITS - 1)) as f32;
/// FLAC's block size limits are 16 and 65535 frames.
const MIN_FRAMES: usize = 16;
const BLOCK: usize = 4096;

pub const INDEX: &str = "index.json";

/// The path of a pack's manifest, relative to the site root.
pub fn manifest_path(id: &str) -> String {
    format!("packs/{id}/manifest.json")
}

/// The path of a sample's object, relative to the site root.
pub fn object_path(hash: &str) -> String {
    format!("objects/{hash}.flac")
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {source}", path.display())]
    Pack {
        path: PathBuf,
        source: Box<PackError>,
    },
    #[error("{}: {}", path.display(), list(problems))]
    Invalid {
        path: PathBuf,
        problems: Vec<Problem>,
    },
    #[error("{}: could not read the audio: {source}", path.display())]
    Decode { path: PathBuf, source: DecodeError },
    #[error(
        "{}: the audio hashes to {actual}, but the pack says {expected} (sample {name:?})",
        path.display()
    )]
    HashMismatch {
        path: PathBuf,
        name: String,
        expected: String,
        actual: String,
    },
    #[error("{}: fewer than {MIN_FRAMES} frames of audio", path.display())]
    TooShort { path: PathBuf },
    #[error(
        "{}: has samples a {BITS}-bit FLAC cannot hold exactly; convert it to a 16 or {BITS}-bit file first",
        path.display()
    )]
    NotLossless { path: PathBuf },
    #[error("{}: could not encode FLAC: {message}", path.display())]
    Encode { path: PathBuf, message: String },
    #[error("{}: the FLAC decodes to a different hash ({actual}), so it was not published", path.display())]
    RoundTrip { path: PathBuf, actual: String },
    #[error("two packs are both called {id:?}: {} and {}", first.display(), second.display())]
    DuplicateId {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("{}: sample {name:?} names no file, so there is no audio to publish", path.display())]
    NoFile { path: PathBuf, name: String },
    #[error("{}: no packs found (expected <dir>/<pack>/pack.ron)", path.display())]
    NoPacks { path: PathBuf },
}

fn list(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> PublishError + '_ {
    move |source| PublishError::Io {
        path: path.to_owned(),
        source,
    }
}

/// A pack found on disk.
#[derive(Debug)]
pub struct FoundPack {
    pub def: PackDef,
    /// The directory holding `pack.ron` and the audio.
    pub dir: PathBuf,
}

/// Reads every `<dir>/<pack>/pack.ron`, sorted by pack id. Does not check
/// them; see [`check`].
pub fn find_packs(dir: &Path) -> Result<Vec<FoundPack>, PublishError> {
    let mut found: Vec<FoundPack> = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(io(dir))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("pack.ron").is_file())
        .collect();
    entries.sort();
    for pack_dir in entries {
        let file = pack_dir.join("pack.ron");
        let text = fs::read_to_string(&file).map_err(io(&file))?;
        let def = PackDef::from_ron(&text).map_err(|source| PublishError::Pack {
            path: file.clone(),
            source: Box::new(source),
        })?;
        if let Some(first) = found.iter().find(|f| f.def.id == def.id) {
            return Err(PublishError::DuplicateId {
                id: def.id,
                first: first.dir.join("pack.ron"),
                second: file,
            });
        }
        found.push(FoundPack { def, dir: pack_dir });
    }
    if found.is_empty() {
        return Err(PublishError::NoPacks {
            path: dir.to_owned(),
        });
    }
    found.sort_by(|a, b| a.def.id.cmp(&b.def.id));
    Ok(found)
}

/// Decodes a sample's file and checks it is the audio the pack names.
fn load(pack: &FoundPack, index: usize) -> Result<(PathBuf, Sample), PublishError> {
    let def = &pack.def.samples[index];
    let rel = def.file.as_deref().ok_or_else(|| PublishError::NoFile {
        path: pack.dir.join("pack.ron"),
        name: def.name.clone(),
    })?;
    let path = pack.dir.join(rel);
    let audio = decode_file(&path).map_err(|source| PublishError::Decode {
        path: path.clone(),
        source,
    })?;
    let sample = Sample::from_audio(&audio);
    let actual = sample.hash();
    if actual != def.hash {
        return Err(PublishError::HashMismatch {
            path,
            name: def.name.clone(),
            expected: def.hash.clone(),
            actual,
        });
    }
    Ok((path, sample))
}

/// Finds the packs under `dir` and checks everything a publish would: each
/// definition, that its files exist, and that each file's audio hashes to
/// what the definition says. Writes nothing.
pub fn check(dir: &Path) -> Result<Vec<FoundPack>, PublishError> {
    let packs = find_packs(dir)?;
    for pack in &packs {
        let problems = validate_dir(&pack.def, &pack.dir);
        if !problems.is_empty() {
            return Err(PublishError::Invalid {
                path: pack.dir.join("pack.ron"),
                problems,
            });
        }
        for i in 0..pack.def.samples.len() {
            load(pack, i)?;
        }
    }
    Ok(packs)
}

/// What a build did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub packs: usize,
    pub samples: usize,
    /// Objects encoded and written.
    pub written: usize,
    /// Objects that were already in the site and left alone.
    pub skipped: usize,
}

/// Builds the site for the packs under `packs_dir` into `out`. An object
/// already in `out` is left as it is, so building into a copy of the live
/// site writes only what is new; `index.json` is written last.
pub fn build(packs_dir: &Path, out: &Path) -> Result<Report, PublishError> {
    let packs = find_packs(packs_dir)?;
    for pack in &packs {
        let problems = validate_dir(&pack.def, &pack.dir);
        if !problems.is_empty() {
            return Err(PublishError::Invalid {
                path: pack.dir.join("pack.ron"),
                problems,
            });
        }
    }
    let objects = out.join("objects");
    fs::create_dir_all(&objects).map_err(io(&objects))?;

    let mut report = Report::default();
    let mut entries = Vec::new();
    for pack in &packs {
        let mut manifest = pack.def.manifest();
        for i in 0..pack.def.samples.len() {
            let hash = &pack.def.samples[i].hash;
            let object = out.join(object_path(hash));
            let (path, sample) = load(pack, i)?;
            if object.is_file() {
                report.skipped += 1;
            } else {
                write_object(&path, &sample, &object)?;
                report.written += 1;
            }
            manifest.samples[i].bytes = Some(fs::metadata(&object).map_err(io(&object))?.len());
            report.samples += 1;
        }
        let manifest_file = out.join(manifest_path(&pack.def.id));
        write_text(&manifest_file, &json(manifest.to_json(), &manifest_file)?)?;
        entries.push(manifest.index_entry(&manifest_path(&pack.def.id)));
        report.packs += 1;
    }
    let index = out.join(INDEX);
    write_text(&index, &json(Index::new(entries).to_json(), &index)?)?;
    Ok(report)
}

fn json(r: Result<String, PackError>, path: &Path) -> Result<String, PublishError> {
    r.map_err(|source| PublishError::Pack {
        path: path.to_owned(),
        source: Box::new(source),
    })
}

fn write_text(path: &Path, text: &str) -> Result<(), PublishError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io(parent))?;
    }
    let partial = path.with_extension("partial");
    fs::write(&partial, text).map_err(io(&partial))?;
    fs::rename(&partial, path).map_err(io(path))
}

/// Encodes `sample` as FLAC at `object`, after checking that decoding the
/// result gives the same hash. The name is taken only once it does.
fn write_object(source: &Path, sample: &Sample, object: &Path) -> Result<(), PublishError> {
    let fail = |message: String| PublishError::Encode {
        path: source.to_owned(),
        message,
    };
    let audio = sample.to_audio();
    if audio.samples.len() / audio.channels < MIN_FRAMES {
        return Err(PublishError::TooShort {
            path: source.to_owned(),
        });
    }
    let mut pcm = Vec::with_capacity(audio.samples.len());
    for &x in &audio.samples {
        let scaled = x * SCALE;
        let int = scaled.round();
        if scaled != int || !(-SCALE..SCALE).contains(&int) {
            return Err(PublishError::NotLossless {
                path: source.to_owned(),
            });
        }
        pcm.push(int as i32);
    }
    let frames = pcm.len() / audio.channels;
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| fail(e.to_string()))?;
    let src = flacenc::source::MemSource::from_samples(
        &pcm,
        audio.channels,
        BITS,
        audio.sample_rate as usize,
    );
    let block = BLOCK.min(frames);
    let mut stream = flacenc::encode_with_fixed_block_size(&config, src, block)
        .map_err(|e| fail(e.to_string()))?;
    // The encoder records a shorter last block as the smallest, which makes
    // Symphonia take the stream for a variable-block one and fail to open it.
    // The format counts every block but the last.
    stream
        .stream_info_mut()
        .set_block_sizes(block, block)
        .map_err(|e| fail(e.to_string()))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| fail(e.to_string()))?;

    let partial = object.with_extension("partial");
    fs::write(&partial, sink.as_slice()).map_err(io(&partial))?;
    let hash = object_hash(&partial).inspect_err(|_| {
        let _ = fs::remove_file(&partial);
    })?;
    if hash != sample.hash() {
        let _ = fs::remove_file(&partial);
        return Err(PublishError::RoundTrip {
            path: source.to_owned(),
            actual: hash,
        });
    }
    fs::rename(&partial, object).map_err(io(object))
}

fn object_hash(path: &Path) -> Result<String, PublishError> {
    let audio = decode_file(path).map_err(|source| PublishError::Decode {
        path: path.to_owned(),
        source,
    })?;
    Ok(Sample::from_audio(&audio).hash())
}

/// Checks a built site: every object each manifest names exists, decodes, and
/// hashes to its own name. Run it before uploading.
pub fn verify(site: &Path) -> Result<usize, PublishError> {
    let index_path = site.join(INDEX);
    let text = fs::read_to_string(&index_path).map_err(io(&index_path))?;
    let index = Index::from_json(&text).map_err(|source| PublishError::Pack {
        path: index_path.clone(),
        source: Box::new(source),
    })?;
    let mut checked = 0;
    for entry in &index.packs {
        let path = site.join(&entry.manifest);
        let text = fs::read_to_string(&path).map_err(io(&path))?;
        let manifest = Manifest::from_json(&text).map_err(|source| PublishError::Pack {
            path: path.clone(),
            source: Box::new(source),
        })?;
        for s in &manifest.samples {
            if !is_hash(&s.hash) {
                return Err(PublishError::Invalid {
                    path,
                    problems: vec![Problem {
                        sample: None,
                        kind: noodle_packs::ProblemKind::BadHash,
                    }],
                });
            }
            let object = site.join(object_path(&s.hash));
            let actual = object_hash(&object)?;
            if actual != s.hash {
                return Err(PublishError::HashMismatch {
                    path: object,
                    name: s.name.clone(),
                    expected: s.hash.clone(),
                    actual,
                });
            }
            checked += 1;
        }
    }
    Ok(checked)
}
