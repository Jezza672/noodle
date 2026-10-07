//! Built-in node library: oscillators, filters, mixing, utilities, and views
//! (meters and scopes) that report to the UI.

mod gain;
mod meter;
mod mix;
mod noise;
mod osc;
mod reverse;
mod scope;
mod svf;
mod voice_mix;

pub use gain::Gain;
pub use meter::Meter;
pub use mix::Mix;
pub use noise::WhiteNoise;
pub use osc::{Saw, Sine};
pub use reverse::Reverse;
pub use scope::Scope;
pub use svf::Svf;
pub use voice_mix::VoiceMix;

use noodle_engine::{Registry, Telemetry};

/// Registers every built-in node type. Returns the telemetry hub that the
/// view nodes (Meter, Scope) report to, for the UI to read. Without it, they
/// still run but nothing can read them.
#[must_use = "the view nodes report to this hub; keep it to read them"]
pub fn register_all(registry: &mut Registry) -> Telemetry {
    let telemetry = Telemetry::new();
    registry.register(Sine);
    registry.register(Saw);
    registry.register(WhiteNoise);
    registry.register(Gain);
    registry.register(Mix);
    registry.register(Svf);
    registry.register(VoiceMix);
    registry.register(Reverse);
    registry.register(Meter::new(&telemetry));
    registry.register(Scope::new(&telemetry));
    telemetry
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_are_unique() {
        // `register` panics on a duplicate ID.
        let _ = super::register_all(&mut noodle_engine::Registry::new());
    }
}
