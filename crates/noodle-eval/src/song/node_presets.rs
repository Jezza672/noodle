//! The system library's node presets: a few useful settings for each built-in
//! node, written as a table. `files()` in `system.rs` turns them into
//! `library/system/nodes.ron`.
//!
//! Compressor, Limiter, Gate and Expander are presets of the one Dynamics node,
//! not groups.
//! The Sampler's presets leave its loaded sample alone.

use noodle_core::library::{Item, NodePreset, slugify};
use noodle_core::{Config, Node, Value};
use noodle_engine::Registry;

/// One preset: a node type, a name, config and parameter values. Anything
/// not listed stays at its default.
struct Row {
    node: &'static str,
    name: &'static str,
    config: Config,
    params: &'static [(&'static str, f32)],
}

fn row(node: &'static str, name: &'static str, params: &'static [(&'static str, f32)]) -> Row {
    Row {
        node,
        name,
        config: Config::new(),
        params,
    }
}

impl Row {
    fn with(mut self, key: &str, value: Value) -> Self {
        self.config.set(key, value);
        self
    }

    fn text(self, key: &str, text: &str) -> Self {
        self.with(key, Value::Text(text.into()))
    }

    fn int(self, key: &str, value: i64) -> Self {
        self.with(key, Value::Int(value))
    }
}

fn table() -> Vec<Row> {
    vec![
        // Oscillators.
        row("noodle.osc.sine", "Sub 55 Hz", &[("frequency", 55.0)]),
        row("noodle.osc.sine", "Concert A", &[("frequency", 440.0)]),
        row("noodle.osc.sine", "Slow Wobble", &[("frequency", 0.5)]),
        row("noodle.osc.saw", "Low Saw", &[("frequency", 110.0)]),
        row("noodle.osc.saw", "Concert A", &[("frequency", 440.0)]),
        row(
            "noodle.osc.triangle",
            "Low Triangle",
            &[("frequency", 110.0)],
        ),
        row("noodle.osc.triangle", "Concert A", &[("frequency", 440.0)]),
        row(
            "noodle.osc.square",
            "Hollow Square",
            &[("frequency", 220.0), ("width", 0.5)],
        ),
        row(
            "noodle.osc.square",
            "Pulse 25%",
            &[("frequency", 220.0), ("width", 0.25)],
        ),
        row(
            "noodle.osc.square",
            "Thin Pulse",
            &[("frequency", 220.0), ("width", 0.1)],
        ),
        // Filters.
        row(
            "noodle.filter.svf",
            "Warm Low Pass",
            &[("cutoff", 800.0), ("resonance", 0.1)],
        ),
        row(
            "noodle.filter.svf",
            "Bright Low Pass",
            &[("cutoff", 5000.0), ("resonance", 0.15)],
        ),
        row(
            "noodle.filter.svf",
            "Resonant Peak",
            &[("cutoff", 1200.0), ("resonance", 0.8)],
        ),
        row(
            "noodle.filter.ladder",
            "Mellow Ladder",
            &[("cutoff", 600.0), ("resonance", 0.1), ("drive", 1.0)],
        ),
        row(
            "noodle.filter.ladder",
            "Acid Squelch",
            &[("cutoff", 700.0), ("resonance", 0.75), ("drive", 2.5)],
        ),
        row(
            "noodle.filter.ladder",
            "Driven Open",
            &[("cutoff", 6000.0), ("resonance", 0.2), ("drive", 4.0)],
        ),
        // Modulation.
        row(
            "noodle.mod.adsr",
            "Pluck",
            &[
                ("attack", 0.002),
                ("decay", 0.18),
                ("sustain", 0.0),
                ("release", 0.1),
            ],
        ),
        row(
            "noodle.mod.adsr",
            "Organ",
            &[
                ("attack", 0.005),
                ("decay", 0.05),
                ("sustain", 1.0),
                ("release", 0.05),
            ],
        ),
        row(
            "noodle.mod.adsr",
            "Slow Pad",
            &[
                ("attack", 0.8),
                ("decay", 0.5),
                ("sustain", 0.8),
                ("release", 1.5),
            ],
        ),
        row(
            "noodle.mod.adsr",
            "Stab",
            &[
                ("attack", 0.001),
                ("decay", 0.25),
                ("sustain", 0.3),
                ("release", 0.15),
            ],
        ),
        row(
            "noodle.mod.lfo",
            "Vibrato",
            &[("rate", 5.5), ("depth", 0.05), ("shape", 0.0)],
        ),
        row(
            "noodle.mod.lfo",
            "Tremolo",
            &[
                ("rate", 6.0),
                ("depth", 0.5),
                ("offset", 0.0),
                ("shape", 1.0),
            ],
        ),
        row(
            "noodle.mod.lfo",
            "Slow Sweep",
            &[("rate", 0.1), ("depth", 1.0), ("shape", 0.0)],
        ),
        row(
            "noodle.mod.lfo",
            "Sample and Hold Noise",
            &[("rate", 8.0), ("depth", 1.0), ("shape", 4.0)],
        ),
        row(
            "noodle.mod.envelope",
            "Pluck",
            &[("length", 0.3), ("time", 0.0)],
        ),
        row(
            "noodle.mod.envelope",
            "Swell",
            &[("length", 2.0), ("time", 0.0)],
        )
        .text("curve", "0 0 0.5;0.85 1 -0.6;1 0 0"),
        row(
            "noodle.mod.envelope",
            "Stab",
            &[("length", 0.6), ("time", 0.0)],
        )
        .text("curve", "0 0 0;0.02 1 -0.4;0.5 0.5 -0.5;1 0 0"),
        row(
            "noodle.mod.envelope",
            "Riser, 4 Beats",
            &[("beats", 4.0), ("time", 1.0)],
        )
        .text("curve", "0 0 0.6;1 1 0"),
        row(
            "noodle.mod.envelope",
            "Faller, 1 Beat",
            &[("beats", 1.0), ("time", 1.0)],
        )
        .text("curve", "0 1 -0.7;1 0 0"),
        row(
            "noodle.mod.envelope",
            "Pump, 1 Beat",
            &[("beats", 1.0), ("time", 1.0)],
        )
        .text("curve", "0 0 0.8;1 1 0"),
        row(
            "noodle.mod.envelope",
            "Sweep, 2 Beats",
            &[("beats", 2.0), ("time", 1.0)],
        )
        .text("curve", "0 0 0.4;0.3 1 -0.6;1 0.2 0"),
        row(
            "noodle.mod.envelope",
            "Looping Swell, Half Beat",
            &[("beats", 0.5), ("time", 1.0), ("loop", 1.0)],
        )
        .text("curve", "0 0 0;0.5 1 0;1 0 0"),
        row(
            "noodle.mod.follower",
            "Fast Peak",
            &[("attack", 0.001), ("release", 0.05), ("mode", 0.0)],
        ),
        row(
            "noodle.mod.follower",
            "Smooth RMS",
            &[("attack", 0.02), ("release", 0.25), ("mode", 1.0)],
        ),
        row("noodle.mod.lag", "Glide", &[("rise", 0.08), ("fall", 0.08)]),
        row(
            "noodle.mod.lag",
            "Slow Rise, Fast Fall",
            &[("rise", 0.5), ("fall", 0.05)],
        ),
        row(
            "noodle.mod.lag",
            "Slew Smoother",
            &[("rise", 0.01), ("fall", 0.01)],
        ),
        row(
            "noodle.mod.comparator",
            "Zero Crossing",
            &[("threshold", 0.0), ("hysteresis", 0.02)],
        ),
        row(
            "noodle.mod.comparator",
            "Half Level Gate",
            &[("threshold", 0.5), ("hysteresis", 0.1)],
        ),
        row(
            "noodle.mod.samplehold",
            "Sample on Trigger",
            &[("mode", 0.0)],
        ),
        row("noodle.mod.samplehold", "Track and Hold", &[("mode", 1.0)]),
        row(
            "noodle.generator.clock",
            "Quarter Notes",
            &[("rate", 2.0), ("width", 0.5)],
        ),
        row(
            "noodle.generator.clock",
            "Eighth Notes, Short",
            &[("rate", 3.0), ("width", 0.2)],
        ),
        row(
            "noodle.generator.clock",
            "Sixteenth Notes",
            &[("rate", 4.0), ("width", 0.5)],
        ),
        row(
            "noodle.generator.clock",
            "Triplet Eighths",
            &[("rate", 8.0), ("width", 0.5)],
        ),
        row(
            "noodle.generator.clock",
            "Bar",
            &[("rate", 0.0), ("width", 0.1)],
        ),
        row("noodle.mod.tempo", "Quarter Notes", &[("division", 2.0)]),
        row("noodle.mod.tempo", "Eighth Notes", &[("division", 3.0)]),
        row("noodle.mod.tempo", "Dotted Eighths", &[("division", 14.0)]),
        row("noodle.mod.tempo", "Bar", &[("division", 0.0)]),
        row(
            "noodle.mod.beat_length",
            "Quarter Notes",
            &[("bpm", 120.0), ("division", 2.0)],
        ),
        row(
            "noodle.mod.beat_length",
            "Dotted Eighths",
            &[("bpm", 120.0), ("division", 14.0)],
        ),
        // Effects.
        // Dynamics.
        row(
            "noodle.effect.dynamics",
            "Compressor",
            &[
                ("mode", 0.0),
                ("threshold", -18.0),
                ("ratio", 4.0),
                ("attack", 0.01),
                ("release", 0.1),
                ("knee", 6.0),
                ("makeup", 0.0),
            ],
        ),
        row(
            "noodle.effect.dynamics",
            "Vocal Compressor",
            &[
                ("mode", 0.0),
                ("threshold", -20.0),
                ("ratio", 3.0),
                ("attack", 0.005),
                ("release", 0.12),
                ("knee", 8.0),
                ("makeup", 4.0),
            ],
        ),
        row(
            "noodle.effect.dynamics",
            "Bus Glue",
            &[
                ("mode", 0.0),
                ("threshold", -14.0),
                ("ratio", 2.0),
                ("attack", 0.03),
                ("release", 0.2),
                ("knee", 10.0),
                ("makeup", 1.5),
            ],
        ),
        row(
            "noodle.effect.dynamics",
            "Limiter",
            &[
                ("mode", 1.0),
                ("threshold", -1.0),
                ("ratio", 100.0),
                ("attack", 0.0005),
                ("release", 0.05),
                ("knee", 0.0),
                ("makeup", 0.0),
            ],
        ),
        row(
            "noodle.effect.dynamics",
            "Gate",
            &[
                ("mode", 2.0),
                ("threshold", -50.0),
                ("ratio", 10.0),
                ("attack", 0.002),
                ("release", 0.1),
                ("knee", 0.0),
                ("makeup", 0.0),
            ],
        ),
        row(
            "noodle.effect.dynamics",
            "Drum Gate",
            &[
                ("mode", 2.0),
                ("threshold", -40.0),
                ("ratio", 10.0),
                ("attack", 0.0005),
                ("release", 0.06),
                ("knee", 0.0),
                ("makeup", 0.0),
            ],
        ),
        row(
            "noodle.effect.dynamics",
            "Expander",
            &[
                ("mode", 3.0),
                ("threshold", -45.0),
                ("ratio", 2.0),
                ("attack", 0.01),
                ("release", 0.15),
                ("knee", 6.0),
                ("makeup", 0.0),
            ],
        ),
        row(
            "noodle.effect.dynamics",
            "De-noise",
            &[
                ("mode", 3.0),
                ("threshold", -55.0),
                ("ratio", 1.6),
                ("attack", 0.02),
                ("release", 0.3),
                ("knee", 12.0),
                ("makeup", 0.0),
            ],
        ),
        row(
            "noodle.effect.distortion",
            "Warm Overdrive",
            &[
                ("drive", 10.0),
                ("shape", 0.0),
                ("level", -6.0),
                ("mix", 1.0),
            ],
        ),
        row(
            "noodle.effect.distortion",
            "Hard Fuzz",
            &[
                ("drive", 28.0),
                ("shape", 1.0),
                ("level", -12.0),
                ("mix", 1.0),
            ],
        ),
        row(
            "noodle.effect.distortion",
            "Wavefolder",
            &[
                ("drive", 18.0),
                ("shape", 2.0),
                ("level", -10.0),
                ("mix", 1.0),
            ],
        ),
        row(
            "noodle.effect.distortion",
            "Subtle Grit",
            &[
                ("drive", 6.0),
                ("shape", 3.0),
                ("level", -3.0),
                ("mix", 0.4),
            ],
        ),
        row(
            "noodle.effect.bitcrusher",
            "Lo-fi",
            &[("bits", 10.0), ("rate", 18000.0), ("mix", 1.0)],
        ),
        row(
            "noodle.effect.bitcrusher",
            "8-bit",
            &[("bits", 8.0), ("rate", 11025.0), ("mix", 1.0)],
        ),
        row(
            "noodle.effect.bitcrusher",
            "Crunch",
            &[("bits", 4.0), ("rate", 8000.0), ("mix", 0.6)],
        ),
        row(
            "noodle.effect.chorus",
            "Classic Chorus",
            &[
                ("rate", 0.6),
                ("depth", 3.0),
                ("delay", 12.0),
                ("feedback", 0.0),
                ("mix", 0.5),
            ],
        ),
        row(
            "noodle.effect.chorus",
            "Wide Shimmer",
            &[
                ("rate", 0.3),
                ("depth", 6.0),
                ("delay", 18.0),
                ("feedback", 0.2),
                ("mix", 0.6),
            ],
        ),
        row(
            "noodle.effect.chorus",
            "Flanger",
            &[
                ("rate", 0.25),
                ("depth", 2.0),
                ("delay", 2.0),
                ("feedback", 0.7),
                ("mix", 0.5),
            ],
        ),
        row(
            "noodle.effect.phaser",
            "Slow Phaser",
            &[
                ("rate", 0.2),
                ("depth", 0.7),
                ("centre", 600.0),
                ("stages", 1.0),
                ("feedback", 0.3),
                ("mix", 0.5),
            ],
        ),
        row(
            "noodle.effect.phaser",
            "Jet Sweep",
            &[
                ("rate", 0.08),
                ("depth", 1.0),
                ("centre", 1200.0),
                ("stages", 3.0),
                ("feedback", 0.7),
                ("mix", 0.6),
            ],
        ),
        row(
            "noodle.effect.phaser",
            "Fast Swirl",
            &[
                ("rate", 3.0),
                ("depth", 0.5),
                ("centre", 900.0),
                ("stages", 0.0),
                ("feedback", 0.1),
                ("mix", 0.5),
            ],
        ),
        row(
            "noodle.effect.reverb",
            "Small Room",
            &[
                ("size", 0.6),
                ("decay", 0.6),
                ("damping", 0.5),
                ("predelay", 0.005),
                ("width", 0.8),
                ("mix", 0.2),
            ],
        ),
        row(
            "noodle.effect.reverb",
            "Large Hall",
            &[
                ("size", 1.2),
                ("decay", 3.5),
                ("damping", 0.4),
                ("predelay", 0.025),
                ("width", 1.0),
                ("mix", 0.3),
            ],
        ),
        row(
            "noodle.effect.reverb",
            "Dark Plate",
            &[
                ("size", 0.9),
                ("decay", 2.0),
                ("damping", 0.8),
                ("predelay", 0.01),
                ("width", 1.0),
                ("mix", 0.3),
            ],
        ),
        row(
            "noodle.effect.reverb",
            "Endless Wash",
            &[
                ("size", 1.5),
                ("decay", 12.0),
                ("damping", 0.3),
                ("predelay", 0.04),
                ("width", 1.0),
                ("mix", 0.5),
            ],
        ),
        row("noodle.util.delay", "Slapback", &[("time", 0.09)]),
        row("noodle.util.delay", "Eighth at 120 bpm", &[("time", 0.25)]),
        row("noodle.util.delay", "Quarter at 120 bpm", &[("time", 0.5)]),
        row("noodle.util.delay", "Long Echo", &[("time", 1.0)]),
        // Parametric EQ, four bands.
        row(
            "noodle.effect.eq",
            "Low Cut and Presence",
            &[
                ("type1", 3.0),
                ("freq1", 80.0),
                ("q1", 0.707),
                ("type2", 0.0),
                ("freq2", 400.0),
                ("gain2", 0.0),
                ("type3", 0.0),
                ("freq3", 3000.0),
                ("gain3", 2.5),
                ("q3", 0.8),
                ("type4", 2.0),
                ("freq4", 9000.0),
                ("gain4", 1.5),
            ],
        )
        .int("bands", 4),
        row(
            "noodle.effect.eq",
            "Scoop Mids",
            &[
                ("type1", 1.0),
                ("freq1", 100.0),
                ("gain1", 2.0),
                ("type2", 0.0),
                ("freq2", 500.0),
                ("gain2", -4.0),
                ("q2", 0.8),
                ("type3", 0.0),
                ("freq3", 1600.0),
                ("gain3", -2.0),
                ("q3", 0.8),
                ("type4", 2.0),
                ("freq4", 8000.0),
                ("gain4", 2.5),
            ],
        )
        .int("bands", 4),
        row(
            "noodle.effect.eq",
            "Telephone",
            &[
                ("type1", 3.0),
                ("freq1", 400.0),
                ("q1", 0.9),
                ("type2", 0.0),
                ("freq2", 1000.0),
                ("gain2", 4.0),
                ("q2", 1.2),
                ("type3", 0.0),
                ("freq3", 2000.0),
                ("gain3", 3.0),
                ("q3", 1.2),
                ("type4", 4.0),
                ("freq4", 3500.0),
                ("q4", 0.9),
            ],
        )
        .int("bands", 4),
        row(
            "noodle.effect.eq",
            "Warm and Smooth",
            &[
                ("type1", 1.0),
                ("freq1", 150.0),
                ("gain1", 2.5),
                ("type2", 0.0),
                ("freq2", 350.0),
                ("gain2", -1.5),
                ("q2", 0.8),
                ("type3", 0.0),
                ("freq3", 3500.0),
                ("gain3", -1.5),
                ("q3", 0.8),
                ("type4", 4.0),
                ("freq4", 12000.0),
                ("q4", 0.707),
            ],
        )
        .int("bands", 4),
        // Events.
        row(
            "noodle.event.arpeggiator",
            "Up Eighths",
            &[
                ("pattern", 0.0),
                ("rate", 1.0),
                ("octaves", 0.0),
                ("gate", 0.6),
            ],
        ),
        row(
            "noodle.event.arpeggiator",
            "Up-Down Sixteenths",
            &[
                ("pattern", 2.0),
                ("rate", 3.0),
                ("octaves", 1.0),
                ("gate", 0.5),
            ],
        ),
        row(
            "noodle.event.arpeggiator",
            "Trance Gate",
            &[
                ("pattern", 0.0),
                ("rate", 3.0),
                ("octaves", 2.0),
                ("gate", 0.3),
            ],
        ),
        row(
            "noodle.event.arpeggiator",
            "Swung Random",
            &[
                ("pattern", 5.0),
                ("rate", 3.0),
                ("octaves", 1.0),
                ("gate", 0.5),
                ("swing", 0.4),
            ],
        ),
        row(
            "noodle.event.arpeggiator",
            "Held Down Eighths",
            &[
                ("pattern", 1.0),
                ("rate", 1.0),
                ("octaves", 0.0),
                ("gate", 0.8),
                ("hold", 1.0),
            ],
        ),
        row(
            "noodle.event.note_split",
            "Keyboard Split at C3",
            &[("high1", 59.0), ("low2", 60.0)],
        )
        .int("outputs", 2),
        row(
            "noodle.event.note_split",
            "Velocity Layers",
            &[("vel_high1", 0.6), ("vel_low2", 0.6)],
        )
        .int("outputs", 2),
        row(
            "noodle.event.note_split",
            "Octave Layer Up",
            &[("transpose2", 12.0)],
        )
        .int("outputs", 2),
        row(
            "noodle.event.note_split",
            "Low Notes and Rest",
            &[("high1", 47.0)],
        )
        .int("outputs", 1)
        .int("rest", 1),
        row(
            "noodle.event.key",
            "Middle C",
            &[("note", 60.0), ("velocity", 0.8)],
        ),
        row(
            "noodle.event.key",
            "Bass C",
            &[("note", 36.0), ("velocity", 0.9)],
        ),
        row(
            "noodle.event.key",
            "Soft High C",
            &[("note", 84.0), ("velocity", 0.5)],
        ),
        row("noodle.event.midi_in", "All Channels", &[("channel", 0.0)]),
        row("noodle.event.midi_in", "Channel 1", &[("channel", 1.0)]),
        row(
            "noodle.event.midi_in",
            "Channel 10 (Drums)",
            &[("channel", 10.0)],
        ),
        // Instrument.
        row(
            "noodle.instrument.sampler",
            "One-shot",
            &[("mode", 0.0), ("loop", 0.0), ("release", 0.02)],
        ),
        row(
            "noodle.instrument.sampler",
            "Gated Loop",
            &[("mode", 1.0), ("loop", 1.0), ("release", 0.15)],
        ),
        row(
            "noodle.instrument.sampler",
            "Pitched Hit",
            &[("mode", 0.0), ("keytrack", 1.0), ("release", 0.05)],
        ),
        row(
            "noodle.instrument.sampler",
            "Fixed Pitch Hit",
            &[("mode", 0.0), ("keytrack", 0.0), ("release", 0.05)],
        ),
        // Voices and mixing.
        row(
            "noodle.poly.voices",
            "Eight Voices",
            &[("tail", 10.0), ("steal_fade", 0.005)],
        )
        .int("voices", 8),
        row(
            "noodle.poly.voices",
            "Four Voices, Quick Steal",
            &[("tail", 4.0), ("steal_fade", 0.002)],
        )
        .int("voices", 4),
        row(
            "noodle.poly.voices",
            "Sixteen Voices, Long Tails",
            &[("tail", 20.0), ("steal_fade", 0.01)],
        )
        .int("voices", 16),
        row("noodle.util.gain", "Unity", &[("gain", 0.0)]),
        row("noodle.util.gain", "Minus 6 dB", &[("gain", -6.0)]),
        row(
            "noodle.util.gain",
            "Headroom, Minus 12 dB",
            &[("gain", -12.0)],
        ),
        row("noodle.util.gain", "Boost 6 dB", &[("gain", 6.0)]),
        row("noodle.util.pan", "Centre", &[("pan", 0.0)]),
        row("noodle.util.pan", "Left", &[("pan", -0.7)]),
        row("noodle.util.pan", "Right", &[("pan", 0.7)]),
        row(
            "noodle.util.surround_pan",
            "Front Centre",
            &[("azimuth", 0.0)],
        ),
        row("noodle.util.surround_pan", "Behind", &[("azimuth", 180.0)]),
        row(
            "noodle.util.surround_pan",
            "Left Surround",
            &[("azimuth", -110.0)],
        ),
        row(
            "noodle.util.surround_pan",
            "Right Surround",
            &[("azimuth", 110.0)],
        ),
        row(
            "noodle.util.surround_pan",
            "Everywhere",
            &[("spread", 1.0), ("lfe", -12.0)],
        ),
        row("noodle.util.convert_layout", "To Mono", &[]).text("layout", "Mono"),
        row("noodle.util.convert_layout", "To Stereo", &[]).text("layout", "Stereo"),
        row("noodle.util.convert_layout", "To Quad", &[]).text("layout", "Quad"),
        row("noodle.util.convert_layout", "To 5.1", &[]).text("layout", "5.1"),
        row("noodle.util.convert_layout", "To 7.1", &[]).text("layout", "7.1"),
        row("noodle.util.vca", "Full Level", &[("level", 1.0)]),
        row("noodle.util.vca", "Half Level", &[("level", 0.5)]),
        row(
            "noodle.util.metronome",
            "Quiet Click",
            &[
                ("on", 1.0),
                ("level", -18.0),
                ("pitch", 1000.0),
                ("accent", 1.0),
            ],
        ),
        row(
            "noodle.util.metronome",
            "Loud High Click",
            &[
                ("on", 1.0),
                ("level", -6.0),
                ("pitch", 2000.0),
                ("accent", 1.0),
            ],
        ),
        row(
            "noodle.util.metronome",
            "Even Click",
            &[
                ("on", 1.0),
                ("level", -12.0),
                ("pitch", 1500.0),
                ("accent", 0.0),
            ],
        ),
        // Math expressions.
        row(
            "noodle.util.math",
            "Scale and Offset",
            &[("b", 1.0), ("c", 0.0)],
        )
        .text("expr", "a * b + c"),
        row("noodle.util.math", "Invert", &[]).text("expr", "1 - a"),
        row("noodle.util.math", "Bipolar to Unipolar", &[]).text("expr", "a * 0.5 + 0.5"),
        row("noodle.util.math", "Crossfade", &[("c", 0.5)]).text("expr", "mix(a, b, c)"),
        row("noodle.util.math", "Soft Clip", &[("b", 2.0)]).text("expr", "tanh(a * b)"),
        row("noodle.util.math", "Rectify", &[]).text("expr", "abs(a)"),
        row("noodle.util.math", "Semitones to Ratio", &[]).text("expr", "2 ^ (a / 12)"),
        row("noodle.util.math", "Clamp", &[("b", -1.0), ("c", 1.0)]).text("expr", "clamp(a, b, c)"),
        // Remap.
        row(
            "noodle.util.remap",
            "Unipolar to Bipolar",
            &[
                ("in_min", 0.0),
                ("in_max", 1.0),
                ("out_min", -1.0),
                ("out_max", 1.0),
            ],
        ),
        row(
            "noodle.util.remap",
            "Invert",
            &[
                ("in_min", 0.0),
                ("in_max", 1.0),
                ("out_min", 1.0),
                ("out_max", 0.0),
            ],
        ),
        row(
            "noodle.util.remap",
            "Cutoff Range",
            &[
                ("in_min", 0.0),
                ("in_max", 1.0),
                ("out_min", 200.0),
                ("out_max", 8000.0),
            ],
        ),
        // Spectral.
        row(
            "noodle.spectral.gate",
            "Denoise",
            &[("threshold", -55.0), ("reduction", 30.0), ("knee", 9.0)],
        ),
        row(
            "noodle.spectral.gate",
            "Loudest Partials Only",
            &[("threshold", -25.0), ("reduction", 80.0), ("knee", 6.0)],
        ),
        row(
            "noodle.spectral.filter",
            "Telephone Band",
            &[("low", 300.0), ("high", 3400.0), ("mode", 0.0)],
        ),
        row(
            "noodle.spectral.filter",
            "Notch Out the Mids",
            &[("low", 800.0), ("high", 2500.0), ("mode", 1.0)],
        ),
        row(
            "noodle.spectral.filter",
            "Low End Only",
            &[("low", 20.0), ("high", 250.0), ("mode", 0.0)],
        ),
        row("noodle.spectral.shift", "One Bin Up", &[("shift", 24.0)]),
        row("noodle.spectral.shift", "Bell-like", &[("shift", 90.0)]),
        row(
            "noodle.spectral.shift",
            "Down a Little",
            &[("shift", -40.0)],
        ),
        row("noodle.spectral.freeze", "Held", &[("freeze", 1.0)]),
        row("noodle.spectral.freeze", "Live", &[("freeze", 0.0)]),
        row("noodle.spectral.blur", "Soften", &[("amount", 0.5)]),
        row("noodle.spectral.blur", "Haze", &[("amount", 0.9)]),
        row("noodle.spectral.blur", "Wash", &[("amount", 0.98)]),
        row("noodle.spectral.cross", "Full Vocode", &[("mix", 1.0)]),
        row("noodle.spectral.cross", "Half and Half", &[("mix", 0.5)]),
        // Offline.
        row(
            "noodle.offline.normalize",
            "Peak to Minus 1 dB",
            &[("level", -1.0), ("mode", 0.0)],
        ),
        row(
            "noodle.offline.normalize",
            "Peak to Zero",
            &[("level", 0.0), ("mode", 0.0)],
        ),
        row(
            "noodle.offline.normalize",
            "Loudness to Minus 16 dB RMS",
            &[("level", -16.0), ("mode", 1.0)],
        ),
        row(
            "noodle.offline.time_stretch",
            "Half Speed",
            &[("ratio", 2.0), ("grain", 0.04)],
        ),
        row(
            "noodle.offline.time_stretch",
            "Double Speed",
            &[("ratio", 0.5), ("grain", 0.04)],
        ),
        row(
            "noodle.offline.time_stretch",
            "Slight Slowdown, Long Grain",
            &[("ratio", 1.1), ("grain", 0.08)],
        ),
    ]
}

/// The node presets, each captured from a node built from its row.
pub fn presets(registry: &Registry) -> Vec<Item> {
    table()
        .into_iter()
        .map(|row| {
            let mut node = Node::new(row.node);
            node.config = row.config;
            node.params = row
                .params
                .iter()
                .map(|&(key, value)| (key.to_string(), value))
                .collect();
            let node_name = registry
                .get(row.node)
                .unwrap_or_else(|| panic!("{} is registered", row.node))
                .info()
                .name;
            let mut preset = NodePreset::capture(&node, row.name);
            preset.slug = slugify(&format!("{node_name} {}", row.name));
            Item::NodePreset(preset)
        })
        .collect()
}
