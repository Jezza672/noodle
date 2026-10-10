//! Rendering without an audio device, as fast as the machine allows.

use std::fmt;
use std::sync::{Arc, Mutex};

use noodle_core::{Endpoint, Graph, Project};

use crate::{
    Cancelled, Controller, Diagnostic, Instance, Layout, NodeError, NodeInfo, NodeType, Progress,
    Registry, Replacements, Settings, SettingsError, Setup, SignalIn, TapSpec, engine,
};

pub struct Render {
    /// Interleaved, with `settings.channels` channels.
    pub samples: Vec<f32>,
    /// Problems found compiling the graph, as for live playback.
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RenderError {
    Settings(SettingsError),
    /// The render wouldn't fit in memory, or its length overflows `usize`.
    TooLong {
        frames: usize,
    },
}

impl From<SettingsError> for RenderError {
    fn from(error: SettingsError) -> Self {
        Self::Settings(error)
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Settings(error) => error.fmt(f),
            Self::TooLong { frames } => {
                write!(f, "{frames} frames is too long to render in memory")
            }
        }
    }
}

impl std::error::Error for RenderError {}

/// Renders `frames` frames of a graph. It runs exactly the engine code that
/// live playback does, just without a device driving it.
///
/// A length too large to allocate is an error rather than an abort, since it
/// usually comes from user input.
pub fn render(
    graph: &Graph,
    registry: &Registry,
    settings: Settings,
    frames: usize,
) -> Result<Render, RenderError> {
    render_with(settings, frames, |controller| {
        controller.update(graph, registry)
    })
}

/// [`render`] for a whole project: its tempo map and automation lanes play
/// from the start of the timeline.
pub fn render_project(
    project: &Project,
    registry: &Registry,
    settings: Settings,
    frames: usize,
) -> Result<Render, RenderError> {
    render_with(settings, frames, |controller| {
        controller.update_project(project, registry)
    })
}

/// Why a streaming render stopped early.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StreamError<E> {
    Render(RenderError),
    /// The [`Progress`] was cancelled.
    Cancelled,
    /// The sink refused a chunk.
    Sink(E),
}

impl<E: fmt::Display> fmt::Display for StreamError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Render(error) => error.fmt(f),
            Self::Cancelled => f.write_str("the render was cancelled"),
            Self::Sink(error) => error.fmt(f),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for StreamError<E> {}

impl<E> From<Cancelled> for StreamError<E> {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

/// [`render_project`] that hands the audio to `sink` in chunks of at most
/// `chunk_frames` frames (interleaved) instead of collecting it, so memory
/// stays flat however long the render is. The samples are the same as
/// [`render_project`]'s. `progress` is updated after every chunk, and a
/// cancel stops the render before the next one.
///
/// Returns the compile problems, as [`Render::diagnostics`] does.
pub fn render_project_streaming<E>(
    project: &Project,
    registry: &Registry,
    settings: Settings,
    frames: usize,
    chunk_frames: usize,
    progress: &Progress,
    sink: impl FnMut(&[f32]) -> Result<(), E>,
) -> Result<Vec<Diagnostic>, StreamError<E>> {
    render_project_streaming_replacing(
        project,
        registry,
        settings,
        frames,
        chunk_frames,
        progress,
        &Replacements::new(),
        sink,
    )
}

/// [`render_project_streaming`] with some outputs played from `replacements`
/// (cached renders of frozen and offline nodes), as live playback does after
/// [`Controller::update_project_replacing`]. This is how an export hears what
/// playback hears.
#[allow(clippy::too_many_arguments)]
pub fn render_project_streaming_replacing<E>(
    project: &Project,
    registry: &Registry,
    settings: Settings,
    frames: usize,
    chunk_frames: usize,
    progress: &Progress,
    replacements: &Replacements,
    mut sink: impl FnMut(&[f32]) -> Result<(), E>,
) -> Result<Vec<Diagnostic>, StreamError<E>> {
    let (mut controller, mut processor) =
        engine(settings).map_err(|error| StreamError::Render(error.into()))?;
    let too_long = || StreamError::Render(RenderError::TooLong { frames });
    let chunk_frames = chunk_frames.clamp(1, frames.max(1));
    let len = chunk_frames
        .checked_mul(settings.channels)
        .ok_or_else(too_long)?;
    let mut chunk = Vec::new();
    chunk.try_reserve_exact(len).map_err(|_| too_long())?;
    chunk.resize(len, 0.0);

    let diagnostics = controller.update_project_replacing(project, registry, replacements);
    let mut done = 0;
    progress.report(0.0)?;
    while done < frames {
        let n = chunk_frames.min(frames - done);
        let samples = &mut chunk[..n * settings.channels];
        processor.process(samples);
        sink(samples).map_err(StreamError::Sink)?;
        done += n;
        progress.report(done as f32 / frames as f32)?;
    }
    Ok(diagnostics)
}

/// Receives the signal at a tapped output, one block at a time, in order.
/// A sink can't fail the render, so one that can fail (a file write) keeps
/// the error and the caller looks at it afterwards.
pub type TapSink = Box<dyn FnMut(SignalIn<'_>) + Send>;

/// An output to watch: `endpoint` is a node and output port key in the
/// flattened graph, as [`Analysis`](crate::Analysis) names them.
pub struct Tap {
    pub endpoint: Endpoint,
    pub sink: TapSink,
}

/// Renders the project from the start for `frames` frames, delivering the
/// signal at each tap to its sink instead of mixing it to an output. Only the
/// nodes that feed a tap run, so nothing plays and nothing with side effects
/// is touched. Outputs in `replacements` play from their sources, which is
/// how a render builds on caches that already exist.
///
/// Progress and cancellation work as in [`render_project_streaming`].
pub fn render_taps<E>(
    project: &Project,
    registry: &Registry,
    settings: Settings,
    frames: usize,
    progress: &Progress,
    replacements: &Replacements,
    taps: Vec<Tap>,
) -> Result<Vec<Diagnostic>, StreamError<E>> {
    let (mut controller, mut processor) =
        engine(settings).map_err(|error| StreamError::Render(error.into()))?;
    let too_long = || StreamError::Render(RenderError::TooLong { frames });
    let chunk_frames = settings.max_frames.max(1).min(frames.max(1));
    let len = chunk_frames
        .checked_mul(settings.channels)
        .ok_or_else(too_long)?;
    let mut chunk = vec![0.0; len];

    let (specs, sinks): (Vec<_>, Vec<_>) = taps
        .into_iter()
        .map(|tap| {
            let sink = Arc::new(Mutex::new(Some(tap.sink)));
            let spec = TapSpec {
                endpoint: tap.endpoint,
                node_type: Arc::new(TapType {
                    sink: Arc::clone(&sink),
                }),
            };
            (spec, sink)
        })
        .unzip();
    let diagnostics = controller.update_project_tapping(project, registry, replacements, &specs);
    drop(sinks);
    let mut done = 0;
    progress.report(0.0)?;
    while done < frames {
        let n = chunk_frames.min(frames - done);
        processor.process(&mut chunk[..n * settings.channels]);
        done += n;
        progress.report(done as f32 / frames as f32)?;
    }
    Ok(diagnostics)
}

const TAP_ID: &str = "noodle.internal.tap";

static TAP_INFO: NodeInfo = NodeInfo {
    id: TAP_ID,
    version: 1,
    name: "Tap",
    category: crate::INTERNAL_CATEGORY,
};

/// The sink node a tap becomes. It hands its sink to the one instance made.
struct TapType {
    sink: Arc<Mutex<Option<TapSink>>>,
}

impl NodeType for TapType {
    fn info(&self) -> &NodeInfo {
        &TAP_INFO
    }

    fn layout(&self, _config: &noodle_core::Config) -> Result<Layout, NodeError> {
        Ok(Layout::realtime().input("in", "In"))
    }

    fn instantiate(&self, _setup: &Setup<'_>) -> Result<Instance, NodeError> {
        let sink = self
            .sink
            .lock()
            .expect("tap lock")
            .take()
            .ok_or_else(|| NodeError::config("a tap can only be instantiated once"))?;
        Ok(Instance::realtime(TapNode { sink }))
    }
}

struct TapNode {
    sink: TapSink,
}

impl crate::Node for TapNode {
    fn process(&mut self, _ctx: &crate::Context, io: crate::Io<'_, '_>) {
        (self.sink)(io.inputs[0]);
    }
}

fn render_with(
    settings: Settings,
    frames: usize,
    update: impl FnOnce(&mut Controller) -> Vec<Diagnostic>,
) -> Result<Render, RenderError> {
    let (mut controller, mut processor) = engine(settings)?;
    let too_long = RenderError::TooLong { frames };
    let len = frames.checked_mul(settings.channels).ok_or(too_long)?;
    let mut samples = Vec::new();
    samples.try_reserve_exact(len).map_err(|_| too_long)?;
    samples.resize(len, 0.0);

    let diagnostics = update(&mut controller);
    processor.process(&mut samples);
    Ok(Render {
        samples,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO: Settings = Settings {
        sample_rate: 48_000.0,
        max_frames: 64,
        channels: 2,
    };

    fn render_frames(settings: Settings, frames: usize) -> Result<Render, RenderError> {
        render(
            &Graph::default(),
            &Registry::with_builtins(),
            settings,
            frames,
        )
    }

    #[test]
    fn renders_the_requested_length() {
        let rendered = render_frames(STEREO, 100).unwrap();
        assert_eq!(rendered.samples.len(), 200);
    }

    #[test]
    fn a_length_that_overflows_is_too_long() {
        let frames = usize::MAX / 2 + 1;
        assert_eq!(
            render_frames(STEREO, frames).err(),
            Some(RenderError::TooLong { frames })
        );
    }

    #[test]
    fn a_length_too_large_to_allocate_is_too_long() {
        // Fits in usize as samples, but not as bytes.
        let frames = usize::MAX / 4;
        let mono = Settings {
            channels: 1,
            ..STEREO
        };
        assert_eq!(
            render_frames(mono, frames).err(),
            Some(RenderError::TooLong { frames })
        );
    }

    #[test]
    fn bad_settings_are_reported() {
        let settings = Settings {
            channels: 0,
            ..STEREO
        };
        assert_eq!(
            render_frames(settings, 10).err(),
            Some(RenderError::Settings(SettingsError::Channels))
        );
    }

    #[test]
    fn streaming_delivers_every_frame_in_bounded_chunks() {
        let project = Project::default();
        let progress = Progress::new();
        let mut sizes = Vec::new();
        render_project_streaming(
            &project,
            &Registry::with_builtins(),
            STEREO,
            250,
            100,
            &progress,
            |chunk| -> Result<(), ()> {
                sizes.push(chunk.len());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(sizes, [200, 200, 100]);
        assert_eq!(progress.fraction(), 1.0);
    }

    #[test]
    fn streaming_stops_when_cancelled() {
        let project = Project::default();
        let progress = Progress::new();
        let mut chunks = 0;
        let result = render_project_streaming(
            &project,
            &Registry::with_builtins(),
            STEREO,
            1000,
            10,
            &progress,
            |_| -> Result<(), ()> {
                chunks += 1;
                if chunks == 3 {
                    progress.cancel();
                }
                Ok(())
            },
        );
        assert_eq!(result.err(), Some(StreamError::Cancelled));
        assert_eq!(chunks, 3);
    }

    #[test]
    fn a_failing_sink_stops_the_render() {
        let result = render_project_streaming(
            &Project::default(),
            &Registry::with_builtins(),
            STEREO,
            100,
            10,
            &Progress::new(),
            |_| Err("disk full"),
        );
        assert_eq!(result.err(), Some(StreamError::Sink("disk full")));
    }
}
