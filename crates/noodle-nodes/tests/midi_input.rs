//! MIDI messages from an input port play a synth through the MIDI In node,
//! and doing so never allocates on the audio thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{OUTPUT_ID, Processor, Registry, Settings, engine};
use noodle_io::MidiBus;
use noodle_nodes::{MIDI_IN_ID, TRACK_INPUT_ID, VOICE_MIX_ID, VOICES_ID, register_library};

struct Guarded;

thread_local! {
    static REALTIME: Cell<bool> = const { Cell::new(false) };
    static VIOLATIONS: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    let _ = REALTIME.try_with(|realtime| {
        if realtime.get() {
            VIOLATIONS.with(|v| v.set(v.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Guarded {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Guarded = Guarded;

fn realtime(f: impl FnOnce()) -> usize {
    REALTIME.set(true);
    f();
    REALTIME.set(false);
    VIOLATIONS.replace(0)
}

const SETTINGS: Settings = Settings {
    sample_rate: 48_000.0,
    max_frames: 256,
    channels: 1,
};

/// MIDI In → Mono Note → saw → VCA (enveloped by an ADSR) → Output.
fn synth() -> (Processor, MidiBus, impl Sized) {
    let (processor, library, controller) = synth_from(&[(MIDI_IN_ID, "out")]);
    (processor, library.midi, controller)
}

/// The same synth played from `sources`, each a node type and the event
/// output it plays from, in place of the MIDI In node. They are nodes 1 up;
/// the synth follows.
fn synth_from(
    sources: &[(&str, &str)],
) -> (Processor, noodle_nodes::Library, noodle_engine::Controller) {
    let mut registry = Registry::with_builtins();
    let library = register_library(&mut registry);
    let mut project = Project::new();
    let mut ids = Vec::new();
    let sources_end = sources.len();
    let mut nodes = vec![];
    for (type_id, _) in sources {
        nodes.push((*type_id, vec![]));
    }
    nodes.extend([
        ("noodle.event.mono", vec![]),
        ("noodle.osc.saw", vec![]),
        (
            "noodle.mod.adsr",
            vec![
                ("attack", 0.001),
                ("decay", 0.01),
                ("sustain", 1.0),
                ("release", 0.01),
            ],
        ),
        ("noodle.util.vca", vec![]),
        (OUTPUT_ID, vec![]),
    ]);
    for (type_id, params) in nodes {
        let id = NodeId(ids.len() as u64 + 1);
        let mut node = Node::new(type_id);
        for (key, value) in params {
            node = node.with_param(key, value);
        }
        Command::AddNode { id, node }.apply(&mut project).unwrap();
        ids.push(id);
    }
    let n = sources_end;
    for (i, (_, port)) in sources.iter().enumerate() {
        Command::Connect(Connection {
            from: Endpoint::new(ids[i], *port),
            to: Endpoint::new(ids[n], "in"),
        })
        .apply(&mut project)
        .unwrap();
    }
    for (from, port, to, to_port) in [
        (n, "pitch", n + 1, "frequency"),
        (n, "gate", n + 2, "gate"),
        (n + 1, "out", n + 3, "in"),
        (n + 2, "out", n + 3, "level"),
        (n + 3, "out", n + 4, "in"),
    ] {
        Command::Connect(Connection {
            from: Endpoint::new(ids[from], port),
            to: Endpoint::new(ids[to], to_port),
        })
        .apply(&mut project)
        .unwrap();
    }
    let (mut controller, processor) = engine(SETTINGS).unwrap();
    let diagnostics = controller.update_project(&project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    (processor, library, controller)
}

fn block(processor: &mut Processor) -> Vec<f32> {
    let mut out = vec![0.0; SETTINGS.max_frames];
    processor.process(&mut out);
    out
}

fn blocks(processor: &mut Processor, n: usize) -> Vec<f32> {
    (0..n).flat_map(|_| block(processor)).collect()
}

fn frequency(samples: &[f32]) -> f32 {
    let crossings = samples
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    crossings as f32 * SETTINGS.sample_rate / samples.len() as f32
}

#[test]
fn a_key_pressed_on_the_port_sounds_at_its_pitch_and_stops_when_released() {
    let (mut processor, midi, _controller) = synth();
    assert!(blocks(&mut processor, 4).iter().all(|&s| s == 0.0));

    // A4 is key 69, 440 Hz. The message plays in the next block.
    midi.send([0x90, 69, 100]);
    let _ = block(&mut processor);
    let sound = blocks(&mut processor, 20);
    assert!(sound.iter().any(|&s| s.abs() > 0.1));
    assert!(
        (frequency(&sound) - 440.0).abs() < 15.0,
        "{}",
        frequency(&sound)
    );

    // A fifth up while it is held plays the newer note.
    midi.send([0x90, 76, 100]);
    let _ = block(&mut processor);
    let sound = blocks(&mut processor, 20);
    assert!(
        (frequency(&sound) - 659.3).abs() < 20.0,
        "{}",
        frequency(&sound)
    );

    // Released: both notes off, and the sound dies away.
    midi.send([0x80, 76, 0]);
    midi.send([0x80, 69, 0]);
    let _ = blocks(&mut processor, 8);
    assert!(blocks(&mut processor, 4).iter().all(|&s| s.abs() < 1e-4));
}

#[test]
fn playing_from_the_port_never_touches_the_allocator() {
    let (mut processor, midi, _controller) = synth();
    let mut violations = 0;
    let mut out = vec![0.0; SETTINGS.max_frames];
    for round in 0..40u8 {
        // The port's thread sends; the audio thread allocates nothing taking
        // it in, whatever it carries.
        midi.send([0x90, 40 + round, 90]);
        midi.send([0xB0, 1, round]);
        midi.send([0xE0, 0, 0x40]);
        violations += realtime(|| processor.process(&mut out));
        midi.send([0x80, 40 + round, 0]);
        violations += realtime(|| processor.process(&mut out));
    }
    // A flood bigger than the queue is dropped, not stored.
    for i in 0..5_000u32 {
        midi.send([0xB0, 1, (i % 128) as u8]);
    }
    violations += realtime(|| processor.process(&mut out));
    assert_eq!(violations, 0, "the audio thread allocated or freed memory");
}

/// MIDI In → Voices (4) → saw → VCA (enveloped by a per-voice ADSR) → Voice
/// Mix → Output: a polyphonic synth played from a keyboard.
fn poly_synth() -> (Processor, MidiBus, impl Sized) {
    let mut registry = Registry::with_builtins();
    let library = register_library(&mut registry);
    let mut project = Project::new();
    let mut ids = Vec::new();
    for (type_id, params) in [
        (MIDI_IN_ID, vec![]),
        (VOICES_ID, vec![]),
        ("noodle.osc.sine", vec![]),
        (
            "noodle.mod.adsr",
            vec![
                ("attack", 0.001),
                ("decay", 0.01),
                ("sustain", 1.0),
                ("release", 0.01),
            ],
        ),
        ("noodle.util.vca", vec![]),
        (VOICE_MIX_ID, vec![]),
        (OUTPUT_ID, vec![]),
    ] {
        let id = NodeId(ids.len() as u64 + 1);
        let mut node = Node::new(type_id);
        for (key, value) in params {
            node = node.with_param(key, value);
        }
        if type_id == VOICES_ID {
            let mut config = Config::new();
            config.set("voices", Value::Int(4));
            node = node.with_config(config);
        }
        Command::AddNode { id, node }.apply(&mut project).unwrap();
        ids.push(id);
    }
    for (from, port, to, to_port) in [
        (0, "out", 1, "in"),
        (1, "pitch", 2, "frequency"),
        (1, "gate", 3, "gate"),
        (2, "out", 4, "in"),
        (3, "out", 4, "level"),
        (4, "out", 5, "in"),
        (5, "out", 6, "in"),
    ] {
        Command::Connect(Connection {
            from: Endpoint::new(ids[from], port),
            to: Endpoint::new(ids[to], to_port),
        })
        .apply(&mut project)
        .unwrap();
    }
    let (mut controller, processor) = engine(SETTINGS).unwrap();
    let diagnostics = controller.update_project(&project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    (processor, library.midi, controller)
}

/// How much of `samples` is a sine at `hz` (Goertzel).
fn strength(samples: &[f32], hz: f32) -> f32 {
    let w = std::f32::consts::TAU * hz / SETTINGS.sample_rate;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &x) in samples.iter().enumerate() {
        re += x * (w * i as f32).cos();
        im += x * (w * i as f32).sin();
    }
    2.0 * (re * re + im * im).sqrt() / samples.len() as f32
}

#[test]
fn a_chord_from_the_keyboard_sounds_every_note_and_releases_one_at_a_time() {
    let (mut processor, midi, _controller) = poly_synth();
    // A3, C#4 and E4: 220, 277.18 and 329.63 Hz. Whole periods are not
    // needed to tell them apart from the other tones at this length.
    for key in [57, 61, 64] {
        midi.send([0x90, key, 100]);
    }
    let _ = block(&mut processor);
    let sound = blocks(&mut processor, 40);
    let notes = [220.0, 277.18, 329.63];
    let heard: Vec<f32> = notes.iter().map(|&hz| strength(&sound, hz)).collect();
    let absent = strength(&sound, 400.0);
    for (hz, level) in notes.iter().zip(&heard) {
        assert!(*level > 0.1, "{hz} Hz is missing: {level}");
        assert!(absent < *level / 4.0, "{absent} vs {level} at {hz} Hz");
    }

    // Releasing the middle note leaves the other two sounding.
    midi.send([0x80, 61, 0]);
    let _ = blocks(&mut processor, 8);
    let sound = blocks(&mut processor, 40);
    // Leakage from its neighbours leaves a little at the released note's pitch.
    assert!(strength(&sound, 277.18) < heard[1] / 5.0);
    assert!(strength(&sound, 220.0) > 0.1);
    assert!(strength(&sound, 329.63) > 0.1);

    midi.send([0x80, 57, 0]);
    midi.send([0x80, 64, 0]);
    let _ = blocks(&mut processor, 8);
    assert!(blocks(&mut processor, 4).iter().all(|&s| s.abs() < 1e-4));
}

#[test]
fn more_keys_than_voices_steal_without_stopping_the_sound_or_allocating() {
    let (mut processor, midi, _controller) = poly_synth();
    let mut violations = 0;
    let mut out = vec![0.0; SETTINGS.max_frames];
    for round in 0..30u8 {
        // Seven keys down at once on four voices, then all up.
        for key in 0..7u8 {
            midi.send([0x90, 48 + round + key, 100]);
        }
        violations += realtime(|| processor.process(&mut out));
        violations += realtime(|| processor.process(&mut out));
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(out.iter().any(|&s| s.abs() > 0.05), "round {round}");
        for key in 0..7u8 {
            midi.send([0x80, 48 + round + key, 0]);
        }
        violations += realtime(|| processor.process(&mut out));
    }
    assert_eq!(violations, 0, "the audio thread allocated or freed memory");
}

/// A synth played by an armed track's own live MIDI, with the timeline
/// stopped, and no MIDI In node anywhere.
#[test]
fn an_armed_track_plays_its_synth_from_the_keyboard_with_the_transport_stopped() {
    let (mut processor, library, controller) = synth_from(&[(TRACK_INPUT_ID, "midi")]);
    controller.transport().stop();
    let track = NodeId(1);
    // Not armed: the keys do nothing.
    library.midi.send([0x90, 69, 100]);
    assert!(blocks(&mut processor, 8).iter().all(|&s| s == 0.0));
    library.midi.send([0x80, 69, 0]);
    let _ = blocks(&mut processor, 8);

    library.clips.set_monitoring(track, true);
    library.midi.send([0x93, 69, 100]);
    let _ = block(&mut processor);
    let sound = blocks(&mut processor, 20);
    assert!(sound.iter().any(|&s| s.abs() > 0.1));
    assert!(
        (frequency(&sound) - 440.0).abs() < 15.0,
        "{}",
        frequency(&sound)
    );

    // Starting and stopping the timeline leaves the held note sounding.
    controller.transport().play();
    assert!(blocks(&mut processor, 8).iter().any(|&s| s.abs() > 0.1));
    controller.transport().stop();
    assert!(blocks(&mut processor, 8).iter().any(|&s| s.abs() > 0.1));

    // Disarming lets it go.
    library.clips.set_monitoring(track, false);
    let _ = blocks(&mut processor, 8);
    assert!(blocks(&mut processor, 4).iter().all(|&s| s.abs() < 1e-4));
}

/// A MIDI In node wired to the same synth as an armed track sends every note
/// twice, under note IDs of their own (a polyphonic synth sounds it twice).
/// Allowed, and documented; what matters is that nothing is left hanging.
#[test]
fn a_wired_midi_in_and_an_armed_track_both_play_and_both_release() {
    let (mut processor, library, controller) =
        synth_from(&[(MIDI_IN_ID, "out"), (TRACK_INPUT_ID, "midi")]);
    controller.transport().stop();
    library.clips.set_monitoring(NodeId(2), true);
    library.midi.send([0x90, 69, 100]);
    let _ = block(&mut processor);
    let sound = blocks(&mut processor, 20);
    assert!(sound.iter().any(|&s| s.abs() > 0.1));
    // Released, both notes end: nothing is left hanging.
    library.midi.send([0x80, 69, 0]);
    let _ = blocks(&mut processor, 8);
    assert!(blocks(&mut processor, 4).iter().all(|&s| s.abs() < 1e-4));
}
