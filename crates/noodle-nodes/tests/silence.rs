//! Silence skipping end to end: voices that aren't sounding cost nothing,
//! lane by lane, and nothing is skipped while it still sounds.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use noodle_core::{Command, Config, Connection, Endpoint, Node, NodeId, Project, Value};
use noodle_engine::{
    Context, Instance, Lane, LaneKernel, NodeError, NodeInfo, NodeType, OUTPUT_ID, PerLane,
    Processor, Registry, Settings, Setup, Skip, engine,
};
use noodle_io::MidiBus;
use noodle_nodes::{MIDI_IN_ID, VOICE_MIX_ID, VOICES_ID, register_library};

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

/// A pass-through that counts the lanes it actually processes, and opts in to
/// skipping like any effect would.
struct Counter(Arc<AtomicUsize>);

static COUNTER_INFO: NodeInfo = NodeInfo {
    id: "test.counter",
    version: 1,
    name: "Counter",
    category: "Test",
};

impl NodeType for Counter {
    fn info(&self) -> &NodeInfo {
        &COUNTER_INFO
    }

    fn layout(&self, _config: &Config) -> Result<noodle_engine::Layout, NodeError> {
        Ok(noodle_engine::Layout::realtime()
            .input("in", "In")
            .output("out", "Out"))
    }

    fn instantiate(&self, setup: &Setup<'_>) -> Result<Instance, NodeError> {
        Ok(Instance::realtime(PerLane::new(
            CountKernel(Arc::clone(&self.0)),
            setup,
        )))
    }
}

struct CountKernel(Arc<AtomicUsize>);

impl LaneKernel for CountKernel {
    type State = ();

    fn skip(&self) -> Skip {
        Skip::AnySilent(&[0])
    }

    fn process_lane(&mut self, _: &mut (), _: &Context, mut lane: Lane<'_, '_>) {
        self.0.fetch_add(1, Ordering::Relaxed);
        let input = lane.inputs.get(0);
        lane.outputs.get_mut(0).copy_from_slice(input);
    }
}

/// MIDI In → Voices (4, tail `tail`) → sine → VCA (enveloped by an ADSR with
/// a 10 ms release) → Counter → Voice Mix → Output.
fn synth(tail: f32) -> (Processor, MidiBus, Arc<AtomicUsize>, impl Sized) {
    let mut registry = Registry::with_builtins();
    let library = register_library(&mut registry);
    let counted = Arc::new(AtomicUsize::new(0));
    registry.register(Counter(Arc::clone(&counted)));
    let mut project = Project::new();
    let mut ids = Vec::new();
    for (type_id, params) in [
        (MIDI_IN_ID, vec![]),
        (VOICES_ID, vec![("tail", tail)]),
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
        ("test.counter", vec![]),
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
        (6, "out", 7, "in"),
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
    (processor, library.midi, counted, controller)
}

fn block(processor: &mut Processor) -> Vec<f32> {
    let mut out = vec![0.0; SETTINGS.max_frames];
    processor.process(&mut out);
    out
}

/// Runs one block and says how many lanes the counter processed in it.
fn lanes_in_block(processor: &mut Processor, counted: &AtomicUsize) -> (usize, Vec<f32>) {
    let before = counted.load(Ordering::Relaxed);
    let out = block(processor);
    (counted.load(Ordering::Relaxed) - before, out)
}

#[test]
fn an_idle_synth_processes_no_voices() {
    let (mut processor, _midi, counted, _controller) = synth(10.0);
    // The very first block is already skipped: nothing has played.
    for _ in 0..8 {
        let (lanes, out) = lanes_in_block(&mut processor, &counted);
        assert_eq!(lanes, 0);
        assert!(out.iter().all(|&s| s == 0.0));
    }
}

#[test]
fn only_the_sounding_voices_are_processed_and_a_release_rings_out_first() {
    let (mut processor, midi, counted, _controller) = synth(10.0);
    let _ = block(&mut processor);

    midi.send([0x90, 60, 100]);
    let _ = block(&mut processor);
    midi.send([0x90, 64, 100]);
    let _ = block(&mut processor);
    for _ in 0..4 {
        let (lanes, out) = lanes_in_block(&mut processor, &counted);
        assert_eq!(lanes, 2, "two notes held, two voices processed");
        assert!(out.iter().any(|&s| s.abs() > 0.1));
    }

    // One key up: its envelope releases (10 ms is 2 blocks), so it keeps
    // processing until the tail has gone, and the sound is not cut short.
    midi.send([0x80, 60, 0]);
    let (lanes, _) = lanes_in_block(&mut processor, &counted);
    assert_eq!(lanes, 2);
    let mut release = Vec::new();
    for _ in 0..6 {
        release.push(lanes_in_block(&mut processor, &counted).0);
    }
    assert_eq!(release[0], 2, "still releasing: {release:?}");
    assert_eq!(
        *release.last().unwrap(),
        1,
        "released voice skipped: {release:?}"
    );

    midi.send([0x80, 64, 0]);
    let mut last = 2;
    for _ in 0..8 {
        last = lanes_in_block(&mut processor, &counted).0;
    }
    assert_eq!(last, 0, "everything released and skipped");
}

#[test]
fn a_skipped_voice_starts_cleanly_when_a_note_takes_it() {
    // A tail of 0 makes released voices inactive at once, so the oscillator is
    // skipped too, not just the envelope's tail.
    let (mut processor, midi, counted, _controller) = synth(0.0);
    for round in 0..3 {
        midi.send([0x90, 69, 100]);
        let _ = block(&mut processor);
        let (lanes, out) = lanes_in_block(&mut processor, &counted);
        assert_eq!(lanes, 1, "round {round}");
        assert!(out.iter().any(|&s| s.abs() > 0.1), "round {round}");
        midi.send([0x80, 69, 0]);
        for _ in 0..8 {
            let _ = block(&mut processor);
        }
        let (lanes, out) = lanes_in_block(&mut processor, &counted);
        assert_eq!(lanes, 0, "round {round}");
        assert!(out.iter().all(|&s| s == 0.0));
    }
}

#[test]
fn skipping_never_allocates_on_the_audio_thread() {
    let (mut processor, midi, _counted, _controller) = synth(0.01);
    let mut violations = 0;
    let mut out = vec![0.0; SETTINGS.max_frames];
    for round in 0..40u8 {
        midi.send([0x90, 40 + round, 90]);
        midi.send([0x90, 52 + round % 5, 90]);
        violations += realtime(|| processor.process(&mut out));
        midi.send([0x80, 40 + round, 0]);
        for _ in 0..4 {
            violations += realtime(|| processor.process(&mut out));
        }
    }
    assert_eq!(violations, 0, "the audio thread allocated or freed memory");
}

/// MIDI In → Voices (4) → unison saw → ladder (swept by an ADSR) → VCA (amp
/// ADSR) → Voice Mix → Output: the subtractive synth, played from a keyboard.
fn subtractive() -> (Processor, MidiBus, impl Sized) {
    let mut registry = Registry::with_builtins();
    let library = register_library(&mut registry);
    let mut project = Project::new();
    let mut ids = Vec::new();
    for (type_id, params) in [
        (MIDI_IN_ID, vec![]),
        (VOICES_ID, vec![("tail", 0.05)]),
        ("noodle.osc.unison_saw", vec![]),
        ("noodle.filter.ladder", vec![("resonance", 0.6)]),
        (
            "noodle.mod.adsr",
            vec![("attack", 0.001), ("release", 0.02)],
        ),
        (
            "noodle.mod.adsr",
            vec![("attack", 0.001), ("release", 0.02)],
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
        (2, "out", 3, "in"),
        (1, "gate", 4, "gate"),
        (4, "out", 3, "cutoff"),
        (1, "gate", 5, "gate"),
        (3, "out", 6, "in"),
        (5, "out", 6, "level"),
        (6, "out", 7, "in"),
        (7, "out", 8, "in"),
    ] {
        Command::Connect(Connection {
            from: Endpoint::new(ids[from], port),
            to: Endpoint::new(ids[to], to_port),
        })
        .apply(&mut project)
        .unwrap();
    }
    let (mut controller, processor) = engine(Settings {
        channels: 2,
        ..SETTINGS
    })
    .unwrap();
    let diagnostics = controller.update_project(&project, &registry);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    (processor, library.midi, controller)
}

#[test]
fn the_subtractive_synth_sounds_in_stereo_and_never_allocates() {
    let (mut processor, midi, _controller) = subtractive();
    let mut out = vec![0.0; SETTINGS.max_frames * 2];
    let mut violations = 0;
    let mut heard = false;
    for round in 0..30u8 {
        midi.send([0x90, 48 + round % 12, 100]);
        midi.send([0x90, 60 + round % 7, 100]);
        violations += realtime(|| processor.process(&mut out));
        heard |= out.iter().any(|&s| s.abs() > 0.05);
        assert!(out.iter().all(|s| s.is_finite()));
        midi.send([0x80, 48 + round % 12, 0]);
        midi.send([0x80, 60 + round % 7, 0]);
        for _ in 0..6 {
            violations += realtime(|| processor.process(&mut out));
        }
    }
    assert!(heard);
    assert_eq!(violations, 0, "the audio thread allocated or freed memory");
    // Everything released and past its tail: silence.
    for _ in 0..40 {
        processor.process(&mut out);
    }
    assert!(out.iter().all(|&s| s == 0.0));
}
