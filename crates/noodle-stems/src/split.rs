//! Split to stems: separating an audio file and writing the parts as files.

use std::fmt;
use std::path::{Path, PathBuf};

use noodle_io::{Audio, decode_file, resample, write_wav};

use crate::{Part, SeparateError, Separator, Stem, Stereo};

/// What to split.
#[derive(Clone, Debug)]
pub struct SplitRequest {
    /// The audio file.
    pub source: PathBuf,
    /// How much of the file to separate, from its start, in its sample frames.
    /// The parts are this long, so a clip that shows a window of the file can
    /// show the same window of each part.
    pub frames: u64,
    /// The folder the parts are written to. It is made if it isn't there.
    pub out_dir: PathBuf,
    /// What the part files are called: `<name>-drums.wav` and so on, with a
    /// number added to the name if any of them exists already.
    pub name: String,
}

/// A part, written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StemFile {
    pub part: Part,
    /// The file's name in the folder.
    pub file: String,
    pub path: PathBuf,
}

/// What a split made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitOutput {
    /// The four stems and then the residual, in [`Part::ALL`] order.
    pub files: Vec<StemFile>,
    /// The sample rate of the files, which is the source's.
    pub sample_rate: u32,
    /// How many frames each file has.
    pub frames: u64,
}

#[derive(Debug)]
pub enum SplitError {
    Cancelled,
    Failed(String),
}

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SplitError::Cancelled => write!(f, "cancelled"),
            SplitError::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for SplitError {}

impl From<SeparateError> for SplitError {
    fn from(error: SeparateError) -> Self {
        match error {
            SeparateError::Cancelled => SplitError::Cancelled,
            SeparateError::Failed(why) => SplitError::Failed(why),
        }
    }
}

/// Separates the start of `request.source` with `separator` and writes the
/// stems and the residual as stereo float WAV files, at the source's sample
/// rate and as long as `request.frames`. `progress` is told how far along it is
/// (0 to 1) and answers whether to go on. A split that fails or is cancelled
/// leaves no files.
pub fn split(
    request: &SplitRequest,
    separator: &mut dyn Separator,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<SplitOutput, SplitError> {
    let fail =
        |what: &str, error: &dyn fmt::Display| SplitError::Failed(format!("{what}: {error}"));
    let frames =
        usize::try_from(request.frames).map_err(|_| SplitError::Failed("too long".into()))?;
    if frames == 0 {
        return Err(SplitError::Failed("there is no audio to split".into()));
    }
    if !progress(0.0) {
        return Err(SplitError::Cancelled);
    }

    let mut audio = decode_file(&request.source)
        .map_err(|e| fail(&format!("can't read {}", request.source.display()), &e))?;
    let rate = audio.sample_rate;
    audio.samples.truncate(frames * audio.channels);
    let stereo = Stereo::from_interleaved(&audio.samples, audio.channels);
    if stereo.is_empty() {
        return Err(SplitError::Failed("there is no audio to split".into()));
    }
    drop(audio);
    if !progress(0.05) {
        return Err(SplitError::Cancelled);
    }

    // The model's rate, there and back.
    let model_rate = separator.sample_rate();
    let mix = to_rate(&stereo, rate, model_rate).map_err(|e| fail("can't resample", &e))?;
    let stems = {
        let mut inner = |f: f32| progress(0.1 + 0.8 * f);
        separator.separate(&mix, &mut inner)?
    };
    drop(mix);
    if stems.iter().any(|s| s.len() != stems[0].len()) {
        return Err(SplitError::Failed(
            "the separator returned stems of different lengths".into(),
        ));
    }

    let mut parts: Vec<Stereo> = Vec::with_capacity(5);
    for stem in &stems {
        parts.push(fit(
            to_rate(stem, model_rate, rate).map_err(|e| fail("can't resample", &e))?,
            stereo.len(),
        ));
    }
    // The residual is made at the source's rate, from the stems as they are
    // written, so the five files add back up to the source.
    let mut residual = stereo.clone();
    for part in &parts {
        for (r, s) in residual.left.iter_mut().zip(&part.left) {
            *r -= s;
        }
        for (r, s) in residual.right.iter_mut().zip(&part.right) {
            *r -= s;
        }
    }
    parts.push(residual);
    if !progress(0.92) {
        return Err(SplitError::Cancelled);
    }

    std::fs::create_dir_all(&request.out_dir)
        .map_err(|e| fail(&format!("can't make {}", request.out_dir.display()), &e))?;
    let name = free_name(&request.out_dir, &request.name);
    let mut files = Vec::new();
    let mut written = Vec::new();
    for (i, (part, audio)) in Part::ALL.iter().zip(&parts).enumerate() {
        let file = format!("{name}-{}.wav", part.label());
        let path = request.out_dir.join(&file);
        let result = write_wav(&path, &audio.to_interleaved(), 2, rate);
        written.push(path.clone());
        if let Err(error) = result {
            remove_all(&written);
            return Err(fail(&format!("can't write {}", path.display()), &error));
        }
        files.push(StemFile {
            part: *part,
            file,
            path,
        });
        if !progress(0.92 + 0.08 * (i + 1) as f32 / 5.0) {
            remove_all(&written);
            return Err(SplitError::Cancelled);
        }
    }
    Ok(SplitOutput {
        files,
        sample_rate: rate,
        frames: stereo.len() as u64,
    })
}

fn remove_all(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

/// `name`, with `-2`, `-3`, … added until none of its part files exist.
fn free_name(dir: &Path, name: &str) -> String {
    let taken = |candidate: &str| {
        Part::ALL.iter().any(|part| {
            dir.join(format!("{candidate}-{}.wav", part.label()))
                .exists()
        })
    };
    if !taken(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| format!("{name}-{n}"))
        .find(|candidate| !taken(candidate))
        .expect("there are only so many files")
}

fn to_rate(audio: &Stereo, from: u32, to: u32) -> Result<Stereo, noodle_io::ResampleError> {
    if from == to {
        return Ok(audio.clone());
    }
    let resampled = resample(
        &Audio {
            samples: audio.to_interleaved(),
            channels: 2,
            sample_rate: from,
        },
        to,
    )?;
    Ok(Stereo::from_interleaved(&resampled.samples, 2))
}

/// `audio` cut or padded with silence to `frames`.
fn fit(mut audio: Stereo, frames: usize) -> Stereo {
    audio.left.resize(frames, 0.0);
    audio.right.resize(frames, 0.0);
    audio
}

// Keeps the stem order the parts are written in tied to the separator's.
const _: () = assert!(Stem::ALL.len() + 1 == Part::ALL.len());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeSeparator;

    /// A tone with a low part, a middle part and a high part.
    fn mixture(frames: usize, rate: u32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f32 / rate as f32;
                let tone = |hz: f32| (std::f32::consts::TAU * hz * t).sin();
                let left = 0.3 * tone(60.0) + 0.2 * tone(700.0) + 0.1 * tone(6_000.0);
                [left, -left * 0.5]
            })
            .collect()
    }

    fn request(dir: &Path, source: PathBuf, frames: u64) -> SplitRequest {
        SplitRequest {
            source,
            frames,
            out_dir: dir.join("stems"),
            name: "song".into(),
        }
    }

    fn sum(files: &[StemFile]) -> Vec<f32> {
        let mut total: Vec<f32> = Vec::new();
        for file in files {
            let audio = noodle_io::read_wav(&file.path).unwrap();
            assert_eq!(audio.channels, 2);
            total.resize(audio.samples.len(), 0.0);
            for (t, s) in total.iter_mut().zip(&audio.samples) {
                *t += s;
            }
        }
        total
    }

    #[test]
    fn five_files_add_back_up_to_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.wav");
        let samples = mixture(20_000, 44_100);
        write_wav(&source, &samples, 2, 44_100).unwrap();
        let out = split(
            &request(dir.path(), source, 20_000),
            &mut FakeSeparator::new(),
            &mut |_| true,
        )
        .unwrap();
        assert_eq!(out.files.len(), 5);
        let labels: Vec<&str> = out.files.iter().map(|f| f.part.label()).collect();
        assert_eq!(labels, ["drums", "bass", "vocals", "other", "residual"]);
        assert_eq!(out.files[0].file, "song-drums.wav");
        assert_eq!((out.sample_rate, out.frames), (44_100, 20_000));
        let total = sum(&out.files);
        assert_eq!(total.len(), samples.len());
        for (i, (t, s)) in total.iter().zip(&samples).enumerate() {
            assert!((t - s).abs() < 1e-4, "sample {i}: {t} against {s}");
        }
        // The residual is real, so the sum isn't the stems alone.
        let residual = noodle_io::read_wav(&out.files[4].path).unwrap();
        assert!(residual.samples.iter().any(|s| s.abs() > 1e-3));
    }

    #[test]
    fn a_source_at_another_rate_keeps_its_rate_and_length() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.wav");
        write_wav(&source, &mixture(30_000, 48_000), 2, 48_000).unwrap();
        // Only the first 24 000 frames, and a separator at 32 kHz.
        let out = split(
            &request(dir.path(), source, 24_000),
            &mut FakeSeparator::at_rate(32_000),
            &mut |_| true,
        )
        .unwrap();
        assert_eq!((out.sample_rate, out.frames), (48_000, 24_000));
        for file in &out.files {
            let audio = noodle_io::read_wav(&file.path).unwrap();
            assert_eq!(audio.sample_rate, 48_000);
            assert_eq!(audio.samples.len(), 24_000 * 2);
        }
    }

    #[test]
    fn mono_sources_are_split_as_stereo() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("mono.wav");
        let mono: Vec<f32> = (0..10_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        write_wav(&source, &mono, 1, 44_100).unwrap();
        let out = split(
            &request(dir.path(), source, 10_000),
            &mut FakeSeparator::new(),
            &mut |_| true,
        )
        .unwrap();
        let total = sum(&out.files);
        for (i, &m) in mono.iter().enumerate() {
            assert!((total[i * 2] - m).abs() < 1e-4);
            assert!((total[i * 2 + 1] - m).abs() < 1e-4);
        }
    }

    #[test]
    fn a_second_split_of_the_same_name_does_not_overwrite_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.wav");
        write_wav(&source, &mixture(5_000, 44_100), 2, 44_100).unwrap();
        let req = request(dir.path(), source, 5_000);
        let first = split(&req, &mut FakeSeparator::new(), &mut |_| true).unwrap();
        let second = split(&req, &mut FakeSeparator::new(), &mut |_| true).unwrap();
        assert_eq!(first.files[0].file, "song-drums.wav");
        assert_eq!(second.files[0].file, "song-2-drums.wav");
        assert!(first.files[0].path.exists() && second.files[0].path.exists());
    }

    #[test]
    fn a_cancelled_split_leaves_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.wav");
        write_wav(&source, &mixture(5_000, 44_100), 2, 44_100).unwrap();
        let req = request(dir.path(), source, 5_000);
        // Cancel at each point in turn, the writes included.
        for stop_after in 0..14 {
            let mut calls = 0;
            let result = split(&req, &mut FakeSeparator::new(), &mut |_| {
                calls += 1;
                calls <= stop_after
            });
            if let Ok(out) = result {
                for file in out.files {
                    std::fs::remove_file(file.path).unwrap();
                }
                continue;
            }
            let left: Vec<_> = std::fs::read_dir(dir.path().join("stems"))
                .map(|d| d.map(|e| e.unwrap().path()).collect())
                .unwrap_or_default();
            assert!(left.is_empty(), "after {stop_after} calls: {left:?}");
        }
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let req = request(dir.path(), dir.path().join("nope.wav"), 100);
        let error = split(&req, &mut FakeSeparator::new(), &mut |_| true).unwrap_err();
        assert!(matches!(error, SplitError::Failed(_)), "{error}");
    }
}
