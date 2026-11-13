//! Exporting the project to an audio file, on a background thread.

use super::*;
use noodle_core::ExportTail;
use noodle_engine::{Job, JobPanicked};
use noodle_io::ExportFormat;
use noodle_io::Loudness;
use noodle_nodes::{
    ExportJobError, ExportReport, ExportRequest, RenderRequest, TrackExport, spawn_export_all,
};

/// What the user chose in the export dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportChoice {
    pub format: ExportFormat,
    /// The speakers the file is for: stereo, or a surround layout, whose
    /// channels the Output nodes' signals fill in WAV order.
    pub layout: noodle_engine::ChannelLayout,
    /// The part to write, from and to in seconds; `None` is the whole
    /// project, to the end of the last clip. Either way the project's export
    /// tail (see [`noodle_core::ExportSettings`]) follows.
    pub range: Option<(f64, f64)>,
    /// Whether to write the mixdown: the project as it plays, master
    /// processing included.
    pub mixdown: bool,
    /// The groups to write a file for each, taken after the group's gain and
    /// mute and before what it feeds. Tracks and buses alike.
    pub groups: Vec<NodeId>,
    /// Brings the mixdown to this loudness in LUFS, by one gain on every
    /// file.
    pub normalize: Option<f64>,
    /// The mix versions to write, by place in the project's list: each is
    /// rendered as its own pass and named `<song> - <version>`. Empty writes
    /// the project as it is, with whichever version is active.
    pub versions: Vec<usize>,
    /// Holds the mixdown's true peak under this many dBTP, after the
    /// normalising gain.
    pub true_peak_ceiling: Option<f64>,
    /// Title, artist and ISRC for the mixdown file; the ISRC may have dashes
    /// and is checked when the export starts.
    pub title: String,
    pub artist: String,
    pub isrc: String,
    /// Write a CUE sheet next to the mixdown file.
    pub cue: bool,
}

#[cfg(test)]
impl ExportChoice {
    /// Just the mixdown, as it is.
    pub fn mixdown(
        format: ExportFormat,
        layout: noodle_engine::ChannelLayout,
        range: Option<(f64, f64)>,
    ) -> Self {
        Self {
            format,
            layout,
            range,
            mixdown: true,
            groups: Vec::new(),
            normalize: None,
            versions: Vec::new(),
            true_peak_ceiling: None,
            title: String::new(),
            artist: String::new(),
            isrc: String::new(),
            cue: false,
        }
    }
}

/// How an export that finished went, for the dialog to show.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSummary {
    /// Where the files are: the one file, or the folder.
    pub location: PathBuf,
    /// Each file written, by name, with how loud it is.
    pub files: Vec<(String, Loudness)>,
    /// The gain normalising applied, in dB.
    pub gain_db: f64,
    /// The CUE sheet written next to the mixdown, if one was asked for.
    pub cue: Option<PathBuf>,
}

/// An export under way.
pub(super) struct Run {
    job: Job<Result<ExportReport, ExportJobError>>,
    path: PathBuf,
    rate: f32,
    /// More than one file: `path` is the folder they are in.
    folder: bool,
    cue: Option<PathBuf>,
}

impl Session {
    /// How long the project is, in seconds: to the end of the last clip,
    /// automation point or tempo change. 0 for an empty one. Looks at every
    /// audio file, so don't call it every frame.
    pub fn project_seconds(&self) -> f64 {
        let rate = self.export_rate();
        self.timeline_end(rate) as f64 / f64::from(rate)
    }

    pub(super) fn export_rate(&self) -> f32 {
        self.audio
            .as_ref()
            .map_or(FREEZE_SETTINGS.sample_rate, |audio| {
                audio.controller.settings().sample_rate
            })
    }

    /// The groups an export can write a file for, in track order: every
    /// top-level group that has an output, tracks and buses alike.
    pub fn exportable_groups(&self) -> Vec<(NodeId, String)> {
        let graph = self.project.graph();
        crate::mixer::strips(&self.project)
            .into_iter()
            .filter(|strip| !graph.group_ports(strip.group).outputs.is_empty())
            .map(|strip| (strip.group, strip.name))
            .collect()
    }

    /// What the project is called in the names of files it exports: its
    /// file's name without the extension.
    pub(super) fn song_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_stem()).map_or_else(
            || "Untitled".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        )
    }

    /// How the last export went, if it wrote anything.
    pub fn last_export(&self) -> Option<&ExportSummary> {
        self.export_summary.as_ref()
    }

    /// Forgets the last export's summary, when the dialog is done with it.
    pub fn clear_last_export(&mut self) {
        self.export_summary = None;
    }

    /// Starts writing the project. With no `groups` chosen `path` is the
    /// mixdown's file; otherwise it is a folder, and the mixdown (if asked
    /// for) and each group go in it, named for what they are. The audio is
    /// rendered as it is played, offline nodes and frozen nodes included,
    /// then written in the chosen format.
    pub fn start_export(&mut self, path: PathBuf, choice: ExportChoice) -> Result<(), String> {
        if self.exporting.is_some() {
            return Err("An export is already running".into());
        }
        if !choice.mixdown && choice.groups.is_empty() {
            return Err("Choose the mixdown or a track to export".into());
        }
        let isrc = if choice.isrc.trim().is_empty() {
            String::new()
        } else {
            noodle_io::normalize_isrc(&choice.isrc).ok_or(
                "The ISRC isn't valid: it is two letters, three letters or digits, then seven digits",
            )?
        };
        let metadata = noodle_io::ExportMetadata {
            title: choice.title.trim().to_owned(),
            artist: choice.artist.trim().to_owned(),
            isrc,
        };
        if choice.layout.channels() > noodle_io::FLAC_MAX_CHANNELS
            && matches!(choice.format, ExportFormat::Flac24 | ExportFormat::Flac16)
        {
            return Err(format!(
                "FLAC holds at most {} channels; choose WAV for {}",
                noodle_io::FLAC_MAX_CHANNELS,
                choice.layout.name()
            ));
        }
        if choice.cue && !choice.mixdown {
            return Err("A CUE sheet goes with the mixdown: tick Mixdown".into());
        }
        let folder = !choice.groups.is_empty() || !choice.versions.is_empty();
        // What each pass writes: the version it plays (none for the project
        // as it is), the mixdown's file and the tracks'.
        let mut passes: Vec<(Option<usize>, Option<PathBuf>, Vec<TrackExport>)> = Vec::new();
        if folder {
            let names: std::collections::BTreeMap<NodeId, String> =
                self.exportable_groups().into_iter().collect();
            let song = self.song_name();
            let mut taken = Vec::new();
            let mut file = |name: &str| {
                let file = unique_file_name(name, &taken);
                taken.push(file.clone());
                path.join(format!("{file}.{}", choice.format.extension()))
            };
            let versions: Vec<Option<usize>> = if choice.versions.is_empty() {
                vec![None]
            } else {
                choice.versions.iter().map(|&v| Some(v)).collect()
            };
            for version in versions {
                let prefix = match version {
                    Some(v) => {
                        let name = self
                            .project
                            .versions()
                            .get(v)
                            .ok_or("A version chosen for export is gone")?;
                        Some(format!("{song} - {name}"))
                    }
                    None => None,
                };
                let mixdown = choice
                    .mixdown
                    .then(|| file(prefix.as_deref().unwrap_or("Mixdown")));
                let mut tracks = Vec::new();
                for &group in &choice.groups {
                    let name = names
                        .get(&group)
                        .cloned()
                        .ok_or("A group chosen for export is gone")?;
                    let name = match &prefix {
                        Some(prefix) => format!("{prefix} - {name}"),
                        None => name,
                    };
                    tracks.push(TrackExport {
                        group,
                        path: file(&name),
                        name,
                    });
                }
                passes.push((version, mixdown, tracks));
            }
            if let Err(e) = std::fs::create_dir_all(&path) {
                return Err(format!("Couldn't make the folder: {e}"));
            }
        } else {
            passes.push((None, Some(path.clone()), Vec::new()));
        }
        let cue = choice
            .cue
            .then(|| {
                passes
                    .first()
                    .and_then(|(_, mixdown, _)| mixdown.as_ref())
                    .map(|m| m.with_extension("cue"))
            })
            .flatten();
        let rate = self.export_rate();
        let settings = Settings {
            sample_rate: rate,
            max_frames: FREEZE_SETTINGS.max_frames,
            channels: choice.layout.channels(),
        };
        let (start, end) = match choice.range {
            Some((from, to)) => (from.max(0.0), to),
            None => (0.0, self.timeline_end(rate) as f64 / f64::from(rate)),
        };
        // The tail is the same for every file, in a range export too.
        let (end, silent_tail) = match self.project.export_settings().tail {
            ExportTail::Fixed { seconds } => (end + seconds.max(0.0), None),
            ExportTail::ToSilence { cap } => (end, Some(cap.clamp(0.0, 60.0))),
        };
        if self.timeline_end(rate) == 0 && choice.range.is_none() {
            return Err("There is nothing to export yet".into());
        }
        let frame = |seconds: f64| (seconds * f64::from(rate)).round() as u64;
        let (start, end) = (frame(start), frame(end));
        let silent_tail = silent_tail.map(frame).filter(|&cap| cap > 0);
        if end <= start {
            return Err("The range to export is empty".into());
        }
        let freezer = if Freezing::in_use(&self.project, &self.registry) {
            Some(self.freezing.open_freezer()?)
        } else {
            None
        };
        let requests: Vec<ExportRequest> = passes
            .into_iter()
            .map(|(version, mixdown, tracks)| ExportRequest {
                render: RenderRequest {
                    project: match version {
                        Some(v) => self.project.with_active_version(v),
                        None => self.project.clone(),
                    },
                    base: self.base().to_owned(),
                    settings,
                    frames: self.freeze_frames(rate),
                    extend_registry: self.extend_registry(),
                },
                start,
                end,
                mixdown,
                tracks,
                format: choice.format,
                normalize: choice.normalize,
                true_peak_ceiling: choice.true_peak_ceiling,
                metadata: metadata.clone(),
                cue: choice.cue,
                freezer: freezer.clone(),
                silent_tail,
            })
            .collect();
        self.exporting = Some(Run {
            job: spawn_export_all(requests),
            path,
            rate,
            folder,
            cue,
        });
        Ok(())
    }

    /// How far the export under way has got, from 0 to 1.
    pub fn export_progress(&self) -> Option<f32> {
        self.exporting.as_ref().map(|run| run.job.fraction())
    }

    /// Stops the export under way. Nothing is left of its file.
    pub fn cancel_export(&mut self) {
        if let Some(run) = &self.exporting {
            run.job.cancel();
        }
    }

    /// Takes in a finished export and says how it went.
    pub(super) fn poll_export(&mut self) {
        let Some(run) = &mut self.exporting else {
            return;
        };
        let Some(done) = run.job.poll() else {
            return;
        };
        let Run {
            path,
            rate,
            folder,
            cue,
            ..
        } = self.exporting.take().expect("just polled");
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.message = Some(match done {
            Ok(Ok(report)) => {
                let seconds = report.frames as f64 / f64::from(rate);
                let mut files = Vec::new();
                files.extend(report.mixdown.map(|l| ("Mixdown".to_owned(), l)));
                files.extend(report.tracks.iter().cloned());
                self.export_summary = Some(ExportSummary {
                    location: path,
                    files,
                    gain_db: report.gain_db,
                    cue,
                });
                let mut text = if folder {
                    let count = report.tracks.len() + usize::from(report.mixdown.is_some());
                    format!("Exported {count} files of {seconds:.1} s to {name}")
                } else {
                    format!("Exported {seconds:.1} s to {name}")
                };
                if !report.is_complete() {
                    text.push_str(", but some audio files couldn't be read");
                }
                let unrendered = report
                    .diagnostics
                    .iter()
                    .filter(|d| matches!(d.problem, noodle_engine::Problem::NotCacheable(_)))
                    .count();
                if unrendered > 0 {
                    text.push_str(&format!(
                        "; {unrendered} offline node{} couldn't be rendered and {} silent",
                        if unrendered == 1 { "" } else { "s" },
                        if unrendered == 1 { "is" } else { "are" },
                    ));
                }
                text
            }
            Ok(Err(ExportJobError::Cancelled)) => "Export cancelled".to_owned(),
            Ok(Err(error)) => format!("Couldn't export: {error}"),
            Err(JobPanicked) => format!("Couldn't export: {JobPanicked}"),
        });
    }
}

/// `name` as a file name that no other in `taken` has, in any case (some
/// file systems don't tell cases apart): characters a file name can't hold
/// become `_`, and a repeat gets a number.
pub(super) fn unique_file_name(name: &str, taken: &[String]) -> String {
    let clean: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim_matches('.');
    let base = if clean.is_empty() { "Track" } else { clean };
    let used = |candidate: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(candidate));
    if !used(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|candidate| !used(candidate))
        .expect("there are only so many names")
}
