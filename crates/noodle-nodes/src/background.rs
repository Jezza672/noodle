//! The offline renderer as a background service: renders a project on its
//! own thread, in chunks, with progress and cancellation, optionally into the
//! on-disk cache.
//!
//! Nothing here runs on the audio thread. The render has an engine and a
//! registry of its own, so it never disturbs live playback.

use std::io;
use std::path::PathBuf;

use noodle_core::{CacheKey, Project};
use noodle_engine::{
    Diagnostic, Job, Progress, Registry, Settings, StreamError, TempoTable,
    render_project_streaming,
};
use noodle_io::{CacheInfo, CacheStore};

use crate::{ClipProblem, FileError, register_library_blocking};

/// Frames handed to the sink at a time: about 340 ms at 48 kHz.
pub const CHUNK_FRAMES: usize = 16_384;

/// What to render, from the start of the timeline.
#[derive(Clone)]
pub struct RenderRequest {
    pub project: Project,
    /// Where clip files are looked up, normally the project file's folder.
    pub base: PathBuf,
    pub settings: Settings,
    pub frames: usize,
}

/// What a finished render reports besides the audio.
#[derive(Clone, Debug, Default)]
pub struct RenderReport {
    /// Problems found compiling the graph.
    pub diagnostics: Vec<Diagnostic>,
    /// Clips that couldn't be scheduled. They are left out.
    pub problems: Vec<ClipProblem>,
    /// Blocks where a clip's audio was missing even after waiting.
    pub underruns: u64,
    /// Files that couldn't be opened; their clips were left silent.
    pub errors: Vec<FileError>,
}

impl RenderReport {
    /// Whether the audio is exactly what the project describes, so it is
    /// safe to cache. A clip left out or silent because of the disk would be
    /// served from the cache long after the disk was fixed.
    pub fn is_complete(&self) -> bool {
        self.problems.is_empty() && self.errors.is_empty() && self.underruns == 0
    }
}

/// Renders `request` on the calling thread, handing the audio to `sink` in
/// chunks. This is [`render_project_with_clips`](crate::render_project_with_clips)
/// without holding the whole render in memory; the samples are the same.
pub fn render_streaming<E>(
    request: &RenderRequest,
    progress: &Progress,
    sink: impl FnMut(&[f32]) -> Result<(), E>,
) -> Result<RenderReport, StreamError<E>> {
    let mut registry = Registry::with_builtins();
    let library = register_library_blocking(&mut registry);
    let settings = request.settings;
    let rate = settings.sample_rate.round() as u32;
    let table = TempoTable::new(request.project.tempo_map(), settings.sample_rate);
    let problems = library
        .clips
        .update(&request.project, &table, rate, &request.base);
    let diagnostics = render_project_streaming(
        &request.project,
        &registry,
        settings,
        request.frames,
        CHUNK_FRAMES,
        progress,
        sink,
    )?;
    Ok(RenderReport {
        diagnostics,
        problems,
        underruns: library.clips.underruns(),
        errors: library.clips.errors(),
    })
}

/// Starts [`render_streaming`] on a background thread.
pub fn spawn_render<E: Send + 'static>(
    request: RenderRequest,
    sink: impl FnMut(&[f32]) -> Result<(), E> + Send + 'static,
) -> Job<Result<RenderReport, StreamError<E>>> {
    Job::spawn(move |progress| render_streaming(&request, progress, sink))
}

/// How a render into the cache ended.
#[derive(Debug)]
pub enum CacheRender {
    /// The cache already held `key`; nothing was rendered.
    Hit(CacheInfo),
    /// Rendered and stored.
    Stored(CacheInfo, RenderReport),
    /// Rendered, but not stored: the audio is incomplete (see
    /// [`RenderReport::is_complete`]), so it must not stand in for the real
    /// thing.
    Incomplete(RenderReport),
}

/// Renders `request` on a background thread into `store` under `key`, unless
/// the store already has it. The entry only appears once the render has
/// finished completely; a cancelled or failed render leaves nothing behind.
///
/// The caller computes `key` (it must cover everything the render depends
/// on, including the settings).
pub fn spawn_render_to_cache(
    request: RenderRequest,
    store: CacheStore,
    key: CacheKey,
) -> Job<Result<CacheRender, StreamError<io::Error>>> {
    Job::spawn(move |progress| {
        if let Some(hit) = store.get(&key) {
            progress.report(1.0)?;
            return Ok(CacheRender::Hit(hit.info()));
        }
        let settings = request.settings;
        let mut writer = store
            .writer(&key, settings.channels, settings.sample_rate.round() as u32)
            .map_err(StreamError::Sink)?;
        let report = render_streaming(&request, progress, |chunk| writer.write(chunk))?;
        if !report.is_complete() {
            return Ok(CacheRender::Incomplete(report));
        }
        let info = writer.commit().map_err(StreamError::Sink)?;
        Ok(CacheRender::Stored(info, report))
    })
}
