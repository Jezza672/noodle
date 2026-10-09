//! Built-in node library: oscillators, filters, mixing, utilities, and views
//! (meters and scopes) that report to the UI.

mod button;
mod clip;
mod gain;
mod meter;
mod metronome;
mod mix;
mod noise;
mod osc;
mod reroute;
mod reverse;
mod scope;
mod stage;
mod svf;
mod voice_mix;

pub use button::{BUTTON_ID, BUTTON_STATE, Button};
pub use clip::{
    ClipFeeds, ClipProblem, ClipSource, ClipStatus, FileError, Schedule, ScheduledClip,
    TRACK_INPUT_ID, TrackInput, active_at,
};
pub use gain::Gain;
pub use meter::{METER_ID, Meter};
pub use metronome::{METRONOME_ID, METRONOME_ON, Metronome};
pub use mix::Mix;
pub use noise::WhiteNoise;
pub use osc::{Saw, Sine};
pub use reroute::{REROUTE_ID, Reroute};
pub use reverse::Reverse;
pub use scope::{SCOPE_ID, Scope};
pub use stage::GroupStage;
pub use svf::Svf;
pub use voice_mix::VoiceMix;

use std::path::Path;

use noodle_core::Project;
use noodle_engine::{
    Registry, Render, RenderError, Settings, Telemetry, TempoTable, render_project,
};

/// What [`register_library`] gives back.
pub struct Library {
    /// The hub the view nodes (Meter, Scope) report to, for the UI to read.
    /// Without it, they still run but nothing can read them.
    pub telemetry: Telemetry,
    /// Where track input nodes get their clips; call
    /// [`ClipFeeds::update`] when clips or the tempo map change.
    pub clips: ClipFeeds,
}

/// Registers every built-in node type.
#[must_use = "the view and track input nodes report to and read from these"]
pub fn register_library(registry: &mut Registry) -> Library {
    register_with(registry, ClipFeeds::default())
}

/// [`register_library`] for rendering offline: the track inputs wait for the
/// disk instead of dropping audio, so the output is the same every time. See
/// [`ClipFeeds::blocking`].
#[must_use = "the track input nodes read from these"]
pub fn register_library_blocking(registry: &mut Registry) -> Library {
    register_with(registry, ClipFeeds::blocking())
}

fn register_with(registry: &mut Registry, clips: ClipFeeds) -> Library {
    let telemetry = Telemetry::new();
    registry.register(Sine);
    registry.register(Saw);
    registry.register(WhiteNoise);
    registry.register(Gain);
    registry.register(Button);
    registry.register(Metronome);
    registry.register(Mix);
    registry.register(Svf);
    registry.register(VoiceMix);
    registry.register(Reverse);
    registry.register(Reroute);
    registry.register(GroupStage);
    registry.register(Meter::new(&telemetry));
    registry.register(Scope::new(&telemetry));
    registry.register(TrackInput::new(&clips));
    Library { telemetry, clips }
}

/// Like [`register_library`], for callers with no use for track clips.
/// Returns the telemetry hub that the view nodes report to.
#[must_use = "the view nodes report to this hub; keep it to read them"]
pub fn register_all(registry: &mut Registry) -> Telemetry {
    register_library(registry).telemetry
}

/// A render of a project with audio clips.
pub struct ClipRender {
    pub render: Render,
    /// Clips that couldn't be scheduled, and why. They are left out.
    pub problems: Vec<ClipProblem>,
    /// Blocks where a clip's audio was missing even after waiting (the disk
    /// stopped answering). Zero in a render that went as it should.
    pub underruns: u64,
    /// Files that couldn't be opened during the render; their clips were
    /// left silent from then on.
    pub errors: Vec<FileError>,
}

/// Renders `frames` frames of `project` offline, with its tempo map,
/// automation lanes and audio clips (files are looked up relative to
/// `base`, normally the folder of the project file). The track inputs wait
/// for the disk, so the same project renders to the same samples every time.
///
/// `registry` should hold the built-in nodes; the library nodes are added to
/// it, so give each call a registry of its own (registering twice panics).
/// A clip whose file can't be read is left silent for the rest of the render
/// rather than waited on, and counted in `underruns`.
pub fn render_project_with_clips(
    project: &Project,
    registry: &mut Registry,
    base: &Path,
    settings: Settings,
    frames: usize,
) -> Result<ClipRender, RenderError> {
    let library = register_library_blocking(registry);
    let rate = settings.sample_rate.round() as u32;
    let table = TempoTable::new(project.tempo_map(), settings.sample_rate);
    let problems = library.clips.update(project, &table, rate, base);
    let render = render_project(project, registry, settings, frames)?;
    Ok(ClipRender {
        render,
        problems,
        underruns: library.clips.underruns(),
        errors: library.clips.errors(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_are_unique() {
        // `register` panics on a duplicate ID.
        let _ = super::register_all(&mut noodle_engine::Registry::new());
    }
}
