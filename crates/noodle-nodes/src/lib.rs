//! Built-in node library: oscillators, filters, mixing, utilities.

mod gain;
mod mix;
mod osc;
mod reverse;
mod svf;
mod voice_mix;

pub use gain::Gain;
pub use mix::Mix;
pub use osc::Sine;
pub use reverse::Reverse;
pub use svf::Svf;
pub use voice_mix::VoiceMix;

use noodle_engine::Registry;

/// Registers every built-in node type.
pub fn register_all(registry: &mut Registry) {
    registry.register(Sine);
    registry.register(Gain);
    registry.register(Mix);
    registry.register(Svf);
    registry.register(VoiceMix);
    registry.register(Reverse);
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_are_unique() {
        // `register` panics on a duplicate ID.
        super::register_all(&mut noodle_engine::Registry::new());
    }
}
