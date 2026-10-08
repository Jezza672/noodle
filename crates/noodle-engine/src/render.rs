//! Rendering without an audio device, as fast as the machine allows.

use std::fmt;

use noodle_core::{Graph, Project};

use crate::{Controller, Diagnostic, Registry, Settings, SettingsError, engine};

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
}
