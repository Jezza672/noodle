//! Rendering one group (a track or a bus) to an audio file that becomes a
//! clip: the part of [`export`](crate::export) that writes a track file,
//! with a tail.
//!
//! The group is rendered the way a track export renders it (one pass through
//! [`render_taps`] from the start of the timeline, offline and frozen nodes
//! standing in as renders), so the clip is the track's own signal at its
//! output, after its gain and mute. The render runs past the end of the
//! range until the effects fall silent, and no further than
//! [`MAX_TAIL_SECONDS`]. The audio is a 32-bit float WAV, so nothing is
//! lost to rounding.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use noodle_core::{Endpoint, NodeId, Tick};
use noodle_engine::{
    Diagnostic, Job, Progress, Registry, StreamError, Tap, TempoTable,
    render_project_streaming_replacing, render_taps,
};
use noodle_io::{ExportError, ExportFormat, ExportWriter};

use crate::export::interleave;
use crate::{
    ClipProblem, ExportJobError, FileError, FreezeError, Freezer, RenderRequest,
    freeze::freeze_within, register_library_blocking,
};

/// The longest a tail runs past the end of the range, in seconds.
pub const MAX_TAIL_SECONDS: f64 = 10.0;
/// A sample this quiet or quieter (about -80 dB) is silence: the tail ends
/// after the last sample above it.
pub const CLIP_SILENCE: f32 = 1.0e-4;
/// How long the fade out is, in frames, when the tail is still sounding at
/// the cap and has to be cut.
const CAP_FADE: usize = 2048;
/// How many frames of the mix are rendered at a time.
const MIX_CHUNK_FRAMES: usize = 4096;

/// What to render, and where to write it.
#[derive(Clone)]
pub struct ClipRequest {
    /// The project and its settings. As in an export, `render.frames` is the
    /// length playback's renders of offline and frozen nodes are keyed by.
    pub render: RenderRequest,
    /// The group to render: a track or a bus. `None` renders the mix, what
    /// the Output nodes play.
    pub group: Option<NodeId>,
    /// What the group is called, for messages.
    pub name: String,
    /// The first frame of the clip, counted from the start of the timeline.
    pub start: u64,
    /// One past the last frame of the range. The clip is at least this long
    /// (to `start`), and longer while the group is still sounding.
    pub end: u64,
    /// The longest the tail may run, in frames.
    pub max_tail: u64,
    /// The WAV file to write.
    pub path: PathBuf,
    /// The cache offline and frozen nodes are rendered into. `None` skips
    /// that: they are not heard.
    pub freezer: Option<Freezer>,
}

/// What a render to a clip reports besides the file.
#[derive(Debug, Default)]
pub struct ClipReport {
    /// Frames in the file.
    pub frames: u64,
    /// Of those, the frames past the end of the range (the tail).
    pub tail: u64,
    /// Whether the tail was still sounding at the cap and was cut.
    pub capped: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub problems: Vec<ClipProblem>,
    pub errors: Vec<FileError>,
    pub underruns: u64,
}

impl ClipReport {
    /// Whether the file holds exactly what the project describes.
    pub fn is_complete(&self) -> bool {
        self.problems.is_empty() && self.errors.is_empty() && self.underruns == 0
    }
}

/// Starts [`render_clip`] on a background thread.
pub fn spawn_render_clip(request: ClipRequest) -> Job<Result<ClipReport, ExportJobError>> {
    Job::spawn(move |progress| render_clip(&request, progress))
}

/// The file as it is being written: the range goes straight to it, the tail
/// waits in memory (it is at most [`MAX_TAIL_SECONDS`]) until its length is
/// known.
struct Writing {
    writer: Option<ExportWriter>,
    channels: usize,
    /// Frames still to drop from the front, to reach `start`.
    skip: usize,
    /// Frames of the range still to write.
    body: usize,
    audible: bool,
    tail: Vec<f32>,
    error: Option<ExportError>,
}

impl Writing {
    fn push(&mut self, chunk: &[f32]) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        let drop = (self.skip * self.channels).min(chunk.len());
        self.skip -= drop / self.channels;
        let keep = &chunk[drop..];
        let now = (self.body * self.channels).min(keep.len());
        let (body, tail) = keep.split_at(now);
        self.body -= now / self.channels;
        self.audible |= body.iter().any(|s| s.abs() > CLIP_SILENCE);
        if let Err(error) = writer.write(body) {
            self.error = Some(error);
            self.writer = None;
            return;
        }
        self.tail.extend_from_slice(tail);
    }
}

/// Runs a render to a clip on the calling thread. Progress covers the
/// renders of offline and frozen nodes first (if any are missing), then the
/// file.
pub fn render_clip(
    request: &ClipRequest,
    progress: &Progress,
) -> Result<ClipReport, ExportJobError> {
    if request.end <= request.start {
        return Err(ExportJobError::EmptyRange);
    }
    let settings = request.render.settings;
    let channels = settings.channels;
    let rate = settings.sample_rate.round() as u32;
    let total = request
        .end
        .checked_add(request.max_tail)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| ExportJobError::Render("the range is too long to render".into()))?;
    let mut render = request.render.clone();
    render.frames = render.frames.max(total);
    let render = &render;
    if let Some(freezer) = &request.freezer {
        freeze_within(freezer, render, progress, (0.0, 0.5)).map_err(|error| match error {
            FreezeError::Cancelled => ExportJobError::Cancelled,
            other => ExportJobError::Freeze(other),
        })?;
        progress.set_window(0.5, 0.5);
    }

    let mut registry = Registry::with_builtins();
    let library = register_library_blocking(&mut registry);
    library.samples.set_dir(render.base.join("samples"));
    if let Some(extend) = &render.extend_registry {
        extend(&mut registry);
    }
    let table = TempoTable::new(render.project.tempo_map(), settings.sample_rate);
    let problems = library
        .clips
        .update(&render.project, &table, rate, &render.base);
    let (replacements, mut notes) = match &request.freezer {
        Some(freezer) => {
            let analysis = freezer.analyze(
                &render.project,
                &registry,
                settings,
                render.frames,
                &render.base,
            );
            let plan = freezer.plan(&analysis, settings, render.frames, true, None);
            (plan.replacements, plan.diagnostics)
        }
        None => (noodle_engine::Replacements::new(), Vec::new()),
    };

    let writer = ExportWriter::create(&request.path, ExportFormat::WavFloat, channels, rate, None)
        .map_err(ExportJobError::File)?;
    let writing = Arc::new(Mutex::new(Writing {
        writer: Some(writer),
        channels,
        skip: request.start as usize,
        body: (request.end - request.start) as usize,
        audible: false,
        tail: Vec::new(),
        error: None,
    }));
    let mut diagnostics = match request.group {
        Some(group) => {
            let graph = render.project.graph();
            let port = graph
                .control_port(group)
                .filter(|_| !graph.group_ports(group).outputs.is_empty())
                .ok_or_else(|| ExportJobError::NoOutput(request.name.clone()))?;
            let tap_writing = Arc::clone(&writing);
            let mut interleaved = Vec::new();
            let tap = Tap {
                endpoint: Endpoint::new(port.stage(), "out"),
                sink: Box::new(move |signal| {
                    interleave(&signal, channels, &mut interleaved);
                    tap_writing.lock().expect("clip lock").push(&interleaved);
                }),
            };
            render_taps::<ExportError>(
                &render.project,
                &registry,
                settings,
                Tick(0),
                None,
                total,
                progress,
                &replacements,
                vec![tap],
            )
        }
        None => render_project_streaming_replacing::<ExportError>(
            &render.project,
            &registry,
            settings,
            total,
            MIX_CHUNK_FRAMES,
            progress,
            &replacements,
            |chunk| {
                writing.lock().expect("clip lock").push(chunk);
                Ok(())
            },
        ),
    }
    .map_err(|error| match error {
        StreamError::Cancelled => ExportJobError::Cancelled,
        StreamError::Sink(error) => ExportJobError::File(error),
        StreamError::Render(error) => ExportJobError::Render(error.to_string()),
    })?;
    progress.set_window(0.0, 1.0);

    let mut writing = writing.lock().expect("clip lock");
    if let Some(error) = writing.error.take() {
        return Err(ExportJobError::File(error));
    }
    let expected = request.end - request.start;
    if writing.body != 0 || writing.skip != 0 {
        // The tap never ran for the whole range.
        return Err(ExportJobError::Render(match diagnostics.first() {
            Some(d) => format!("{} produced no audio: {}", request.name, d.problem),
            None => format!("{} produced no audio", request.name),
        }));
    }
    // The tail ends after the last sample that isn't silence.
    let tail_frames = writing.tail.len() / channels;
    let sounding = writing
        .tail
        .chunks(channels)
        .rposition(|frame| frame.iter().any(|s| s.abs() > CLIP_SILENCE))
        .map_or(0, |last| last + 1);
    if !writing.audible && sounding == 0 {
        return Err(ExportJobError::Silent(request.name.clone()));
    }
    let capped = sounding > 0 && sounding == tail_frames;
    let mut tail = std::mem::take(&mut writing.tail);
    tail.truncate(sounding * channels);
    if capped {
        // Cut while still sounding: fade out rather than click.
        let fade = CAP_FADE.min(sounding);
        for i in 0..fade {
            let gain = 1.0 - (i + 1) as f32 / fade as f32;
            let frame = (sounding - fade + i) * channels;
            for sample in &mut tail[frame..frame + channels] {
                *sample *= gain;
            }
        }
    }
    let mut writer = writing.writer.take().expect("writer is kept until here");
    writer.write(&tail).map_err(ExportJobError::File)?;
    writer.finish().map_err(ExportJobError::File)?;

    diagnostics.append(&mut notes);
    Ok(ClipReport {
        frames: expected + sounding as u64,
        tail: sounding as u64,
        capped,
        diagnostics,
        problems,
        errors: library.clips.errors(),
        underruns: library.clips.underruns(),
    })
}
