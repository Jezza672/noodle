//! Rendering without an audio device, as fast as the machine allows.

use noodle_core::Graph;

use crate::{Diagnostic, Registry, Settings, SettingsError, engine};

pub struct Render {
    /// Interleaved, with `settings.channels` channels.
    pub samples: Vec<f32>,
    /// Problems found compiling the graph, as for live playback.
    pub diagnostics: Vec<Diagnostic>,
}

/// Renders `frames` frames of a graph. It runs exactly the engine code that
/// live playback does, just without a device driving it.
pub fn render(
    graph: &Graph,
    registry: &Registry,
    settings: Settings,
    frames: usize,
) -> Result<Render, SettingsError> {
    let (mut controller, mut processor) = engine(settings)?;
    let diagnostics = controller.update(graph, registry);
    let mut samples = vec![0.0; frames * settings.channels];
    processor.process(&mut samples);
    Ok(Render {
        samples,
        diagnostics,
    })
}
