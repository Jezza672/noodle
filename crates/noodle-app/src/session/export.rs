//! Exporting the project to an audio file, on a background thread.

use super::*;
use noodle_engine::{Job, JobPanicked};
use noodle_io::ExportFormat;
use noodle_nodes::{ExportJobError, ExportReport, ExportRequest, RenderRequest, spawn_export};

/// Seconds a whole-project export runs past the last clip, for tails.
const EXPORT_TAIL: f64 = 4.0;
/// Channels in an export: the main output, as two.
const EXPORT_CHANNELS: usize = 2;

/// What the user chose in the export dialog.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportChoice {
    pub format: ExportFormat,
    /// The part to write, from and to in seconds; `None` is the whole
    /// project, to the end of the last clip and a tail.
    pub range: Option<(f64, f64)>,
}

/// An export under way.
pub(super) struct Run {
    job: Job<Result<ExportReport, ExportJobError>>,
    path: PathBuf,
    seconds: f64,
}

impl Session {
    /// How long the project is, in seconds: to the end of the last clip,
    /// automation point or tempo change. 0 for an empty one. Looks at every
    /// audio file, so don't call it every frame.
    pub fn project_seconds(&self) -> f64 {
        let rate = self.export_rate();
        self.timeline_end(rate) as f64 / f64::from(rate)
    }

    fn export_rate(&self) -> f32 {
        self.audio
            .as_ref()
            .map_or(FREEZE_SETTINGS.sample_rate, |audio| {
                audio.controller.settings().sample_rate
            })
    }

    /// Starts writing the project to `path`. The audio is rendered as it is
    /// played, offline nodes and frozen nodes included, then written in the
    /// chosen format.
    pub fn start_export(&mut self, path: PathBuf, choice: ExportChoice) -> Result<(), String> {
        if self.exporting.is_some() {
            return Err("An export is already running".into());
        }
        let rate = self.export_rate();
        let settings = Settings {
            sample_rate: rate,
            max_frames: FREEZE_SETTINGS.max_frames,
            channels: EXPORT_CHANNELS,
        };
        let (start, end) = match choice.range {
            Some((from, to)) => (from.max(0.0), to),
            None => (
                0.0,
                self.timeline_end(rate) as f64 / f64::from(rate) + EXPORT_TAIL,
            ),
        };
        if self.timeline_end(rate) == 0 && choice.range.is_none() {
            return Err("There is nothing to export yet".into());
        }
        let frame = |seconds: f64| (seconds * f64::from(rate)).round() as u64;
        let (start, end) = (frame(start), frame(end));
        if end <= start {
            return Err("The range to export is empty".into());
        }
        let freezer = if Freezing::in_use(&self.project, &self.registry) {
            Some(self.freezing.open_freezer()?)
        } else {
            None
        };
        let request = ExportRequest {
            render: RenderRequest {
                project: self.project.clone(),
                base: self.base().to_owned(),
                settings,
                frames: self.freeze_frames(rate),
                extend_registry: None,
            },
            start,
            end,
            path: path.clone(),
            format: choice.format,
            freezer,
        };
        self.exporting = Some(Run {
            job: spawn_export(request),
            path,
            seconds: (end - start) as f64 / f64::from(rate),
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
        let Run { path, seconds, .. } = self.exporting.take().expect("just polled");
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.message = Some(match done {
            Ok(Ok(report)) => {
                let mut text = format!("Exported {seconds:.1} s to {name}");
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
