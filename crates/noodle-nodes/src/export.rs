//! Exporting a project to an audio file: the offline renderer run over the
//! whole project (or a range of it), writing to a file in WAV or FLAC.
//!
//! An export hears what playback hears. Offline nodes and frozen nodes are
//! rendered into the cache first (the same renders live playback uses, keyed
//! by the same length), then the project is rendered with those renders
//! standing in for them. The render always starts at the beginning of the
//! timeline, so nodes with memory (delays, envelopes, filters) are in the
//! state they would be in on the way there; a range only chooses which part
//! of the render is written.

use std::path::PathBuf;

use noodle_engine::{
    Cancelled, Diagnostic, Job, Progress, Registry, StreamError, TempoTable,
    render_project_streaming_replacing,
};
use noodle_io::{ExportError, ExportFormat, ExportWriter};

use crate::{
    CHUNK_FRAMES, ClipProblem, FileError, FreezeError, Freezer, RenderRequest,
    freeze::freeze_within, register_library_blocking,
};

/// What to write, and where.
#[derive(Clone)]
pub struct ExportRequest {
    /// The project, its settings and its clip folder. `render.frames` is the
    /// length that renders of offline and frozen nodes are keyed by: the
    /// same length live playback uses, so they are the same renders.
    pub render: RenderRequest,
    /// The first frame written, counted from the start of the timeline.
    pub start: u64,
    /// One past the last frame written. Past the end of what the project
    /// plays the audio is silence (or an effect's tail).
    pub end: u64,
    pub path: PathBuf,
    pub format: ExportFormat,
    /// The cache offline and frozen nodes are rendered into. `None` skips
    /// that: they are not heard.
    pub freezer: Option<Freezer>,
}

/// What an export reports besides the file.
#[derive(Debug, Default)]
pub struct ExportReport {
    /// Frames written.
    pub frames: u64,
    /// Problems found compiling the graph, and offline nodes that couldn't
    /// be rendered and so are silent in the file.
    pub diagnostics: Vec<Diagnostic>,
    /// Clips that couldn't be scheduled. They are left out of the file.
    pub problems: Vec<ClipProblem>,
    /// Files that couldn't be read; their clips are silent from then on.
    pub errors: Vec<FileError>,
    /// Blocks where a clip's audio was missing even after waiting.
    pub underruns: u64,
}

impl ExportReport {
    /// Whether the file holds exactly what the project describes.
    pub fn is_complete(&self) -> bool {
        self.problems.is_empty() && self.errors.is_empty() && self.underruns == 0
    }
}

#[derive(Debug)]
pub enum ExportJobError {
    Cancelled,
    /// The range is empty or backwards.
    EmptyRange,
    /// A freeze or offline node couldn't be rendered.
    Freeze(FreezeError),
    /// The file couldn't be written.
    File(ExportError),
    /// The engine wouldn't start or the project is too long to render.
    Render(String),
}

impl std::fmt::Display for ExportJobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("the export was cancelled"),
            Self::EmptyRange => f.write_str("the range to export is empty"),
            Self::Freeze(error) => write!(f, "{error}"),
            Self::File(error) => write!(f, "the file couldn't be written: {error}"),
            Self::Render(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for ExportJobError {}

impl From<Cancelled> for ExportJobError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

/// Starts [`export`] on a background thread.
pub fn spawn_export(request: ExportRequest) -> Job<Result<ExportReport, ExportJobError>> {
    Job::spawn(move |progress| export(&request, progress))
}

/// Runs an export on the calling thread. Progress covers the renders of
/// offline and frozen nodes first (if any are missing), then the file.
pub fn export(
    request: &ExportRequest,
    progress: &Progress,
) -> Result<ExportReport, ExportJobError> {
    if request.end <= request.start {
        return Err(ExportJobError::EmptyRange);
    }
    let settings = request.render.settings;
    let end = usize::try_from(request.end)
        .map_err(|_| ExportJobError::Render("the range is too long to render".into()))?;
    // The renders of offline and frozen nodes take the first half of the
    // bar, the file the rest.
    if let Some(freezer) = &request.freezer {
        freeze_within(freezer, &request.render, progress, (0.0, 0.5)).map_err(
            |error| match error {
                FreezeError::Cancelled => ExportJobError::Cancelled,
                other => ExportJobError::Freeze(other),
            },
        )?;
    }

    let mut registry = Registry::with_builtins();
    let library = register_library_blocking(&mut registry);
    if let Some(extend) = &request.render.extend_registry {
        extend(&mut registry);
    }
    let rate = settings.sample_rate.round() as u32;
    let table = TempoTable::new(request.render.project.tempo_map(), settings.sample_rate);
    let problems =
        library
            .clips
            .update(&request.render.project, &table, rate, &request.render.base);
    let (replacements, mut notes) = match &request.freezer {
        Some(freezer) => {
            let analysis = freezer.analyze(
                &request.render.project,
                &registry,
                settings,
                request.render.frames,
                &request.render.base,
            );
            let plan = freezer.plan(&analysis, settings, request.render.frames, true);
            (plan.replacements, plan.diagnostics)
        }
        None => (noodle_engine::Replacements::new(), Vec::new()),
    };

    let mut writer = ExportWriter::create(
        &request.path,
        request.format,
        settings.channels,
        rate,
        Some(request.end - request.start),
    )
    .map_err(ExportJobError::File)?;
    // The first `start` frames are rendered but not written.
    let mut skip = request.start as usize * settings.channels;
    let mut written = 0u64;
    let (window_start, window_span) = if request.freezer.is_some() {
        (0.5, 0.5)
    } else {
        (0.0, 1.0)
    };
    progress.set_window(window_start, window_span);
    let mut diagnostics = render_project_streaming_replacing(
        &request.render.project,
        &registry,
        settings,
        end,
        CHUNK_FRAMES,
        progress,
        &replacements,
        |chunk: &[f32]| {
            let drop = skip.min(chunk.len());
            skip -= drop;
            let keep = &chunk[drop..];
            written += (keep.len() / settings.channels) as u64;
            writer.write(keep)
        },
    )
    .map_err(|error| match error {
        StreamError::Cancelled => ExportJobError::Cancelled,
        StreamError::Sink(error) => ExportJobError::File(error),
        StreamError::Render(error) => ExportJobError::Render(error.to_string()),
    })?;
    progress.set_window(0.0, 1.0);
    // Offline nodes that couldn't be rendered were silent in the file.
    diagnostics.append(&mut notes);
    writer.finish().map_err(ExportJobError::File)?;
    Ok(ExportReport {
        frames: written,
        diagnostics,
        problems,
        errors: library.clips.errors(),
        underruns: library.clips.underruns(),
    })
}
