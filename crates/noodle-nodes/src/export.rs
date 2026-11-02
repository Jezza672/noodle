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
//!
//! An export writes a mixdown, track files, or both. The mixdown is the
//! project as playback plays it, with whatever sits between the tracks and
//! the Outputs. A track file is the signal at a group's output, after the
//! group's gain and mute, before anything the group feeds into: all the track
//! files come from one pass through [`render_taps`], so they are the same
//! length and see the same render. Every file is measured as it is written
//! (integrated loudness and true peak, see [`noodle_io::LoudnessMeter`]), and
//! the export can bring the mixdown to a target loudness, by one gain that
//! the track files share so they still sum to it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use noodle_core::{Endpoint, NodeId, Tick};
use noodle_engine::{
    Cancelled, Diagnostic, Job, Progress, Registry, StreamError, Tap, TempoTable,
    render_project_streaming_replacing, render_taps,
};
use noodle_io::{ExportError, ExportFormat, ExportWriter, Loudness, LoudnessMeter};

use crate::{
    CHUNK_FRAMES, ClipProblem, FileError, FreezeError, Freezer, RenderRequest,
    freeze::freeze_within, register_library_blocking,
};

/// What to write, and where.
#[derive(Clone)]
pub struct ExportRequest {
    /// The project, its settings and its clip folder. `render.frames` is the
    /// length that renders of offline and frozen nodes are keyed by: the
    /// same length live playback uses, so they are the same renders. A
    /// range that ends later gets renders of its own, long enough to cover
    /// it, rather than silence past the end of playback's.
    pub render: RenderRequest,
    /// The first frame written, counted from the start of the timeline.
    pub start: u64,
    /// One past the last frame written. Past the end of what the project
    /// plays the audio is silence (or an effect's tail).
    pub end: u64,
    /// Where to write the mixdown, if one is wanted.
    pub mixdown: Option<PathBuf>,
    /// The groups to write a file for each.
    pub tracks: Vec<TrackExport>,
    pub format: ExportFormat,
    /// Brings the mixdown to this loudness in LUFS, by a gain on every file.
    /// `None` leaves the levels as they are.
    pub normalize: Option<f64>,
    /// The cache offline and frozen nodes are rendered into. `None` skips
    /// that: they are not heard.
    pub freezer: Option<Freezer>,
}

/// A group to write to its own file.
#[derive(Clone, Debug)]
pub struct TrackExport {
    /// A track, or any group, a bus included.
    pub group: NodeId,
    /// What the group is called, for messages.
    pub name: String,
    pub path: PathBuf,
}

/// What an export reports besides the files.
#[derive(Debug, Default)]
pub struct ExportReport {
    /// Frames written to each file.
    pub frames: u64,
    /// How loud the mixdown is as written (or would be, when it is only
    /// measured for normalising); `None` if the export neither wrote nor
    /// needed one.
    pub mixdown: Option<Loudness>,
    /// How loud each track file is, by name, in the order asked for.
    pub tracks: Vec<(String, Loudness)>,
    /// The gain normalising applied to every file, in dB; 0 without it.
    pub gain_db: f64,
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
    /// No mixdown and no track was asked for.
    NothingChosen,
    /// A group chosen for export has no output to take a file from.
    NoOutput(String),
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
            Self::NothingChosen => f.write_str("there is nothing chosen to export"),
            Self::NoOutput(name) => write!(f, "{name} has no output to export"),
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
    if request.mixdown.is_none() && request.tracks.is_empty() {
        return Err(ExportJobError::NothingChosen);
    }
    let settings = request.render.settings;
    let end = usize::try_from(request.end)
        .map_err(|_| ExportJobError::Render("the range is too long to render".into()))?;
    // Renders of offline and frozen nodes must reach the end of the file, or
    // they would play zeros from where they stop.
    let mut render = request.render.clone();
    render.frames = render.frames.max(end);
    let render = &render;
    // The renders of offline and frozen nodes take the first half of the
    // bar, the file the rest.
    if let Some(freezer) = &request.freezer {
        freeze_within(freezer, render, progress, (0.0, 0.5)).map_err(|error| match error {
            FreezeError::Cancelled => ExportJobError::Cancelled,
            other => ExportJobError::Freeze(other),
        })?;
    }

    let mut registry = Registry::with_builtins();
    let library = register_library_blocking(&mut registry);
    library.samples.set_dir(render.base.join("samples"));
    if let Some(extend) = &render.extend_registry {
        extend(&mut registry);
    }
    let rate = settings.sample_rate.round() as u32;
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

    let channels = settings.channels;
    let expected = request.end - request.start;
    // Where each pass's share of the progress bar goes.
    let passes = usize::from(request.normalize.is_some())
        + usize::from(request.mixdown.is_some() || request.normalize.is_some())
        + usize::from(!request.tracks.is_empty());
    let (first, span) = if request.freezer.is_some() {
        (0.5, 0.5)
    } else {
        (0.0, 1.0)
    };
    let mut pass = 0;
    let mut next_window = |progress: &Progress| {
        let share = span / passes.max(1) as f32;
        progress.set_window(first + share * pass as f32, share);
        pass += 1;
    };
    let new_sink = |path: &Option<PathBuf>, gain: f32| -> Result<Sink, ExportJobError> {
        let writer = match path {
            Some(path) => Some(
                ExportWriter::create(path, request.format, channels, rate, Some(expected))
                    .map_err(ExportJobError::File)?,
            ),
            None => None,
        };
        Ok(Sink::new(
            writer,
            rate,
            channels,
            gain,
            request.start as usize,
        ))
    };
    let mut frames = 0;
    let mut diagnostics = Vec::new();

    // The mixdown: the whole project through the Outputs.
    let mut gain_db = 0.0;
    let mut mixdown = None;
    let mut mix_pass = |mut sink: Sink, progress: &Progress| -> Result<Sink, ExportJobError> {
        let found = render_project_streaming_replacing(
            &request.render.project,
            &registry,
            settings,
            end,
            CHUNK_FRAMES,
            progress,
            &replacements,
            |chunk: &[f32]| sink.push(chunk),
        )
        .map_err(|error| match error {
            StreamError::Cancelled => ExportJobError::Cancelled,
            StreamError::Sink(error) => ExportJobError::File(error),
            StreamError::Render(error) => ExportJobError::Render(error.to_string()),
        })?;
        diagnostics.extend(found);
        Ok(sink)
    };
    if let Some(target) = request.normalize {
        next_window(progress);
        let (loudness, _) = mix_pass(new_sink(&None, 1.0)?, progress)?.finish()?;
        if loudness.lufs.is_finite() {
            gain_db = target - loudness.lufs;
        }
        mixdown = Some(loudness);
    }
    let gain = 10f32.powf(gain_db as f32 / 20.0);
    if request.mixdown.is_some() {
        next_window(progress);
        let sink = mix_pass(new_sink(&request.mixdown, gain)?, progress)?;
        let (loudness, written) = sink.finish()?;
        mixdown = Some(loudness);
        frames = written;
    } else if let Some(loudness) = &mut mixdown {
        // Only measured: say what it would have been with the gain.
        loudness.lufs += gain_db;
        loudness.true_peak_db += gain_db;
        loudness.sample_peak_db += gain_db;
    }

    // The track files: one pass, one tap per group.
    let mut tracks = Vec::new();
    if !request.tracks.is_empty() {
        next_window(progress);
        let graph = render.project.graph();
        let mut taps = Vec::new();
        let mut outs = Vec::new();
        let failure: Arc<Mutex<Option<ExportError>>> = Arc::new(Mutex::new(None));
        for track in &request.tracks {
            let port = graph
                .control_port(track.group)
                .filter(|_| !graph.group_ports(track.group).outputs.is_empty())
                .ok_or_else(|| ExportJobError::NoOutput(track.name.clone()))?;
            let sink = Arc::new(Mutex::new(Some(new_sink(&Some(track.path.clone()), gain)?)));
            let (tap_sink, tap_failure) = (Arc::clone(&sink), Arc::clone(&failure));
            let mut interleaved = Vec::new();
            taps.push(Tap {
                endpoint: Endpoint::new(port.stage(), "out"),
                sink: Box::new(move |signal| {
                    interleave(&signal, channels, &mut interleaved);
                    let mut guard = tap_sink.lock().expect("track lock");
                    let Some(sink) = guard.as_mut() else { return };
                    if let Err(error) = sink.push(&interleaved) {
                        // The render can't be failed from here: keep the
                        // first error, stop writing, and look afterwards.
                        tap_failure
                            .lock()
                            .expect("failure lock")
                            .get_or_insert(error);
                        *guard = None;
                    }
                }),
            });
            outs.push((track.name.clone(), sink));
        }
        let found = render_taps::<ExportError>(
            &render.project,
            &registry,
            settings,
            Tick(0),
            None,
            end,
            progress,
            &replacements,
            taps,
        )
        .map_err(|error| match error {
            StreamError::Cancelled => ExportJobError::Cancelled,
            StreamError::Sink(error) => ExportJobError::File(error),
            StreamError::Render(error) => ExportJobError::Render(error.to_string()),
        })?;
        diagnostics.extend(found);
        if let Some(error) = failure.lock().expect("failure lock").take() {
            return Err(ExportJobError::File(error));
        }
        for (name, sink) in outs {
            let sink = sink.lock().expect("track lock").take();
            let Some(sink) = sink else { continue };
            let (loudness, written) = sink.finish()?;
            // A tap that never ran writes a short file.
            if written != expected {
                return Err(ExportJobError::Render(match diagnostics.first() {
                    Some(d) => format!("{name} produced no audio: {}", d.problem),
                    None => format!("{name} produced no audio"),
                }));
            }
            frames = written;
            tracks.push((name, loudness));
        }
    }
    progress.set_window(0.0, 1.0);
    // Offline nodes that couldn't be rendered were silent in the files.
    diagnostics.append(&mut notes);
    Ok(ExportReport {
        frames,
        mixdown,
        tracks,
        gain_db,
        diagnostics,
        problems,
        errors: library.clips.errors(),
        underruns: library.clips.underruns(),
    })
}

/// Interleaves a tapped signal into `channels` channels, the way an Output
/// node puts it on the main bus: a mono signal feeds the first two channels,
/// otherwise channel n feeds channel n, and voices are summed.
fn interleave(signal: &noodle_engine::SignalIn<'_>, channels: usize, out: &mut Vec<f32>) {
    let frames = signal.frames();
    let shape = signal.shape();
    out.clear();
    out.resize(frames * channels, 0.0);
    for channel in 0..channels {
        let source = match shape.channels {
            1 if channel < 2 => 0,
            1 => continue,
            n if channel < n => channel,
            _ => continue,
        };
        for voice in 0..shape.voices {
            for (frame, sample) in signal.lane(voice, source).iter().enumerate() {
                out[frame * channels + channel] += sample;
            }
        }
    }
}

/// One file being written, and measured, from rendered chunks.
struct Sink {
    writer: Option<ExportWriter>,
    meter: LoudnessMeter,
    channels: usize,
    gain: f32,
    /// Samples (not frames) still to drop from the front.
    skip: usize,
    written: u64,
    scratch: Vec<f32>,
}

impl Sink {
    /// A sink that drops the first `skip` frames. With no `writer` it only
    /// measures.
    fn new(
        writer: Option<ExportWriter>,
        rate: u32,
        channels: usize,
        gain: f32,
        skip: usize,
    ) -> Self {
        Self {
            writer,
            meter: LoudnessMeter::new(rate, channels),
            channels,
            gain,
            skip: skip * channels,
            written: 0,
            scratch: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[f32]) -> Result<(), ExportError> {
        let drop = self.skip.min(chunk.len());
        self.skip -= drop;
        let keep = &chunk[drop..];
        self.written += (keep.len() / self.channels) as u64;
        let keep = if self.gain == 1.0 {
            keep
        } else {
            self.scratch.clear();
            self.scratch.extend(keep.iter().map(|s| s * self.gain));
            &self.scratch
        };
        self.meter.push(keep);
        match &mut self.writer {
            Some(writer) => writer.write(keep),
            None => Ok(()),
        }
    }

    /// Completes the file. Returns how loud it is and the frames in it.
    fn finish(self) -> Result<(Loudness, u64), ExportJobError> {
        if let Some(writer) = self.writer {
            writer.finish().map_err(ExportJobError::File)?;
        }
        Ok((self.meter.finish(), self.written))
    }
}
