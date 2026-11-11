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
    Cancelled, Diagnostic, Job, Progress, StreamError, Tap, render_project_streaming_replacing,
    render_taps,
};
use noodle_io::{
    ExportError, ExportFormat, ExportMetadata, ExportWriter, Loudness, LoudnessMeter,
    TruePeakLimiter, cue_sheet,
};

use crate::level::db_to_gain;
use crate::render_clip::CLIP_SILENCE;
use crate::{
    CHUNK_FRAMES, ClipProblem, FileError, FreezeError, Freezer, RenderRequest, background::prepare,
    freeze::freeze_within,
};

/// How long the fade is, in frames, when a silent tail is cut while it still
/// sounds.
const CAP_FADE: usize = 2048;

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
    /// Holds the mixdown's true peak under this many dBTP, with a lookahead
    /// limiter after the normalising gain. `None` leaves the peaks as they
    /// are. Track files are not limited, so they still sum to the mixdown.
    pub true_peak_ceiling: Option<f64>,
    /// Title, artist and ISRC, written into the mixdown file.
    pub metadata: ExportMetadata,
    /// Writes a CUE sheet next to the mixdown file.
    pub cue: bool,
    /// The cache offline and frozen nodes are rendered into. `None` skips
    /// that: they are not heard.
    pub freezer: Option<Freezer>,
    /// `None`: the files end at `end`. `Some(cap)`: `end` is where the
    /// project proper stops, and the render runs on for up to `cap` frames
    /// until every file is silent, then all files are cut to the same
    /// length, the latest any of them is still sounding. A tail still
    /// sounding at the cap fades out.
    pub silent_tail: Option<u64>,
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
    /// A render to a clip came out silent, so there is no clip to make.
    Silent(String),
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
            Self::Silent(name) => write!(f, "{name} is silent, so there is nothing to render"),
        }
    }
}

impl std::error::Error for ExportJobError {}

impl From<StreamError<ExportError>> for ExportJobError {
    fn from(error: StreamError<ExportError>) -> Self {
        match error {
            StreamError::Cancelled => Self::Cancelled,
            StreamError::Sink(error) => Self::File(error),
            StreamError::Render(error) => Self::Render(error.to_string()),
        }
    }
}

impl From<Cancelled> for ExportJobError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

/// Starts [`export`] on a background thread.
pub fn spawn_export(request: ExportRequest) -> Job<Result<ExportReport, ExportJobError>> {
    Job::spawn(move |progress| export(&request, progress))
}

/// Starts the exports in `requests` on a background thread, one after the
/// other, as one job: a batch export of every mix version. The progress runs
/// across all of them, and the report is theirs added together: no mixdown of
/// its own, and every file in `tracks` (mixdowns first in each request).
pub fn spawn_export_all(requests: Vec<ExportRequest>) -> Job<Result<ExportReport, ExportJobError>> {
    Job::spawn(move |progress| {
        let count = requests.len().max(1) as f32;
        let mut all = ExportReport::default();
        for (i, request) in requests.iter().enumerate() {
            progress.set_outer(i as f32 / count, 1.0 / count);
            let mut report = export(request, progress)?;
            all.frames = all.frames.max(report.frames);
            if let Some(mixdown) = report.mixdown {
                let name = request
                    .mixdown
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                all.tracks.push((name, mixdown));
            }
            all.tracks.append(&mut report.tracks);
            // Each version is brought to the loudness on its own, so there is
            // no one gain to report for several.
            if count <= 1.0 {
                all.gain_db = report.gain_db;
            }
            all.diagnostics.append(&mut report.diagnostics);
            all.problems.append(&mut report.problems);
            all.errors.append(&mut report.errors);
            all.underruns += report.underruns;
        }
        progress.set_outer(0.0, 1.0);
        Ok(all)
    })
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
    let end = request
        .end
        .checked_add(request.silent_tail.unwrap_or(0))
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| ExportJobError::Render("the range is too long to render".into()))?;
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

    let (registry, library, problems) = prepare(render);
    let rate = settings.sample_rate.round() as u32;
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
    // What the files hold before any silent tail is cut.
    let expected = end as u64 - request.start;
    let body = request.end - request.start;
    // The ceiling applies to the mixdown file, so only when one is written.
    let ceiling = request
        .true_peak_ceiling
        .filter(|_| request.mixdown.is_some());
    // Where each pass's share of the progress bar goes.
    let passes = usize::from(request.normalize.is_some())
        + usize::from(request.normalize.is_some() && ceiling.is_some())
        + usize::from(request.mixdown.is_some())
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
    let new_sink = |path: &Option<PathBuf>,
                    gain: f32,
                    limit: Option<f64>,
                    metadata: ExportMetadata|
     -> Result<Sink, ExportJobError> {
        let writer = match path {
            Some(path) => Some(
                ExportWriter::create_tagged(
                    path,
                    request.format,
                    channels,
                    rate,
                    Some(expected),
                    metadata,
                )
                .map_err(ExportJobError::File)?,
            ),
            None => None,
        };
        let mut sink = Sink::new(writer, rate, channels, gain, request.start as usize);
        if path.is_some() && request.silent_tail.is_some() {
            sink.body_left = body;
        }
        if let Some(ceiling) = limit {
            sink.limiter = Some(TruePeakLimiter::new(rate, channels, ceiling));
        }
        Ok(sink)
    };
    let untagged = ExportMetadata::default;
    let mut frames = 0;
    let mut diagnostics = Vec::new();

    // The mixdown: the whole project through the Outputs.
    let mut gain_db = 0.0;
    let mut mixdown = None;
    let mut mix_sink: Option<Sink> = None;
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
        )?;
        diagnostics.extend(found);
        Ok(sink)
    };
    if let Some(target) = request.normalize {
        next_window(progress);
        let (loudness, _) =
            mix_pass(new_sink(&None, 1.0, None, untagged())?, progress)?.finish(0, false)?;
        if loudness.lufs.is_finite() {
            gain_db = target - loudness.lufs;
        }
        mixdown = Some(loudness);
        if let (Some(ceiling), true) = (ceiling, loudness.lufs.is_finite()) {
            // The limiter takes some loudness with the peaks: measure what
            // it leaves and make the gain up, once.
            next_window(progress);
            let gain = db_to_gain(gain_db as f32);
            let sink = new_sink(&None, gain, Some(ceiling), untagged())?;
            let (limited, _) = mix_pass(sink, progress)?.finish(0, false)?;
            if limited.lufs.is_finite() && (limited.lufs - target).abs() > 0.05 {
                gain_db += target - limited.lufs;
            }
        }
    }
    let gain = db_to_gain(gain_db as f32);
    if request.mixdown.is_some() {
        next_window(progress);
        mix_sink = Some(mix_pass(
            new_sink(&request.mixdown, gain, ceiling, request.metadata.clone())?,
            progress,
        )?);
    } else if let Some(loudness) = &mut mixdown {
        // Only measured: say what it would have been with the gain.
        loudness.lufs += gain_db;
        loudness.true_peak_db += gain_db;
        loudness.sample_peak_db += gain_db;
    }

    // The track files: one pass, one tap per group.
    let mut tracks = Vec::new();
    let mut track_sinks: Vec<(String, Sink)> = Vec::new();
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
            let sink = Arc::new(Mutex::new(Some(new_sink(
                &Some(track.path.clone()),
                gain,
                None,
                untagged(),
            )?)));
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
        )?;
        diagnostics.extend(found);
        if let Some(error) = failure.lock().expect("failure lock").take() {
            return Err(ExportJobError::File(error));
        }
        for (name, sink) in outs {
            let sink = sink.lock().expect("track lock").take();
            let Some(sink) = sink else { continue };
            // A tap that never ran writes a short file.
            if sink.received != expected {
                return Err(ExportJobError::Render(match diagnostics.first() {
                    Some(d) => format!("{name} produced no audio: {}", d.problem),
                    None => format!("{name} produced no audio"),
                }));
            }
            track_sinks.push((name, sink));
        }
    }
    // One tail for every file: as long as the latest sounding one.
    let held = expected - body;
    let sounding = mix_sink
        .iter()
        .chain(track_sinks.iter().map(|(_, sink)| sink))
        .map(Sink::audible_tail)
        .max()
        .unwrap_or(0) as u64;
    let capped = request.silent_tail.is_some() && sounding == held && held > 0;
    let keep = if request.silent_tail.is_some() {
        sounding as usize
    } else {
        0
    };
    if let Some(sink) = mix_sink {
        let (loudness, written) = sink.finish(keep, capped)?;
        mixdown = Some(loudness);
        frames = written;
        if let (true, Some(path)) = (request.cue, &request.mixdown) {
            let name = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let cue = cue_sheet(&name, request.format, &request.metadata);
            std::fs::write(path.with_extension("cue"), cue)
                .map_err(|e| ExportJobError::File(e.into()))?;
        }
    }
    for (name, sink) in track_sinks {
        let (loudness, written) = sink.finish(keep, capped)?;
        frames = written;
        tracks.push((name, loudness));
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
pub(crate) fn interleave(
    signal: &noodle_engine::SignalIn<'_>,
    channels: usize,
    out: &mut Vec<f32>,
) {
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
    /// Frames received after the skip, written or held.
    received: u64,
    /// Frames still to write straight through; what comes after is held (it
    /// is the tail, whose length is not known until every file is done).
    body_left: u64,
    held: Vec<f32>,
    scratch: Vec<f32>,
    /// Limits what is written (and measured) after the gain, to hold the true
    /// peak under a ceiling.
    limiter: Option<TruePeakLimiter>,
    limited: Vec<f32>,
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
            received: 0,
            body_left: u64::MAX,
            held: Vec::new(),
            scratch: Vec::new(),
            limiter: None,
            limited: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[f32]) -> Result<(), ExportError> {
        let drop = self.skip.min(chunk.len());
        self.skip -= drop;
        let keep = &chunk[drop..];
        let frames = (keep.len() / self.channels) as u64;
        self.received += frames;
        let now = frames.min(self.body_left);
        self.body_left -= now;
        let (body, tail) = keep.split_at(now as usize * self.channels);
        self.held.extend_from_slice(tail);
        self.emit(body)
    }

    /// Frames of the held tail up to the last sample that isn't silence.
    fn audible_tail(&self) -> usize {
        self.held
            .chunks(self.channels)
            .rposition(|frame| frame.iter().any(|s| s.abs() > CLIP_SILENCE))
            .map_or(0, |last| last + 1)
    }

    /// Gains, measures and writes samples.
    fn emit(&mut self, samples: &[f32]) -> Result<(), ExportError> {
        self.written += (samples.len() / self.channels) as u64;
        let samples = if self.gain == 1.0 {
            samples
        } else {
            self.scratch.clear();
            self.scratch.extend(samples.iter().map(|s| s * self.gain));
            &self.scratch
        };
        if let Some(limiter) = &mut self.limiter {
            self.limited.clear();
            limiter.push(samples, &mut self.limited);
            return record(&mut self.meter, &mut self.writer, &self.limited);
        }
        record(&mut self.meter, &mut self.writer, samples)
    }

    /// Completes the file with the first `tail` frames of what was held,
    /// faded out if `fade` (the tail was cut while sounding). Returns how
    /// loud it is and the frames in it.
    fn finish(mut self, tail: usize, fade: bool) -> Result<(Loudness, u64), ExportJobError> {
        let mut held = std::mem::take(&mut self.held);
        held.truncate(tail * self.channels);
        if fade {
            let length = CAP_FADE.min(tail);
            for i in 0..length {
                let gain = 1.0 - (i + 1) as f32 / length as f32;
                let frame = (tail - length + i) * self.channels;
                for sample in &mut held[frame..frame + self.channels] {
                    *sample *= gain;
                }
            }
        }
        self.emit(&held).map_err(ExportJobError::File)?;
        if let Some(mut limiter) = self.limiter.take() {
            // The frames the limiter still holds.
            self.limited.clear();
            limiter.finish(&mut self.limited);
            record(&mut self.meter, &mut self.writer, &self.limited)
                .map_err(ExportJobError::File)?;
        }
        if let Some(writer) = self.writer {
            writer.finish().map_err(ExportJobError::File)?;
        }
        Ok((self.meter.finish(), self.written))
    }
}

/// Measures `samples` and writes them, if there is a file.
fn record(
    meter: &mut LoudnessMeter,
    writer: &mut Option<ExportWriter>,
    samples: &[f32],
) -> Result<(), ExportError> {
    meter.push(samples);
    match writer {
        Some(writer) => writer.write(samples),
        None => Ok(()),
    }
}
