//! Built-in node library: oscillators, filters, mixing, utilities, and views
//! (meters and scopes) that report to the UI.

mod adsr;
mod arpeggiator;
mod automation;
mod background;
mod beat_length;
mod bitcrush;
mod button;
mod cached;
mod chorus;
mod clip;
mod clock;
mod comparator;
mod control;
mod curve;
mod delay;
mod distortion;
mod dynamics;
mod envelope;
mod eq;
mod export;
mod follower;
mod freeze;
mod fundamental;
mod gain;
mod ladder;
mod lag;
mod lfo;
mod math;
mod meter;
mod metronome;
mod midi_in;
mod mix;
mod noise;
mod normalize;
mod note_split;
mod notes;
mod osc;
mod pan;
mod phaser;
mod random_select;
mod remap;
mod reroute;
mod reverb;
mod reverse;
mod sample;
mod sample_hold;
mod sample_input;
mod sampler;
mod sampler_stretch;
mod scope;
mod script;
mod spectral;
mod spectrum;
mod stage;
mod surround;
mod svf;
mod tempo;
mod time_stretch;
mod vca;
mod voice_mix;
mod voices;

pub use adsr::{ADSR_ID, Adsr, AdsrShape};
pub use arpeggiator::{
    ARPEGGIATOR_ID, Arpeggiator, RATE_NAMES as ARP_RATE_NAMES, RATE_TICKS as ARP_RATE_TICKS,
};
pub use automation::{AUTOMATION_NODE_ID, AUTOMATION_VALUE, Automation};
pub use background::{
    CHUNK_FRAMES, CacheRender, ExtendRegistry, RenderReport, RenderRequest, profile,
    render_streaming, spawn_render, spawn_render_to_cache,
};
pub use beat_length::{BEAT_LENGTH_ID, BeatLength};
pub use bitcrush::{BITCRUSHER_ID, Bitcrusher, quantise};
pub use button::{BUTTON_ID, BUTTON_STATE, Button};
pub use cached::{CACHED_ID, CachedPlayer};
pub use chorus::{CHORUS_ID, Chorus};
pub use clip::{
    ClipFeeds, ClipProblem, ClipSource, ClipStatus, FileError, Schedule, ScheduledClip,
    StretchSource, Stretcher, TRACK_INPUT_ID, TrackInput, WarpMap, active_at,
};
pub use clock::{CLOCK_ID, Clock, DIVISION_TICKS, DIVISIONS as CLOCK_DIVISIONS};
pub use comparator::{COMPARATOR_ID, Comparator};
pub use control::{
    MIDI_CONTROL_ID, MidiControl, PITCH_BEND as CONTROL_PITCH_BEND, PRESSURE as CONTROL_PRESSURE,
};
pub use curve::{Curve, CurvePoint, Handle, LUT_STEPS, lookup};
pub use delay::{DELAY_ID, Delay, MAX_TIME as DELAY_MAX_TIME};
pub use distortion::{DISTORTION_ID, Distortion, SHAPES as DISTORTION_SHAPES, shaper};
pub use dynamics::{
    DYNAMICS_ID, Dynamics, DynamicsMode, MAX_LOOKAHEAD_MS, attack_response, reduction_db,
    release_response, transfer_db,
};
pub use envelope::{
    ENVELOPE_CURVE_KEY, ENVELOPE_ID, ENVELOPE_SINGLE_KEY, Envelope, EnvelopeCurve, EnvelopePoint,
    is_single as envelope_is_single,
};
pub use eq::{
    EQ_ID, EQ_TYPES, EqBand, EqType, MAX_BANDS as EQ_MAX_BANDS, ParametricEq,
    default_freq as eq_default_freq, eq_response_db, param_index as eq_param,
};
pub use export::{ExportJobError, ExportReport, ExportRequest, export, spawn_export};
pub use follower::{EnvelopeFollower, FOLLOWER_ID};
pub use freeze::{
    FreezeError, FreezePlan, FreezeReport, Freezer, TargetState, default_cache_dir, freeze,
    spawn_freeze,
};
pub use fundamental::{LISTEN_SECONDS, fundamental, key_of};
pub use gain::{GAIN_ID, Gain};
pub use ladder::{LADDER_ID, LADDER_SELF_OSCILLATES, Ladder, ladder_response};
pub use lag::{LAG_ID, Lag};
pub use lfo::{LFO_ID, Lfo, lfo_wave};
pub use math::{MATH_ID, Math};
pub use meter::{METER_ID, Meter};
pub use metronome::{METRONOME_ID, METRONOME_ON, Metronome};
pub use midi_in::{MIDI_IN_ID, MidiIn};
pub use mix::Mix;
pub use noise::WhiteNoise;
pub use normalize::{NORMALIZE_ID, Normalize};
pub use note_split::{NOTE_SPLIT_ID, NoteSplit, param_index as note_split_param};
pub use notes::{KEY_ID, Key, MONO_NOTE_ID, MonoNote};
pub use osc::{Saw, Sine, Square, Triangle};
pub use pan::{PAN_ID, Pan};
pub use phaser::{PHASER_ID, Phaser, STAGES as PHASER_STAGES};
pub use random_select::{MAX_INPUTS as RANDOM_SELECT_MAX_INPUTS, RANDOM_SELECT_ID, RandomSelect};
pub use remap::{REMAP_CURVE_KEY, REMAP_ID, Remap};
pub use reroute::{REROUTE_ID, Reroute};
pub use reverb::{MAX_PREDELAY as REVERB_MAX_PREDELAY, REVERB_ID, Reverb};
pub use reverse::Reverse;
pub use sample::{Sample, SampleError, SampleRef, SampleStore};
pub use sample_hold::{SAMPLE_HOLD_ID, SampleHold};
pub use sample_input::{SAMPLE_INPUT_ID, SampleInput};
pub use sampler::{GAIN_ENV_KEY, GAIN_ENV_SCALE, SAMPLE_KEY, SAMPLER_ID, Sampler, WARP_KEY};
pub use scope::{SCOPE_ID, Scope};
pub use script::{
    DEFAULT_SCRIPT, MAX_INPUTS as SCRIPT_MAX_INPUTS, MAX_OUTPUTS as SCRIPT_MAX_OUTPUTS, SCRIPT_ID,
    SCRIPT_KEY, Script, ScriptPorts, check as check_script,
};
pub use spectral::{
    ANALYZE_ID, Analyze, CROSS_ID, Cross, SPECTRAL_BLUR_ID, SPECTRAL_FILTER_ID, SPECTRAL_FREEZE_ID,
    SPECTRAL_GATE_ID, SPECTRAL_SHIFT_ID, SYNTHESIZE_ID, SpectralBlur, SpectralFilter,
    SpectralFreeze, SpectralGate, SpectralShift, Synthesize,
};
pub use spectrum::{
    FLOOR_DB as SPECTRUM_FLOOR_DB, HIGH_HZ as SPECTRUM_HIGH_HZ, LOW_HZ as SPECTRUM_LOW_HZ,
    SPECTRUM_ID, Spectrum, SpectrumAnalyser, WINDOW as SPECTRUM_WINDOW, column_frequency,
    columns_from_bins,
};
pub use stage::GroupStage;
pub use surround::{
    CONVERT_LAYOUT_ID, ConvertLayout, LAYOUT_KEY, MERGE_CHANNELS_ID, MergeChannels,
    SPLIT_CHANNELS_ID, SURROUND_PAN_ID, SplitChannels, SurroundPan, default_layout,
};
pub use svf::{SVF_ID, Svf, svf_response};
pub use tempo::{TEMPO_ID, Tempo};
pub use time_stretch::{TIME_STRETCH_ID, TimeStretch};
pub use vca::{VCA_ID, Vca};
pub use voice_mix::{VOICE_MIX_ID, VoiceMix};
pub use voices::{VOICES_ID, Voices};

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
    /// Where MIDI In nodes get their messages from; connect a port to it
    /// with [`noodle_io::connect_midi`].
    pub midi: noodle_io::MidiBus,
    /// Where Sampler nodes get their samples, by hash. Give it a folder
    /// ([`SampleStore::set_dir`]) to read them from disk.
    pub samples: SampleStore,
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
    registry.register(Square);
    registry.register(Triangle);
    registry.register(Pan);
    registry.register(SurroundPan);
    registry.register(ConvertLayout);
    registry.register(SplitChannels);
    registry.register(MergeChannels);
    registry.register(WhiteNoise);
    registry.register(Gain::new(&telemetry));
    registry.register(Button);
    registry.register(Metronome);
    registry.register(Mix::new(&telemetry));
    // The builtin Output has no scope; this one reports to the hub.
    registry.replace(noodle_engine::Output::new(&telemetry));
    registry.register(Svf);
    registry.register(Ladder);
    registry.register(Vca);
    registry.register(Math);
    registry.register(Script);
    registry.register(Delay);
    registry.register(Dynamics::new(&telemetry));
    registry.register(ParametricEq);
    registry.register(Reverb);
    registry.register(Remap);
    registry.register(Adsr);
    registry.register(Lfo);
    registry.register(Envelope);
    registry.register(EnvelopeFollower);
    registry.register(SampleHold);
    registry.register(Lag);
    registry.register(Analyze);
    registry.register(Synthesize);
    registry.register(SpectralGate);
    registry.register(SpectralFilter);
    registry.register(SpectralShift);
    registry.register(SpectralFreeze);
    registry.register(SpectralBlur);
    registry.register(Cross);
    registry.register(Comparator);
    registry.register(Automation);
    registry.register(Clock);
    registry.register(Tempo);
    registry.register(BeatLength);
    registry.register(SampleInput);
    registry.register(Distortion);
    registry.register(Bitcrusher);
    registry.register(Chorus);
    registry.register(Phaser);
    registry.register(Key);
    registry.register(MonoNote);
    registry.register(MidiControl);
    registry.register(NoteSplit);
    registry.register(RandomSelect);
    registry.register(Arpeggiator);
    registry.register(Voices);
    registry.register(VoiceMix::new(&telemetry));
    registry.register(Reverse);
    registry.register(Normalize);
    registry.register(TimeStretch);
    registry.register(Reroute);
    registry.register(GroupStage::new(&telemetry));
    registry.register(Meter::new(&telemetry));
    registry.register(Scope::new(&telemetry));
    registry.register(Spectrum::new(&telemetry));
    let midi = noodle_io::MidiBus::new();
    registry.register(TrackInput::with_midi(&clips, &midi));
    registry.register(MidiIn::new(&midi));
    let samples = SampleStore::new();
    registry.register(if clips.is_blocking() {
        Sampler::blocking(&samples, &telemetry)
    } else {
        Sampler::new(&samples, &telemetry)
    });
    Library {
        telemetry,
        clips,
        midi,
        samples,
    }
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
    library.samples.set_dir(base.join("samples"));
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
