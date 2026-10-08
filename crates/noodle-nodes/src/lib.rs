//! Built-in node library: oscillators, filters, mixing, utilities, and views
//! (meters and scopes) that report to the UI.

mod clip;
mod gain;
mod meter;
mod mix;
mod noise;
mod osc;
mod reroute;
mod reverse;
mod scope;
mod svf;
mod voice_mix;

pub use clip::{
    ClipFeeds, ClipProblem, ClipSource, ClipStatus, Schedule, ScheduledClip, TRACK_INPUT_ID,
    TrackInput, active_at,
};
pub use gain::Gain;
pub use meter::{METER_ID, Meter};
pub use mix::Mix;
pub use noise::WhiteNoise;
pub use osc::{Saw, Sine};
pub use reroute::{REROUTE_ID, Reroute};
pub use reverse::Reverse;
pub use scope::{SCOPE_ID, Scope};
pub use svf::Svf;
pub use voice_mix::VoiceMix;

use noodle_engine::{Registry, Telemetry};

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
    let telemetry = Telemetry::new();
    let clips = ClipFeeds::default();
    registry.register(Sine);
    registry.register(Saw);
    registry.register(WhiteNoise);
    registry.register(Gain);
    registry.register(Mix);
    registry.register(Svf);
    registry.register(VoiceMix);
    registry.register(Reverse);
    registry.register(Reroute);
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

#[cfg(test)]
mod tests {
    #[test]
    fn ids_are_unique() {
        // `register` panics on a duplicate ID.
        let _ = super::register_all(&mut noodle_engine::Registry::new());
    }
}
